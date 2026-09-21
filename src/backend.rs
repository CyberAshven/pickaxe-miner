//! Native production GPU backend selection: auto | cuda | hip.
//! No CPU mining fallback.

use cudarc::driver::{sys, CudaContext};

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
        BackendKind::Cuda | BackendKind::Auto => match list_cuda_devices() {
            Ok(d) if !d.is_empty() => Ok(d),
            Ok(_) => Err("CUDA runtime present but zero devices".into()),
            Err(e) if matches!(prefer, BackendKind::Cuda) => Err(e),
            Err(e) => Err(format!(
                "{e} No validated native GPU backend is available; HIP/ROCm auto-detection is not implemented yet."
            )),
        },
        BackendKind::Hip => Err(
            "HIP/ROCm backend not wired yet. Use --backend cuda on NVIDIA or wait for HIP.".into(),
        ),
    }
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
            "[{}] {} | {} | backend={} | VRAM {} | {}",
            d.index,
            d.vendor,
            d.name,
            d.backend.as_str(),
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
}
