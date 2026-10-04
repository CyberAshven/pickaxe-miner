//! Persistent native HIP PHOTON candidate engine.

use crate::cuda_photon::{PhotonCudaBatchResult, PhotonCudaWinner};
use crate::m29_table::{self, M29TableSource};
use crate::tx::{self, PhotonLayout};
use libloading::Library;
use num_bigint::BigUint;
use secp256k1::{PublicKey, SecretKey};
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;

/// Template buffer size: the widest layout with GPU kernels.
const MAX_TX_BYTES: usize = 615 + PhotonLayout::MAX_SHIFT;
const SIGNATURE_BYTES: usize = 64;
const POINT_WORDS: usize = 24;
const FIXED_D_WORDS: usize = 32 * 256 * 8;
const HIP_SUCCESS: c_int = 0;
const HIP_MEMCPY_HOST_TO_DEVICE: c_int = 1;
const HIP_MEMCPY_DEVICE_TO_HOST: c_int = 2;
const HIP_CODE_OBJECT_NAMES: [&str; 5] = [
    "stage_a_rfc6979.hsaco",
    "photon_stage_b16.hsaco",
    "photon_c1_schnorr.hsaco",
    "stage_c_hash.hsaco",
    "photon_t2_tail.hsaco",
];
/// The shared Rust GPU engine built for AMD: the same source as photon_rust.ptx.
const HIP_RUST_CODE_OBJECT: &str = "photon_rust.hsaco";

// #### PR #22: swappable HIP kernel builds
// What: PICKAXE_HIP_KERNELS=rust loads photon_rust.hsaco, built from the shared
// Rust engine; the default loads the CUDA C++ kernels compiled for HIP.
// Why: one Rust source for NVIDIA and AMD, while the proven C++ build stays
// the default until the Rust build has mined on AMD hardware.
// Check: both builds ship for every architecture; the Rust launch geometry
// must match the group sizes the AMD build was compiled with.
/// Which kernel build the native HIP engine loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HipKernelSource {
    Cpp,
    Rust,
}

impl HipKernelSource {
    /// Parses PICKAXE_HIP_KERNELS; unset or empty keeps the C++ default.
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") | Some("cpp") => Ok(Self::Cpp),
            Some("rust") => Ok(Self::Rust),
            Some(other) => Err(format!(
                "PICKAXE_HIP_KERNELS must be cpp or rust, not {other:?}"
            )),
        }
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        Self::parse(std::env::var("PICKAXE_HIP_KERNELS").ok().as_deref())
    }

    fn code_objects(self) -> &'static [&'static str] {
        match self {
            Self::Cpp => &HIP_CODE_OBJECT_NAMES,
            Self::Rust => &[HIP_RUST_CODE_OBJECT],
        }
    }

    /// T2 filter group size: the AMD Rust build computes its index from 256.
    fn t2_filter_threads(self) -> u32 {
        match self {
            Self::Cpp => 128,
            Self::Rust => 256,
        }
    }

    fn build_hint(self, architecture: &str) -> String {
        match self {
            Self::Cpp => format!(
                "run `python tools/build_hip.py --arch {architecture}` with a matching ROCm/HIP compiler"
            ),
            Self::Rust => format!("run `python tools/build_rust_amd.py --arch {architecture}`"),
        }
    }
}

/// Non-T2 filter kernels: the C++ C1 and C2/C3 stages, or the Rust dual filter.
#[derive(Clone, Copy)]
enum PlainFilter {
    Cpp {
        stage_c1: usize,
        stage_c3: usize,
    },
    Rust {
        dual_filter: [usize; PhotonLayout::GPU_SHIFTS.len()],
    },
}
const HIP_STAGE_A_SYMBOL: &str = "pickaxe_stage_a_rfc6979";
const HIP_STAGE_B_SYMBOLS: [&str; 4] = [
    "pickaxe_photon_b16_part0",
    "pickaxe_photon_b16_part1",
    "pickaxe_photon_b16_part2",
    "pickaxe_photon_b16_part3",
];
const HIP_STAGE_C1_SYMBOL: &str = "pickaxe_photon_c1_schnorr";
const HIP_STAGE_C3_SYMBOL: &str = "pickaxe_stage_c_hash_filter";
const HIP_STAGE_C1_DUAL_SYMBOL: &str = "pickaxe_photon_c1_schnorr_dual_batched";
/// T2 searches 65,536 token amounts under each signature.
const T2_CANDIDATES: u32 = 65_536;
/// Signature windows per T2 batch, matching the CUDA T2 group.
const T2_GROUP_WINDOWS: u32 = 256;
pub(crate) const HIP_T2_GROUP_CANDIDATES: u32 = T2_GROUP_WINDOWS * T2_CANDIDATES;
/// A batch that starts inside a window spans one extra signature window.
const T2_MAX_WINDOWS: usize = T2_GROUP_WINDOWS as usize + 1;
/// The C++ group kernel's block size; a block crosses at most one window edge.
const T2_THREADS: u32 = 128;
/// Transaction bytes 0..384 stay fixed for every nonce and amount.
const T2_MIDSTATE_BYTES: usize = 384;
const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HipArgKind {
    Ptr,
    U32,
    U64,
}

const HIP_STAGE_A_ABI: [HipArgKind; 6] = [
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
];
const HIP_STAGE_B_ABI: [HipArgKind; 4] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
];
const HIP_STAGE_C1_ABI: [HipArgKind; 7] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
];
const HIP_STAGE_C3_ABI: [HipArgKind; 11] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::U32,
];
const HIP_STAGE_C1_DUAL_ABI: [HipArgKind; 9] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::U32,
];
const HIP_T2_PREPARE_ABI: [HipArgKind; 9] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
];
const HIP_T2_FILTER_GROUP_ABI: [HipArgKind; 14] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U64,
    HipArgKind::U64,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::U32,
    HipArgKind::U32,
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
];
const HIP_RUST_DUAL_FILTER_ABI: [HipArgKind; 12] = [
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::U32,
    HipArgKind::U32,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
    HipArgKind::Ptr,
];
const MAX_HIP_KERNEL_ARGS: usize = HIP_T2_FILTER_GROUP_ABI.len();

type HipError = c_int;
type HipInit = unsafe extern "C" fn(c_uint) -> HipError;
type HipSetDevice = unsafe extern "C" fn(c_int) -> HipError;
type HipGetErrorString = unsafe extern "C" fn(HipError) -> *const c_char;
type HipGetDeviceProperties = unsafe extern "C" fn(*mut c_void, c_int) -> HipError;
type HipStreamCreate = unsafe extern "C" fn(*mut *mut c_void) -> HipError;
type HipStreamDestroy = unsafe extern "C" fn(*mut c_void) -> HipError;
type HipStreamSynchronize = unsafe extern "C" fn(*mut c_void) -> HipError;
type HipMalloc = unsafe extern "C" fn(*mut *mut c_void, usize) -> HipError;
type HipFree = unsafe extern "C" fn(*mut c_void) -> HipError;
type HipMemcpy = unsafe extern "C" fn(*mut c_void, *const c_void, usize, c_int) -> HipError;
type HipMemsetAsync = unsafe extern "C" fn(*mut c_void, c_int, usize, *mut c_void) -> HipError;
type HipModuleLoad = unsafe extern "C" fn(*mut *mut c_void, *const c_char) -> HipError;
type HipModuleUnload = unsafe extern "C" fn(*mut c_void) -> HipError;
type HipModuleGetFunction =
    unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const c_char) -> HipError;
type HipModuleLaunchKernel = unsafe extern "C" fn(
    *mut c_void,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    *mut c_void,
    *mut *mut c_void,
    *mut *mut c_void,
) -> HipError;

#[cfg(target_os = "windows")]
const HIP_LIBRARY_CANDIDATES: &[&str] = &["amdhip64.dll", "amdhip64_6.dll"];
#[cfg(not(target_os = "windows"))]
const HIP_LIBRARY_CANDIDATES: &[&str] = &["libamdhip64.so", "libamdhip64.so.6", "libamdhip64.so.5"];

struct HipApi {
    _library: Library,
    set_device: HipSetDevice,
    get_error_string: HipGetErrorString,
    stream_create: HipStreamCreate,
    stream_destroy: HipStreamDestroy,
    stream_synchronize: HipStreamSynchronize,
    malloc: HipMalloc,
    free: HipFree,
    memcpy: HipMemcpy,
    memset_async: HipMemsetAsync,
    module_load: HipModuleLoad,
    module_unload: HipModuleUnload,
    module_get_function: HipModuleGetFunction,
    module_launch_kernel: HipModuleLaunchKernel,
    get_device_properties: HipGetDeviceProperties,
}

impl HipApi {
    /// Loads HipApi state needed by the HIP GPU pipeline.
    fn load() -> Result<Arc<Self>, String> {
        let mut failures = Vec::new();
        for candidate in HIP_LIBRARY_CANDIDATES {
            let library = match unsafe { Library::new(*candidate) } {
                Ok(library) => library,
                Err(error) => {
                    failures.push(format!("{candidate}: {error}"));
                    continue;
                }
            };
            return unsafe { Self::from_library(library) }.map(Arc::new);
        }
        Err(format!(
            "HIP/ROCm runtime unavailable: {}",
            failures.join("; ")
        ))
    }

