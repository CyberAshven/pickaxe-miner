//! GPU backend selection and discovery: auto | cuda | hip | wgpu.
//! No CPU mining fallback.

use cudarc::driver::{sys, CudaContext};
use libloading::Library;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int};

pub use crate::backend_kind::BackendKind;

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
}

/// Enumerates devices available to each mining backend.
pub fn list_devices(prefer: BackendKind) -> Result<Vec<GpuDevice>, String> {
    match prefer {
        BackendKind::Cuda => list_cuda_devices(),
        BackendKind::Hip => list_hip_devices(),
        BackendKind::Wgpu => list_wgpu_devices(),
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

            if devices.is_empty() {
                Err(format!("no GPU device found ({})", errors.join("; ")))
            } else {
                Ok(devices)
            }
        }
    }
}

/// Resolves the selected backend and GPU ordinal.
pub fn resolve_mining_device(
    prefer: BackendKind,
    requested_index: Option<u32>,
) -> Result<GpuDevice, String> {
    match prefer {
        BackendKind::Cuda => select_backend_device(list_cuda_devices()?, requested_index, prefer),
        BackendKind::Hip => select_backend_device(list_hip_devices()?, requested_index, prefer),
        BackendKind::Wgpu => select_backend_device(list_wgpu_devices()?, requested_index, prefer),
        BackendKind::Auto => {
            let cuda = list_cuda_devices().unwrap_or_default();
            let hip = list_hip_devices().unwrap_or_default();
            let wgpu = list_wgpu_devices().unwrap_or_default();
            select_auto_device(cuda, hip, wgpu, requested_index).ok_or_else(
                || match requested_index {
                    Some(index) => format!(
                        "no GPU device at backend-local ordinal {index}; run `pickaxe devices`"
                    ),
                    None => "no GPU device found; run `pickaxe devices`".into(),
                },
            )
        }
    }
}

/// Picks a device of one backend: the requested ordinal, or its preferred GPU.
fn select_backend_device(
    devices: Vec<GpuDevice>,
    requested_index: Option<u32>,
    backend: BackendKind,
) -> Result<GpuDevice, String> {
    match requested_index {
        Some(index) => find_device(devices, index, backend),
        None => prefer_discrete(devices).ok_or_else(|| {
            format!(
                "no {} GPU found; run `pickaxe devices`",
                backend.as_str().to_ascii_uppercase()
            )
        }),
    }
}

/// The first discrete GPU in priority order; an integrated GPU (such as an
/// AMD APU or Apple Silicon) only when no discrete GPU is present.
fn prefer_discrete(devices: impl IntoIterator<Item = GpuDevice>) -> Option<GpuDevice> {
    let mut integrated = None;
    for device in devices {
        if !device.integrated {
            return Some(device);
        }
        integrated.get_or_insert(device);
    }
    integrated
}

/// Chooses the preferred supported GPU automatically.
// #### PR #22: discrete GPUs on their native T2 engines, integrated on Vulkan
// What: without --device, auto takes the first discrete GPU from CUDA, then
// native HIP (when code objects for its architecture are installed), then
// the portable WGPU engine. With no discrete GPU, an integrated one mines on
// WGPU first (AMD APU-only PCs, Apple Silicon). An explicit --backend takes
// its first discrete GPU; --device picks any GPU, integrated included.
// Why: native HIP now runs the T2 window search, AMD's native path for
// discrete Radeon GPUs. Integrated GPUs share the CPU package and, under
// Windows, never complete a HIP launch, so they stay on Vulkan.
// Check: `pickaxe devices` on AMD-only and NVIDIA+integrated machines; a
// discrete Radeon without matching code objects falls back to WGPU.
fn select_auto_device(
    cuda: Vec<GpuDevice>,
    hip: Vec<GpuDevice>,
    wgpu: Vec<GpuDevice>,
    wanted: Option<u32>,
) -> Option<GpuDevice> {
    if let Some(index) = wanted {
        // An explicit ordinal is the operator's choice, integrated GPUs included.
        return cuda
            .into_iter()
            .chain(hip)
            .chain(wgpu)
            .find(|device| device.index == index);
    }
    let hip: Vec<GpuDevice> = hip.into_iter().filter(|device| device.ready).collect();
    if let Some(discrete) = cuda
        .iter()
        .chain(&hip)
        .chain(&wgpu)
        .find(|device| !device.integrated)
    {
        return Some(discrete.clone());
    }
    wgpu.into_iter().chain(hip).next()
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
        BackendKind::Auto => {
            Err("auto backend must be resolved before production mining starts".into())
        }
    }
}

/// Finds the GPU matching a backend and device ordinal.
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
pub(crate) fn production_wgpu_backends() -> wgpu::Backends {
    crate::wgpu_photon::production_wgpu_backends()
}

#[cfg(feature = "portable-wgpu")]
/// Enumerates supported WGPU hardware adapters.
fn list_wgpu_devices() -> Result<Vec<GpuDevice>, String> {
    let backends = production_wgpu_backends();
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    let instance = wgpu::Instance::new(descriptor);
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
            integrated: false,
            ready: true,
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
                integrated: crate::hip_photon::is_apu_architecture(&architecture),
                ready: crate::hip_photon::code_objects_installed(&architecture)
                    && !(cfg!(windows) && crate::hip_photon::is_apu_architecture(&architecture)),
                detail,
            });
        }
        Ok(out)
    }
}

/// Reads CUDA device details for selection and display.
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
        assert_eq!(
            production_wgpu_backends(),
            if cfg!(target_os = "macos") {
                wgpu::Backends::METAL
            } else {
                wgpu::Backends::VULKAN
            }
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
        }];
        let selected = find_device(devices, 2, BackendKind::Hip).unwrap();
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
        }
    }

    fn integrated_fixture(index: u32, vendor: &str, backend: BackendKind) -> GpuDevice {
        GpuDevice {
            integrated: true,
            ..fixture_device(index, vendor, backend)
        }
    }

    #[test]
    fn auto_selection_prefers_cuda_over_wgpu_for_nvidia() {
        let selected = select_auto_device(
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
        let selected = select_auto_device(
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
        let selected = select_auto_device(
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
        let selected = select_auto_device(
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
        let selected = select_auto_device(
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
        let selected = select_auto_device(
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
        let selected = select_auto_device(
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
        let selected = select_backend_device(devices, None, BackendKind::Wgpu).unwrap();
        assert_eq!(selected.index, 1);
    }

    #[test]
    fn explicit_backend_and_device_can_select_an_integrated_gpu() {
        let devices = vec![
            integrated_fixture(0, "AMD", BackendKind::Hip),
            fixture_device(1, "AMD", BackendKind::Hip),
        ];
        let selected = select_backend_device(devices, Some(0), BackendKind::Hip).unwrap();
        assert!(selected.integrated);
    }

    #[test]
    fn explicit_backend_without_device_falls_back_to_a_lone_integrated_gpu() {
        let devices = vec![integrated_fixture(0, "AMD", BackendKind::Hip)];
        let selected = select_backend_device(devices, None, BackendKind::Hip).unwrap();
        assert!(selected.integrated);
    }
}
