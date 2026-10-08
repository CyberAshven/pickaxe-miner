//! GPU backend selection and discovery: auto | cuda | hip | wgpu.
//! No CPU mining fallback.

use cudarc::driver::{sys, CudaContext};
use libloading::Library;
use std::ffi::CStr;
use std::fmt;
use std::os::raw::{c_char, c_int};

pub use crate::backend_kind::{BackendKind, DeviceSelection};

#[derive(Debug, Clone)]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub vram_bytes: Option<u64>,
    pub backend: BackendKind,
    pub detail: String,
    /// Integrated GPUs share the CPU package; auto selection uses one only
    /// when no discrete GPU is present (AMD APU-only PCs, Apple Silicon).
    pub integrated: bool,
    /// Whether this backend can mine on the device as installed; native HIP
    /// needs code objects built for the device's architecture.
    pub ready: bool,
    /// PCI location; the same physical GPU reports it through every engine.
    pub pci: Option<PciAddress>,
}

/// A GPU's PCI location: domain, bus, device and function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PciAddress {
    pub domain: u32,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    /// Parses the hexadecimal `domain:bus:device.function` form that CUDA,
    /// HIP and wgpu report, with or without the domain.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_end_matches('\0');
        let (location, function) = text.rsplit_once('.')?;
        let fields: Vec<&str> = location.split(':').collect();
        let (domain, bus, device) = match fields.as_slice() {
            [domain, bus, device] => (*domain, *bus, *device),
            [bus, device] => ("0", *bus, *device),
            _ => return None,
        };
        Some(Self {
            domain: u32::from_str_radix(domain, 16).ok()?,
            bus: u8::from_str_radix(bus, 16).ok()?,
            device: u8::from_str_radix(device, 16).ok()?,
            function: u8::from_str_radix(function, 16).ok()?,
        })
    }
}

impl fmt::Display for PciAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04x}:{:02x}:{:02x}.{}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

/// Enumerates devices available to each mining backend.
pub fn list_devices(prefer: BackendKind) -> Result<Vec<GpuDevice>, String> {
    match prefer {
        BackendKind::Cuda => list_cuda_devices(),
        BackendKind::Hip => list_hip_devices(),
        BackendKind::Wgpu => list_wgpu_devices(),
        BackendKind::OpenCl => list_opencl_devices(),
        BackendKind::Auto => {
            let cuda = list_cuda_devices();
            let hip = list_hip_devices();
            let wgpu = list_wgpu_devices();
            let mut devices = Vec::new();
            let mut errors = Vec::new();

            match cuda {
                Ok(mut found) => devices.append(&mut found),
                Err(error) => errors.push(error),
            }
            match hip {
                Ok(mut found) => devices.append(&mut found),
                Err(error) => errors.push(error),
            }
            match wgpu {
                Ok(mut found) => devices.append(&mut found),
                Err(error) => errors.push(error),
            }
            // OpenCL only when no other engine found a GPU (see mining_gpus).
            if devices.is_empty() {
                match list_opencl_devices() {
                    Ok(mut found) => devices.append(&mut found),
                    Err(error) => errors.push(error),
                }
            }

            if devices.is_empty() {
                Err(format!("no GPU device found ({})", errors.join("; ")))
            } else {
                Ok(devices)
            }
        }
    }
}

/// Resolves one GPU, for commands that run a single GPU at a time.
pub fn resolve_mining_device(
    prefer: BackendKind,
    requested_index: Option<u32>,
) -> Result<GpuDevice, String> {
    let selection = requested_index.map_or(DeviceSelection::Default, |index| {
        DeviceSelection::Indices(vec![index])
    });
    let gpus = resolve_mining_devices(prefer, &selection)?;
    Ok(gpus
        .into_iter()
        .next()
        .expect("a resolved GPU selection is never empty"))
}

/// Resolves every GPU that mines under `selection`.
pub fn resolve_mining_devices(
    prefer: BackendKind,
    selection: &DeviceSelection,
) -> Result<Vec<GpuDevice>, String> {
    match prefer {
        BackendKind::Cuda => select_gpus(list_cuda_devices()?, selection, prefer),
        BackendKind::Hip => select_gpus(list_hip_devices()?, selection, prefer),
        BackendKind::Wgpu => select_gpus(list_wgpu_devices()?, selection, prefer),
        BackendKind::OpenCl => select_gpus(list_opencl_devices()?, selection, prefer),
        BackendKind::Auto => select_gpus(mining_gpus(), selection, prefer),
    }
}

/// Every GPU automatic selection can mine on: one entry per physical GPU.
pub fn mining_gpus() -> Vec<GpuDevice> {
    let gpus = physical_gpus(
        list_cuda_devices().unwrap_or_default(),
        list_hip_devices().unwrap_or_default(),
        list_wgpu_devices().unwrap_or_default(),
    );
    if !gpus.is_empty() {
        return gpus;
    }
    // #### PR #32: OpenCL is loaded only when no other engine found a GPU,
    // such as an older integrated GPU without Vulkan or DirectX 12, or an
    // ARM GPU without Vulkan. A machine whose GPUs another engine drives
    // never loads an OpenCL driver, so a broken one cannot affect it;
    // `--backend opencl` chooses OpenCL explicitly.
    physical_gpus_with_opencl(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        list_opencl_devices().unwrap_or_default(),
    )
}

