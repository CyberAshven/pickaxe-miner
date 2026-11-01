//! #### PR #32
//! OpenCL engine for GPUs the other engines cannot drive: older integrated
//! GPUs without Vulkan or DirectX 12, older discrete cards, and ARM GPUs
//! (Mali, Adreno) with an OpenCL driver. The OpenCL library is loaded at
//! runtime, so a machine without one still runs every other engine.
//!
//! T2 only: the CPU makes each window's signature (one per 65,536
//! candidates) with the same signing code that verifies winners, and the GPU
//! hashes every candidate of the window in OpenCL C (`opencl_t2.cl`), with
//! the shared Rust T2 filter's transaction layout, amount coordinate and
//! target rule.

use crate::backend::{GpuDevice, PciAddress};
use crate::backend_kind::BackendKind;
use crate::crypto;
use crate::gpu_types::{
    PhotonCudaBatchResult, PhotonCudaWinner, OPENCL_MAX_BATCH_CANDIDATES, THROTTLED_BATCH_DIVISOR,
};
use crate::protocol::ProofRule;
use crate::tx::{self, PhotonLayout};
use libloading::Library;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};
use std::ptr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const KERNEL_SOURCE: &str = include_str!("opencl_t2.cl");
/// Candidates per T2 window: one signature covers 65,536 amount values.
const WINDOW: u32 = 65_536;
/// The padded tail after the first 448 bytes: blocks 7, 8 and 9.
const TAIL_WORDS: usize = 48;
/// Batches start at one window and double while they finish quickly.
const START_BATCH: u32 = WINDOW;
const ESCALATE_BATCH: Duration = Duration::from_millis(150);
const TARGET_BATCH: Duration = Duration::from_millis(400);
/// Signed windows kept between batches, so a window split across batches is
/// signed once.
const WINDOW_CACHE: usize = 64;

type ClInt = i32;
type ClUint = u32;
type Handle = *mut c_void;

const CL_SUCCESS: ClInt = 0;
const CL_TRUE: ClUint = 1;
const CL_DEVICE_TYPE_GPU: u64 = 1 << 2;
const CL_PLATFORM_NAME: ClUint = 0x0902;
const CL_DEVICE_MAX_COMPUTE_UNITS: ClUint = 0x1002;
const CL_DEVICE_GLOBAL_MEM_SIZE: ClUint = 0x101f;
const CL_DEVICE_NAME: ClUint = 0x102b;
const CL_DEVICE_VENDOR: ClUint = 0x102c;
const CL_DEVICE_VERSION: ClUint = 0x102f;
const CL_DEVICE_HOST_UNIFIED_MEMORY: ClUint = 0x1035;
const CL_DEVICE_PCI_BUS_ID_NV: ClUint = 0x4008;
const CL_DEVICE_PCI_SLOT_ID_NV: ClUint = 0x4009;
const CL_DEVICE_PCI_DOMAIN_ID_NV: ClUint = 0x400a;
const CL_DEVICE_TOPOLOGY_AMD: ClUint = 0x4037;
const CL_DEVICE_BOARD_NAME_AMD: ClUint = 0x4038;
const CL_DEVICE_PCI_BUS_INFO_KHR: ClUint = 0x410f;
const CL_PROGRAM_BUILD_LOG: ClUint = 0x1183;
const CL_MEM_READ_WRITE: u64 = 1;
const CL_MEM_READ_ONLY: u64 = 1 << 2;

