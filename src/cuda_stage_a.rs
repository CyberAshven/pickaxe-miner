//! CUDA Stage A launcher via cudarc (load PTX at runtime).
//! Product path = GPU. Falls back with a clear error if no NVIDIA device.

use cudarc::driver::{CudaContext, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::Ptx;
use std::path::PathBuf;
use std::sync::Arc;

/// Returns the compiled stage A CUDA PTX path.
fn ptx_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cuda/build/stage_a_hash256.ptx")
}

/// Run HASH256(nonce_le || target32) on GPU for `n` nonces starting at `nonce_base`.
/// Returns digests as 32-byte big-endian arrays.
pub fn stage_a_hash256_batch(
    nonce_base: u32,
    target32: &[u8; 32],
    n: u32,
) -> Result<Vec<[u8; 32]>, String> {
    if n == 0 {
        return Ok(Vec::new());
    }
    let ptx_file = ptx_path();
    if !ptx_file.is_file() {
        return Err(format!(
            "missing PTX at {} - build with nvcc -ptx first",
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
        .load_function("pickaxe_stage_a_hash256")
        .map_err(|e| format!("load fn: {e}"))?;

    let mut target_be = [0u32; 8];
    for (i, word) in target_be.iter_mut().enumerate() {
        let j = i * 4;
        *word = u32::from_be_bytes([
            target32[j],
            target32[j + 1],
            target32[j + 2],
            target32[j + 3],
        ]);
    }

    let target_gpu = stream
        .clone_htod(&target_be)
        .map_err(|e| format!("memcpy target: {e}"))?;
    let out_words = (n as usize) * 8;
    let mut out_gpu = stream
        .alloc_zeros::<u32>(out_words)
        .map_err(|e| format!("alloc out: {e}"))?;

    let cfg = LaunchConfig::for_num_elems(n);
    let mut builder = stream.launch_builder(&func);
    let n_arg = n;
    let nb = nonce_base;
    unsafe {
        builder
            .arg(&nb)
            .arg(&target_gpu)
            .arg(&mut out_gpu)
            .arg(&n_arg);
        builder.launch(cfg).map_err(|e| format!("launch: {e}"))?;
    }
    let out: Vec<u32> = stream
        .clone_dtoh(&out_gpu)
        .map_err(|e| format!("memcpy back: {e}"))?;

    let mut digests = Vec::with_capacity(n as usize);
    for i in 0..(n as usize) {
        let mut d = [0u8; 32];
        for t in 0..8 {
            let w = out[i * 8 + t].to_be_bytes();
            d[t * 4..t * 4 + 4].copy_from_slice(&w);
        }
        digests.push(d);
    }
    let _ = Arc::strong_count(&ctx);
    Ok(digests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn host_hash256_nonce_target(nonce: u32, target: &[u8; 32]) -> [u8; 32] {
        let mut msg = Vec::with_capacity(36);
        msg.extend_from_slice(&nonce.to_le_bytes());
        msg.extend_from_slice(target);
        let first = Sha256::digest(&msg);
        let second = Sha256::digest(first);
        let mut out = [0u8; 32];
        out.copy_from_slice(&second);
        out
    }

    #[test]
    fn gpu_matches_host_if_cuda_present() {
        let target = [0u8; 32];
        match stage_a_hash256_batch(0, &target, 4) {
            Ok(gpu) => {
                for (i, g) in gpu.iter().enumerate() {
                    let h = host_hash256_nonce_target(i as u32, &target);
                    assert_eq!(*g, h, "mismatch at nonce {i}");
                }
            }
            Err(e) => {
                // CI / machines without GPU: skip
                eprintln!("skip GPU test: {e}");
            }
        }
    }
}