/// Applies a selection to a GPU list. With `--backend auto` the numbers are
/// positions in the physical GPU list that `devices` prints; with an explicit
/// backend they are that backend's own ordinals.
fn select_gpus(
    gpus: Vec<GpuDevice>,
    selection: &DeviceSelection,
    backend: BackendKind,
) -> Result<Vec<GpuDevice>, String> {
    let selected = match selection {
        DeviceSelection::Indices(wanted) => wanted
            .iter()
            .map(|&index| {
                let found = if backend == BackendKind::Auto {
                    gpus.get(index as usize).cloned()
                } else {
                    gpus.iter().find(|gpu| gpu.index == index).cloned()
                };
                found.ok_or_else(|| {
                    if backend == BackendKind::Auto {
                        format!("no GPU {index}; run `pickaxe devices` for the GPU numbers")
                    } else {
                        format!(
                            "{} device {index} not found; run `pickaxe devices --backend {}`",
                            backend.as_str().to_ascii_uppercase(),
                            backend.as_str()
                        )
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        DeviceSelection::WithIntegrated => gpus,
        DeviceSelection::Default => {
            let discrete: Vec<GpuDevice> =
                gpus.iter().filter(|gpu| !gpu.integrated).cloned().collect();
            if discrete.is_empty() {
                gpus
            } else {
                discrete
            }
        }
    };
    if selected.is_empty() {
        return Err(if backend == BackendKind::Auto {
            "no GPU device found; run `pickaxe devices`".into()
        } else {
            format!(
                "no {} GPU found; run `pickaxe devices`",
                backend.as_str().to_ascii_uppercase()
            )
        });
    }
    Ok(selected)
}

// #### PR #22: every GPU mines once, on its best engine
// What: CUDA, HIP and wgpu each list the GPUs they can drive, so one card can
// appear in several lists. Entries with the same PCI address are one GPU;
// when a driver reports no address, the same vendor and name from another
// engine is. A discrete GPU mines on the first engine ready for it: CUDA,
// then native HIP (code objects for its architecture), then wgpu. An
// integrated GPU prefers wgpu, because under Windows integrated GPUs never
// complete a HIP launch. By default every discrete GPU mines, and integrated
// GPUs only when no discrete GPU exists; --include-integrated adds them and
// --device picks GPUs by the numbers `devices` prints.
// Why: one miner drives all of a machine's GPUs, so they share one job and
// never compete for the same reward, and no card may mine twice.
// Check: `pickaxe devices` lists each physical GPU once with its engine.
//
// #### PR #32: OpenCL comes last for every GPU, so a GPU another engine can
// drive keeps that engine, and one only OpenCL sees (an older integrated GPU
// without Vulkan or DirectX 12, an ARM GPU) still mines. Two integrated GPUs
// of one vendor are one GPU when no PCI address tells them apart, because
// OpenCL may name a GPU differently from the other engines.
fn physical_gpus(
    cuda: Vec<GpuDevice>,
    hip: Vec<GpuDevice>,
    wgpu: Vec<GpuDevice>,
) -> Vec<GpuDevice> {
    physical_gpus_with_opencl(cuda, hip, wgpu, Vec::new())
}

fn physical_gpus_with_opencl(
    cuda: Vec<GpuDevice>,
    hip: Vec<GpuDevice>,
    wgpu: Vec<GpuDevice>,
    opencl: Vec<GpuDevice>,
) -> Vec<GpuDevice> {
    let mut groups: Vec<Vec<GpuDevice>> = Vec::new();
    for device in cuda.into_iter().chain(hip).chain(wgpu).chain(opencl) {
        match groups
            .iter_mut()
            .find(|group| same_physical_gpu(group, &device))
        {
            Some(group) => group.push(device),
            None => groups.push(vec![device]),
        }
    }
    groups
        .into_iter()
        .filter_map(|group| {
            let integrated = group.iter().any(|device| device.integrated);
            let pci = group.iter().find_map(|device| device.pci);
            let priority = if integrated {
                [
                    BackendKind::Wgpu,
                    BackendKind::Hip,
                    BackendKind::Cuda,
                    BackendKind::OpenCl,
                ]
            } else {
                [
                    BackendKind::Cuda,
                    BackendKind::Hip,
                    BackendKind::Wgpu,
                    BackendKind::OpenCl,
                ]
            };
            priority
                .iter()
                .find_map(|backend| {
                    group
                        .iter()
                        .find(|device| device.backend == *backend && device.ready)
                })
                .cloned()
                .map(|mut chosen| {
                    chosen.integrated = integrated;
                    chosen.pci = chosen.pci.or(pci);
                    chosen
                })
        })
        .collect()
}

/// Whether `device` is another engine's entry for the GPU in `group`.
fn same_physical_gpu(group: &[GpuDevice], device: &GpuDevice) -> bool {
    if group.iter().any(|member| member.backend == device.backend) {
        return false;
    }
    if let Some(pci) = device.pci {
        if group.iter().any(|member| member.pci.is_some()) {
            return group.iter().any(|member| member.pci == Some(pci));
        }
    }
    group.iter().any(|member| {
        member.vendor.eq_ignore_ascii_case(&device.vendor)
            && (same_name(&member.name, &device.name) || (member.integrated && device.integrated))
    })
}

/// Equal names, allowing a driver suffix such as Mesa's " (RADV NAVI31)".
fn same_name(left: &str, right: &str) -> bool {
    let (left, right) = (left.to_ascii_lowercase(), right.to_ascii_lowercase());
    left == right
        || left.starts_with(&format!("{right} ("))
        || right.starts_with(&format!("{left} ("))
}

/// Rejects GPU backends that are not production-ready.
pub fn require_production_mining_backend(backend: BackendKind) -> Result<(), String> {
    match backend {
        BackendKind::Cuda | BackendKind::Hip => Ok(()),
        BackendKind::Wgpu => {
            if cfg!(feature = "portable-wgpu") {
                Ok(())
            } else {
                Err("wgpu fallback is not compiled; rebuild with --features portable-wgpu".into())
            }
        }
        BackendKind::OpenCl => {
            if cfg!(feature = "opencl") {
                Ok(())
            } else {
                Err("OpenCL is not compiled; rebuild with --features opencl".into())
            }
        }
        BackendKind::Auto => {
            Err("auto backend must be resolved before production mining starts".into())
        }
    }
}

#[cfg(feature = "opencl")]
fn list_opencl_devices() -> Result<Vec<GpuDevice>, String> {
    crate::opencl_photon::list_opencl_devices()
}

#[cfg(not(feature = "opencl"))]
fn list_opencl_devices() -> Result<Vec<GpuDevice>, String> {
    Err("OpenCL is not compiled; rebuild with --features opencl".into())
}

#[cfg(feature = "portable-wgpu")]
/// Checks whether a WGPU adapter is backed by hardware.
fn is_hardware_wgpu_device_type(device_type: wgpu::DeviceType) -> bool {
    matches!(
        device_type,
        wgpu::DeviceType::DiscreteGpu
            | wgpu::DeviceType::IntegratedGpu
            | wgpu::DeviceType::VirtualGpu
    )
}

#[cfg(feature = "portable-wgpu")]
/// Maps a WGPU vendor ID to a display name.
fn wgpu_vendor_name(vendor: u32) -> String {
    match vendor {
        0x10de => "NVIDIA".into(),
        0x1002 | 0x1022 => "AMD".into(),
        0x8086 => "Intel".into(),
        0x106b => "Apple".into(),
        other if other != 0 => format!("PCI vendor 0x{other:04x}"),
        _ => "Unknown".into(),
    }
}

#[cfg(feature = "portable-wgpu")]
/// Limits WGPU discovery to production-capable APIs.
pub(crate) fn production_wgpu_backends() -> Result<wgpu::Backends, String> {
    crate::wgpu_photon::production_wgpu_backends()
}

#[cfg(feature = "portable-wgpu")]
/// Enumerates supported WGPU hardware adapters.
fn list_wgpu_devices() -> Result<Vec<GpuDevice>, String> {
    let backends = production_wgpu_backends()?;
    let instance = wgpu::Instance::new(crate::wgpu_photon::production_instance_descriptor(
        backends,
    )?);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let mut out = Vec::new();

    for adapter in adapters {
        let info = adapter.get_info();
        if !is_hardware_wgpu_device_type(info.device_type) {
            continue;
        }

        let index = out.len() as u32;
        let vendor = wgpu_vendor_name(info.vendor);
        let detail = format!(
            "wgpu ordinal={index}; backend={:?}; type={:?}; pci_device=0x{:04x}; driver={}; driver_info={}",
            info.backend,
            info.device_type,
            info.device,
            info.driver,
            info.driver_info
        );
        out.push(GpuDevice {
            index,
            name: info.name,
            vendor,
            vram_bytes: None,
            backend: BackendKind::Wgpu,
            detail,
            integrated: info.device_type == wgpu::DeviceType::IntegratedGpu,
            ready: true,
            pci: PciAddress::parse(&info.device_pci_bus_id),
        });
    }

    if out.is_empty() {
        Err("WGPU found no hardware GPU adapters".into())
    } else {
        Ok(out)
    }
}

#[cfg(not(feature = "portable-wgpu"))]
/// Enumerates supported WGPU hardware adapters.
fn list_wgpu_devices() -> Result<Vec<GpuDevice>, String> {
    Err("wgpu fallback is not compiled; rebuild with --features portable-wgpu".into())
}

/// Enumerates CUDA devices usable for mining.
fn list_cuda_devices() -> Result<Vec<GpuDevice>, String> {
    if cfg!(target_os = "macos") {
        return Err("CUDA is unavailable on macOS; use the wgpu Metal backend".into());
    }
    // #### PR #22: a missing NVIDIA driver is an error, not a crash
    // What: check that the driver library loads before the first CUDA call.
    // Why: cudarc panics when it cannot load nvcuda.dll or libcuda.so, and
    // auto discovery (the dashboard at startup, `devices`) probes CUDA first,
    // so PCs without an NVIDIA driver crashed before reaching HIP or WGPU
    // (issue #30).
    // Check: on such a PC, `devices` and `mine` list the AMD or Intel GPUs.
    // SAFETY: loads and releases the driver library by name, as cudarc does on
    // first use; no CUDA function is called.
    if !unsafe { sys::is_culib_present() } {
        return Err("CUDA unavailable: no NVIDIA driver found".into());
    }
    // Driver must be initialized before get_count (same path CudaContext::new uses).
    cudarc::driver::result::init()
        .map_err(|e| format!("CUDA unavailable: {e}. Install or fix the CUDA runtime."))?;
    let n = cudarc::driver::result::device::get_count()
        .map_err(|e| format!("CUDA unavailable: {e}. Install or fix the CUDA runtime."))?;
    let mut out = Vec::new();
    for i in 0..n {
        let ctx = CudaContext::new(i as usize)
            .map_err(|e| format!("CUDA device {i} init failed: {e}"))?;
        let info = cuda_device_info(i as u32, &ctx);
        out.push(GpuDevice {
            index: i as u32,
            name: info.name,
            vendor: "NVIDIA".into(),
            vram_bytes: info.vram,
            backend: BackendKind::Cuda,
            detail: info.detail,
            integrated: false,
            // #### PR #22: the shipped CUDA kernels are PTX for sm_120, which
            // the driver compiles for compute capability 12.0 and newer only.
            // Other NVIDIA GPUs mine on the portable engine instead.
            ready: info.compute >= (12, 0),
            pci: info.pci,
        });
    }
    Ok(out)
}

type HipError = c_int;
type HipDevice = c_int;
type HipInit = unsafe extern "C" fn(u32) -> HipError;
type HipGetDeviceCount = unsafe extern "C" fn(*mut c_int) -> HipError;
type HipDeviceGet = unsafe extern "C" fn(*mut HipDevice, c_int) -> HipError;
type HipDeviceGetName = unsafe extern "C" fn(*mut c_char, c_int, HipDevice) -> HipError;
type HipSetDevice = unsafe extern "C" fn(c_int) -> HipError;
type HipMemGetInfo = unsafe extern "C" fn(*mut usize, *mut usize) -> HipError;
type HipRuntimeGetVersion = unsafe extern "C" fn(*mut c_int) -> HipError;
type HipDriverGetVersion = unsafe extern "C" fn(*mut c_int) -> HipError;
type HipGetErrorString = unsafe extern "C" fn(HipError) -> *const c_char;
type HipDeviceGetPciBusId = unsafe extern "C" fn(*mut c_char, c_int, HipDevice) -> HipError;

#[cfg(target_os = "windows")]
const HIP_LIBRARY_CANDIDATES: &[&str] = &["amdhip64.dll", "amdhip64_6.dll"];

#[cfg(not(target_os = "windows"))]
const HIP_LIBRARY_CANDIDATES: &[&str] = &["libamdhip64.so", "libamdhip64.so.6", "libamdhip64.so.5"];

/// Loads an available HIP runtime shared library.
fn load_hip_library() -> Result<Library, String> {
    let mut errors = Vec::new();
    for candidate in HIP_LIBRARY_CANDIDATES {
        let loaded = unsafe { Library::new(*candidate) };
        match loaded {
            Ok(library) => return Ok(library),
            Err(error) => errors.push(format!("{candidate}: {error}")),
        }
    }
    Err(format!(
        "HIP/ROCm runtime unavailable: {}",
        errors.join("; ")
    ))
}

/// Formats a HIP runtime error code for display.
fn hip_error(library: &Library, code: HipError, operation: &str) -> String {
    let detail = unsafe {
        library
            .get::<HipGetErrorString>(b"hipGetErrorString\0")
            .ok()
            .and_then(|get_error_string| {
                let pointer = get_error_string(code);
                (!pointer.is_null()).then(|| CStr::from_ptr(pointer).to_string_lossy().into_owned())
            })
    };
    match detail {
        Some(detail) => format!("{operation} failed with HIP error {code}: {detail}"),
        None => format!("{operation} failed with HIP error {code}"),
    }
}

/// Enumerates HIP GPUs available to the miner.
fn list_hip_devices() -> Result<Vec<GpuDevice>, String> {
    let library = load_hip_library()?;
    unsafe {
        let hip_init = library
            .get::<HipInit>(b"hipInit\0")
            .map_err(|error| format!("load hipInit: {error}"))?;
        let hip_get_device_count = library
            .get::<HipGetDeviceCount>(b"hipGetDeviceCount\0")
            .map_err(|error| format!("load hipGetDeviceCount: {error}"))?;
        let hip_device_get = library
            .get::<HipDeviceGet>(b"hipDeviceGet\0")
            .map_err(|error| format!("load hipDeviceGet: {error}"))?;
        let hip_device_get_name = library
            .get::<HipDeviceGetName>(b"hipDeviceGetName\0")
            .map_err(|error| format!("load hipDeviceGetName: {error}"))?;
        let hip_set_device = library
            .get::<HipSetDevice>(b"hipSetDevice\0")
            .map_err(|error| format!("load hipSetDevice: {error}"))?;
        let hip_mem_get_info = library
            .get::<HipMemGetInfo>(b"hipMemGetInfo\0")
            .map_err(|error| format!("load hipMemGetInfo: {error}"))?;
        let hip_runtime_get_version = library
            .get::<HipRuntimeGetVersion>(b"hipRuntimeGetVersion\0")
            .map_err(|error| format!("load hipRuntimeGetVersion: {error}"))?;
        let hip_driver_get_version = library
            .get::<HipDriverGetVersion>(b"hipDriverGetVersion\0")
            .map_err(|error| format!("load hipDriverGetVersion: {error}"))?;
        // Optional: matches this GPU with the same card in the wgpu list.
        let hip_device_get_pci_bus_id = library
            .get::<HipDeviceGetPciBusId>(b"hipDeviceGetPCIBusId\0")
            .ok();

        let init_code = hip_init(0);
        if init_code != 0 {
            return Err(hip_error(&library, init_code, "hipInit"));
        }

        let mut count = 0;
        let count_code = hip_get_device_count(&mut count);
        if count_code != 0 {
            return Err(hip_error(&library, count_code, "hipGetDeviceCount"));
        }
        if count <= 0 {
            return Err("HIP runtime present but zero AMD devices".into());
        }

        let mut runtime_version = 0;
        let _ = hip_runtime_get_version(&mut runtime_version);
        let mut driver_version = 0;
        let _ = hip_driver_get_version(&mut driver_version);

        let mut out = Vec::with_capacity(count as usize);
        for ordinal in 0..count {
            let mut device = 0;
            let device_code = hip_device_get(&mut device, ordinal);
            if device_code != 0 {
                return Err(hip_error(&library, device_code, "hipDeviceGet"));
            }

            let mut name_buffer: [c_char; 256] = [0; 256];
            let name_code =
                hip_device_get_name(name_buffer.as_mut_ptr(), name_buffer.len() as c_int, device);
            if name_code != 0 {
                return Err(hip_error(&library, name_code, "hipDeviceGetName"));
            }
            let name = CStr::from_ptr(name_buffer.as_ptr())
                .to_string_lossy()
                .trim()
                .to_string();

            let mut vram = None;
            if hip_set_device(ordinal) == 0 {
                let mut free_bytes = 0usize;
                let mut total_bytes = 0usize;
                if hip_mem_get_info(&mut free_bytes, &mut total_bytes) == 0 && total_bytes != 0 {
                    vram = Some(total_bytes as u64);
                }
            }
            let vram_gb = vram
                .map(|bytes| format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0)))
                .unwrap_or_else(|| "N/A".into());
            let architecture = crate::hip_photon::detected_architecture(ordinal as usize)
                .unwrap_or_else(|_| "unknown".into());
            let detail = format!(
                "hip ordinal={ordinal}; gfx={architecture}; runtime={runtime_version}; driver={driver_version}; vram={vram_gb}"
            );
            let pci = hip_device_get_pci_bus_id.as_ref().and_then(|get_bus_id| {
                let mut bus_id: [c_char; 64] = [0; 64];
                (get_bus_id(bus_id.as_mut_ptr(), bus_id.len() as c_int, device) == 0)
                    .then(|| PciAddress::parse(&CStr::from_ptr(bus_id.as_ptr()).to_string_lossy()))
                    .flatten()
            });
            out.push(GpuDevice {
                pci,
                index: ordinal as u32,
                name: if name.is_empty() {
                    format!("AMD GPU {ordinal}")
                } else {
                    name
                },
                vendor: "AMD".into(),
                vram_bytes: vram,
                backend: BackendKind::Hip,
                integrated: crate::hip_photon::is_apu_architecture(&architecture),
                ready: crate::hip_photon::code_objects_installed(&architecture)
                    && !(cfg!(windows) && crate::hip_photon::is_apu_architecture(&architecture)),
                detail,
            });
        }
        Ok(out)
    }
}

/// CUDA device details for selection and display.
struct CudaInfo {
    name: String,
    detail: String,
    vram: Option<u64>,
    compute: (i32, i32),
    pci: Option<PciAddress>,
}

/// Reads CUDA device details for selection and display.
fn cuda_device_info(index: u32, ctx: &CudaContext) -> CudaInfo {
    // Best-effort via driver sys; fall back to ordinal if attrs fail.
    let mut name_buf: [c_char; 256] = [0; 256];
    let name = unsafe {
        let mut dev: sys::CUdevice = 0;
        if sys::cuDeviceGet(&mut dev, index as i32) == sys::CUresult::CUDA_SUCCESS {
            let _ = sys::cuDeviceGetName(name_buf.as_mut_ptr(), name_buf.len() as i32, dev);
        }
        let bytes: Vec<u8> = name_buf
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        let s = String::from_utf8_lossy(&bytes).trim().to_string();
        if s.is_empty() {
            format!("NVIDIA GPU {index}")
        } else {
            s
        }
    };

    let mut major = 0i32;
    let mut minor = 0i32;
    let mut total_mem: usize = 0;
    let mut bus_id: [c_char; 64] = [0; 64];
    let mut pci = None;
    unsafe {
        let mut dev: sys::CUdevice = 0;
        if sys::cuDeviceGet(&mut dev, index as i32) == sys::CUresult::CUDA_SUCCESS {
            let _ = sys::cuDeviceGetAttribute(
                &mut major,
                sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR,
                dev,
            );
            let _ = sys::cuDeviceGetAttribute(
                &mut minor,
                sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR,
                dev,
            );
            let _ = sys::cuDeviceTotalMem_v2(&mut total_mem, dev);
            if sys::cuDeviceGetPCIBusId(bus_id.as_mut_ptr(), bus_id.len() as i32, dev)
                == sys::CUresult::CUDA_SUCCESS
            {
                pci = PciAddress::parse(&CStr::from_ptr(bus_id.as_ptr()).to_string_lossy());
            }
        }
    }
    let _ = ctx; // keep context live for future probes
    let vram = if total_mem > 0 {
        Some(total_mem as u64)
    } else {
        None
    };
    let vram_gb = vram.map(|b| format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0)));
    let detail = format!(
        "cuda ordinal={index}; sm_{}{}; vram={}",
        major,
        minor,
        vram_gb.unwrap_or_else(|| "N/A".into())
    );
    CudaInfo {
        name,
        detail,
        vram,
        compute: (major, minor),
        pci,
    }
}

/// Prints available backend and GPU device information.
pub fn print_devices(prefer: BackendKind) -> Result<(), String> {
    let devices = list_devices(prefer)?;
    println!("backend preference: {}", prefer.as_str());
    for d in devices {
        let vram = d
            .vram_bytes
            .map(|b| format!("{:.1} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0)))
            .unwrap_or_else(|| "N/A".into());
        println!(
            "[{}:{}] {} | {} | VRAM {} | {}",
            d.backend.as_str(),
            d.index,
            d.vendor,
            d.name,
            vram,
            d.detail
        );
    }
    if prefer == BackendKind::Auto {
        let gpus = mining_gpus();
        let mines_by_default =
            select_gpus(gpus.clone(), &DeviceSelection::Default, prefer).unwrap_or_default();
        println!("mining GPUs (numbers for --device):");
        for (number, gpu) in gpus.iter().enumerate() {
            let default = mines_by_default
                .iter()
                .any(|other| (other.backend, other.index) == (gpu.backend, gpu.index));
            println!(
                "  {number}: {} | {}:{} | {} | {}",
                gpu.name,
                gpu.backend.as_str(),
                gpu.index,
                if gpu.integrated {
                    "integrated"
                } else {
                    "discrete"
                },
                if default {
                    "mines by default"
                } else {
                    "add with --include-integrated or --device"
                }
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_backend_parser_accepts_wgpu_surface() {
        assert_eq!(BackendKind::parse("auto").unwrap(), BackendKind::Auto);
        assert_eq!(BackendKind::parse("cuda").unwrap(), BackendKind::Cuda);
        assert_eq!(BackendKind::parse("hip").unwrap(), BackendKind::Hip);
        assert_eq!(BackendKind::parse("rocm").unwrap(), BackendKind::Hip);
        assert_eq!(BackendKind::parse("wgpu").unwrap(), BackendKind::Wgpu);
    }

    // #### PR #22: a missing NVIDIA driver is an error, not a crash
    #[test]
    fn cuda_discovery_without_a_driver_reports_an_error() {
        // Runs where no NVIDIA driver is installed, such as the CI runners.
        if cfg!(target_os = "macos") || unsafe { sys::is_culib_present() } {
            return;
        }
        let error = list_cuda_devices().unwrap_err();
        assert!(error.contains("no NVIDIA driver"), "{error}");
        // Auto discovery moves on to HIP and WGPU instead of panicking.
        let _ = list_devices(BackendKind::Auto);
    }

    #[cfg(feature = "portable-wgpu")]
    #[test]
    fn wgpu_discovery_excludes_cpu_and_other_adapters() {
        assert!(is_hardware_wgpu_device_type(wgpu::DeviceType::DiscreteGpu));
        assert!(is_hardware_wgpu_device_type(
            wgpu::DeviceType::IntegratedGpu
        ));
        assert!(is_hardware_wgpu_device_type(wgpu::DeviceType::VirtualGpu));
        assert!(!is_hardware_wgpu_device_type(wgpu::DeviceType::Cpu));
        assert!(!is_hardware_wgpu_device_type(wgpu::DeviceType::Other));
    }

    #[cfg(feature = "portable-wgpu")]
    #[test]
    fn production_wgpu_surface_matches_platform() {
        if std::env::var_os("PICKAXE_WGPU_API").is_some() {
            return;
        }
        assert_eq!(
            production_wgpu_backends(),
            Ok(if cfg!(target_os = "macos") {
                wgpu::Backends::METAL
            } else {
                wgpu::Backends::VULKAN
            })
        );
    }

    #[test]
    fn production_backend_gate_accepts_validated_gpu_engines() {
        assert!(require_production_mining_backend(BackendKind::Cuda).is_ok());
        assert!(require_production_mining_backend(BackendKind::Hip).is_ok());
        assert_eq!(
            require_production_mining_backend(BackendKind::Wgpu).is_ok(),
            cfg!(feature = "portable-wgpu")
        );
        assert!(require_production_mining_backend(BackendKind::Auto).is_err());
    }

    #[test]
    fn explicit_device_selection_uses_backend_local_ordinal() {
        let devices = vec![GpuDevice {
            index: 2,
            name: "test".into(),
            vendor: "AMD".into(),
            vram_bytes: Some(1024),
            backend: BackendKind::Hip,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: None,
        }];
        let selected = backend_pick(devices, Some(2), BackendKind::Hip).unwrap();
        assert_eq!(selected.backend, BackendKind::Hip);
        assert_eq!(selected.index, 2);
    }

    fn fixture_device(index: u32, vendor: &str, backend: BackendKind) -> GpuDevice {
        GpuDevice {
            index,
            name: format!("{vendor} fixture"),
            vendor: vendor.into(),
            vram_bytes: None,
            backend,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: None,
        }
    }

    fn integrated_fixture(index: u32, vendor: &str, backend: BackendKind) -> GpuDevice {
        GpuDevice {
            integrated: true,
            ..fixture_device(index, vendor, backend)
        }
    }

    fn at_bus(device: GpuDevice, bus: u8) -> GpuDevice {
        GpuDevice {
            pci: Some(PciAddress {
                domain: 0,
                bus,
                device: 0,
                function: 0,
            }),
            ..device
        }
    }

    #[test]
    fn opencl_is_the_last_engine_and_mines_only_what_no_other_engine_sees() {
        // The same integrated GPU through wgpu and OpenCL, under different
        // names and without PCI addresses: one GPU, mined through wgpu.
        let mut opencl_view = integrated_fixture(0, "AMD", BackendKind::OpenCl);
        opencl_view.name = "gfx1036".into();
        let gpus = physical_gpus_with_opencl(
            Vec::new(),
            Vec::new(),
            vec![integrated_fixture(0, "AMD", BackendKind::Wgpu)],
            vec![opencl_view],
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].backend, BackendKind::Wgpu);
        // A discrete card the other engines drive keeps its engine.
        let gpus = physical_gpus_with_opencl(
            vec![at_bus(fixture_device(0, "NVIDIA", BackendKind::Cuda), 1)],
            Vec::new(),
            Vec::new(),
            vec![at_bus(fixture_device(0, "NVIDIA", BackendKind::OpenCl), 1)],
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].backend, BackendKind::Cuda);
        // A GPU only OpenCL sees, such as an old integrated GPU, mines on it.
        let gpus = physical_gpus_with_opencl(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![integrated_fixture(0, "Intel", BackendKind::OpenCl)],
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].backend, BackendKind::OpenCl);
        assert_eq!(BackendKind::parse("opencl").unwrap(), BackendKind::OpenCl);
        assert_eq!(BackendKind::OpenCl.as_str(), "opencl");
    }

    /// The first GPU automatic selection mines on, or the GPU numbered `wanted`.
    fn auto(
        cuda: Vec<GpuDevice>,
        hip: Vec<GpuDevice>,
        wgpu: Vec<GpuDevice>,
        wanted: Option<u32>,
    ) -> Option<GpuDevice> {
        let selection = wanted.map_or(DeviceSelection::Default, |index| {
            DeviceSelection::Indices(vec![index])
        });
        select_gpus(
            physical_gpus(cuda, hip, wgpu),
            &selection,
            BackendKind::Auto,
        )
        .ok()?
        .into_iter()
        .next()
    }

    /// The first GPU an explicit backend mines on, or its ordinal `wanted`.
    fn backend_pick(
        devices: Vec<GpuDevice>,
        wanted: Option<u32>,
        backend: BackendKind,
    ) -> Result<GpuDevice, String> {
        let selection = wanted.map_or(DeviceSelection::Default, |index| {
            DeviceSelection::Indices(vec![index])
        });
        select_gpus(devices, &selection, backend).map(|gpus| gpus[0].clone())
    }

    #[test]
    fn auto_selection_prefers_cuda_over_wgpu_for_nvidia() {
        let selected = auto(
            vec![fixture_device(0, "NVIDIA", BackendKind::Cuda)],
            Vec::new(),
            vec![fixture_device(0, "NVIDIA", BackendKind::Wgpu)],
            None,
        )
        .unwrap();
        assert_eq!(selected.backend, BackendKind::Cuda);
    }

    #[test]
    fn auto_selection_prefers_native_hip_t2_for_a_discrete_amd_gpu() {
        let selected = auto(
            Vec::new(),
            vec![fixture_device(0, "AMD", BackendKind::Hip)],
            vec![fixture_device(0, "AMD", BackendKind::Wgpu)],
            None,
        )
        .unwrap();
        assert_eq!(selected.backend, BackendKind::Hip);
    }

    #[test]
    fn auto_selection_uses_vulkan_when_hip_code_objects_are_missing() {
        let selected = auto(
            Vec::new(),
            vec![GpuDevice {
                ready: false,
                ..fixture_device(0, "AMD", BackendKind::Hip)
            }],
            vec![fixture_device(0, "AMD", BackendKind::Wgpu)],
            None,
        )
        .unwrap();
        assert_eq!(selected.backend, BackendKind::Wgpu);
    }

    #[test]
    fn auto_selection_falls_back_to_hip_without_a_wgpu_device() {
        let selected = auto(
            Vec::new(),
            vec![fixture_device(0, "AMD", BackendKind::Hip)],
            Vec::new(),
            None,
        )
        .unwrap();
        assert_eq!(selected.backend, BackendKind::Hip);
    }

    #[test]
    fn auto_selection_skips_an_integrated_gpu_listed_before_the_discrete_one() {
        let selected = auto(
            Vec::new(),
            vec![integrated_fixture(0, "AMD", BackendKind::Hip)],
            vec![
                integrated_fixture(0, "AMD", BackendKind::Wgpu),
                fixture_device(1, "AMD", BackendKind::Wgpu),
            ],
            None,
        )
        .unwrap();
        assert!(!selected.integrated);
        assert_eq!((selected.backend, selected.index), (BackendKind::Wgpu, 1));
    }

    #[test]
    fn explicit_device_can_select_an_integrated_gpu() {
        let selected = auto(
            vec![fixture_device(0, "NVIDIA", BackendKind::Cuda)],
            Vec::new(),
            vec![integrated_fixture(1, "AMD", BackendKind::Wgpu)],
            Some(1),
        )
        .unwrap();
        assert!(selected.integrated);
    }

    #[test]
    fn auto_selection_uses_an_integrated_gpu_when_no_discrete_gpu_exists() {
        let selected = auto(
            Vec::new(),
            vec![integrated_fixture(0, "AMD", BackendKind::Hip)],
            vec![integrated_fixture(0, "AMD", BackendKind::Wgpu)],
            None,
        )
        .unwrap();
        assert!(selected.integrated);
        assert_eq!(selected.backend, BackendKind::Wgpu);
    }

    #[test]
    fn explicit_backend_without_device_skips_integrated_gpus() {
        let devices = vec![
            integrated_fixture(0, "AMD", BackendKind::Wgpu),
            fixture_device(1, "NVIDIA", BackendKind::Wgpu),
        ];
        let selected = backend_pick(devices, None, BackendKind::Wgpu).unwrap();
        assert_eq!(selected.index, 1);
    }

    #[test]
    fn explicit_backend_and_device_can_select_an_integrated_gpu() {
        let devices = vec![
            integrated_fixture(0, "AMD", BackendKind::Hip),
            fixture_device(1, "AMD", BackendKind::Hip),
        ];
        let selected = backend_pick(devices, Some(0), BackendKind::Hip).unwrap();
        assert!(selected.integrated);
    }

    #[test]
    fn explicit_backend_without_device_falls_back_to_a_lone_integrated_gpu() {
        let devices = vec![integrated_fixture(0, "AMD", BackendKind::Hip)];
        let selected = backend_pick(devices, None, BackendKind::Hip).unwrap();
        assert!(selected.integrated);
    }

    // #### PR #22: every GPU mines once, on its best engine
    #[test]
    fn pci_addresses_parse_in_every_engine_format() {
        let expected = PciAddress {
            domain: 0,
            bus: 1,
            device: 0,
            function: 0,
        };
        for text in [
            "0000:01:00.0",
            "00000000:01:00.0",
            "01:00.0",
            " 0000:01:00.0 ",
        ] {
            assert_eq!(PciAddress::parse(text), Some(expected), "{text}");
        }
        assert_eq!(
            PciAddress::parse("0001:C5:1F.7"),
            Some(PciAddress {
                domain: 1,
                bus: 0xc5,
                device: 0x1f,
                function: 7,
            })
        );
        for text in ["", "01:00", "x:01:00.0", "0000:01:00:00.0", "0000:100:00.0"] {
            assert_eq!(PciAddress::parse(text), None, "{text}");
        }
        assert_eq!(expected.to_string(), "0000:01:00.0");
    }

    #[test]
    fn device_selection_takes_all_one_number_or_a_list() {
        assert_eq!(DeviceSelection::parse("all"), Ok(DeviceSelection::Default));
        assert_eq!(DeviceSelection::parse("ALL"), Ok(DeviceSelection::Default));
        assert_eq!(
            DeviceSelection::parse("2"),
            Ok(DeviceSelection::Indices(vec![2]))
        );
        assert_eq!(
            DeviceSelection::parse(" 0, 2,5 "),
            Ok(DeviceSelection::Indices(vec![0, 2, 5]))
        );
        for bad in ["", "x", "0,", "0,0", "-1"] {
            assert!(DeviceSelection::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            DeviceSelection::Default.with_integrated(true),
            DeviceSelection::WithIntegrated
        );
        assert_eq!(
            DeviceSelection::Default.with_integrated(false),
            DeviceSelection::Default
        );
        assert_eq!(
            DeviceSelection::Indices(vec![1]).with_integrated(true),
            DeviceSelection::Indices(vec![1])
        );
        assert_eq!(
            DeviceSelection::Indices(vec![3]).single_index(),
            Ok(Some(3))
        );
        assert_eq!(DeviceSelection::Default.single_index(), Ok(None));
        assert!(DeviceSelection::Indices(vec![0, 1]).single_index().is_err());
        assert_eq!(DeviceSelection::Indices(vec![0, 2]).to_string(), "0,2");
    }

    #[test]
    fn one_card_listed_by_cuda_and_wgpu_mines_once_on_cuda() {
        // An RTX 50 laptop with an AMD iGPU, as wgpu and CUDA list it.
        let gpus = physical_gpus(
            vec![at_bus(fixture_device(0, "NVIDIA", BackendKind::Cuda), 1)],
            Vec::new(),
            vec![
                at_bus(integrated_fixture(0, "AMD", BackendKind::Wgpu), 0xc5),
                at_bus(fixture_device(1, "NVIDIA", BackendKind::Wgpu), 1),
            ],
        );
        let engines: Vec<_> = gpus
            .iter()
            .map(|gpu| (gpu.backend, gpu.index, gpu.integrated))
            .collect();
        assert_eq!(
            engines,
            [(BackendKind::Cuda, 0, false), (BackendKind::Wgpu, 0, true)]
        );
        let default =
            select_gpus(gpus.clone(), &DeviceSelection::Default, BackendKind::Auto).unwrap();
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].backend, BackendKind::Cuda);
        let both = select_gpus(gpus, &DeviceSelection::WithIntegrated, BackendKind::Auto).unwrap();
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn nvidia_cards_without_sm120_mine_on_wgpu() {
        let gpus = physical_gpus(
            vec![at_bus(
                GpuDevice {
                    ready: false,
                    ..fixture_device(0, "NVIDIA", BackendKind::Cuda)
                },
                3,
            )],
            Vec::new(),
            vec![at_bus(fixture_device(0, "NVIDIA", BackendKind::Wgpu), 3)],
        );
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].backend, BackendKind::Wgpu);
    }

    #[test]
    fn identical_cards_at_different_addresses_both_mine() {
        let gpus = physical_gpus(
            vec![
                at_bus(fixture_device(0, "NVIDIA", BackendKind::Cuda), 1),
                at_bus(fixture_device(1, "NVIDIA", BackendKind::Cuda), 2),
            ],
            Vec::new(),
            vec![
                at_bus(fixture_device(0, "NVIDIA", BackendKind::Wgpu), 2),
                at_bus(fixture_device(1, "NVIDIA", BackendKind::Wgpu), 1),
            ],
        );
        let engines: Vec<_> = gpus.iter().map(|gpu| (gpu.backend, gpu.index)).collect();
        assert_eq!(engines, [(BackendKind::Cuda, 0), (BackendKind::Cuda, 1)]);
        let picked =
            select_gpus(gpus, &DeviceSelection::Indices(vec![1]), BackendKind::Auto).unwrap();
        assert_eq!((picked[0].backend, picked[0].index), (BackendKind::Cuda, 1));
    }

    #[test]
    fn without_addresses_each_engine_entry_pairs_with_one_card() {
        // Two identical cards; wgpu reports no PCI addresses (for example DirectX 12).
        let gpus = physical_gpus(
            vec![
                fixture_device(0, "NVIDIA", BackendKind::Cuda),
                fixture_device(1, "NVIDIA", BackendKind::Cuda),
            ],
            Vec::new(),
            vec![
                fixture_device(0, "NVIDIA", BackendKind::Wgpu),
                fixture_device(1, "NVIDIA", BackendKind::Wgpu),
            ],
        );
        assert_eq!(gpus.len(), 2);
        assert!(gpus.iter().all(|gpu| gpu.backend == BackendKind::Cuda));
        // A Mesa suffix still names the same card; a different model does not.
        assert!(same_name(
            "AMD Radeon RX 7900 XTX",
            "AMD Radeon RX 7900 XTX (RADV NAVI31)"
        ));
        assert!(!same_name("AMD Radeon RX 7900", "AMD Radeon RX 7900 XTX"));
    }

    #[test]
    fn explicit_backend_selects_every_discrete_gpu_by_default() {
        let devices = vec![
            fixture_device(0, "NVIDIA", BackendKind::Cuda),
            fixture_device(1, "NVIDIA", BackendKind::Cuda),
        ];
        let all = select_gpus(
            devices.clone(),
            &DeviceSelection::Default,
            BackendKind::Cuda,
        )
        .unwrap();
        assert_eq!(all.len(), 2);
        assert!(select_gpus(
            devices,
            &DeviceSelection::Indices(vec![5]),
            BackendKind::Cuda
        )
        .unwrap_err()
        .contains("CUDA device 5 not found"));
        assert!(
            select_gpus(Vec::new(), &DeviceSelection::Default, BackendKind::Auto)
                .unwrap_err()
                .contains("no GPU device found")
        );
    }
}
