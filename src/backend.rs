//! Native production GPU backend selection: auto | cuda | hip.
//! No CPU mining fallback.

use cudarc::driver::{sys, CudaContext};
use libloading::Library;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Auto,
    Cuda,
    Hip,
}

impl BackendKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cuda" => Ok(Self::Cuda),
            "hip" | "rocm" => Ok(Self::Hip),
            other => Err(format!("unknown backend `{other}` (auto|cuda|hip)")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cuda => "cuda",
            Self::Hip => "hip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub vram_bytes: Option<u64>,
    pub backend: BackendKind,
    pub detail: String,
}

pub fn list_devices(prefer: BackendKind) -> Result<Vec<GpuDevice>, String> {
    match prefer {
        BackendKind::Cuda => list_cuda_devices(),
        BackendKind::Hip => list_hip_devices(),
        BackendKind::Auto => {
            let cuda = list_cuda_devices();
            let hip = list_hip_devices();
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

            if devices.is_empty() {
                Err(format!(
                    "no validated native GPU device found ({})",
                    errors.join("; ")
                ))
            } else {
                Ok(devices)
            }
        }
    }
}

pub fn resolve_mining_device(
    prefer: BackendKind,
    requested_index: Option<u32>,
) -> Result<GpuDevice, String> {
    let wanted = requested_index.unwrap_or(0);
    match prefer {
        BackendKind::Cuda => find_device(list_cuda_devices()?, wanted, BackendKind::Cuda),
        BackendKind::Hip => find_device(list_hip_devices()?, wanted, BackendKind::Hip),
        BackendKind::Auto => {
            if let Ok(cuda) = list_cuda_devices() {
                if let Some(device) = cuda.into_iter().find(|device| device.index == wanted) {
                    return Ok(device);
                }
            }
            if let Ok(hip) = list_hip_devices() {
                if let Some(device) = hip.into_iter().find(|device| device.index == wanted) {
                    return Ok(device);
                }
            }
            Err(format!(
                "no native GPU device at backend-local ordinal {wanted}; run `pickaxe devices`"
            ))
        }
    }
}

fn find_device(
    devices: Vec<GpuDevice>,
    wanted: u32,
    backend: BackendKind,
) -> Result<GpuDevice, String> {
    devices
        .into_iter()
        .find(|device| device.index == wanted)
        .ok_or_else(|| {
            format!(
                "{} device {wanted} not found; run `pickaxe devices --backend {}`",
                backend.as_str().to_ascii_uppercase(),
                backend.as_str()
            )
        })
}

fn list_cuda_devices() -> Result<Vec<GpuDevice>, String> {
    // Driver must be initialized before get_count (same path CudaContext::new uses).
    cudarc::driver::result::init()
        .map_err(|e| format!("CUDA unavailable: {e}. Install or fix the CUDA runtime."))?;
    let n = cudarc::driver::result::device::get_count()
        .map_err(|e| format!("CUDA unavailable: {e}. Install or fix the CUDA runtime."))?;
    let mut out = Vec::new();
    for i in 0..n {
        let ctx = CudaContext::new(i as usize)
            .map_err(|e| format!("CUDA device {i} init failed: {e}"))?;
        let (name, detail, vram) = cuda_device_info(i as u32, &ctx);
        out.push(GpuDevice {
            index: i as u32,
            name,
            vendor: "NVIDIA".into(),
            vram_bytes: vram,
            backend: BackendKind::Cuda,
            detail,
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

#[cfg(target_os = "windows")]
const HIP_LIBRARY_CANDIDATES: &[&str] = &["amdhip64.dll", "amdhip64_6.dll"];

#[cfg(not(target_os = "windows"))]
const HIP_LIBRARY_CANDIDATES: &[&str] = &["libamdhip64.so", "libamdhip64.so.6", "libamdhip64.so.5"];

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

            let mut name_buffer = [0 as c_char; 256];
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
            out.push(GpuDevice {
                index: ordinal as u32,
                name: if name.is_empty() {
                    format!("AMD GPU {ordinal}")
                } else {
                    name
                },
                vendor: "AMD".into(),
                vram_bytes: vram,
                backend: BackendKind::Hip,
                detail,
            });
        }
        Ok(out)
    }
}

fn cuda_device_info(index: u32, ctx: &CudaContext) -> (String, String, Option<u64>) {
    // Best-effort via driver sys; fall back to ordinal if attrs fail.
    let mut name_buf = [0i8; 256];
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
    (name, detail, vram)
}

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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_backend_parser_rejects_detached_wgpu() {
        assert_eq!(BackendKind::parse("auto").unwrap(), BackendKind::Auto);
        assert_eq!(BackendKind::parse("cuda").unwrap(), BackendKind::Cuda);
        assert_eq!(BackendKind::parse("hip").unwrap(), BackendKind::Hip);
        assert_eq!(BackendKind::parse("rocm").unwrap(), BackendKind::Hip);

        let error = BackendKind::parse("wgpu").unwrap_err();
        assert_eq!(error, "unknown backend `wgpu` (auto|cuda|hip)");
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
        }];
        let selected = find_device(devices, 2, BackendKind::Hip).unwrap();
        assert_eq!(selected.backend, BackendKind::Hip);
        assert_eq!(selected.index, 2);
    }
}
