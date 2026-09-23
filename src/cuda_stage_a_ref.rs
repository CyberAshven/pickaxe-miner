//! Reference-correct CUDA Stage A.
//!
//! The authoritative PHOTON path starts with:
//! - SHA256(nonceLE || target)
//! - deterministic BCH Schnorr RFC6979 nonce generation
//!
//! This module is a correctness bridge toward the complete persistent A->B->C
//! pipeline. It is not yet the production search engine.

use cudarc::driver::{CudaContext, LaunchConfig, PushKernelArg};
use cudarc::nvrtc::Ptx;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageARecord {
    pub message_sha256: [u8; 32],
    pub rfc6979_nonce: [u8; 32],
}

/// Returns the reference stage A CUDA PTX path.
fn ptx_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cuda/build/stage_a_rfc6979.ptx")
}

/// Runs the reference stage A kernel over a candidate batch.
pub fn stage_a_reference_batch(
    nonce_base: u32,
    target32: &[u8; 32],
    private_key32: &[u8; 32],
    n: u32,
) -> Result<Vec<StageARecord>, String> {
    if n == 0 {
        return Ok(Vec::new());
    }

    let ptx_file = ptx_path();
    if !ptx_file.is_file() {
        return Err(format!(
            "missing PTX at {} - build cuda/stage_a_rfc6979.cu first",
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
        .load_function("pickaxe_stage_a_rfc6979")
        .map_err(|e| format!("load fn: {e}"))?;

    let target_gpu = stream
        .clone_htod(target32)
        .map_err(|e| format!("htod target: {e}"))?;
    let private_gpu = stream
        .clone_htod(private_key32)
        .map_err(|e| format!("htod private key: {e}"))?;
    let mut message_gpu = stream
        .alloc_zeros::<u8>((n as usize) * 32)
        .map_err(|e| format!("alloc message hashes: {e}"))?;
    let mut nonce_gpu = stream
        .alloc_zeros::<u8>((n as usize) * 32)
        .map_err(|e| format!("alloc RFC6979 nonces: {e}"))?;

    let cfg = LaunchConfig {
        grid_dim: (n.div_ceil(128), 1, 1),
        block_dim: (128, 1, 1),
        shared_mem_bytes: 0,
    };
    let mut builder = stream.launch_builder(&func);
    unsafe {
        builder
            .arg(&nonce_base)
            .arg(&target_gpu)
            .arg(&private_gpu)
            .arg(&mut message_gpu)
            .arg(&mut nonce_gpu)
            .arg(&n);
        builder.launch(cfg).map_err(|e| format!("launch: {e}"))?;
    }

    let messages: Vec<u8> = stream
        .clone_dtoh(&message_gpu)
        .map_err(|e| format!("dtoh message hashes: {e}"))?;
    let nonces: Vec<u8> = stream
        .clone_dtoh(&nonce_gpu)
        .map_err(|e| format!("dtoh RFC6979 nonces: {e}"))?;

    Ok((0..n as usize)
        .map(|i| {
            let mut message_sha256 = [0u8; 32];
            let mut rfc6979_nonce = [0u8; 32];
            message_sha256.copy_from_slice(&messages[i * 32..(i + 1) * 32]);
            rfc6979_nonce.copy_from_slice(&nonces[i * 32..(i + 1) * 32]);
            StageARecord {
                message_sha256,
                rfc6979_nonce,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crypto, cuda_stage_b, stage_b, tx};

    fn should_skip_cuda_error(error: &str) -> bool {
        crate::cuda_photon::cuda_unavailable_for_tests(error)
    }

    #[test]
    fn gpu_stage_a_matches_photon_rfc6979_vector_if_cuda_present() {
        let target: [u8; 32] =
            hex::decode("ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000")
                .unwrap()
                .try_into()
                .unwrap();
        let mut private_key = [0u8; 32];
        private_key[31] = 1;

        let gpu = match stage_a_reference_batch(0x1234_5678, &target, &private_key, 1) {
            Ok(records) => records,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip reference Stage A GPU test: {error}");
                return;
            }
            Err(error) => panic!("reference Stage A failed: {error}"),
        };

        let expected_message =
            tx::photon_message_sha256(0x1234_5678, &hex::encode(target)).unwrap();
        let expected_nonce = crypto::bch_rfc6979_nonce(&private_key, &expected_message).unwrap();
        assert_eq!(
            hex::encode(gpu[0].message_sha256),
            "098d398ffeb43910012db426eb01279563beaf5e070abae77afacf312030457f"
        );
        assert_eq!(
            hex::encode(gpu[0].rfc6979_nonce),
            "615da2b700fbb1ae10a72a391ce49b17cd4d0f614311ac7d216281217fd7797a"
        );
        assert_eq!(gpu[0].message_sha256, expected_message);
        assert_eq!(gpu[0].rfc6979_nonce, expected_nonce);
    }

    #[test]
    fn gpu_stage_a_feeds_stage_b_reference_point_if_cuda_present() {
        let target: [u8; 32] =
            hex::decode("ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000")
                .unwrap()
                .try_into()
                .unwrap();
        let mut private_key = [0u8; 32];
        private_key[31] = 1;

        let stage_a = match stage_a_reference_batch(0x1234_5678, &target, &private_key, 1) {
            Ok(records) => records,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip A->B GPU test: {error}");
                return;
            }
            Err(error) => panic!("reference Stage A failed: {error}"),
        };

        let gpu_b = match cuda_stage_b::stage_b_kg_batch(&[stage_a[0].rfc6979_nonce]) {
            Ok(points) => points,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip A->B GPU test: {error}");
                return;
            }
            Err(error) => panic!("Stage B failed: {error}"),
        };
        let host_b = stage_b::kg_batch_host(&[stage_a[0].rfc6979_nonce]).unwrap();
        assert_eq!(gpu_b, host_b);
    }
}
