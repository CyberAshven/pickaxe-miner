//! Persistent native HIP PHOTON candidate engine.

use crate::cuda_photon::{PhotonCudaBatchResult, PhotonCudaWinner};
use crate::m29_table::{self, M29TableSource};
use libloading::Library;
use num_bigint::BigUint;
use secp256k1::{PublicKey, SecretKey};
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;

const TX_BYTES: usize = 615;
const TARGET_OFFSET: usize = 394;
const SIGNATURE_BYTES: usize = 64;
const POINT_WORDS: usize = 24;
const FIXED_D_WORDS: usize = 32 * 256 * 8;
const HIP_SUCCESS: c_int = 0;
const HIP_MEMCPY_HOST_TO_DEVICE: c_int = 1;
const HIP_MEMCPY_DEVICE_TO_HOST: c_int = 2;

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

    unsafe fn from_library(library: Library) -> Result<Self, String> {
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

    fn synchronize(&self) -> Result<(), String> {
        self.api.check(
            unsafe { (self.api.stream_synchronize)(self.raw as *mut c_void) },
            "synchronize HIP stream",
        )
    }
}

impl Drop for HipStream {
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
    _stage_a_module: HipModule,
    _stage_b_module: HipModule,
    _stage_c1_module: HipModule,
    _stage_c3_module: HipModule,
    stage_a: usize,
    stage_b: [usize; 4],
    stage_c1: usize,
    stage_c3: usize,
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
    max_candidates: u32,
    winner_cap: u32,
    #[allow(dead_code)]
    table_source: M29TableSource,
    #[allow(dead_code)]
    architecture: String,
    job_ready: bool,
}

#[repr(C, align(64))]
struct HipDevicePropertiesStorage([u8; 8192]);

fn parse_gfx_arch(properties: &[u8]) -> Option<String> {
    let start = properties.windows(3).position(|window| window == b"gfx")?;
    let tail = &properties[start..];
    let len = tail
        .iter()
        .position(|byte| !byte.is_ascii_alphanumeric())
        .unwrap_or(tail.len());
    (len > 3).then(|| String::from_utf8_lossy(&tail[..len]).into_owned())
}

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

pub fn detected_architecture(device_ordinal: usize) -> Result<String, String> {
    let api = HipApi::load()?;
    device_architecture(&api, device_ordinal)
}

fn code_object_dir(architecture: &str) -> PathBuf {
    if let Ok(path) = std::env::var("PICKAXE_HIP_CODE_OBJECT_DIR") {
        PathBuf::from(path)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("hip")
            .join("build")
            .join(architecture)
    }
}

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

fn launch(
    api: &HipApi,
    stream: &HipStream,
    function: usize,
    grid_x: u32,
    block_x: u32,
    params: &mut [*mut c_void],
    label: &str,
) -> Result<(), String> {
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
                params.as_mut_ptr(),
                ptr::null_mut(),
            )
        },
        label,
    )
}

fn ptr_arg(value: &mut usize) -> *mut c_void {
    (value as *mut usize).cast()
}

fn u32_arg(value: &mut u32) -> *mut c_void {
    (value as *mut u32).cast()
}