    /// Resolves the required HIP runtime symbols from a loaded library.
    unsafe fn from_library(library: Library) -> Result<Self, String> {
        /// Loads a typed symbol from the HIP runtime library.
        unsafe fn symbol<T: Copy>(library: &Library, name: &'static [u8]) -> Result<T, String> {
            unsafe { library.get::<T>(name) }
                .map(|symbol| *symbol)
                .map_err(|error| {
                    format!(
                        "load HIP symbol {}: {error}",
                        String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
                    )
                })
        }

        let init = unsafe { symbol::<HipInit>(&library, b"hipInit\0")? };
        let init_code = unsafe { init(0) };
        if init_code != HIP_SUCCESS {
            return Err(format!("hipInit failed with HIP error {init_code}"));
        }
        let get_device_properties = unsafe {
            library
                .get::<HipGetDeviceProperties>(b"hipGetDevicePropertiesR0600\0")
                .or_else(|_| library.get::<HipGetDeviceProperties>(b"hipGetDeviceProperties\0"))
                .map(|symbol| *symbol)
                .map_err(|error| format!("load hipGetDeviceProperties: {error}"))?
        };

        Ok(Self {
            set_device: unsafe { symbol(&library, b"hipSetDevice\0")? },
            get_error_string: unsafe { symbol(&library, b"hipGetErrorString\0")? },
            stream_create: unsafe { symbol(&library, b"hipStreamCreate\0")? },
            stream_destroy: unsafe { symbol(&library, b"hipStreamDestroy\0")? },
            stream_synchronize: unsafe { symbol(&library, b"hipStreamSynchronize\0")? },
            malloc: unsafe { symbol(&library, b"hipMalloc\0")? },
            free: unsafe { symbol(&library, b"hipFree\0")? },
            memcpy: unsafe { symbol(&library, b"hipMemcpy\0")? },
            memset_async: unsafe { symbol(&library, b"hipMemsetAsync\0")? },
            module_load: unsafe { symbol(&library, b"hipModuleLoad\0")? },
            module_unload: unsafe { symbol(&library, b"hipModuleUnload\0")? },
            module_get_function: unsafe { symbol(&library, b"hipModuleGetFunction\0")? },
            module_launch_kernel: unsafe { symbol(&library, b"hipModuleLaunchKernel\0")? },
            get_device_properties,
            _library: library,
        })
    }

    /// Formats the last HIP runtime error for reporting.
    fn error(&self, code: HipError, operation: &str) -> String {
        let detail = unsafe {
            let pointer = (self.get_error_string)(code);
            (!pointer.is_null()).then(|| CStr::from_ptr(pointer).to_string_lossy().into_owned())
        };
        match detail {
            Some(detail) => format!("{operation} failed with HIP error {code}: {detail}"),
            None => format!("{operation} failed with HIP error {code}"),
        }
    }

    /// Converts a HIP return code into a descriptive result.
    fn check(&self, code: HipError, operation: &str) -> Result<(), String> {
        if code == HIP_SUCCESS {
            Ok(())
        } else {
            Err(self.error(code, operation))
        }
    }
}

struct HipBuffer {
    api: Arc<HipApi>,
    ptr: usize,
    bytes: usize,
}

impl HipBuffer {
    /// Allocates a persistent HIP device buffer.
    fn allocate(api: &Arc<HipApi>, bytes: usize, label: &str) -> Result<Self, String> {
        let mut raw = ptr::null_mut();
        api.check(
            unsafe { (api.malloc)(&mut raw, bytes) },
            &format!("alloc {label}"),
        )?;
        Ok(Self {
            api: Arc::clone(api),
            ptr: raw as usize,
            bytes,
        })
    }

    /// Copies host bytes into a HIP device buffer.
    fn copy_from<T>(&self, source: &[T], label: &str) -> Result<(), String> {
        let bytes = std::mem::size_of_val(source);
        if bytes > self.bytes {
            return Err(format!(
                "{label} upload is {bytes} bytes but HIP buffer capacity is {}",
                self.bytes
            ));
        }
        self.api.check(
            unsafe {
                (self.api.memcpy)(
                    self.ptr as *mut c_void,
                    source.as_ptr().cast(),
                    bytes,
                    HIP_MEMCPY_HOST_TO_DEVICE,
                )
            },
            label,
        )
    }

    /// Copies bytes from a HIP device buffer to the host.
    fn copy_to<T>(&self, destination: &mut [T], label: &str) -> Result<(), String> {
        let bytes = std::mem::size_of_val(destination);
        if bytes > self.bytes {
            return Err(format!(
                "{label} readback is {bytes} bytes but HIP buffer capacity is {}",
                self.bytes
            ));
        }
        self.api.check(
            unsafe {
                (self.api.memcpy)(
                    destination.as_mut_ptr().cast(),
                    self.ptr as *const c_void,
                    bytes,
                    HIP_MEMCPY_DEVICE_TO_HOST,
                )
            },
            label,
        )
    }
}

impl Drop for HipBuffer {
    /// Releases resources owned by HipBuffer.
    fn drop(&mut self) {
        if self.ptr != 0 {
            unsafe {
                (self.api.free)(self.ptr as *mut c_void);
            }
        }
    }
}

struct HipStream {
    api: Arc<HipApi>,
    raw: usize,
}

impl HipStream {
    /// Creates a HIP stream for ordered GPU work.
    fn create(api: &Arc<HipApi>) -> Result<Self, String> {
        let mut raw = ptr::null_mut();
        api.check(
            unsafe { (api.stream_create)(&mut raw) },
            "create HIP stream",
        )?;
        Ok(Self {
            api: Arc::clone(api),
            raw: raw as usize,
        })
    }

    /// Waits for queued HIP stream operations to finish.
    fn synchronize(&self) -> Result<(), String> {
        self.api.check(
            unsafe { (self.api.stream_synchronize)(self.raw as *mut c_void) },
            "synchronize HIP stream",
        )
    }
}

impl Drop for HipStream {
    /// Releases resources owned by HipStream.
    fn drop(&mut self) {
        if self.raw != 0 {
            unsafe {
                (self.api.stream_destroy)(self.raw as *mut c_void);
            }
        }
    }
}

struct HipModule {
    api: Arc<HipApi>,
    raw: usize,
}

impl HipModule {
    /// Loads HipModule state needed by the HIP GPU pipeline.
    fn load(api: &Arc<HipApi>, path: &Path) -> Result<Self, String> {
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| format!("HIP code-object path contains NUL: {}", path.display()))?;
        let mut raw = ptr::null_mut();
        api.check(
            unsafe { (api.module_load)(&mut raw, c_path.as_ptr()) },
            &format!("load HIP code object {}", path.display()),
        )?;
        Ok(Self {
            api: Arc::clone(api),
            raw: raw as usize,
        })
    }

    /// Resolves a kernel function from a loaded HIP module.
    fn function(&self, name: &str) -> Result<usize, String> {
        let name = CString::new(name).map_err(|_| "HIP kernel name contains NUL".to_string())?;
        let mut raw = ptr::null_mut();
        self.api.check(
            unsafe {
                (self.api.module_get_function)(&mut raw, self.raw as *mut c_void, name.as_ptr())
            },
            "resolve HIP kernel",
        )?;
        Ok(raw as usize)
    }
}

impl Drop for HipModule {
    /// Releases resources owned by HipModule.
    fn drop(&mut self) {
        if self.raw != 0 {
            unsafe {
                (self.api.module_unload)(self.raw as *mut c_void);
            }
        }
    }
}

pub struct HipPhotonEngine {
    api: Arc<HipApi>,
    stream: HipStream,
    _modules: Vec<HipModule>,
    kernel_source: HipKernelSource,
    stage_a: usize,
    stage_b: [usize; 4],
    plain_filter: PlainFilter,
    stage_c1_dual: usize,
    t2_prepare: [usize; PhotonLayout::GPU_SHIFTS.len()],
    t2_filter_group: [usize; PhotonLayout::GPU_SHIFTS.len()],
    table_gpu: HipBuffer,
    target_gpu: HipBuffer,
    private_key_gpu: HipBuffer,
    public_key_gpu: HipBuffer,
    fixed_d_gpu: HipBuffer,
    message_hashes_gpu: HipBuffer,
    rfc6979_gpu: HipBuffer,
    points_gpu: HipBuffer,
    signatures_gpu: HipBuffer,
    template_gpu: HipBuffer,
    winner_count_gpu: HipBuffer,
    winner_nonces_gpu: HipBuffer,
    winner_hashes_gpu: HipBuffer,
    negated_s_gpu: HipBuffer,
    midstate_gpu: HipBuffer,
    middle_schedule_gpu: HipBuffer,
    window_txs_gpu: HipBuffer,
    window_prefixes_gpu: HipBuffer,
    t2_target_gpu: HipBuffer,
    winner_j_gpu: HipBuffer,
    max_candidates: u32,
    winner_cap: u32,
    #[allow(dead_code)]
    table_source: M29TableSource,
    #[allow(dead_code)]
    architecture: String,
    layout: PhotonLayout,
    positive_target: bool,
    job_ready: bool,
    t2_requested: bool,
    t2: Option<HipT2Amounts>,
}

