//! Incremental-k search for unfunded PHOTON search identities only.
#![cfg_attr(not(any(feature = "incremental-k", test)), allow(dead_code))]
use super::*;
use crate::tx;

fn load_filters(engine: &CudaPhotonEngine, file: &str) -> Result<[CudaFunction; 4], String> {
    Ok([
        load_function(&engine._ctx, file, STAGE_C3_FUNCTIONS[0])?,
        load_function(&engine._ctx, file, STAGE_C3_FUNCTIONS[1])?,
        load_function(&engine._ctx, file, STAGE_C3_FUNCTIONS[2])?,
        load_function(&engine._ctx, file, STAGE_C3_FUNCTIONS[3])?,
    ])
}

pub(super) struct Incremental {
    walk: CudaFunction,
    filters: [CudaFunction; 4],
    message: CudaSlice<u8>,
    step: CudaSlice<u32>,
    stride: u32,
    per_lane: u32,
    pub(super) c1_per_thread: u32,
}

impl Incremental {
    pub(super) fn new(engine: &CudaPhotonEngine, per_lane: u32) -> Result<Self, String> {
        if !(1..=128).contains(&per_lane) {
            return Err("invalid incremental lane size".into());
        }
        Ok(Self {
            walk: load_function(
                &engine._ctx,
                "photon_incremental_k.ptx",
                "pickaxe_photon_incremental_k",
            )
            .map_err(|error| error.to_string())?,
            filters: load_filters(engine, "photon_incremental_c3.ptx")?,
            message: engine
                .stream
                .alloc_zeros(32)
                .map_err(|error| error.to_string())?,
            step: engine
                .stream
                .alloc_zeros(16)
                .map_err(|error| error.to_string())?,
            stride: 0,
            per_lane,
            c1_per_thread: 8,
        })
    }

    pub(super) fn set_message(
        &mut self,
        engine: &CudaPhotonEngine,
        target: &[u8; 32],
        nonce: u32,
    ) -> Result<(), String> {
        let hash = tx::photon_message_sha256(nonce, &hex::encode(target))
            .map_err(|error| error.to_string())?;
        engine
            .stream
            .memcpy_htod(&hash, &mut self.message)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn batch(
        &mut self,
        engine: &mut CudaPhotonEngine,
        base: u32,
        count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if !engine.job_ready
            || count > engine.max_candidates
            || u64::from(base) + u64::from(count) > 1u64 << 32
        {
            return Err("incremental batch is unconfigured, oversized, or crosses the 32-bit scalar index boundary".into());
        }
        if count == 0 {
            return Ok(PhotonCudaBatchResult {
                candidates: 0,
                total_winners: 0,
                winners: vec![],
            });
        }
        let blocks = count.div_ceil(self.per_lane).div_ceil(64);
        let stride = blocks * 64;
        if self.stride != stride {
            let point = PublicKey::from_secret_key(
                &SecretKey::from_secret_bytes({
                    let mut bytes = [0u8; 32];
                    bytes[24..].copy_from_slice(&u64::from(stride).to_be_bytes());
                    bytes
                })
                .map_err(|error| error.to_string())?,
            )
            .serialize_uncompressed();
            let words: Vec<u32> = point[1..]
                .as_chunks::<32>()
                .0
                .iter()
                .flat_map(|coordinate| {
                    coordinate
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .rev()
                        .map(|word| u32::from_be_bytes([word[0], word[1], word[2], word[3]]))
                })
                .collect();
            engine
                .stream
                .memcpy_htod(&words, &mut self.step)
                .map_err(|error| error.to_string())?;
            self.stride = stride;
        }
        engine
            .stream
            .memset_zeros(&mut engine.winner_count_gpu)
            .map_err(|error| error.to_string())?;
        unsafe {
            engine
                .stream
                .launch_builder(&self.walk)
                .arg(&base)
                .arg(&count)
                .arg(&engine.table_gpu)
                .arg(&self.step)
                .arg(&self.message)
                .arg(&mut engine.message_hashes_gpu)
                .arg(&mut engine.rfc6979_gpu)
                .arg(&mut engine.points_gpu)
                .launch(LaunchConfig {
                    grid_dim: (blocks, 1, 1),
                    block_dim: (64, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| format!("incremental point walk: {error}"))?;
        }
        std::mem::swap(&mut self.filters, &mut engine.stage_c3);
        let result = engine.finish_batch(base, count, self.c1_per_thread);
        std::mem::swap(&mut self.filters, &mut engine.stage_c3);
        result
    }
}