impl HipPhotonEngine {
    pub fn new(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
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
        let directory = code_object_dir(&architecture);
        for name in [
            "stage_a_rfc6979.hsaco",
            "photon_stage_b16.hsaco",
            "photon_c1_schnorr.hsaco",
            "stage_c_hash.hsaco",
        ] {
            let path = directory.join(name);
            if !path.is_file() {
                return Err(format!(
                    "missing PHOTON HIP code object {} for {architecture}; run `python tools/build_hip.py --arch {architecture}` with a matching ROCm/HIP compiler",
                    path.display()
                ));
            }
            verify_code_object_architecture(&path, &architecture)?;
        }

        let stream = HipStream::create(&api)?;
        let stage_a_module = HipModule::load(&api, &directory.join("stage_a_rfc6979.hsaco"))?;
        let stage_b_module = HipModule::load(&api, &directory.join("photon_stage_b16.hsaco"))?;
        let stage_c1_module = HipModule::load(&api, &directory.join("photon_c1_schnorr.hsaco"))?;
        let stage_c3_module = HipModule::load(&api, &directory.join("stage_c_hash.hsaco"))?;
        let stage_a = stage_a_module.function("pickaxe_stage_a_rfc6979")?;
        let stage_b = [
            stage_b_module.function("pickaxe_photon_b16_part0")?,
            stage_b_module.function("pickaxe_photon_b16_part1")?,
            stage_b_module.function("pickaxe_photon_b16_part2")?,
            stage_b_module.function("pickaxe_photon_b16_part3")?,
        ];
        let stage_c1 = stage_c1_module.function("pickaxe_photon_c1_schnorr")?;
        let stage_c3 = stage_c3_module.function("pickaxe_stage_c_hash_filter")?;

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
            template_gpu: HipBuffer::allocate(&api, TX_BYTES, "transaction template")?,
            winner_count_gpu: HipBuffer::allocate(&api, 4, "winner count")?,
            winner_nonces_gpu: HipBuffer::allocate(&api, winner_cap as usize * 4, "winner nonces")?,
            winner_hashes_gpu: HipBuffer::allocate(
                &api,
                winner_cap as usize * 32,
                "winner hashes",
            )?,
            api,
            stream,
            _stage_a_module: stage_a_module,
            _stage_b_module: stage_b_module,
            _stage_c1_module: stage_c1_module,
            _stage_c3_module: stage_c3_module,
            stage_a,
            stage_b,
            stage_c1,
            stage_c3,
            table_gpu,
            max_candidates,
            winner_cap,
            table_source,
            architecture,
            job_ready: false,
        })
    }

    #[allow(dead_code)]
    pub fn table_source(&self) -> M29TableSource {
        self.table_source
    }
    #[allow(dead_code)]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    #[allow(dead_code)]
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
    }

    pub fn set_job(
        &mut self,
        template: &[u8; TX_BYTES],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        if template[TARGET_OFFSET..TARGET_OFFSET + 32] != target[..] {
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
        self.job_ready = true;
        Ok(())
    }

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
        if candidate_count > self.max_candidates {
            return Err(format!(
                "PHOTON HIP batch {candidate_count} exceeds persistent capacity {}",
                self.max_candidates
            ));
        }
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
        )?;

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
            candidate_count.div_ceil(128),
            128,
            &mut stage_a_params,
            "launch PHOTON HIP Stage A",
        )?;

        self.search_batch_finish(nonce_base, candidate_count)
    }

    fn search_batch_finish(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        self.search_stage_b(candidate_count)?;
        self.search_stage_c1(candidate_count)?;
        self.search_stage_c23(nonce_base, candidate_count)
    }

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
                candidate_count.div_ceil(64),
                64,
                &mut params,
                &format!("launch PHOTON HIP Stage B part {part}"),
            )?;
        }
        Ok(())
    }

    fn search_stage_c1(&mut self, candidate_count: u32) -> Result<(), String> {
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
            self.stage_c1,
            candidate_count.div_ceil(64),
            64,
            &mut params,
            "launch PHOTON HIP Stage C1",
        )
    }

    fn search_stage_c23(
        &mut self,
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
        ];
        launch(
            &self.api,
            &self.stream,
            self.stage_c3,
            candidate_count.div_ceil(128),
            128,
            &mut params,
            "launch PHOTON HIP Stage C2/C3",
        )?;
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
                PhotonCudaWinner { nonce, digest }
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

    #[test]
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
    fn default_code_object_path_is_architecture_scoped() {
        let expected = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("hip")
            .join("build")
            .join("gfx1036");
        if std::env::var_os("PICKAXE_HIP_CODE_OBJECT_DIR").is_none() {
            assert_eq!(code_object_dir("gfx1036"), expected);
        }
    }

    #[test]
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
        let directory = code_object_dir(&architecture);
        let all_present = [
            "stage_a_rfc6979.hsaco",
            "photon_stage_b16.hsaco",
            "photon_c1_schnorr.hsaco",
            "stage_c_hash.hsaco",
        ]
        .iter()
        .all(|name| directory.join(name).is_file());
        if all_present {
            eprintln!(
                "skip missing-artifact assertion: PHOTON HIP code objects already exist for {architecture}"
            );
            return;
        }

        let error = match HipPhotonEngine::new(0, 1, 1) {
            Ok(_) => panic!("HIP engine unexpectedly started without all required code objects"),
            Err(error) => error,
        };
        assert!(error.contains("missing PHOTON HIP code object"));
        assert!(error.contains(&architecture));
    }
}
