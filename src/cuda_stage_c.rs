//! Persistent CUDA Stage C: completed PHOTON transaction HASH256 + target filter.
//!
//! This module implements the authoritative Stage C boundary from the M67.38
//! reference. It accepts device-bound Schnorr signatures, injects nonce/signature
//! into the immutable transaction template of any GPU layout, hashes the
//! completed transaction, applies the contract's proof rule, and returns only a
//! bounded winner set. The HIP backend compiles the same kernel source, so these
//! CUDA tests also check AMD's Stage C logic.

use crate::protocol::ProofRule;
use crate::tx::PhotonLayout;
use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, LaunchConfig, PushKernelArg,
};
use cudarc::nvrtc::Ptx;
use std::path::PathBuf;
use std::sync::Arc;

const MAX_TX_BYTES: usize = 615 + PhotonLayout::MAX_SHIFT;
const SIGNATURE_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCWinner {
    pub nonce: u32,
    pub digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCBatchResult {
    pub candidates: u32,
    pub total_winners: u32,
    pub winners: Vec<StageCWinner>,
}

impl StageCBatchResult {
    /// Checks whether a stage C result exceeds its readback capacity.
    pub fn truncated(&self) -> bool {
        self.total_winners as usize > self.winners.len()
    }
}

pub struct CudaStageC {
    _ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    filter_fn: CudaFunction,
    probe_fn: CudaFunction,
    compare_probe_fn: CudaFunction,
    template_gpu: CudaSlice<u8>,
    target_gpu: CudaSlice<u8>,
    signatures_gpu: CudaSlice<u8>,
    winner_count_gpu: CudaSlice<u32>,
    winner_nonces_gpu: CudaSlice<u32>,
    winner_hashes_gpu: CudaSlice<u8>,
    signature_staging: Vec<u8>,
    max_candidates: u32,
    winner_cap: u32,
    shift: u32,
    positive_rule: u32,
}

impl CudaStageC {
    /// Allocates CUDA stage C buffers and loads the filter kernels.
    pub fn new(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
    ) -> Result<Self, String> {
        if max_candidates == 0 {
            return Err("Stage C max_candidates must be greater than zero".into());
        }
        if winner_cap == 0 {
            return Err("Stage C winner_cap must be greater than zero".into());
        }

        let ptx_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cuda/build/stage_c_hash.ptx");
        if !ptx_path.is_file() {
            return Err(format!(
                "missing PTX at {} - build cuda/stage_c_hash.cu first",
                ptx_path.display()
            ));
        }
        let ptx_src = std::fs::read_to_string(&ptx_path).map_err(|e| e.to_string())?;
        let ctx = CudaContext::new(device_ordinal).map_err(|e| format!("cuda context: {e}"))?;
        let stream = ctx.default_stream();
        let module = ctx
            .load_module(Ptx::from_src(ptx_src))
            .map_err(|e| format!("load Stage C module: {e}"))?;
        let filter_fn = module
            .load_function("pickaxe_stage_c_hash_filter")
            .map_err(|e| format!("load Stage C filter: {e}"))?;
        let probe_fn = module
            .load_function("pickaxe_stage_c_probe")
            .map_err(|e| format!("load Stage C probe: {e}"))?;
        let compare_probe_fn = module
            .load_function("pickaxe_stage_c_compare_probe")
            .map_err(|e| format!("load Stage C compare probe: {e}"))?;

        let template_gpu = stream
            .alloc_zeros::<u8>(MAX_TX_BYTES)
            .map_err(|e| format!("alloc Stage C template: {e}"))?;
        let target_gpu = stream
            .alloc_zeros::<u8>(32)
            .map_err(|e| format!("alloc Stage C target: {e}"))?;
        let signatures_gpu = stream
            .alloc_zeros::<u8>((max_candidates as usize) * SIGNATURE_BYTES)
            .map_err(|e| format!("alloc Stage C signatures: {e}"))?;
        let winner_count_gpu = stream
            .alloc_zeros::<u32>(1)
            .map_err(|e| format!("alloc Stage C winner count: {e}"))?;
        let winner_nonces_gpu = stream
            .alloc_zeros::<u32>(winner_cap as usize)
            .map_err(|e| format!("alloc Stage C winner nonces: {e}"))?;
        let winner_hashes_gpu = stream
            .alloc_zeros::<u8>((winner_cap as usize) * 32)
            .map_err(|e| format!("alloc Stage C winner hashes: {e}"))?;

        Ok(Self {
            _ctx: ctx,
            stream,
            filter_fn,
            probe_fn,
            compare_probe_fn,
            template_gpu,
            target_gpu,
            signatures_gpu,
            winner_count_gpu,
            winner_nonces_gpu,
            winner_hashes_gpu,
            signature_staging: Vec::with_capacity((max_candidates as usize) * SIGNATURE_BYTES),
            max_candidates,
            winner_cap,
            shift: 0,
            positive_rule: 0,
        })
    }

    /// Uploads PHOTON job material to the stage C kernels.
    pub fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        rule: ProofRule,
    ) -> Result<(), String> {
        let layout = PhotonLayout::for_tx_len(template.len())?;
        let target_offset = layout.target_offset();
        if template[target_offset..target_offset + 32] != target[..] {
            return Err(format!(
                "Stage C target must match transaction template bytes {target_offset}..{}",
                target_offset + 31
            ));
        }
        let mut padded = [0u8; MAX_TX_BYTES];
        padded[..template.len()].copy_from_slice(template);
        self.stream
            .memcpy_htod(&padded, &mut self.template_gpu)
            .map_err(|e| format!("upload Stage C template: {e}"))?;
        self.stream
            .memcpy_htod(target, &mut self.target_gpu)
            .map_err(|e| format!("upload Stage C target: {e}"))?;
        self.shift = u32::try_from(layout.shift()).map_err(|_| "layout shift exceeds u32")?;
        self.positive_rule = u32::from(rule == ProofRule::Positive);
        Ok(())
    }

    /// Hashes stage C candidates and filters them against the target.
    pub fn hash_and_filter(
        &mut self,
        nonce_base: u32,
        signatures: &[[u8; SIGNATURE_BYTES]],
    ) -> Result<StageCBatchResult, String> {
        let candidate_count = u32::try_from(signatures.len())
            .map_err(|_| "Stage C candidate count does not fit u32")?;
        if candidate_count == 0 {
            return Ok(StageCBatchResult {
                candidates: 0,
                total_winners: 0,
                winners: Vec::new(),
            });
        }
        if candidate_count > self.max_candidates {
            return Err(format!(
                "Stage C batch {candidate_count} exceeds persistent capacity {}",
                self.max_candidates
            ));
        }

        self.signature_staging.clear();
        for signature in signatures {
            self.signature_staging.extend_from_slice(signature);
        }
        self.stream
            .memcpy_htod(&self.signature_staging, &mut self.signatures_gpu)
            .map_err(|e| format!("upload Stage C signatures: {e}"))?;
        self.stream
            .memset_zeros(&mut self.winner_count_gpu)
            .map_err(|e| format!("reset Stage C winner count: {e}"))?;

        let cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(128), 1, 1),
            block_dim: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut builder = self.stream.launch_builder(&self.filter_fn);
        unsafe {
            builder
                .arg(&self.template_gpu)
                .arg(&self.signatures_gpu)
                .arg(&nonce_base)
                .arg(&self.target_gpu)
                .arg(&candidate_count)
                .arg(&self.winner_cap)
                .arg(&mut self.winner_count_gpu)
                .arg(&mut self.winner_nonces_gpu)
                .arg(&mut self.winner_hashes_gpu)
                .arg(&self.shift)
                .arg(&self.positive_rule);
            builder
                .launch(cfg)
                .map_err(|e| format!("launch Stage C: {e}"))?;
        }

        let counts: Vec<u32> = self
            .stream
            .clone_dtoh(&self.winner_count_gpu)
            .map_err(|e| format!("read Stage C winner count: {e}"))?;
        let total_winners = counts.first().copied().unwrap_or(0);
        let returned = total_winners.min(self.winner_cap) as usize;
        let mut winners = Vec::with_capacity(returned);
        if returned > 0 {
            let nonces: Vec<u32> = self
                .stream
                .clone_dtoh(&self.winner_nonces_gpu)
                .map_err(|e| format!("read Stage C winner nonces: {e}"))?;
            let hashes: Vec<u8> = self
                .stream
                .clone_dtoh(&self.winner_hashes_gpu)
                .map_err(|e| format!("read Stage C winner hashes: {e}"))?;
            for i in 0..returned {
                let mut digest = [0u8; 32];
                digest.copy_from_slice(&hashes[i * 32..(i + 1) * 32]);
                winners.push(StageCWinner {
                    nonce: nonces[i],
                    digest,
                });
            }
        }

        Ok(StageCBatchResult {
            candidates: candidate_count,
            total_winners,
            winners,
        })
    }

    #[cfg(test)]
    /// Reads the stage C hash of a candidate for comparison.
    fn probe_hash(
        &mut self,
        nonce: u32,
        signature: &[u8; SIGNATURE_BYTES],
    ) -> Result<([u8; 32], [u8; 32]), String> {
        let signature_gpu = self
            .stream
            .clone_htod(signature)
            .map_err(|e| format!("upload Stage C probe signature: {e}"))?;
        let mut first_gpu = self
            .stream
            .alloc_zeros::<u8>(32)
            .map_err(|e| format!("alloc Stage C probe SHA256: {e}"))?;
        let mut final_gpu = self
            .stream
            .alloc_zeros::<u8>(32)
            .map_err(|e| format!("alloc Stage C probe HASH256: {e}"))?;
        let mut builder = self.stream.launch_builder(&self.probe_fn);
        unsafe {
            builder
                .arg(&self.template_gpu)
                .arg(&signature_gpu)
                .arg(&nonce)
                .arg(&mut first_gpu)
                .arg(&mut final_gpu);
            builder
                .launch(LaunchConfig {
                    grid_dim: (1, 1, 1),
                    block_dim: (1, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|e| format!("launch Stage C probe: {e}"))?;
        }
        let first = self
            .stream
            .clone_dtoh(&first_gpu)
            .map_err(|e| format!("read Stage C probe SHA256: {e}"))?;
        let final_hash = self
            .stream
            .clone_dtoh(&final_gpu)
            .map_err(|e| format!("read Stage C probe HASH256: {e}"))?;
        Ok((
            first.try_into().map_err(|_| "bad Stage C SHA256 length")?,
            final_hash
                .try_into()
                .map_err(|_| "bad Stage C HASH256 length")?,
        ))
    }

    #[cfg(test)]
    /// Compares a probe hash against the strict PHOTON target.
    fn compare_probe(&mut self, hash: &[u8; 32], target: &[u8; 32]) -> Result<bool, String> {
        let hash_gpu = self
            .stream
            .clone_htod(hash)
            .map_err(|e| format!("upload Stage C compare hash: {e}"))?;
        let target_gpu = self
            .stream
            .clone_htod(target)
            .map_err(|e| format!("upload Stage C compare target: {e}"))?;
        let mut out_gpu = self
            .stream
            .alloc_zeros::<u32>(1)
            .map_err(|e| format!("alloc Stage C compare output: {e}"))?;
        let mut builder = self.stream.launch_builder(&self.compare_probe_fn);
        unsafe {
            builder.arg(&hash_gpu).arg(&target_gpu).arg(&mut out_gpu);
            builder
                .launch(LaunchConfig {
                    grid_dim: (1, 1, 1),
                    block_dim: (1, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|e| format!("launch Stage C compare probe: {e}"))?;
        }
        let out = self
            .stream
            .clone_dtoh(&out_gpu)
            .map_err(|e| format!("read Stage C compare output: {e}"))?;
        Ok(out.first().copied().unwrap_or(0) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{search, tx};

    const TX_BYTES: usize = 615;

    fn should_skip_cuda_error(error: &str) -> bool {
        crate::cuda_photon::cuda_unavailable_for_tests(error)
    }

    fn vector_parts() -> ([u8; TX_BYTES], u32, [u8; 32], [u8; SIGNATURE_BYTES]) {
        let raw = hex::decode(include_str!("../reference/photon_vector_tx.hex").trim()).unwrap();
        let mut template: [u8; TX_BYTES] = raw.try_into().unwrap();
        let nonce = u32::from_le_bytes(template[390..394].try_into().unwrap());
        let target: [u8; 32] = template[394..426].try_into().unwrap();
        let signature: [u8; SIGNATURE_BYTES] = template[426..490].try_into().unwrap();
        template[390..394].fill(0);
        template[426..490].fill(0);
        (template, nonce, target, signature)
    }

    #[test]
    fn gpu_stage_c_matches_authoritative_photon_vector_if_cuda_present() {
        let (template, nonce, target, signature) = vector_parts();
        let mut stage = match CudaStageC::new(0, 8, 4) {
            Ok(stage) => stage,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip Stage C vector test: {error}");
                return;
            }
            Err(error) => panic!("Stage C init failed: {error}"),
        };
        stage
            .set_job(&template, &target, ProofRule::Absolute)
            .unwrap();
        let (first, final_hash) = stage.probe_hash(nonce, &signature).unwrap();
        assert_eq!(
            hex::encode(first),
            "117b954c8ade1b14993b870d9a6c79309ceaba1b2b3aba4f08e3bf2ae39732bb"
        );
        assert_eq!(
            hex::encode(final_hash),
            "051e3c16e4d1bd51d59030594462372acc787d0ee3f77bb2724950bd6ad64c28"
        );
    }

    #[test]
    fn gpu_stage_c_target_comparison_is_strict_little_endian_if_cuda_present() {
        let (template, _, target, _) = vector_parts();
        let mut stage = match CudaStageC::new(0, 1, 1) {
            Ok(stage) => stage,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip Stage C comparator test: {error}");
                return;
            }
            Err(error) => panic!("Stage C init failed: {error}"),
        };
        stage
            .set_job(&template, &target, ProofRule::Absolute)
            .unwrap();

        let hash = [0x55u8; 32];
        assert!(!stage.compare_probe(&hash, &hash).unwrap());

        let mut one_more = hash;
        one_more[0] = one_more[0].wrapping_add(1);
        assert!(stage.compare_probe(&hash, &one_more).unwrap());

        let mut one_less = hash;
        one_less[0] = one_less[0].wrapping_sub(1);
        assert!(!stage.compare_probe(&hash, &one_less).unwrap());
    }

    #[test]
    fn gpu_stage_c_winner_readback_is_bounded_if_cuda_present() {
        let (mut template, _, _, _) = vector_parts();
        let target = [0xffu8; 32];
        template[394..426].copy_from_slice(&target);
        let signatures = vec![[0u8; SIGNATURE_BYTES]; 32];
        let mut stage = match CudaStageC::new(0, 32, 4) {
            Ok(stage) => stage,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip Stage C bounded winner test: {error}");
                return;
            }
            Err(error) => panic!("Stage C init failed: {error}"),
        };
        stage
            .set_job(&template, &target, ProofRule::Absolute)
            .unwrap();
        let result = stage.hash_and_filter(1000, &signatures).unwrap();
        assert_eq!(result.candidates, 32);
        assert_eq!(result.total_winners, 32);
        assert_eq!(result.winners.len(), 4);
        assert!(result.truncated());

        for winner in &result.winners {
            let index = (winner.nonce - 1000) as usize;
            let mut completed = template;
            completed[390..394].copy_from_slice(&winner.nonce.to_le_bytes());
            completed[426..490].copy_from_slice(&signatures[index]);
            assert_eq!(winner.digest, search::hash256(&completed));
            assert!(search::meets_target_le(&winner.digest, &target));
        }
    }

    #[test]
    fn gpu_stage_c_filters_every_layout_with_its_proof_rule_if_cuda_present() {
        // Absolute admits digest top bytes 0x00..=0x1f and 0x80..=0x9f;
        // Positive only the first range.
        let mut target = [0xffu8; 32];
        target[31] = 0x20;
        let count = 256u32;
        let signatures = (0..count)
            .map(|index| {
                let half = search::hash256(&index.to_le_bytes());
                let mut signature = [0u8; SIGNATURE_BYTES];
                signature[..32].copy_from_slice(&half);
                signature[32..].copy_from_slice(&half);
                signature
            })
            .collect::<Vec<_>>();
        let mut stage = match CudaStageC::new(0, count, count) {
            Ok(stage) => stage,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip Stage C layout test: {error}");
                return;
            }
            Err(error) => panic!("Stage C init failed: {error}"),
        };
        let public_key = hex::encode(crate::crypto::compressed_pubkey(&[0x11; 32]).unwrap());
        for shift in PhotonLayout::GPU_SHIFTS {
            let template = tx::template_for_shift(shift, |age| tx::TemplateParams {
                prev_tx_hash_hex: "aa".repeat(32),
                prev_index: 0,
                age,
                public_key_hex: public_key.clone(),
                target_hex: hex::encode(target),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 15_971_500,
                relay_fee_sats_per_kb: 1_000,
                contract_token_amount: 2_099_905_002_035_715,
                reward_amount: 4_999_773_813,
                payout_locking: tx::cashaddr_to_p2pkh_locking(
                    "zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3",
                )
                .unwrap(),
            });
            let layout = PhotonLayout::for_tx_len(template.len()).unwrap();
            for rule in [ProofRule::Absolute, ProofRule::Positive] {
                stage.set_job(&template, &target, rule).unwrap();
                let base = 0x5000_0000 + shift as u32;
                let result = stage.hash_and_filter(base, &signatures).unwrap();
                let mut expected = Vec::new();
                for (index, signature) in signatures.iter().enumerate() {
                    let nonce = base + index as u32;
                    let mut completed = template.clone();
                    let n = layout.nonce_offset();
                    completed[n..n + 4].copy_from_slice(&nonce.to_le_bytes());
                    let s = layout.signature_offset();
                    completed[s..s + SIGNATURE_BYTES].copy_from_slice(signature);
                    let digest = search::hash256(&completed);
                    if search::meets_target_le_for_rule(&digest, &target, rule) {
                        expected.push((nonce, digest));
                    }
                }
                let mut found = result
                    .winners
                    .iter()
                    .map(|winner| (winner.nonce, winner.digest))
                    .collect::<Vec<_>>();
                found.sort_unstable();
                assert!(
                    !expected.is_empty(),
                    "shift {shift} {rule:?}: no host winners"
                );
                assert_eq!(
                    result.total_winners as usize,
                    expected.len(),
                    "shift {shift} {rule:?}"
                );
                assert_eq!(found, expected, "shift {shift} {rule:?}");
            }
        }
    }
}