type InfoFn = unsafe extern "system" fn(Handle, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type ReleaseFn = unsafe extern "system" fn(Handle) -> ClInt;
type TransferFn = unsafe extern "system" fn(
    Handle,
    Handle,
    ClUint,
    usize,
    usize,
    *mut c_void,
    ClUint,
    *const Handle,
    *mut Handle,
) -> ClInt;

type PlatformIdsFn = unsafe extern "system" fn(ClUint, *mut Handle, *mut ClUint) -> ClInt;
type DeviceIdsFn =
    unsafe extern "system" fn(Handle, u64, ClUint, *mut Handle, *mut ClUint) -> ClInt;
type CreateContextFn = unsafe extern "system" fn(
    *const isize,
    ClUint,
    *const Handle,
    *const c_void,
    *mut c_void,
    *mut ClInt,
) -> Handle;
type CreateQueueFn = unsafe extern "system" fn(Handle, Handle, u64, *mut ClInt) -> Handle;
type CreateProgramFn = unsafe extern "system" fn(
    Handle,
    ClUint,
    *const *const c_char,
    *const usize,
    *mut ClInt,
) -> Handle;
type BuildProgramFn = unsafe extern "system" fn(
    Handle,
    ClUint,
    *const Handle,
    *const c_char,
    *const c_void,
    *mut c_void,
) -> ClInt;
type BuildInfoFn =
    unsafe extern "system" fn(Handle, Handle, ClUint, usize, *mut c_void, *mut usize) -> ClInt;
type CreateKernelFn = unsafe extern "system" fn(Handle, *const c_char, *mut ClInt) -> Handle;
type CreateBufferFn =
    unsafe extern "system" fn(Handle, u64, usize, *mut c_void, *mut ClInt) -> Handle;
type KernelArgFn = unsafe extern "system" fn(Handle, ClUint, usize, *const c_void) -> ClInt;
type LaunchFn = unsafe extern "system" fn(
    Handle,
    Handle,
    ClUint,
    *const usize,
    *const usize,
    *const usize,
    ClUint,
    *const Handle,
    *mut Handle,
) -> ClInt;

/// The OpenCL entry points Pickaxe uses, from the installed driver.
struct OpenCl {
    get_platform_ids: PlatformIdsFn,
    get_platform_info: InfoFn,
    get_device_ids: DeviceIdsFn,
    get_device_info: InfoFn,
    create_context: CreateContextFn,
    create_command_queue: CreateQueueFn,
    create_program_with_source: CreateProgramFn,
    build_program: BuildProgramFn,
    get_program_build_info: BuildInfoFn,
    create_kernel: CreateKernelFn,
    create_buffer: CreateBufferFn,
    set_kernel_arg: KernelArgFn,
    enqueue_write_buffer: TransferFn,
    enqueue_read_buffer: TransferFn,
    enqueue_nd_range_kernel: LaunchFn,
    finish: ReleaseFn,
    release_mem_object: ReleaseFn,
    release_kernel: ReleaseFn,
    release_program: ReleaseFn,
    release_command_queue: ReleaseFn,
    release_context: ReleaseFn,
    // Kept loaded for the process's lifetime; the entry points point into it.
    _library: Library,
}

// SAFETY: the entry points are plain function pointers into a library that
// stays loaded; OpenCL's API is thread-safe apart from setting one kernel's
// arguments, which each engine does from its own thread only.
unsafe impl Send for OpenCl {}
unsafe impl Sync for OpenCl {}

impl OpenCl {
    fn load() -> Result<Self, String> {
        let names: &[&str] = if cfg!(windows) {
            &["OpenCL.dll"]
        } else if cfg!(target_os = "macos") {
            &["/System/Library/Frameworks/OpenCL.framework/OpenCL"]
        } else {
            &["libOpenCL.so.1", "libOpenCL.so"]
        };
        // SAFETY: loading the platform's OpenCL ICD loader runs only its
        // documented initialization.
        let library = names
            .iter()
            .find_map(|name| unsafe { Library::new(*name) }.ok())
            .ok_or("no OpenCL driver is installed")?;
        macro_rules! symbol {
            ($name:literal) => {
                // SAFETY: each symbol has the C signature of the field it
                // fills, as declared in the Khronos OpenCL 1.2 headers.
                *unsafe { library.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|_| format!("the OpenCL driver lacks {}", $name))?
            };
        }
        Ok(Self {
            get_platform_ids: symbol!("clGetPlatformIDs"),
            get_platform_info: symbol!("clGetPlatformInfo"),
            get_device_ids: symbol!("clGetDeviceIDs"),
            get_device_info: symbol!("clGetDeviceInfo"),
            create_context: symbol!("clCreateContext"),
            create_command_queue: symbol!("clCreateCommandQueue"),
            create_program_with_source: symbol!("clCreateProgramWithSource"),
            build_program: symbol!("clBuildProgram"),
            get_program_build_info: symbol!("clGetProgramBuildInfo"),
            create_kernel: symbol!("clCreateKernel"),
            create_buffer: symbol!("clCreateBuffer"),
            set_kernel_arg: symbol!("clSetKernelArg"),
            enqueue_write_buffer: symbol!("clEnqueueWriteBuffer"),
            enqueue_read_buffer: symbol!("clEnqueueReadBuffer"),
            enqueue_nd_range_kernel: symbol!("clEnqueueNDRangeKernel"),
            finish: symbol!("clFinish"),
            release_mem_object: symbol!("clReleaseMemObject"),
            release_kernel: symbol!("clReleaseKernel"),
            release_program: symbol!("clReleaseProgram"),
            release_command_queue: symbol!("clReleaseCommandQueue"),
            release_context: symbol!("clReleaseContext"),
            _library: library,
        })
    }
}

/// The process-wide OpenCL driver, loaded on first use.
fn opencl() -> Result<&'static OpenCl, String> {
    static DRIVER: OnceLock<Result<OpenCl, String>> = OnceLock::new();
    DRIVER
        .get_or_init(OpenCl::load)
        .as_ref()
        .map_err(Clone::clone)
}

fn check(code: ClInt, operation: &str) -> Result<(), String> {
    if code == CL_SUCCESS {
        Ok(())
    } else {
        Err(format!("OpenCL {operation} failed (error {code})"))
    }
}

/// Reads a string property of a platform or device.
fn info_string(get: InfoFn, object: Handle, param: ClUint) -> Option<String> {
    let mut size = 0usize;
    // SAFETY: a size query writes only `size`.
    if unsafe { get(object, param, 0, ptr::null_mut(), &mut size) } != CL_SUCCESS || size == 0 {
        return None;
    }
    let mut bytes = vec![0u8; size];
    // SAFETY: the buffer holds the reported size.
    if unsafe {
        get(
            object,
            param,
            size,
            bytes.as_mut_ptr().cast(),
            ptr::null_mut(),
        )
    } != CL_SUCCESS
    {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).trim().to_owned())
}

/// Reads a fixed-size property of a device.
fn info_value<T: Copy + Default>(get: InfoFn, object: Handle, param: ClUint) -> Option<T> {
    let mut value = T::default();
    // SAFETY: the driver writes at most `size_of::<T>()` bytes into `value`.
    let code = unsafe {
        get(
            object,
            param,
            std::mem::size_of::<T>(),
            (&mut value as *mut T).cast(),
            ptr::null_mut(),
        )
    };
    (code == CL_SUCCESS).then_some(value)
}

