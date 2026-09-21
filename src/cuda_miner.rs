//! Persistent CUDA miner (PERFORMANCE CONTRACT).
//! Context/module/buffers created once. Host receives compact winners only.

use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, LaunchConfig, PushKernelArg,
};
use cudarc::nvrtc::Ptx;
use std::path::PathBuf;
use std::sync::Arc;

const WINNER_CAP: u32 = 64;

#[derive(Debug, Clone)]
pub struct Winner {
    pub nonce: u32,
    pub digest: [u8; 32],
    pub generation_id: u64,
}

pub struct CudaMiner {
    ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    mine_fn: CudaFunction,
    target_be: CudaSlice<u32>,
    target_le: CudaSlice<u8>,
    winner_count: CudaSlice<u32>,
    winner_nonces: CudaSlice<u32>,
    winner_digests: CudaSlice<u8>,
    hashes_done: CudaSlice<u64>,
    batch: u32,
}

impl CudaMiner {
    pub fn new(device_ordinal: usize, batch: u32) -> Result<Self, String> {
        let ptx_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cuda/build/stage_a_mine.ptx");
        if !ptx_path.is_file() {
            return Err(format!("missing PTX {}", ptx_path.display()));
        }
        let ptx_src = std::fs::read_to_string(&ptx_path).map_err(|e| e.to_string())?;
        let ctx = CudaContext::new(device_ordinal).map_err(|e| format!("cuda context: {e}"))?;
        let stream = ctx.default_stream();
        let module = ctx
            .load_module(Ptx::from_src(ptx_src))
            .map_err(|e| format!("load module: {e}"))?;
        let mine_fn = module
            .load_function("pickaxe_stage_a_mine")
            .map_err(|e| format!("load fn: {e}"))?;

        let target_be = stream
            .alloc_zeros::<u32>(8)
            .map_err(|e| format!("alloc target_be: {e}"))?;
        let target_le = stream
            .alloc_zeros::<u8>(32)
            .map_err(|e| format!("alloc target_le: {e}"))?;
        let winner_count = stream
            .alloc_zeros::<u32>(1)
            .map_err(|e| format!("alloc winner_count: {e}"))?;
        let winner_nonces = stream
            .alloc_zeros::<u32>(WINNER_CAP as usize)
            .map_err(|e| format!("alloc winner_nonces: {e}"))?;
        let winner_digests = stream
            .alloc_zeros::<u8>((WINNER_CAP as usize) * 32)
            .map_err(|e| format!("alloc winner_digests: {e}"))?;
        let hashes_done = stream
            .alloc_zeros::<u64>(1)
            .map_err(|e| format!("alloc hashes_done: {e}"))?;

        Ok(Self {
            ctx,
            stream,
            mine_fn,
            target_be,
            target_le,
            winner_count,
            winner_nonces,
            winner_digests,
            hashes_done,
            batch,
        })
    }

    pub fn set_target(&mut self, target32: &[u8; 32]) -> Result<(), String> {
        let mut target_be = [0u32; 8];
        for i in 0..8 {
            let j = i * 4;
            target_be[i] = u32::from_be_bytes([
                target32[j],
                target32[j + 1],
                target32[j + 2],
                target32[j + 3],
            ]);
        }
        self.stream
            .memcpy_htod(&target_be, &mut self.target_be)
            .map_err(|e| format!("htod target_be: {e}"))?;
        self.stream
            .memcpy_htod(target32.as_slice(), &mut self.target_le)
            .map_err(|e| format!("htod target_le: {e}"))?;
        Ok(())
    }

    /// Run one batch. Returns (hashes_this_batch, winners). Does not recreate context.
    pub fn mine_batch(&mut self, nonce_base: u32) -> Result<(u64, Vec<Winner>), String> {
        // reset counters
        self.stream
            .memset_zeros(&mut self.winner_count)
            .map_err(|e| format!("zero winner_count: {e}"))?;
        self.stream
            .memset_zeros(&mut self.hashes_done)
            .map_err(|e| format!("zero hashes: {e}"))?;

        let n = self.batch;
        let cfg = LaunchConfig {
            grid_dim: (n.div_ceil(256), 1, 1),
            block_dim: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let cap = WINNER_CAP;
        let mut builder = self.stream.launch_builder(&self.mine_fn);
        unsafe {
            builder
                .arg(&nonce_base)
                .arg(&self.target_be)
                .arg(&self.target_le)
                .arg(&mut self.winner_count)
                .arg(&mut self.winner_nonces)
                .arg(&mut self.winner_digests)
                .arg(&cap)
                .arg(&mut self.hashes_done)
                .arg(&n);
            builder.launch(cfg).map_err(|e| format!("launch: {e}"))?;
        }

        let count_v: Vec<u32> = self
            .stream
            .clone_dtoh(&self.winner_count)
            .map_err(|e| format!("dtoh count: {e}"))?;
        let hashes_v: Vec<u64> = self
            .stream
            .clone_dtoh(&self.hashes_done)
            .map_err(|e| format!("dtoh hashes: {e}"))?;
        let count = count_v.first().copied().unwrap_or(0).min(WINNER_CAP);
        let hashes = hashes_v.first().copied().unwrap_or(n as u64);

        let mut winners = Vec::new();
        if count > 0 {
            let nonces: Vec<u32> = self
                .stream
                .clone_dtoh(&self.winner_nonces)
                .map_err(|e| format!("dtoh nonces: {e}"))?;
            let digests: Vec<u8> = self
                .stream
                .clone_dtoh(&self.winner_digests)
                .map_err(|e| format!("dtoh digests: {e}"))?;
            for i in 0..(count as usize) {
                let mut d = [0u8; 32];
                d.copy_from_slice(&digests[i * 32..(i + 1) * 32]);
                winners.push(Winner {
                    nonce: nonces[i],
                    digest: d,
                    generation_id: 0,
                });
            }
        }
        let _ = Arc::strong_count(&self.ctx);
        Ok((hashes, winners))
    }

    pub fn set_batch(&mut self, batch: u32) {
        self.batch = batch.max(256);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{hash256, meets_target_le, photon_m1_message};

    #[test]
    fn persistent_mine_finds_easy_target_if_cuda() {
        let target = [0xffu8; 32];
        // very easy: almost everything meets
        match CudaMiner::new(0, 1024) {
            Ok(mut miner) => {
                miner.set_target(&target).unwrap();
                let (h, winners) = miner.mine_batch(0).unwrap();
                assert_eq!(h, 1024);
                assert!(!winners.is_empty());
                for w in &winners {
                    let msg = photon_m1_message(w.nonce, &target);
                    let d = hash256(&msg);
                    assert_eq!(d, w.digest);
                    assert!(meets_target_le(&d, &target));
                }
                // second batch reuses same miner (persistence)
                let (h2, _) = miner.mine_batch(1024).unwrap();
                assert_eq!(h2, 1024);
            }
            Err(e) => eprintln!("skip CudaMiner test: {e}"),
        }
    }
}