/// Token amounts of the active T2 job; amount j moves from the reward to the baton.
#[derive(Clone, Copy)]
struct HipT2Amounts {
    baton: u64,
    reward: u64,
}

#[repr(C, align(64))]
struct HipDevicePropertiesStorage([u8; 8192]);

/// Extracts the AMD GPU architecture from device properties.
fn parse_gfx_arch(properties: &[u8]) -> Option<String> {
    let start = properties.windows(3).position(|window| window == b"gfx")?;
    let tail = &properties[start..];
    let len = tail
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric())
        .unwrap_or(tail.len());
    (len > 3).then(|| String::from_utf8_lossy(&tail[..len]).into_owned())
}

/// Reads the architecture supported by a HIP device.
fn device_architecture(api: &HipApi, device_ordinal: usize) -> Result<String, String> {
    let mut properties = HipDevicePropertiesStorage([0u8; 8192]);
    api.check(
        unsafe {
            (api.get_device_properties)(properties.0.as_mut_ptr().cast(), device_ordinal as c_int)
        },
        "query HIP device properties",
    )?;
    parse_gfx_arch(&properties.0).ok_or_else(|| {
        format!(
            "HIP device {device_ordinal} did not report a gfx architecture; refusing an unqualified code object"
        )
    })
}

/// Identifies the current HIP GPU architecture.
pub fn detected_architecture(device_ordinal: usize) -> Result<String, String> {
    let api = HipApi::load()?;
    device_architecture(&api, device_ordinal)
}

/// Lists candidate locations for HIP code objects.
fn code_object_candidate_dirs(
    architecture: &str,
    override_dir: Option<PathBuf>,
    executable_dir: Option<&Path>,
    manifest_dir: &Path,
) -> Vec<PathBuf> {
    if let Some(path) = override_dir {
        return vec![path];
    }

    let mut candidates = Vec::new();
    if let Some(executable_dir) = executable_dir {
        candidates.push(executable_dir.join("hip").join("build").join(architecture));
        candidates.push(executable_dir.join("hip").join(architecture));
    }
    candidates.push(manifest_dir.join("hip").join("build").join(architecture));
    candidates.dedup();
    candidates
}

/// Lists runtime and executable-relative code object directories.
fn code_object_candidate_dirs_for_runtime(architecture: &str) -> Vec<PathBuf> {
    let override_dir = std::env::var_os("PICKAXE_HIP_CODE_OBJECT_DIR").map(PathBuf::from);
    let executable = std::env::current_exe().ok();
    let executable_dir = executable.as_deref().and_then(Path::parent);
    code_object_candidate_dirs(
        architecture,
        override_dir,
        executable_dir,
        Path::new(env!("CARGO_MANIFEST_DIR")),
    )
}

/// Checks that all code objects of one kernel build are present.
fn directory_has_complete_code_objects(directory: &Path, source: HipKernelSource) -> bool {
    source
        .code_objects()
        .iter()
        .all(|name| directory.join(name).is_file())
}

/// Finds a complete HIP code object directory for the device.
fn resolve_code_object_dir(architecture: &str, source: HipKernelSource) -> Result<PathBuf, String> {
    let candidates = code_object_candidate_dirs_for_runtime(architecture);
    if let Some(directory) = candidates
        .iter()
        .find(|directory| directory_has_complete_code_objects(directory, source))
    {
        return Ok(directory.clone());
    }

    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "missing complete PHOTON HIP code-object set for {architecture}; searched: {searched}; {}",
        source.build_hint(architecture)
    ))
}

/// Rejects a code object built for the wrong GPU architecture.
fn verify_code_object_architecture(path: &Path, architecture: &str) -> Result<(), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("read HIP code object {}: {error}", path.display()))?;
    let mut targets = Vec::new();
    let mut index = 0usize;
    while index + 3 <= bytes.len() {
        if &bytes[index..index + 3] != b"gfx" {
            index += 1;
            continue;
        }
        let start = index;
        index += 3;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric()
                || matches!(bytes[index], b':' | b'+' | b'-' | b'_'))
        {
            index += 1;
        }
        if index > start + 3 {
            let target = String::from_utf8_lossy(&bytes[start..index]).into_owned();
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }

    if targets
        .iter()
        .any(|target| target.split(':').next() == Some(architecture))
    {
        return Ok(());
    }

    let observed = if targets.is_empty() {
        "no gfx target metadata found".to_string()
    } else {
        format!("found {}", targets.join(", "))
    };
    Err(format!(
        "PHOTON HIP code object {} is not bound to detected architecture {architecture}: {observed}",
        path.display()
    ))
}

/// Builds the fixed scalar lookup table used by PHOTON kernels.
fn fixed_d_table(private_key: &[u8; 32]) -> Vec<u32> {
    let order = BigUint::from_bytes_be(
        &hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
            .expect("secp256k1 order constant"),
    );
    let d = BigUint::from_bytes_be(private_key);
    let mut factor = d;
    let mut table = vec![0u32; FIXED_D_WORDS];
    for byte_position in 0..32usize {
        for digit in 1..256usize {
            let value = (&factor * BigUint::from(digit as u32)) % &order;
            let limbs = value.to_u32_digits();
            let base = (byte_position * 256 + digit) * 8;
            for (limb, word) in limbs.into_iter().take(8).enumerate() {
                table[base + limb] = word;
            }
        }
        factor = (factor * BigUint::from(256u32)) % &order;
    }
    table
}

/// Launches a HIP kernel with the given arguments and grid size.
fn launch(
    api: &HipApi,
    stream: &HipStream,
    function: usize,
    geometry: (u32, u32),
    params: &mut [HipKernelArg],
    expected_abi: &[HipArgKind],
    label: &str,
) -> Result<(), String> {
    let (grid_x, block_x) = geometry;
    if params.len() != expected_abi.len() {
        return Err(format!(
            "{label}: host HIP argument count {} does not match contract {}",
            params.len(),
            expected_abi.len()
        ));
    }
    let mut raw_params = [ptr::null_mut(); MAX_HIP_KERNEL_ARGS];
    for (index, (param, expected)) in params.iter().zip(expected_abi).enumerate() {
        if param.kind != *expected {
            return Err(format!(
                "{label}: host HIP argument {index} is {:?}, contract requires {:?}",
                param.kind, expected
            ));
        }
        raw_params[index] = param.raw;
    }
    api.check(
        unsafe {
            (api.module_launch_kernel)(
                function as *mut c_void,
                grid_x,
                1,
                1,
                block_x,
                1,
                1,
                0,
                stream.raw as *mut c_void,
                raw_params.as_mut_ptr(),
                ptr::null_mut(),
            )
        },
        label,
    )
}

#[derive(Clone, Copy)]
struct HipKernelArg {
    raw: *mut c_void,
    kind: HipArgKind,
}

/// Packs a device pointer as a HIP kernel argument.
fn ptr_arg(value: &mut usize) -> HipKernelArg {
    HipKernelArg {
        raw: (value as *mut usize).cast(),
        kind: HipArgKind::Ptr,
    }
}

/// Packs a 32-bit value as a HIP kernel argument.
fn u32_arg(value: &mut u32) -> HipKernelArg {
    HipKernelArg {
        raw: (value as *mut u32).cast(),
        kind: HipArgKind::U32,
    }
}

/// Packs a 64-bit value as a HIP kernel argument.
fn u64_arg(value: &mut u64) -> HipKernelArg {
    HipKernelArg {
        raw: (value as *mut u64).cast(),
        kind: HipArgKind::U64,
    }
}

/// Splits a flattened T2 batch into its first nonce, first amount and window count.
fn t2_group_windows(base: u32, count: u32) -> Result<(u32, u32, u32), String> {
    if count == 0 || count > HIP_T2_GROUP_CANDIDATES {
        return Err(format!(
            "HIP T2 batch of {count} candidates must hold 1..={HIP_T2_GROUP_CANDIDATES}"
        ));
    }
    let j_base = base % T2_CANDIDATES;
    Ok((
        base / T2_CANDIDATES,
        j_base,
        (j_base + count).div_ceil(T2_CANDIDATES),
    ))
}

/// SHA-256 state after the transaction bytes 0..384, fixed for every nonce and amount.
fn t2_midstate(template: &[u8]) -> [u32; 8] {
    let mut state = SHA256_INITIAL_STATE;
    sha2::block_api::compress256(
        &mut state,
        template[..T2_MIDSTATE_BYTES].as_chunks::<64>().0,
    );
    state
}

/// Message schedule of bytes 512..575, which `supports_t2_window` keeps free
/// of the varying amounts.
fn t2_middle_schedule(template: &[u8]) -> [u32; 64] {
    let mut schedule = [0u32; 64];
    for (word, bytes) in template[512..576].as_chunks::<4>().0.iter().enumerate() {
        schedule[word] = u32::from_be_bytes(*bytes);
    }
    for word in 16..64 {
        let a = schedule[word - 15];
        let b = schedule[word - 2];
        let sigma0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
        let sigma1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
        schedule[word] = schedule[word - 16]
            .wrapping_add(sigma0)
            .wrapping_add(schedule[word - 7])
            .wrapping_add(sigma1);
    }
    schedule
}