/// One OpenCL GPU, in enumeration order across platforms.
struct Found {
    device: Handle,
    name: String,
    vendor: String,
    platform: String,
    version: String,
    memory: u64,
    units: u32,
    integrated: bool,
    pci: Option<PciAddress>,
}

fn find_devices(cl: &OpenCl) -> Result<Vec<Found>, String> {
    let mut count = 0;
    // SAFETY: a count query writes only `count`.
    let code = unsafe { (cl.get_platform_ids)(0, ptr::null_mut(), &mut count) };
    if code != CL_SUCCESS || count == 0 {
        return Err("no OpenCL platform found".into());
    }
    let mut platforms = vec![ptr::null_mut(); count as usize];
    // SAFETY: the array holds `count` handles.
    check(
        unsafe { (cl.get_platform_ids)(count, platforms.as_mut_ptr(), ptr::null_mut()) },
        "platform listing",
    )?;
    let mut found = Vec::new();
    for platform in platforms {
        let platform_name =
            info_string(cl.get_platform_info, platform, CL_PLATFORM_NAME).unwrap_or_default();
        let mut devices = 0;
        // SAFETY: a count query writes only `devices`.
        let code = unsafe {
            (cl.get_device_ids)(
                platform,
                CL_DEVICE_TYPE_GPU,
                0,
                ptr::null_mut(),
                &mut devices,
            )
        };
        if code != CL_SUCCESS || devices == 0 {
            continue;
        }
        let mut handles = vec![ptr::null_mut(); devices as usize];
        // SAFETY: the array holds `devices` handles.
        let code = unsafe {
            (cl.get_device_ids)(
                platform,
                CL_DEVICE_TYPE_GPU,
                devices,
                handles.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        if code != CL_SUCCESS {
            continue;
        }
        for device in handles {
            let get = cl.get_device_info;
            let vendor = info_string(get, device, CL_DEVICE_VENDOR).unwrap_or_default();
            // AMD names its devices by architecture ("gfx1036"); the board name
            // matches the other engines' names.
            let name = info_string(get, device, CL_DEVICE_BOARD_NAME_AMD)
                .filter(|name| !name.is_empty())
                .or_else(|| info_string(get, device, CL_DEVICE_NAME))
                .unwrap_or_else(|| "OpenCL GPU".into());
            found.push(Found {
                device,
                pci: device_pci(get, device),
                name,
                vendor,
                platform: platform_name.clone(),
                version: info_string(get, device, CL_DEVICE_VERSION).unwrap_or_default(),
                memory: info_value::<u64>(get, device, CL_DEVICE_GLOBAL_MEM_SIZE).unwrap_or(0),
                units: info_value::<u32>(get, device, CL_DEVICE_MAX_COMPUTE_UNITS).unwrap_or(0),
                integrated: info_value::<u32>(get, device, CL_DEVICE_HOST_UNIFIED_MEMORY)
                    .is_some_and(|unified| unified != 0),
            });
        }
    }
    Ok(found)
}

/// The device's PCI location from the Khronos, AMD or NVIDIA extension.
fn device_pci(get: InfoFn, device: Handle) -> Option<PciAddress> {
    if let Some(info) = info_value::<[u32; 4]>(get, device, CL_DEVICE_PCI_BUS_INFO_KHR) {
        return Some(PciAddress {
            domain: info[0],
            bus: info[1] as u8,
            device: info[2] as u8,
            function: info[3] as u8,
        });
    }
    // cl_device_topology_amd: a type word (1 is PCIe), then bus, device and
    // function in the last three bytes of 24.
    if let Some(topology) = info_value::<[u8; 24]>(get, device, CL_DEVICE_TOPOLOGY_AMD) {
        if u32::from_ne_bytes([topology[0], topology[1], topology[2], topology[3]]) == 1 {
            return Some(PciAddress {
                domain: 0,
                bus: topology[21],
                device: topology[22],
                function: topology[23],
            });
        }
    }
    let bus = info_value::<u32>(get, device, CL_DEVICE_PCI_BUS_ID_NV)?;
    let slot = info_value::<u32>(get, device, CL_DEVICE_PCI_SLOT_ID_NV)?;
    Some(PciAddress {
        domain: info_value::<u32>(get, device, CL_DEVICE_PCI_DOMAIN_ID_NV).unwrap_or(0),
        bus: bus as u8,
        device: (slot >> 3) as u8,
        function: (slot & 7) as u8,
    })
}

/// Normalizes a vendor string to the names the other engines use.
fn vendor_name(vendor: &str) -> String {
    let lower = vendor.to_ascii_lowercase();
    if lower.contains("nvidia") {
        "NVIDIA".into()
    } else if lower.contains("advanced micro devices") || lower.contains("amd") {
        "AMD".into()
    } else if lower.contains("intel") {
        "Intel".into()
    } else if lower.contains("arm") {
        "ARM".into()
    } else if lower.contains("qualcomm") {
        "Qualcomm".into()
    } else if lower.contains("apple") {
        "Apple".into()
    } else {
        vendor.to_owned()
    }
}

/// Every OpenCL GPU, for `devices` and engine selection.
pub(crate) fn list_opencl_devices() -> Result<Vec<GpuDevice>, String> {
    let cl = opencl()?;
    Ok(find_devices(cl)?
        .into_iter()
        .enumerate()
        .map(|(index, found)| GpuDevice {
            index: index as u32,
            vendor: vendor_name(&found.vendor),
            vram_bytes: (found.memory > 0).then_some(found.memory),
            backend: BackendKind::OpenCl,
            detail: format!(
                "{}; {} compute units; platform {}",
                found.version, found.units, found.platform
            ),
            integrated: found.integrated,
            ready: true,
            pci: found.pci,
            name: found.name,
        })
        .collect())
}

/// One signed window: the SHA-256 state after 448 bytes and the padded tail.
#[derive(Clone)]
struct Window {
    state: [u32; 8],
    tail: [u32; TAIL_WORDS],
}

struct Job {
    template: Vec<u8>,
    layout: PhotonLayout,
    target_hex: String,
    key: [u8; 32],
    baton: u64,
    reward: u64,
    midstate: [u32; 8],
    windows: HashMap<u32, Window>,
}

pub struct OpenClPhotonEngine {
    cl: &'static OpenCl,
    device: Handle,
    context: Handle,
    queue: Handle,
    /// One program and kernel per layout shift, built on first use.
    kernels: HashMap<usize, (Handle, Handle)>,
    states: Handle,
    tails: Handle,
    target: Handle,
    winner_count: Handle,
    winner_window: Handle,
    winner_j: Handle,
    winner_hash: Handle,
    max_candidates: u32,
    max_windows: usize,
    winner_cap: u32,
    recommended: u32,
    strict: bool,
    job: Option<Job>,
    name: String,
}

// SAFETY: the engine owns its OpenCL objects and is used by one thread at a
// time; OpenCL objects may move between threads.
unsafe impl Send for OpenClPhotonEngine {}

impl OpenClPhotonEngine {
    pub fn new(
        device_ordinal: usize,
        max_batch_candidates: u32,
        winner_buffer_cap: u32,
    ) -> Result<Self, String> {
        let cl = opencl()?;
        let devices = find_devices(cl)?;
        // Tests may name their device, so a test never runs on another GPU
        // (the mining GPU) when the enumeration order differs.
        #[cfg(test)]
        let named = std::env::var("PICKAXE_TEST_OPENCL_DEVICE").ok();
        #[cfg(not(test))]
        let named: Option<String> = None;
        let found = match named {
            Some(name) => devices
                .into_iter()
                .find(|device| device.name.contains(&name)),
            None => devices.into_iter().nth(device_ordinal),
        }
        .ok_or_else(|| format!("no OpenCL GPU {device_ordinal}"))?;
        let max_candidates = max_batch_candidates.clamp(WINDOW, OPENCL_MAX_BATCH_CANDIDATES);
        // A batch spans at most one more window than it fills.
        let max_windows = (max_candidates / WINDOW) as usize + 2;
        let winner_cap = winner_buffer_cap.max(1);
        let mut error = CL_SUCCESS;
        // SAFETY: one valid device handle; no properties or callback.
        let context = unsafe {
            (cl.create_context)(
                ptr::null(),
                1,
                &found.device,
                ptr::null(),
                ptr::null_mut(),
                &mut error,
            )
        };
        check(error, "context creation")?;
        let mut engine = Self {
            cl,
            device: found.device,
            context,
            queue: ptr::null_mut(),
            kernels: HashMap::new(),
            states: ptr::null_mut(),
            tails: ptr::null_mut(),
            target: ptr::null_mut(),
            winner_count: ptr::null_mut(),
            winner_window: ptr::null_mut(),
            winner_j: ptr::null_mut(),
            winner_hash: ptr::null_mut(),
            max_candidates,
            max_windows,
            winner_cap,
            recommended: START_BATCH,
            strict: false,
            job: None,
            name: found.name,
        };
        // SAFETY: a valid context and device, in-order queue.
        engine.queue = unsafe { (cl.create_command_queue)(context, found.device, 0, &mut error) };
        check(error, "command queue creation")?;
        engine.states = engine.buffer(CL_MEM_READ_ONLY, max_windows * 8 * 4)?;
        engine.tails = engine.buffer(CL_MEM_READ_ONLY, max_windows * TAIL_WORDS * 4)?;
        engine.target = engine.buffer(CL_MEM_READ_ONLY, 32)?;
        engine.winner_count = engine.buffer(CL_MEM_READ_WRITE, 4)?;
        engine.winner_window = engine.buffer(CL_MEM_READ_WRITE, winner_cap as usize * 4)?;
        engine.winner_j = engine.buffer(CL_MEM_READ_WRITE, winner_cap as usize * 4)?;
        engine.winner_hash = engine.buffer(CL_MEM_READ_WRITE, winner_cap as usize * 32)?;
        Ok(engine)
    }

    fn buffer(&self, flags: u64, size: usize) -> Result<Handle, String> {
        let mut error = CL_SUCCESS;
        // SAFETY: a valid context; no host pointer.
        let buffer = unsafe {
            (self.cl.create_buffer)(self.context, flags, size, ptr::null_mut(), &mut error)
        };
        check(error, "buffer allocation")?;
        Ok(buffer)
    }

    /// The GPU's name, as the dashboard shows it.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_proof_rule(&mut self, rule: ProofRule) {
        self.strict = rule == ProofRule::Positive;
    }

    pub fn recommended_batch_candidates(&self) -> u32 {
        self.recommended
    }

    pub fn persistent_device_bytes(&self) -> usize {
        self.max_windows * (8 + TAIL_WORDS) * 4 + 32 + 4 + self.winner_cap as usize * (4 + 4 + 32)
    }

    pub fn table_source(&self) -> &'static str {
        "OpenCL C T2 kernel; windows signed on the CPU"
    }

    /// The kernel for one layout shift, built on first use.
    fn kernel(&mut self, shift: usize) -> Result<Handle, String> {
        if let Some((_, kernel)) = self.kernels.get(&shift) {
            return Ok(*kernel);
        }
        let cl = self.cl;
        let source = KERNEL_SOURCE.as_ptr().cast::<c_char>();
        let length = KERNEL_SOURCE.len();
        let mut error = CL_SUCCESS;
        // SAFETY: one source string of the given length.
        let program = unsafe {
            (cl.create_program_with_source)(self.context, 1, &source, &length, &mut error)
        };
        check(error, "program creation")?;
        let options = CString::new(format!("-D PHOTON_SHIFT={shift}"))
            .map_err(|_| "invalid OpenCL build options")?;
        // SAFETY: one valid device; synchronous build.
        let built = unsafe {
            (cl.build_program)(
                program,
                1,
                &self.device,
                options.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
            )
        };
        if built != CL_SUCCESS {
            let log = self.build_log(program);
            // SAFETY: the program is ours and no longer used.
            unsafe { (cl.release_program)(program) };
            return Err(format!(
                "OpenCL could not build the T2 kernel (error {built}): {}",
                log.chars().take(2000).collect::<String>()
            ));
        }
        let name = CString::new("photon_t2").expect("kernel name");
        // SAFETY: a built program containing this kernel.
        let kernel = unsafe { (cl.create_kernel)(program, name.as_ptr(), &mut error) };
        if error != CL_SUCCESS {
            // SAFETY: the program is ours and no longer used.
            unsafe { (cl.release_program)(program) };
            return Err(format!(
                "OpenCL could not create the T2 kernel (error {error})"
            ));
        }
        self.kernels.insert(shift, (program, kernel));
        Ok(kernel)
    }

    fn build_log(&self, program: Handle) -> String {
        let mut size = 0usize;
        // SAFETY: a size query writes only `size`.
        let code = unsafe {
            (self.cl.get_program_build_info)(
                program,
                self.device,
                CL_PROGRAM_BUILD_LOG,
                0,
                ptr::null_mut(),
                &mut size,
            )
        };
        if code != CL_SUCCESS || size == 0 {
            return "no build log".into();
        }
        let mut bytes = vec![0u8; size];
        // SAFETY: the buffer holds the reported size.
        let code = unsafe {
            (self.cl.get_program_build_info)(
                program,
                self.device,
                CL_PROGRAM_BUILD_LOG,
                size,
                bytes.as_mut_ptr().cast(),
                ptr::null_mut(),
            )
        };
        if code != CL_SUCCESS {
            return "no build log".into();
        }
        String::from_utf8_lossy(&bytes)
            .trim_end_matches('\0')
            .trim()
            .to_owned()
    }

    /// Installs a T2-eligible job. Jobs whose amounts cannot carry the T2
    /// coordinate are refused, since this engine signs only once per window.
    pub fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        let layout = PhotonLayout::for_tx_len(template.len())?;
        if !tx::supports_t2_window(template)? {
            return Err(
                "the OpenCL engine mines T2 jobs only, and this job's amounts cannot carry T2"
                    .into(),
            );
        }
        let shift = layout.shift();
        let amount =
            |at: usize| u64::from_le_bytes(template[at..at + 8].try_into().expect("8-byte amount"));
        let mut midstate = INITIAL_STATE;
        for block in template[..384].as_chunks::<64>().0 {
            compress(&mut midstate, block);
        }
        self.kernel(shift)?;
        self.write(self.target, target)?;
        self.job = Some(Job {
            template: template.to_vec(),
            layout,
            target_hex: hex::encode(target),
            key: *private_key,
            baton: amount(491 + shift),
            reward: amount(578 + shift),
            midstate,
            windows: HashMap::new(),
        });
        Ok(())
    }

    fn write(&self, buffer: Handle, bytes: &[u8]) -> Result<(), String> {
        // SAFETY: a blocking write of `bytes` into a buffer at least as large.
        check(
            unsafe {
                (self.cl.enqueue_write_buffer)(
                    self.queue,
                    buffer,
                    CL_TRUE,
                    0,
                    bytes.len(),
                    bytes.as_ptr().cast_mut().cast(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                )
            },
            "buffer write",
        )
    }

    fn read(&self, buffer: Handle, bytes: &mut [u8]) -> Result<(), String> {
        // SAFETY: a blocking read into `bytes` from a buffer at least as large.
        check(
            unsafe {
                (self.cl.enqueue_read_buffer)(
                    self.queue,
                    buffer,
                    CL_TRUE,
                    0,
                    bytes.len(),
                    bytes.as_mut_ptr().cast(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                )
            },
            "buffer read",
        )
    }

    fn arg<T>(&self, kernel: Handle, index: ClUint, value: &T) -> Result<(), String> {
        // SAFETY: `value` is a plain value or buffer handle of its own size.
        check(
            unsafe {
                (self.cl.set_kernel_arg)(
                    kernel,
                    index,
                    std::mem::size_of::<T>(),
                    (value as *const T).cast(),
                )
            },
            "kernel argument",
        )
    }

    /// Searches `candidate_count` T2 positions from `nonce_base`: the window
    /// is the high 16 bits (the signature nonce), the amount coordinate the
    /// low 16.
    pub fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if candidate_count == 0 {
            return Ok(PhotonCudaBatchResult {
                candidates: 0,
                total_winners: 0,
                winners: Vec::new(),
            });
        }
        if candidate_count > self.max_candidates {
            return Err(format!(
                "OpenCL batch of {candidate_count} exceeds its capacity of {}",
                self.max_candidates
            ));
        }
        let last = nonce_base
            .checked_add(candidate_count - 1)
            .ok_or("OpenCL candidate range crosses the key-rotation boundary")?;
        let started = Instant::now();
        let first_window = nonce_base / WINDOW;
        let windows = (last / WINDOW - first_window + 1) as usize;
        if windows > self.max_windows {
            return Err("OpenCL batch spans too many windows".into());
        }
        let job = self.job.as_mut().ok_or("OpenCL job not set")?;
        let shift = job.layout.shift();
        let mut states = Vec::with_capacity(windows * 32);
        let mut tails = Vec::with_capacity(windows * TAIL_WORDS * 4);
        for window in first_window..first_window + windows as u32 {
            let signed = signed_window(job, window)?;
            states.extend(signed.state.iter().flat_map(|word| word.to_le_bytes()));
            tails.extend(signed.tail.iter().flat_map(|word| word.to_le_bytes()));
        }
        let (baton, reward) = (job.baton, job.reward);
        let kernel = self.kernel(shift)?;
        self.write(self.states, &states)?;
        self.write(self.tails, &tails)?;
        self.write(self.winner_count, &0u32.to_le_bytes())?;
        let strict = u32::from(self.strict);
        let offset = nonce_base % WINDOW;
        self.arg(kernel, 0, &self.states)?;
        self.arg(kernel, 1, &self.tails)?;
        self.arg(kernel, 2, &baton)?;
        self.arg(kernel, 3, &reward)?;
        self.arg(kernel, 4, &self.target)?;
        self.arg(kernel, 5, &strict)?;
        self.arg(kernel, 6, &offset)?;
        self.arg(kernel, 7, &candidate_count)?;
        self.arg(kernel, 8, &self.winner_cap)?;
        self.arg(kernel, 9, &self.winner_count)?;
        self.arg(kernel, 10, &self.winner_window)?;
        self.arg(kernel, 11, &self.winner_j)?;
        self.arg(kernel, 12, &self.winner_hash)?;
        let global = candidate_count as usize;
        // SAFETY: a built kernel with every argument set; the driver chooses
        // the work-group size, which old devices limit.
        check(
            unsafe {
                (self.cl.enqueue_nd_range_kernel)(
                    self.queue,
                    kernel,
                    1,
                    ptr::null(),
                    &global,
                    ptr::null(),
                    0,
                    ptr::null(),
                    ptr::null_mut(),
                )
            },
            "kernel launch",
        )?;
        // SAFETY: a valid queue.
        check(unsafe { (self.cl.finish)(self.queue) }, "kernel run")?;
        let mut count = [0u8; 4];
        self.read(self.winner_count, &mut count)?;
        let total_winners = u32::from_le_bytes(count);
        let returned = total_winners.min(self.winner_cap) as usize;
        let mut winners = Vec::with_capacity(returned);
        if returned > 0 {
            let mut window_bytes = vec![0u8; returned * 4];
            let mut j_bytes = vec![0u8; returned * 4];
            let mut hashes = vec![0u8; returned * 32];
            self.read(self.winner_window, &mut window_bytes)?;
            self.read(self.winner_j, &mut j_bytes)?;
            self.read(self.winner_hash, &mut hashes)?;
            for slot in 0..returned {
                let window = u32::from_le_bytes(
                    window_bytes[slot * 4..slot * 4 + 4]
                        .try_into()
                        .expect("4 bytes"),
                );
                let j = u32::from_le_bytes(
                    j_bytes[slot * 4..slot * 4 + 4].try_into().expect("4 bytes"),
                );
                winners.push(PhotonCudaWinner {
                    nonce: first_window + window,
                    digest: hashes[slot * 32..slot * 32 + 32]
                        .try_into()
                        .expect("32-byte digest"),
                    schnorr_k: None,
                    tail_j: Some(j as u16),
                    tail_value_sats: None,
                });
            }
        }
        // Throttled intensity runs a quarter of the recommended batch: scale
        // its time to the full batch, as the portable engine does.
        let full = self.recommended;
        let scale = if candidate_count == full {
            Some(1)
        } else if candidate_count.checked_mul(THROTTLED_BATCH_DIVISOR) == Some(full) {
            Some(THROTTLED_BATCH_DIVISOR)
        } else {
            None
        };
        if let Some(scale) = scale {
            let elapsed = started.elapsed().saturating_mul(scale);
            self.recommended = if elapsed <= ESCALATE_BATCH {
                full.saturating_mul(2).min(self.max_candidates)
            } else if elapsed > TARGET_BATCH {
                (full / 2).max(START_BATCH)
            } else {
                full
            };
        }
        Ok(PhotonCudaBatchResult {
            candidates: candidate_count,
            total_winners,
            winners,
        })
    }
}

