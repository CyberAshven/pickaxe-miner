//! GPU candidate search (PERFORMANCE CONTRACT).
//! Production hot path = persistent CudaMiner. CPU hash256 is reference/tests only.

use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// The legacy persistent CUDA kernel only hashes nonceLE || target and is not
/// PHOTON proof-of-work. Keep product mining fail-closed until the exact
/// reference A -> B -> C transaction pipeline is wired end-to-end.
pub const REFERENCE_GPU_PIPELINE_READY: bool = false;

#[derive(Debug, Clone, Default)]
pub struct MiningJob {
    pub height: u32,
    pub baton_txid: String,
    pub baton_vout: u32,
    pub baton_height: u32,
    pub baton_value_sats: u64,
    pub age: u32,
    pub target_le_hex: String,
    pub token_amount: u128,
    pub reward_raw: u128,
    pub payout_address: String,
    pub source_identity: String,
    pub generation_id: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub candidates: u64,
    pub intensity: u8,
    pub state: MiningState,
    pub elapsed_secs: u64,
    pub rate: f64,
    pub winners: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MiningState {
    #[default]
    Stopped,
    Mining,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCommand {
    SetIntensity(u8),
    Pause,
    Resume,
}

/// Double SHA-256 (HASH256) ├â┬ó├óΓÇÜ┬¼├óΓé¼┬¥ tests / rare winner verify only.
pub fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

#[cfg(test)]
pub fn photon_m1_message(nonce: u32, target32: &[u8; 32]) -> [u8; 36] {
    let mut msg = [0u8; 36];
    msg[0..4].copy_from_slice(&nonce.to_le_bytes());
    msg[4..36].copy_from_slice(target32);
    msg
}

pub fn parse_hex32(hex: &str) -> Result<[u8; 32], String> {
    let h = hex.trim();
    if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("target must be 64 hex chars (32 bytes)".into());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] =
            u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).map_err(|_| "invalid hex".to_string())?;
    }
    Ok(out)
}

pub fn meets_target_le(digest: &[u8; 32], target_le: &[u8; 32]) -> bool {
    // PHOTON / Codex audit: strict hash < target (equality is NOT a win).
    for i in (0..32).rev() {
        if digest[i] < target_le[i] {
            return true;
        }
        if digest[i] > target_le[i] {
            return false;
        }
    }
    false
}

pub struct SearchHandle {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    started: Instant,
}

impl SearchHandle {
    pub fn start(intensity: u8, job: MiningJob) -> Result<Self, String> {
        if !(10..=100).contains(&intensity) {
            return Err("intensity must be 10..=100".into());
        }
        parse_hex32(&job.target_le_hex)?;
        if job.generation_id == 0 {
            return Err("mining job generation_id must be nonzero".into());
        }
        let _ = job;
        if !REFERENCE_GPU_PIPELINE_READY {
            return Err(
                "production mining is gated: legacy CUDA HASH256(nonceLE || target) is not PHOTON proof-of-work; exact GPU RFC6979/kG/Schnorr/full-transaction HASH256 pipeline must complete first"
                    .into(),
            );
        }
        Err("reference GPU pipeline readiness flag is inconsistent".into())
    }

    pub fn apply_control(&self, command: RuntimeCommand) -> Result<SearchStats, String> {
        match command {
            RuntimeCommand::SetIntensity(value) => {
                if !(10..=100).contains(&value) {
                    return Err("intensity must be 10..=100".into());
                }
                self.intensity.store(value, Ordering::Relaxed);
            }
            RuntimeCommand::Pause => self.paused.store(true, Ordering::Relaxed),
            RuntimeCommand::Resume => self.paused.store(false, Ordering::Relaxed),
        }
        Ok(self.snapshot())
    }

    fn state(&self) -> MiningState {
        if self.stop.load(Ordering::Relaxed) {
            MiningState::Stopped
        } else if self.paused.load(Ordering::Relaxed) {
            MiningState::Paused
        } else {
            MiningState::Mining
        }
    }

    pub fn stop(self) -> SearchStats {
        self.stop.store(true, Ordering::Relaxed);
        self.snapshot_final()
    }

    fn snapshot_final(&self) -> SearchStats {
        let candidates = self.candidates.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        SearchStats {
            candidates,
            intensity: self.intensity.load(Ordering::Relaxed),
            state: self.state(),
            elapsed_secs: elapsed as u64,
            rate: candidates as f64 / elapsed,
            winners: self.winners.load(Ordering::Relaxed),
        }
    }

    pub fn snapshot(&self) -> SearchStats {
        self.snapshot_final()
    }
    pub fn set_intensity(&self, value: u8) -> Result<(), String> {
        self.apply_control(RuntimeCommand::SetIntensity(value))?;
        Ok(())
    }

    pub fn toggle_pause(&self) -> bool {
        let next = !self.paused.load(Ordering::Relaxed);
        if next {
            let _ = self.apply_control(RuntimeCommand::Pause);
        } else {
            let _ = self.apply_control(RuntimeCommand::Resume);
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash256_empty() {
        let d = hash256(b"");
        assert_eq!(
            hex::encode(d),
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
        );
    }

    #[test]
    fn equality_is_not_a_win() {
        let t = parse_hex32(&"aa".repeat(32)).unwrap();
        assert!(!meets_target_le(&t, &t));
    }

    #[test]
    fn parse_and_meet_max_target() {
        let t = parse_hex32(&"ff".repeat(32)).unwrap();
        let d = hash256(b"x");
        assert!(meets_target_le(&d, &t));
    }

    #[test]
    fn production_search_refuses_legacy_hash_prototype() {
        let job = MiningJob {
            height: 1,
            target_le_hex: "ff".repeat(32),
            baton_txid: "00".repeat(32),
            generation_id: 1,
            ..MiningJob::default()
        };
        let error = SearchHandle::start(100, job)
            .err()
            .expect("legacy product search must stay gated");
        assert!(error.contains("not PHOTON proof-of-work"));
    }

    #[test]
    fn production_search_rejects_unpublished_generation() {
        let job = MiningJob {
            height: 1,
            target_le_hex: "ff".repeat(32),
            baton_txid: "00".repeat(32),
            generation_id: 0,
            ..MiningJob::default()
        };
        let error = match SearchHandle::start(100, job) {
            Ok(_) => panic!("generation zero must not start"),
            Err(error) => error,
        };
        assert!(error.contains("generation_id must be nonzero"));
    }
}