/// AMD APU targets (integrated Radeon GPUs) from the LLVM AMDGPU processor list.
pub(crate) fn is_apu_architecture(architecture: &str) -> bool {
    matches!(
        architecture,
        "gfx902"
            | "gfx909"
            | "gfx90c"
            | "gfx1033"
            | "gfx1035"
            | "gfx1036"
            | "gfx1103"
            | "gfx1150"
            | "gfx1151"
            | "gfx1152"
    )
}

/// Whether the selected kernel build is installed for an architecture.
pub(crate) fn code_objects_installed(architecture: &str) -> bool {
    HipKernelSource::from_env().is_ok_and(|source| {
        code_object_candidate_dirs_for_runtime(architecture)
            .iter()
            .any(|directory| directory_has_complete_code_objects(directory, source))
    })
}

/// Loads one kernel per GPU transaction layout from a module.
fn shift_functions(
    module: &HipModule,
    name: impl Fn(usize) -> String,
) -> Result<[usize; PhotonLayout::GPU_SHIFTS.len()], String> {
    let mut functions = [0usize; PhotonLayout::GPU_SHIFTS.len()];
    for (slot, shift) in PhotonLayout::GPU_SHIFTS.iter().enumerate() {
        functions[slot] = module.function(&name(*shift))?;
    }
    Ok(functions)
}

/// Kernel symbol of the Rust non-T2 dual filter for one layout.
fn rust_dual_filter_symbol(shift: usize) -> String {
    if shift == 0 {
        "pickaxe_stage_c_dual_filter_rfc".into()
    } else {
        format!("pickaxe_stage_c_dual_filter_rfc_shift{shift}")
    }
}

impl HipPhotonEngine {
    /// Creates a HipPhotonEngine for the HIP GPU pipeline.
    pub fn new(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
    ) -> Result<Self, String> {
        Self::new_with_source(
            device_ordinal,
            max_candidates,
            winner_cap,
            HipKernelSource::from_env()?,
        )
    }

    /// Creates the engine with an explicit kernel build.
    pub(crate) fn new_with_source(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
        kernel_source: HipKernelSource,
    ) -> Result<Self, String> {
        if max_candidates == 0 || winner_cap == 0 {
            return Err("HIP candidate and winner capacities must be non-zero".into());
        }
        let api = HipApi::load()?;
        api.check(
            unsafe { (api.set_device)(device_ordinal as c_int) },
            "select HIP device",
        )?;
        let architecture = device_architecture(&api, device_ordinal)?;
        let directory = resolve_code_object_dir(&architecture, kernel_source)?;
        // #### PR #22: native HIP stays off integrated GPUs under Windows
        // What: refuse integrated (APU) targets on Windows after the
        // code-object check, before any module load or launch.
        // Why: on a gfx1036 APU the HIP 5.7 runtime in AMD's Windows driver
        // loads v4 and v5 code objects but never completes the first launch,
        // so mining would hang. AMD's Windows HIP runtime targets discrete
        // Radeon GPUs; integrated GPUs mine through the Vulkan engine.
        // Check: discrete Windows HIP is untested on hardware; remove this
        // once an APU benchmark completes.
        if cfg!(windows) && is_apu_architecture(&architecture) {
            return Err(format!(
                "native HIP does not complete a launch on integrated {architecture} GPUs \
                 under Windows; use the default Vulkan engine (--backend wgpu)"
            ));
        }
        for name in kernel_source.code_objects() {
            let path = directory.join(name);
            verify_code_object_architecture(&path, &architecture)?;
        }

        let stream = HipStream::create(&api)?;
        let load = |name: &str| HipModule::load(&api, &directory.join(name));
        let (modules, stage_a_module, stage_b_module, stage_c1_module, t2_module) =
            match kernel_source {
                HipKernelSource::Cpp => {
                    let modules = vec![
                        load("stage_a_rfc6979.hsaco")?,
                        load("photon_stage_b16.hsaco")?,
                        load("photon_c1_schnorr.hsaco")?,
                        load("stage_c_hash.hsaco")?,
                        load("photon_t2_tail.hsaco")?,
                    ];
                    (modules, 0, 1, 2, 4)
                }
                HipKernelSource::Rust => (vec![load(HIP_RUST_CODE_OBJECT)?], 0, 0, 0, 0),
            };
        let stage_a = modules[stage_a_module].function(HIP_STAGE_A_SYMBOL)?;
        let stage_b = [
            modules[stage_b_module].function(HIP_STAGE_B_SYMBOLS[0])?,
            modules[stage_b_module].function(HIP_STAGE_B_SYMBOLS[1])?,
            modules[stage_b_module].function(HIP_STAGE_B_SYMBOLS[2])?,
            modules[stage_b_module].function(HIP_STAGE_B_SYMBOLS[3])?,
        ];
        let stage_c1_dual = modules[stage_c1_module].function(HIP_STAGE_C1_DUAL_SYMBOL)?;
        let t2_prepare = shift_functions(&modules[t2_module], |shift| {
            format!("pickaxe_t2_prepare_shift{shift}")
        })?;
        let t2_filter_group = shift_functions(&modules[t2_module], |shift| {
            format!("pickaxe_t2_filter_group_shift{shift}")
        })?;
        let plain_filter = match kernel_source {
            HipKernelSource::Cpp => PlainFilter::Cpp {
                stage_c1: modules[2].function(HIP_STAGE_C1_SYMBOL)?,
                stage_c3: modules[3].function(HIP_STAGE_C3_SYMBOL)?,
            },
            HipKernelSource::Rust => PlainFilter::Rust {
                dual_filter: shift_functions(&modules[0], rust_dual_filter_symbol)?,
            },
        };

        let (table_bytes, table_source) = m29_table::load_or_generate_m29_g16()?;
        let table_gpu = HipBuffer::allocate(&api, table_bytes.len(), "64 MiB M29 table")?;
        table_gpu.copy_from(&table_bytes, "upload 64 MiB M29 table")?;
        drop(table_bytes);

        Ok(Self {
            target_gpu: HipBuffer::allocate(&api, 32, "target")?,
            private_key_gpu: HipBuffer::allocate(&api, 32, "private key")?,
            public_key_gpu: HipBuffer::allocate(&api, 33, "public key")?,
            fixed_d_gpu: HipBuffer::allocate(&api, FIXED_D_WORDS * 4, "fixed-d table")?,
            message_hashes_gpu: HipBuffer::allocate(
                &api,
                max_candidates as usize * 32,
                "message hashes",
            )?,
            rfc6979_gpu: HipBuffer::allocate(
                &api,
                max_candidates as usize * 32,
                "RFC6979 scalars",
            )?,
            points_gpu: HipBuffer::allocate(
                &api,
                max_candidates as usize * POINT_WORDS * 4,
                "Stage B points",
            )?,
            signatures_gpu: HipBuffer::allocate(
                &api,
                max_candidates as usize * SIGNATURE_BYTES,
                "signatures",
            )?,
            template_gpu: HipBuffer::allocate(&api, MAX_TX_BYTES, "transaction template")?,
            winner_count_gpu: HipBuffer::allocate(&api, 4, "winner count")?,
            winner_nonces_gpu: HipBuffer::allocate(&api, winner_cap as usize * 4, "winner nonces")?,
            winner_hashes_gpu: HipBuffer::allocate(
                &api,
                winner_cap as usize * 32,
                "winner hashes",
            )?,
            negated_s_gpu: HipBuffer::allocate(
                &api,
                (max_candidates as usize).max(T2_MAX_WINDOWS) * 32,
                "negated nonce scalars",
            )?,
            midstate_gpu: HipBuffer::allocate(&api, 8 * 4, "T2 transaction midstate")?,
            middle_schedule_gpu: HipBuffer::allocate(&api, 64 * 4, "T2 middle schedule")?,
            window_txs_gpu: HipBuffer::allocate(
                &api,
                T2_MAX_WINDOWS * MAX_TX_BYTES,
                "T2 window transactions",
            )?,
            // The Rust build also stores each window's round-10 head after the prefixes.
            window_prefixes_gpu: HipBuffer::allocate(
                &api,
                T2_MAX_WINDOWS * 16 * 4,
                "T2 window prefixes",
            )?,
            t2_target_gpu: HipBuffer::allocate(&api, 33, "T2 target and proof rule")?,
            winner_j_gpu: HipBuffer::allocate(&api, winner_cap as usize * 4, "winner amounts")?,
            api,
            stream,
            _modules: modules,
            kernel_source,
            stage_a,
            stage_b,
            plain_filter,
            stage_c1_dual,
            t2_prepare,
            t2_filter_group,
            table_gpu,
            max_candidates,
            winner_cap,
            table_source,
            architecture,
            layout: PhotonLayout::BASE,
            positive_target: false,
            job_ready: false,
            t2_requested: false,
            t2: None,
        })
    }

