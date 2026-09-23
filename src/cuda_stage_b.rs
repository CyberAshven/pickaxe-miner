//! CUDA Stage B launcher: batch k*G via cudarc + PTX.
//! Product path = GPU. Host oracle lives in `stage_b` for verification.

use cudarc::driver::{CudaContext, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::Ptx;
use std::path::PathBuf;

/// Returns the compiled stage B CUDA PTX path.
fn ptx_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cuda/build/stage_b_kg.ptx")
}

/// Batch compressed k*G on GPU for big-endian 32-byte scalars.
pub fn stage_b_kg_batch(scalars: &[[u8; 32]]) -> Result<Vec<[u8; 33]>, String> {
    let n = scalars.len() as u32;
    if n == 0 {
        return Ok(Vec::new());
    }
    let ptx_file = ptx_path();
    if !ptx_file.is_file() {
        return Err(format!(
            "missing PTX at {} â€” build with nvcc -ptx first",
            ptx_file.display()
        ));
    }
    let ptx_src = std::fs::read_to_string(&ptx_file).map_err(|e| e.to_string())?;
    let ctx = CudaContext::new(0).map_err(|e| format!("cuda context: {e}"))?;
    let stream = ctx.default_stream();
    let module = ctx
        .load_module(Ptx::from_src(ptx_src))
        .map_err(|e| format!("load module: {e}"))?;
    let func = module
        .load_function("pickaxe_stage_b_kg")
        .map_err(|e| format!("load fn: {e}"))?;

    let mut flat = Vec::with_capacity(scalars.len() * 32);
    for s in scalars {
        flat.extend_from_slice(s);
    }
    let scalars_gpu = stream
        .clone_htod(&flat)
        .map_err(|e| format!("htod scalars: {e}"))?;
    let mut out_gpu = stream
        .alloc_zeros::<u8>((n as usize) * 33)
        .map_err(|e| format!("alloc out: {e}"))?;

    let cfg = LaunchConfig {
        grid_dim: (n, 1, 1),
        block_dim: (1, 1, 1),
        shared_mem_bytes: 0,
    };
    let mut builder = stream.launch_builder(&func);
    let n_arg = n;
    unsafe {
        builder.arg(&scalars_gpu).arg(&mut out_gpu).arg(&n_arg);
        builder.launch(cfg).map_err(|e| format!("launch: {e}"))?;
    }
    let out: Vec<u8> = stream
        .clone_dtoh(&out_gpu)
        .map_err(|e| format!("dtoh: {e}"))?;

    let mut pubs = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let mut pk = [0u8; 33];
        pk.copy_from_slice(&out[i * 33..(i + 1) * 33]);
        pubs.push(pk);
    }
    Ok(pubs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage_b;

    #[test]
    fn gpu_kg_matches_host_if_cuda_present() {
        let mut one = [0u8; 32];
        one[31] = 1;
        let mut two = [0u8; 32];
        two[31] = 2;
        let mut rnd = [0u8; 32];
        rnd[0] = 0x12;
        rnd[15] = 0xab;
        rnd[31] = 0x34;
        let scalars = vec![one, two, rnd];

        let host = match stage_b::kg_batch_host(&scalars) {
            Ok(v) => v,
            Err(e) => panic!("host kg: {e}"),
        };

        match stage_b_kg_batch(&scalars) {
            Ok(gpu) => {
                assert_eq!(gpu.len(), host.len());
                for (i, (g, h)) in gpu.iter().zip(host.iter()).enumerate() {
                    assert_eq!(hex::encode(g), hex::encode(h), "mismatch at index {i}");
                }
            }
            Err(e) => {
                let el = e.to_lowercase();
                if crate::cuda_photon::cuda_unavailable_for_tests(&el) {
                    eprintln!("skip Stage B GPU test: {e}");
                } else {
                    panic!("unexpected Stage B error: {e}");
                }
            }
        }
    }
}