/// Signs one window on the CPU and prepares its hashing inputs, once per
/// window while it stays cached.
fn signed_window(job: &mut Job, nonce: u32) -> Result<Window, String> {
    if let Some(window) = job.windows.get(&nonce) {
        return Ok(window.clone());
    }
    let message = tx::photon_message_sha256(nonce, &job.target_hex)?;
    let signature = crypto::bch_schnorr_sign(&job.key, &message)?;
    let shift = job.layout.shift();
    let mut transaction = job.template.clone();
    let n = job.layout.nonce_offset();
    transaction[n..n + 4].copy_from_slice(&nonce.to_le_bytes());
    let s = job.layout.signature_offset();
    transaction[s..s + 64].copy_from_slice(&signature);
    let mut state = job.midstate;
    compress(
        &mut state,
        transaction[384..448].try_into().expect("64-byte block"),
    );
    // Bytes 448..639 of the padded message, with both amounts left zero.
    let length = 615 + shift;
    let mut padded = [0u8; TAIL_WORDS * 4];
    padded[..length - 448].copy_from_slice(&transaction[448..length]);
    padded[length - 448] = 0x80;
    padded[184..].copy_from_slice(&(length as u64 * 8).to_be_bytes());
    padded[491 + shift - 448..499 + shift - 448].fill(0);
    padded[578 + shift - 448..586 + shift - 448].fill(0);
    let mut tail = [0u32; TAIL_WORDS];
    for (word, bytes) in tail.iter_mut().zip(padded.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    let window = Window { state, tail };
    if job.windows.len() >= WINDOW_CACHE {
        job.windows.clear();
    }
    job.windows.insert(nonce, window.clone());
    Ok(window)
}

impl Drop for OpenClPhotonEngine {
    fn drop(&mut self) {
        let cl = self.cl;
        // SAFETY: every handle below is ours and released once; null handles
        // (from a failed constructor) are skipped.
        unsafe {
            for (program, kernel) in self.kernels.values() {
                (cl.release_kernel)(*kernel);
                (cl.release_program)(*program);
            }
            for buffer in [
                self.states,
                self.tails,
                self.target,
                self.winner_count,
                self.winner_window,
                self.winner_j,
                self.winner_hash,
            ] {
                if !buffer.is_null() {
                    (cl.release_mem_object)(buffer);
                }
            }
            if !self.queue.is_null() {
                (cl.release_command_queue)(self.queue);
            }
            if !self.context.is_null() {
                (cl.release_context)(self.context);
            }
        }
    }
}

const INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const ROUND: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// One SHA-256 compression on the host, for the per-window midstate.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*bytes);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (round, word) in ROUND.iter().zip(w.iter()) {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*round)
            .wrapping_add(*word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (word, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proof;
    use secp256k1::{PublicKey, SecretKey};

    /// The host's half of the engine (midstate, signed window, padded tail)
    /// finished on the CPU the way the kernel finishes it, against the
    /// transaction hashed whole.
    fn cpu_finish(window: &Window, shift: usize, baton: u64, reward: u64, j: u32) -> [u8; 32] {
        let mut m = window.tail;
        let put = |m: &mut [u32; TAIL_WORDS], position: usize, value: u64| {
            m[position >> 2] |= ((value & 0xff) as u32) << (8 * (3 - (position & 3)));
        };
        for i in 0..8 {
            put(
                &mut m,
                491 + shift - 448 + i,
                (baton + u64::from(j)) >> (8 * i),
            );
            put(
                &mut m,
                578 + shift - 448 + i,
                (reward - u64::from(j)) >> (8 * i),
            );
        }
        let mut state = window.state;
        for block in m.as_chunks::<16>().0 {
            let bytes: Vec<u8> = block.iter().flat_map(|word| word.to_be_bytes()).collect();
            compress(&mut state, bytes.as_slice().try_into().unwrap());
        }
        let first: Vec<u8> = state.iter().flat_map(|word| word.to_be_bytes()).collect();
        let second = <sha2::Sha256 as sha2::Digest>::digest(&first);
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&second);
        digest
    }

    fn template_for(
        deployment: &crate::protocol::PhotonDeployment,
        age: u32,
        key: &[u8; 32],
        target: &[u8; 32],
        baton: u128,
        reward: u128,
    ) -> Vec<u8> {
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(*key).unwrap()).serialize();
        tx::build_photon_template_bytes_for_deployment(
            &tx::TemplateParams {
                prev_tx_hash_hex: "11".repeat(32),
                prev_index: 0,
                age,
                public_key_hex: hex::encode(public),
                target_hex: hex::encode(target),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 48_635_000,
                relay_fee_sats_per_kb: 1100,
                contract_token_amount: baton + reward,
                reward_amount: reward,
                payout_locking: tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)
                    .unwrap(),
            },
            deployment,
        )
        .unwrap()
    }

    /// One test job's inputs, for hashing candidates on the CPU.
    struct Case<'a> {
        template: &'a [u8],
        layout: PhotonLayout,
        key: &'a [u8; 32],
        target: &'a [u8; 32],
        baton: u128,
        reward: u128,
    }

    /// The whole transaction for one candidate, hashed on the CPU.
    fn cpu_digest(case: &Case<'_>, nonce: u32, j: u32) -> [u8; 32] {
        let message = tx::photon_message_sha256(nonce, &hex::encode(case.target)).unwrap();
        let signature = crypto::bch_schnorr_sign(case.key, &message).unwrap();
        let mut completed = case.template.to_vec();
        let n = case.layout.nonce_offset();
        completed[n..n + 4].copy_from_slice(&nonce.to_le_bytes());
        let s = case.layout.signature_offset();
        completed[s..s + 64].copy_from_slice(&signature);
        let shift = case.layout.shift();
        completed[491 + shift..499 + shift]
            .copy_from_slice(&((case.baton + u128::from(j)) as u64).to_le_bytes());
        completed[578 + shift..586 + shift]
            .copy_from_slice(&((case.reward - u128::from(j)) as u64).to_le_bytes());
        proof::hash256(&completed)
    }

    #[test]
    fn host_preparation_finishes_to_the_whole_transaction_hash() {
        let key = [0x21; 32];
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        // Both amounts carry or borrow inside the window.
        let reward = (1u128 << 33) + 7;
        let baton = (1u128 << 49) + u128::from(u32::MAX) - 7;
        for deployment in [
            crate::protocol::MAINNET_V0_PHOTON,
            crate::protocol::MAINNET_PHOTON,
            crate::protocol::CHIPNET_PHOTON,
        ] {
            for age in [0, 17, 128, 32768] {
                let Ok(layout) = PhotonLayout::for_age_with_deployment(age, &deployment) else {
                    continue;
                };
                let template = template_for(&deployment, age, &key, &target, baton, reward);
                assert!(tx::supports_t2_window(&template).unwrap());
                let mut midstate = INITIAL_STATE;
                for block in template[..384].as_chunks::<64>().0 {
                    compress(&mut midstate, block);
                }
                let shift = layout.shift();
                let mut job = Job {
                    template: template.clone(),
                    layout,
                    target_hex: hex::encode(target),
                    key,
                    baton: u64::from_le_bytes(
                        template[491 + shift..499 + shift].try_into().unwrap(),
                    ),
                    reward: u64::from_le_bytes(
                        template[578 + shift..586 + shift].try_into().unwrap(),
                    ),
                    midstate,
                    windows: HashMap::new(),
                };
                let case = Case {
                    template: &template,
                    layout,
                    key: &key,
                    target: &target,
                    baton,
                    reward,
                };
                for (nonce, j) in [(0, 0), (0, 7), (0x1234, 65535), (u32::MAX, 9)] {
                    let window = signed_window(&mut job, nonce).unwrap();
                    assert_eq!(
                        cpu_finish(&window, shift, job.baton, job.reward, j),
                        cpu_digest(&case, nonce, j),
                        "age {age}, nonce {nonce}, j {j}"
                    );
                }
            }
        }
        // The host compression is SHA-256's.
        let mut state = INITIAL_STATE;
        let mut block = [0u8; 64];
        block[0] = 0x80;
        compress(&mut state, &block);
        let empty: Vec<u8> = state.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(
            hex::encode(empty),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn vendor_names_match_the_other_engines() {
        assert_eq!(vendor_name("Advanced Micro Devices, Inc."), "AMD");
        assert_eq!(vendor_name("NVIDIA Corporation"), "NVIDIA");
        assert_eq!(vendor_name("Intel(R) Corporation"), "Intel");
        assert_eq!(vendor_name("ARM"), "ARM");
        assert_eq!(vendor_name("QUALCOMM"), "Qualcomm");
    }

    /// Opt-in: the kernel against the CPU for every candidate of several
    /// layouts and window boundaries, on a GPU chosen by name, e.g.
    /// `PICKAXE_TEST_OPENCL_DEVICE="AMD Radeon" cargo test --release --lib
    /// opencl_kernel_matches_the_cpu -- --ignored`.
    #[test]
    #[ignore = "needs an OpenCL GPU; choose it with PICKAXE_TEST_OPENCL_DEVICE"]
    fn opencl_kernel_matches_the_cpu_for_every_candidate() {
        let mut engine = OpenClPhotonEngine::new(0, 1 << 20, 4096).unwrap();
        println!("OpenCL device: {}", engine.name());
        let key = [0x31; 32];
        let mut target = [0xff; 32];
        target[31] = 0x10;
        let reward = (1u128 << 33) + 7;
        let baton = (1u128 << 49) + u128::from(u32::MAX) - 7;
        let mut checked = 0usize;
        for deployment in [
            crate::protocol::MAINNET_V0_PHOTON,
            crate::protocol::MAINNET_PHOTON,
            crate::protocol::CHIPNET_PHOTON,
        ] {
            for age in [0, 17, 128, 32768] {
                let Ok(layout) = PhotonLayout::for_age_with_deployment(age, &deployment) else {
                    continue;
                };
                let template = template_for(&deployment, age, &key, &target, baton, reward);
                engine.set_proof_rule(deployment.proof_rule);
                engine.set_job(&template, &target, &key).unwrap();
                for (base, count) in [
                    (0, 67),
                    (65500, 128),
                    (0x1234ffff, 129),
                    (u32::MAX - 66, 67),
                ] {
                    let actual = engine.search_batch(base, count).unwrap();
                    assert!(!actual.truncated());
                    let mut expected = std::collections::BTreeMap::new();
                    for candidate in u64::from(base)..u64::from(base) + u64::from(count) {
                        let nonce = (candidate / 65536) as u32;
                        let j = (candidate & 65535) as u32;
                        let case = Case {
                            template: &template,
                            layout,
                            key: &key,
                            target: &target,
                            baton,
                            reward,
                        };
                        let digest = cpu_digest(&case, nonce, j);
                        if proof::meets_target_le_for_rule(&digest, &target, deployment.proof_rule)
                        {
                            expected.insert((nonce, j as u16), digest);
                        }
                        checked += 1;
                    }
                    let returned: std::collections::BTreeMap<_, _> = actual
                        .winners
                        .iter()
                        .map(|winner| ((winner.nonce, winner.tail_j.unwrap()), winner.digest))
                        .collect();
                    assert_eq!(returned, expected, "age {age}, base {base}");
                }
            }
        }
        println!("OpenCL T2 candidates checked against the CPU: {checked}");
    }
}