    #[allow(dead_code)]
    /// Returns the origin of the precomputed GPU lookup table.
    pub fn table_source(&self) -> M29TableSource {
        self.table_source
    }
    #[allow(dead_code)]
    /// Returns the architecture of the selected HIP GPU.
    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    #[allow(dead_code)]
    /// Returns the size of persistent HIP device allocations.
    pub fn persistent_device_bytes(&self) -> usize {
        self.table_gpu.bytes
            + self.target_gpu.bytes
            + self.private_key_gpu.bytes
            + self.public_key_gpu.bytes
            + self.fixed_d_gpu.bytes
            + self.message_hashes_gpu.bytes
            + self.rfc6979_gpu.bytes
            + self.points_gpu.bytes
            + self.signatures_gpu.bytes
            + self.template_gpu.bytes
            + self.winner_count_gpu.bytes
            + self.winner_nonces_gpu.bytes
            + self.winner_hashes_gpu.bytes
            + self.negated_s_gpu.bytes
            + self.midstate_gpu.bytes
            + self.middle_schedule_gpu.bytes
            + self.window_txs_gpu.bytes
            + self.window_prefixes_gpu.bytes
            + self.t2_target_gpu.bytes
            + self.winner_j_gpu.bytes
    }

    /// Searches 65,536 token amounts under each signature (T2), as CUDA does.
    pub fn enable_t2_search(&mut self) -> Result<(), String> {
        if self.job_ready {
            return Err("enable HIP T2 search before the first job".into());
        }
        if (self.max_candidates as usize) < T2_MAX_WINDOWS {
            return Err(format!(
                "HIP T2 needs signature buffers for {T2_MAX_WINDOWS} windows"
            ));
        }
        self.t2_requested = true;
        Ok(())
    }

    /// Candidates per batch while the current job runs in T2 mode.
    pub fn t2_group_batch_candidates(&self) -> Option<u32> {
        self.t2.map(|_| HIP_T2_GROUP_CANDIDATES)
    }

    /// Selects the positive ScriptNum proof rule for the next job.
    pub fn set_positive_target_rule(&mut self, enabled: bool) {
        self.positive_target = enabled;
        self.job_ready = false;
    }

    /// Uploads validated PHOTON job material to HIP device buffers.
    pub fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        self.job_ready = false;
        let layout = PhotonLayout::for_tx_len(template.len())?;
        let target_offset = layout.target_offset();
        if template[target_offset..target_offset + 32] != target[..] {
            return Err("PHOTON HIP target does not match transaction template".into());
        }
        let secret = SecretKey::from_secret_bytes(*private_key)
            .map_err(|error| format!("invalid PHOTON signing key: {error}"))?;
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let fixed_d = fixed_d_table(private_key);
        self.target_gpu.copy_from(target, "upload target")?;
        self.private_key_gpu
            .copy_from(private_key, "upload private key")?;
        self.public_key_gpu
            .copy_from(&public_key, "upload public key")?;
        self.fixed_d_gpu
            .copy_from(&fixed_d, "upload fixed-d table")?;
        self.template_gpu
            .copy_from(template, "upload transaction template")?;
        // The T2 kernels and the Rust dual filter resume from the midstate and
        // read the proof rule from byte 32 of the target.
        self.midstate_gpu
            .copy_from(&t2_midstate(template), "upload transaction midstate")?;
        let mut rule_target = [0u8; 33];
        rule_target[..32].copy_from_slice(target);
        rule_target[32] = u8::from(self.positive_target);
        self.t2_target_gpu
            .copy_from(&rule_target, "upload target and proof rule")?;
        self.t2 = None;
        if self.t2_requested && tx::supports_t2_window(template)? {
            self.t2 = Some(self.set_t2_template(template, layout)?);
        }
        self.layout = layout;
        self.job_ready = true;
        Ok(())
    }

    /// Uploads the per-job T2 middle schedule and returns the token amounts.
    fn set_t2_template(
        &mut self,
        template: &[u8],
        layout: PhotonLayout,
    ) -> Result<HipT2Amounts, String> {
        let shift = layout.shift();
        let baton = u64::from_le_bytes(template[491 + shift..499 + shift].try_into().unwrap());
        let reward = u64::from_le_bytes(template[578 + shift..586 + shift].try_into().unwrap());
        tx::t2_reward_amount(
            u128::from(baton) + u128::from(reward),
            u128::from(reward),
            u16::MAX,
        )?;
        self.middle_schedule_gpu
            .copy_from(&t2_middle_schedule(template), "upload T2 middle schedule")?;
        Ok(HipT2Amounts { baton, reward })
    }

    /// Runs a bounded PHOTON candidate batch on the HIP GPU.
    pub fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if !self.job_ready {
            return Err("PHOTON HIP job is not configured".into());
        }
        if candidate_count == 0 {
            return Ok(PhotonCudaBatchResult {
                candidates: 0,
                total_winners: 0,
                winners: Vec::new(),
            });
        }
        if let Some(amounts) = self.t2 {
            return self.search_t2_batch(nonce_base, candidate_count, amounts);
        }
        if candidate_count > self.max_candidates {
            return Err(format!(
                "PHOTON HIP batch {candidate_count} exceeds persistent capacity {}",
                self.max_candidates
            ));
        }
        self.reset_winner_count()?;
        self.launch_stage_a(nonce_base, candidate_count)?;
        self.search_batch_finish(nonce_base, candidate_count)
    }

    /// Clears the device winner counter before a filter launch.
    fn reset_winner_count(&self) -> Result<(), String> {
        self.api.check(
            unsafe {
                (self.api.memset_async)(
                    self.winner_count_gpu.ptr as *mut c_void,
                    0,
                    4,
                    self.stream.raw as *mut c_void,
                )
            },
            "reset HIP winner count",
        )
    }

    /// Runs the HIP stage A RFC6979 nonce kernel for consecutive nonces.
    fn launch_stage_a(&mut self, nonce_base: u32, candidate_count: u32) -> Result<(), String> {
        let mut nonce_arg = nonce_base;
        let mut target = self.target_gpu.ptr;
        let mut private_key = self.private_key_gpu.ptr;
        let mut message_hashes = self.message_hashes_gpu.ptr;
        let mut rfc6979 = self.rfc6979_gpu.ptr;
        let mut count = candidate_count;
        let mut stage_a_params = [
            u32_arg(&mut nonce_arg),
            ptr_arg(&mut target),
            ptr_arg(&mut private_key),
            ptr_arg(&mut message_hashes),
            ptr_arg(&mut rfc6979),
            u32_arg(&mut count),
        ];
        launch(
            &self.api,
            &self.stream,
            self.stage_a,
            (candidate_count.div_ceil(128), 128),
            &mut stage_a_params,
            &HIP_STAGE_A_ABI,
            "launch PHOTON HIP Stage A",
        )
    }

    // #### PR #22: native HIP T2 window search for AMD GPUs
    // What: one batch signs each 65,536-amount window (Stage A nonces, Stage B
    // points, dual C1 signatures and negated s), prepares the windows, then
    // filters every amount with the CUDA C++ T2 kernels compiled for HIP.
    // Why: native HIP previously signed every candidate, the slow pre-T2 path.
    // The kernels, launch geometry and winner records match CUDA's C++ T2.
    // Check: tail_j winners must rebuild on the CPU; the HIP vector test
    // compares every returned digest with a CPU-signed transaction.
    fn search_t2_batch(
        &mut self,
        base: u32,
        count: u32,
        amounts: HipT2Amounts,
    ) -> Result<PhotonCudaBatchResult, String> {
        let (nonce_base, j_base, window_count) = t2_group_windows(base, count)?;
        self.launch_stage_a(nonce_base, window_count)?;
        self.search_stage_b(window_count)?;
        self.launch_stage_c1_dual(window_count)?;

        let kernel = self.layout.kernel_index();
        let mut template = self.template_gpu.ptr;
        let mut midstate = self.midstate_gpu.ptr;
        let mut signatures = self.signatures_gpu.ptr;
        let mut negated_s = self.negated_s_gpu.ptr;
        let mut points = self.points_gpu.ptr;
        let mut nonce_arg = nonce_base;
        let mut windows = window_count;
        let mut window_txs = self.window_txs_gpu.ptr;
        let mut window_prefixes = self.window_prefixes_gpu.ptr;
        let mut prepare_params = [
            ptr_arg(&mut template),
            ptr_arg(&mut midstate),
            ptr_arg(&mut signatures),
            ptr_arg(&mut negated_s),
            ptr_arg(&mut points),
            u32_arg(&mut nonce_arg),
            u32_arg(&mut windows),
            ptr_arg(&mut window_txs),
            ptr_arg(&mut window_prefixes),
        ];
        launch(
            &self.api,
            &self.stream,
            self.t2_prepare[kernel],
            (window_count.div_ceil(T2_THREADS), T2_THREADS),
            &mut prepare_params,
            &HIP_T2_PREPARE_ABI,
            "launch PHOTON HIP T2 window prepare",
        )?;

        self.reset_winner_count()?;
        let filter_threads = self.kernel_source.t2_filter_threads();
        let mut middle_schedule = self.middle_schedule_gpu.ptr;
        let mut baton = amounts.baton;
        let mut reward = amounts.reward;
        let mut target = self.t2_target_gpu.ptr;
        let mut j_base_arg = j_base;
        let mut count_arg = count;
        let mut winner_cap = self.winner_cap;
        let mut winner_count = self.winner_count_gpu.ptr;
        let mut winner_nonces = self.winner_nonces_gpu.ptr;
        let mut winner_j = self.winner_j_gpu.ptr;
        let mut winner_hashes = self.winner_hashes_gpu.ptr;
        let mut filter_params = [
            ptr_arg(&mut window_txs),
            ptr_arg(&mut window_prefixes),
            ptr_arg(&mut middle_schedule),
            u64_arg(&mut baton),
            u64_arg(&mut reward),
            ptr_arg(&mut target),
            u32_arg(&mut nonce_arg),
            u32_arg(&mut j_base_arg),
            u32_arg(&mut count_arg),
            u32_arg(&mut winner_cap),
            ptr_arg(&mut winner_count),
            ptr_arg(&mut winner_nonces),
            ptr_arg(&mut winner_j),
            ptr_arg(&mut winner_hashes),
        ];
        launch(
            &self.api,
            &self.stream,
            self.t2_filter_group[kernel],
            (count.div_ceil(filter_threads), filter_threads),
            &mut filter_params,
            &HIP_T2_FILTER_GROUP_ABI,
            "launch PHOTON HIP T2 amount filter",
        )?;
        self.stream.synchronize()?;

        let mut total_winners = [0u32; 1];
        self.winner_count_gpu
            .copy_to(&mut total_winners, "read HIP T2 winner count")?;
        let returned = total_winners[0].min(self.winner_cap) as usize;
        let mut nonces = vec![0u32; returned];
        let mut js = vec![0u32; returned];
        let mut hashes = vec![0u8; returned * 32];
        if returned != 0 {
            self.winner_nonces_gpu
                .copy_to(&mut nonces, "read HIP T2 winner nonces")?;
            self.winner_j_gpu
                .copy_to(&mut js, "read HIP T2 winner amounts")?;
            self.winner_hashes_gpu
                .copy_to(&mut hashes, "read HIP T2 winner hashes")?;
        }
        let winners = (0..returned)
            .map(|slot| PhotonCudaWinner {
                nonce: nonces[slot],
                digest: hashes[slot * 32..(slot + 1) * 32].try_into().unwrap(),
                schnorr_k: None,
                tail_j: Some(js[slot] as u16),
                tail_value_sats: None,
            })
            .collect();
        Ok(PhotonCudaBatchResult {
            candidates: count,
            total_winners: total_winners[0],
            winners,
        })
    }

    /// Runs the dual C1 kernel: signatures plus negated s for the T2 square check.
    fn launch_stage_c1_dual(&mut self, window_count: u32) -> Result<(), String> {
        let mut message_hashes = self.message_hashes_gpu.ptr;
        let mut rfc6979 = self.rfc6979_gpu.ptr;
        let mut points = self.points_gpu.ptr;
        let mut public_key = self.public_key_gpu.ptr;
        let mut fixed_d = self.fixed_d_gpu.ptr;
        let mut signatures = self.signatures_gpu.ptr;
        let mut negated_s = self.negated_s_gpu.ptr;
        let mut count = window_count;
        let mut per_thread = 1u32;
        let mut params = [
            ptr_arg(&mut message_hashes),
            ptr_arg(&mut rfc6979),
            ptr_arg(&mut points),
            ptr_arg(&mut public_key),
            ptr_arg(&mut fixed_d),
            ptr_arg(&mut signatures),
            ptr_arg(&mut negated_s),
            u32_arg(&mut count),
            u32_arg(&mut per_thread),
        ];
        launch(
            &self.api,
            &self.stream,
            self.stage_c1_dual,
            (window_count.div_ceil(64), 64),
            &mut params,
            &HIP_STAGE_C1_DUAL_ABI,
            "launch PHOTON HIP T2 Stage C1",
        )
    }

    /// Reads back and verifies a completed HIP search batch.
    fn search_batch_finish(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        self.search_stage_b(candidate_count)?;
        match self.plain_filter {
            PlainFilter::Cpp { stage_c1, stage_c3 } => {
                self.search_stage_c1(stage_c1, candidate_count)?;
                self.search_stage_c23(stage_c3, nonce_base, candidate_count)
            }
            PlainFilter::Rust { dual_filter } => {
                self.launch_stage_c1_dual(candidate_count)?;
                self.search_rust_dual_filter(
                    dual_filter[self.layout.kernel_index()],
                    nonce_base,
                    candidate_count,
                )
            }
        }
    }

    /// Runs the HIP stage B candidate-point kernel.
    fn search_stage_b(&mut self, candidate_count: u32) -> Result<(), String> {
        for (part, function) in self.stage_b.iter().copied().enumerate() {
            let mut rfc6979 = self.rfc6979_gpu.ptr;
            let mut table = self.table_gpu.ptr;
            let mut points = self.points_gpu.ptr;
            let mut count = candidate_count;
            let mut params = [
                ptr_arg(&mut rfc6979),
                ptr_arg(&mut table),
                ptr_arg(&mut points),
                u32_arg(&mut count),
            ];
            launch(
                &self.api,
                &self.stream,
                function,
                (candidate_count.div_ceil(64), 64),
                &mut params,
                &HIP_STAGE_B_ABI,
                &format!("launch PHOTON HIP Stage B part {part}"),
            )?;
        }
        Ok(())
    }

    /// Runs the HIP stage C1 Schnorr candidate filter.
    fn search_stage_c1(&mut self, function: usize, candidate_count: u32) -> Result<(), String> {
        let mut message_hashes = self.message_hashes_gpu.ptr;
        let mut rfc6979 = self.rfc6979_gpu.ptr;
        let mut points = self.points_gpu.ptr;
        let mut public_key = self.public_key_gpu.ptr;
        let mut fixed_d = self.fixed_d_gpu.ptr;
        let mut signatures = self.signatures_gpu.ptr;
        let mut count = candidate_count;
        let mut params = [
            ptr_arg(&mut message_hashes),
            ptr_arg(&mut rfc6979),
            ptr_arg(&mut points),
            ptr_arg(&mut public_key),
            ptr_arg(&mut fixed_d),
            ptr_arg(&mut signatures),
            u32_arg(&mut count),
        ];
        launch(
            &self.api,
            &self.stream,
            function,
            (candidate_count.div_ceil(64), 64),
            &mut params,
            &HIP_STAGE_C1_ABI,
            "launch PHOTON HIP Stage C1",
        )
    }

    /// Runs the remaining HIP stage C hash and target filters.
    fn search_stage_c23(
        &mut self,
        function: usize,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        let mut template = self.template_gpu.ptr;
        let mut signatures = self.signatures_gpu.ptr;
        let mut nonce_arg = nonce_base;
        let mut target = self.target_gpu.ptr;
        let mut count = candidate_count;
        let mut winner_cap = self.winner_cap;
        let mut winner_count = self.winner_count_gpu.ptr;
        let mut winner_nonces = self.winner_nonces_gpu.ptr;
        let mut winner_hashes = self.winner_hashes_gpu.ptr;
        let mut shift = u32::try_from(self.layout.shift()).expect("layout shift fits u32");
        let mut positive_rule = u32::from(self.positive_target);
        let mut params = [
            ptr_arg(&mut template),
            ptr_arg(&mut signatures),
            u32_arg(&mut nonce_arg),
            ptr_arg(&mut target),
            u32_arg(&mut count),
            u32_arg(&mut winner_cap),
            ptr_arg(&mut winner_count),
            ptr_arg(&mut winner_nonces),
            ptr_arg(&mut winner_hashes),
            u32_arg(&mut shift),
            u32_arg(&mut positive_rule),
        ];
        launch(
            &self.api,
            &self.stream,
            function,
            (candidate_count.div_ceil(128), 128),
            &mut params,
            &HIP_STAGE_C3_ABI,
            "launch PHOTON HIP Stage C2/C3",
        )?;
        self.read_plain_winners(candidate_count)
    }

    /// Runs the Rust non-T2 dual filter: C1 wrote both s values for each nonce.
    fn search_rust_dual_filter(
        &mut self,
        function: usize,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        self.reset_winner_count()?;
        let mut template = self.template_gpu.ptr;
        let mut midstate = self.midstate_gpu.ptr;
        let mut signatures = self.signatures_gpu.ptr;
        let mut negated_s = self.negated_s_gpu.ptr;
        let mut points = self.points_gpu.ptr;
        let mut nonce_arg = nonce_base;
        let mut target = self.t2_target_gpu.ptr;
        let mut count = candidate_count;
        let mut winner_cap = self.winner_cap;
        let mut winner_count = self.winner_count_gpu.ptr;
        let mut winner_nonces = self.winner_nonces_gpu.ptr;
        let mut winner_hashes = self.winner_hashes_gpu.ptr;
        let mut params = [
            ptr_arg(&mut template),
            ptr_arg(&mut midstate),
            ptr_arg(&mut signatures),
            ptr_arg(&mut negated_s),
            ptr_arg(&mut points),
            u32_arg(&mut nonce_arg),
            ptr_arg(&mut target),
            u32_arg(&mut count),
            u32_arg(&mut winner_cap),
            ptr_arg(&mut winner_count),
            ptr_arg(&mut winner_nonces),
            ptr_arg(&mut winner_hashes),
        ];
        launch(
            &self.api,
            &self.stream,
            function,
            (candidate_count.div_ceil(128), 128),
            &mut params,
            &HIP_RUST_DUAL_FILTER_ABI,
            "launch PHOTON HIP Rust dual filter",
        )?;
        self.read_plain_winners(candidate_count)
    }

    /// Waits for a non-T2 filter and reads its winning nonces and hashes.
    fn read_plain_winners(
        &mut self,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        self.stream.synchronize()?;

        let mut total_winners = [0u32; 1];
        self.winner_count_gpu
            .copy_to(&mut total_winners, "read HIP winner count")?;
        let returned = total_winners[0].min(self.winner_cap) as usize;
        let mut nonces = vec![0u32; returned];
        let mut hashes = vec![0u8; returned * 32];
        if returned != 0 {
            self.winner_nonces_gpu
                .copy_to(&mut nonces, "read HIP winner nonces")?;
            self.winner_hashes_gpu
                .copy_to(&mut hashes, "read HIP winner hashes")?;
        }
        let winners = nonces
            .into_iter()
            .enumerate()
            .map(|(index, nonce)| {
                let mut digest = [0u8; 32];
                digest.copy_from_slice(&hashes[index * 32..(index + 1) * 32]);
                PhotonCudaWinner {
                    nonce,
                    digest,
                    schnorr_k: None,
                    tail_j: None,
                    tail_value_sats: None,
                }
            })
            .collect();

        Ok(PhotonCudaBatchResult {
            candidates: candidate_count,
            total_winners: total_winners[0],
            winners,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Converts HIP argument kinds to the kernel contract's ABI names.
    fn abi_kinds(abi: &[HipArgKind]) -> Vec<&'static str> {
        abi.iter()
            .map(|kind| match kind {
                HipArgKind::Ptr => "ptr",
                HipArgKind::U32 => "u32",
                HipArgKind::U64 => "u64",
            })
            .collect()
    }

    #[test]
    /// Checks that rust launch contract matches hip artifact contract.
    fn rust_launch_contract_matches_hip_artifact_contract() {
        assert_eq!(
            std::mem::size_of::<usize>(),
            8,
            "HIP HSACO contract is 64-bit"
        );
        let contract: serde_json::Value =
            serde_json::from_str(include_str!("../hip/kernel_contract.json"))
                .expect("parse HIP kernel contract");
        assert_eq!(contract["architecture"], "gfx1036");
        let objects = contract["code_objects"]
            .as_array()
            .expect("HIP contract code_objects array");
        let files = objects
            .iter()
            .map(|object| object["file"].as_str().expect("HIP contract filename"))
            .collect::<Vec<_>>();
        assert_eq!(files, HIP_CODE_OBJECT_NAMES);

        let mut expected = vec![
            (HIP_STAGE_A_SYMBOL.to_string(), abi_kinds(&HIP_STAGE_A_ABI)),
            (
                HIP_STAGE_B_SYMBOLS[0].to_string(),
                abi_kinds(&HIP_STAGE_B_ABI),
            ),
            (
                HIP_STAGE_B_SYMBOLS[1].to_string(),
                abi_kinds(&HIP_STAGE_B_ABI),
            ),
            (
                HIP_STAGE_B_SYMBOLS[2].to_string(),
                abi_kinds(&HIP_STAGE_B_ABI),
            ),
            (
                HIP_STAGE_B_SYMBOLS[3].to_string(),
                abi_kinds(&HIP_STAGE_B_ABI),
            ),
            (
                HIP_STAGE_C1_SYMBOL.to_string(),
                abi_kinds(&HIP_STAGE_C1_ABI),
            ),
            (
                HIP_STAGE_C1_DUAL_SYMBOL.to_string(),
                abi_kinds(&HIP_STAGE_C1_DUAL_ABI),
            ),
            (
                HIP_STAGE_C3_SYMBOL.to_string(),
                abi_kinds(&HIP_STAGE_C3_ABI),
            ),
        ];
        for shift in PhotonLayout::GPU_SHIFTS {
            expected.push((
                format!("pickaxe_t2_prepare_shift{shift}"),
                abi_kinds(&HIP_T2_PREPARE_ABI),
            ));
        }
        for shift in PhotonLayout::GPU_SHIFTS {
            expected.push((
                format!("pickaxe_t2_filter_group_shift{shift}"),
                abi_kinds(&HIP_T2_FILTER_GROUP_ABI),
            ));
        }
        let actual = objects
            .iter()
            .flat_map(|object| {
                object["kernels"]
                    .as_array()
                    .expect("HIP contract kernels array")
            })
            .map(|kernel| {
                let symbol = kernel["symbol"].as_str().expect("HIP kernel symbol");
                let kinds = kernel["args"]
                    .as_array()
                    .expect("HIP kernel args")
                    .iter()
                    .map(|arg| arg["kind"].as_str().expect("HIP arg kind"))
                    .collect::<Vec<_>>();
                (symbol.to_string(), kinds)
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn rust_launch_contract_matches_rust_hip_artifact_contract() {
        let contract: serde_json::Value =
            serde_json::from_str(include_str!("../hip/rust_kernel_contract.json"))
                .expect("parse Rust HIP kernel contract");
        let objects = contract["code_objects"].as_array().unwrap();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0]["file"], HIP_RUST_CODE_OBJECT);
        let mut expected = vec![(HIP_STAGE_A_SYMBOL.to_string(), abi_kinds(&HIP_STAGE_A_ABI))];
        for symbol in HIP_STAGE_B_SYMBOLS {
            expected.push((symbol.to_string(), abi_kinds(&HIP_STAGE_B_ABI)));
        }
        expected.push((
            HIP_STAGE_C1_DUAL_SYMBOL.to_string(),
            abi_kinds(&HIP_STAGE_C1_DUAL_ABI),
        ));
        for shift in PhotonLayout::GPU_SHIFTS {
            expected.push((
                format!("pickaxe_t2_prepare_shift{shift}"),
                abi_kinds(&HIP_T2_PREPARE_ABI),
            ));
        }
        for shift in PhotonLayout::GPU_SHIFTS {
            expected.push((
                format!("pickaxe_t2_filter_group_shift{shift}"),
                abi_kinds(&HIP_T2_FILTER_GROUP_ABI),
            ));
        }
        for shift in PhotonLayout::GPU_SHIFTS {
            expected.push((
                rust_dual_filter_symbol(shift),
                abi_kinds(&HIP_RUST_DUAL_FILTER_ABI),
            ));
        }
        let actual = objects[0]["kernels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kernel| {
                let kinds = kernel["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|arg| arg["kind"].as_str().unwrap())
                    .collect::<Vec<_>>();
                (kernel["symbol"].as_str().unwrap().to_string(), kinds)
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn hip_kernel_source_defaults_to_cpp_until_rust_is_requested() {
        assert_eq!(HipKernelSource::parse(None).unwrap(), HipKernelSource::Cpp);
        assert_eq!(
            HipKernelSource::parse(Some("")).unwrap(),
            HipKernelSource::Cpp
        );
        assert_eq!(
            HipKernelSource::parse(Some("cpp")).unwrap(),
            HipKernelSource::Cpp
        );
        assert_eq!(
            HipKernelSource::parse(Some(" rust ")).unwrap(),
            HipKernelSource::Rust
        );
        assert!(HipKernelSource::parse(Some("cuda")).is_err());
        assert_eq!(HipKernelSource::Rust.code_objects(), [HIP_RUST_CODE_OBJECT]);
        assert_eq!(HipKernelSource::Cpp.code_objects(), HIP_CODE_OBJECT_NAMES);
    }

    #[test]
    fn t2_group_windows_split_flattened_candidates() {
        assert_eq!(t2_group_windows(0, 16).unwrap(), (0, 0, 1));
        // A batch starting 16 amounts before a window edge needs two signatures.
        assert_eq!(t2_group_windows(65_520, 32).unwrap(), (0, 65_520, 2));
        assert_eq!(t2_group_windows(65_536, 16).unwrap(), (1, 0, 1));
        assert_eq!(
            t2_group_windows(3 * 65_536 + 1, HIP_T2_GROUP_CANDIDATES).unwrap(),
            (3, 1, T2_MAX_WINDOWS as u32)
        );
        assert!(t2_group_windows(0, 0).is_err());
        assert!(t2_group_windows(0, HIP_T2_GROUP_CANDIDATES + 1).is_err());
    }

    /// One SHA-256 compression driven by a precomputed 64-word schedule.
    fn compress_with_schedule(state: &mut [u32; 8], schedule: &[u32; 64]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for round in 0..64 {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[round])
                .wrapping_add(schedule[round]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (word, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(value);
        }
    }

    #[test]
    fn t2_midstate_and_middle_schedule_rebuild_the_confirmed_parent_hash() {
        // The confirmed Chipnet v3.2 parent: the GPU resumes from the 384-byte
        // midstate and drives block 8 (bytes 512..575) from the host schedule.
        let template = hex::decode(
            include_str!("../reference/photon_v32_chipnet_confirmed_parent.hex").trim(),
        )
        .unwrap();
        let mut padded = template.clone();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&((template.len() as u64) * 8).to_be_bytes());
        assert_eq!(padded.len(), 640);

        let mut state = t2_midstate(&template);
        for (index, block) in padded[T2_MIDSTATE_BYTES..]
            .as_chunks::<64>()
            .0
            .iter()
            .enumerate()
        {
            if T2_MIDSTATE_BYTES + index * 64 == 512 {
                compress_with_schedule(&mut state, &t2_middle_schedule(&template));
            } else {
                sha2::block_api::compress256(&mut state, std::slice::from_ref(block));
            }
        }
        let first: Vec<u8> = state.iter().flat_map(|word| word.to_be_bytes()).collect();
        let digest: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&first).into();
        assert_eq!(digest, crate::proof::hash256(&template));
    }

    #[test]
    fn integrated_amd_targets_are_recognized() {
        assert!(is_apu_architecture("gfx1036"));
        assert!(is_apu_architecture("gfx1103"));
        assert!(!is_apu_architecture("gfx1030"));
        assert!(!is_apu_architecture("gfx1100"));
        assert!(!is_apu_architecture("gfx1201"));
    }

    /// A fully CPU-signed PHOTON claim for one nonce and reward amount.
    fn signed_t2_template(target: [u8; 32], reward: u128, nonce: u32) -> Vec<u8> {
        let key = [0x11; 32];
        let public = crate::crypto::compressed_pubkey(&key).unwrap();
        let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
        let signature = crate::crypto::bch_schnorr_sign(&key, &message).unwrap();
        tx::template_for_shift(0, |age| tx::TemplateParams {
            prev_tx_hash_hex: "aa".repeat(32),
            prev_index: 0,
            age,
            public_key_hex: hex::encode(public),
            target_hex: hex::encode(target),
            signature_hex: hex::encode(signature),
            nonce,
            contract_value_sats: 15_971_500,
            relay_fee_sats_per_kb: 1_000,
            contract_token_amount: 2_099_905_002_035_715,
            reward_amount: reward,
            payout_locking: tx::cashaddr_to_p2pkh_locking(
                "zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3",
            )
            .unwrap(),
        })
    }

    #[test]
    fn t2_group_winners_match_cpu_signed_windows_if_hip_present() {
        for source in [HipKernelSource::Cpp, HipKernelSource::Rust] {
            t2_group_winners_match_cpu_signed_windows(source);
        }
    }

    fn t2_group_winners_match_cpu_signed_windows(source: HipKernelSource) {
        const REWARD: u128 = 4_999_773_813;
        let target = [0xff; 32];
        let key = [0x11; 32];
        let mut engine = match HipPhotonEngine::new_with_source(0, 65_536, 8, source) {
            Ok(engine) => engine,
            Err(error) => {
                eprintln!("skip HIP T2 vector ({source:?}): {error}");
                return;
            }
        };
        engine.enable_t2_search().unwrap();
        engine
            .set_job(&signed_t2_template(target, REWARD, 0), &target, &key)
            .unwrap();
        assert_eq!(
            engine.t2_group_batch_candidates(),
            Some(HIP_T2_GROUP_CANDIDATES)
        );
        // Within one window, across a window edge, and in a later window.
        for (base, count) in [(65_520, 16), (65_536, 16), (131_056, 16)] {
            let result = engine.search_batch(base, count).unwrap();
            assert_eq!(result.candidates, count);
            assert_eq!(result.total_winners, count);
            for winner in result.winners {
                let j = winner.tail_j.unwrap();
                assert_eq!(winner.nonce, base >> 16);
                let raw = signed_t2_template(target, REWARD - u128::from(j), winner.nonce);
                assert_eq!(winner.digest, crate::proof::hash256(&raw));
            }
        }
    }

    #[test]
    /// Checks that gfx architecture is extracted without binding to property layout.
    fn gfx_architecture_is_extracted_without_binding_to_property_layout() {
        let mut properties = [0u8; 256];
        properties[37..45].copy_from_slice(b"gfx1036\0");
        assert_eq!(parse_gfx_arch(&properties).as_deref(), Some("gfx1036"));
        assert_eq!(
            parse_gfx_arch(b"device\0gfx1036:sramecc-:xnack+\0").as_deref(),
            Some("gfx1036")
        );
    }

    #[test]
    /// Checks that code object search prefers portable release layout.
    fn code_object_search_prefers_portable_release_layout() {
        let executable_dir = Path::new("release-root");
        let manifest_dir = Path::new("source-root");
        let candidates =
            code_object_candidate_dirs("gfx1036", None, Some(executable_dir), manifest_dir);
        assert_eq!(
            candidates,
            vec![
                executable_dir.join("hip").join("build").join("gfx1036"),
                executable_dir.join("hip").join("gfx1036"),
                manifest_dir.join("hip").join("build").join("gfx1036"),
            ]
        );
    }

    #[test]
    /// Checks that code object override disables implicit search paths.
    fn code_object_override_disables_implicit_search_paths() {
        let override_dir = PathBuf::from("custom-hip-artifacts");
        let candidates = code_object_candidate_dirs(
            "gfx1036",
            Some(override_dir.clone()),
            Some(Path::new("release-root")),
            Path::new("source-root"),
        );
        assert_eq!(candidates, vec![override_dir]);
    }

    #[test]
    /// Checks that code object set requires every production stage.
    fn code_object_set_requires_every_production_stage() {
        let directory =
            std::env::temp_dir().join(format!("pickaxe-hip-complete-set-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("create HIP complete-set test directory");
        for name in HIP_CODE_OBJECT_NAMES {
            fs::write(directory.join(name), b"probe").expect("write HIP code-object probe");
        }
        assert!(directory_has_complete_code_objects(
            &directory,
            HipKernelSource::Cpp
        ));

        fs::remove_file(directory.join(HIP_CODE_OBJECT_NAMES[2]))
            .expect("remove one HIP code-object probe");
        assert!(!directory_has_complete_code_objects(
            &directory,
            HipKernelSource::Cpp
        ));

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    /// Checks that hip code object must match detected architecture.
    fn hip_code_object_must_match_detected_architecture() {
        let directory =
            std::env::temp_dir().join(format!("pickaxe-hip-arch-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("create HIP architecture test directory");
        let path = directory.join("probe.hsaco");

        fs::write(
            &path,
            b"\x7fELF\0amdgcn-amd-amdhsa--gfx1036:sramecc-:xnack+\0",
        )
        .expect("write matching HIP code object probe");
        verify_code_object_architecture(&path, "gfx1036")
            .expect("matching HIP architecture must be accepted");

        let error = verify_code_object_architecture(&path, "gfx1100")
            .expect_err("mismatched HIP architecture must fail closed");
        assert!(error.contains("gfx1100"));
        assert!(error.contains("gfx1036"));

        fs::write(&path, b"\x7fELF\0amdgcn-amd-amdhsa--gfx10360\0")
            .expect("write prefix-collision HIP code object probe");
        verify_code_object_architecture(&path, "gfx1036")
            .expect_err("longer HIP architecture must not match by prefix");

        fs::write(&path, b"\x7fELF\0no-amdgpu-target-metadata\0")
            .expect("write metadata-free HIP code object probe");
        let error = verify_code_object_architecture(&path, "gfx1036")
            .expect_err("missing HIP architecture metadata must fail closed");
        assert!(error.contains("no gfx target metadata found"));

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&directory);
    }

    #[test]
    /// Checks that native hip missing code objects fail closed if runtime is present.
    fn native_hip_missing_code_objects_fail_closed_if_runtime_is_present() {
        let api = match HipApi::load() {
            Ok(api) => api,
            Err(error) => {
                eprintln!("skip HIP fail-closed probe: {error}");
                return;
            }
        };
        let architecture = match device_architecture(&api, 0) {
            Ok(architecture) => architecture,
            Err(error) => {
                eprintln!("skip HIP fail-closed probe: {error}");
                return;
            }
        };
        let candidates = code_object_candidate_dirs_for_runtime(&architecture);
        if candidates
            .iter()
            .any(|directory| directory_has_complete_code_objects(directory, HipKernelSource::Cpp))
        {
            eprintln!(
                "skip missing-artifact assertion: PHOTON HIP code objects already exist for {architecture}"
            );
            return;
        }

        let error = match HipPhotonEngine::new(0, 1, 1) {
            Ok(_) => panic!("HIP engine unexpectedly started without all required code objects"),
            Err(error) => error,
        };
        assert!(error.contains("missing complete PHOTON HIP code-object set"));
        assert!(error.contains(&architecture));
    }
}
