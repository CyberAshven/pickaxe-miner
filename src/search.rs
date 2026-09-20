//! CPU candidate search (Stage 3b). GPU/wgpu after correctness.
//! Electrum / win-tx owned by Dev Assist — do not put WSS or tx build here.

use crate::config::RuntimeConfig;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Live job fields filled by the Electrum module later.
#[derive(Debug, Clone, Default)]
pub struct MiningJob {
    pub height: u32,
    pub target_le_hex: String,
    pub baton_txid: String,
}

#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub candidates: u64,
    pub intensity: u8,
    pub elapsed_secs: u64,
    pub rate: f64,
}

/// Double SHA-256 (HASH256), Bitcoin-style.
pub fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Milestone-1 style message: nonce (LE u32) || 32-byte target.
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
        out[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16)
            .map_err(|_| "invalid hex".to_string())?;
    }
    Ok(out)
}

/// Compare HASH256 digest to little-endian unsigned target (digest interpreted LE).
pub fn meets_target_le(digest: &[u8; 32], target_le: &[u8; 32]) -> bool {
    // Compare as little-endian 256-bit: walk from most-significant byte (index 31) down.
    for i in (0..32).rev() {
        if digest[i] < target_le[i] {
            return true;
        }
        if digest[i] > target_le[i] {
            return false;
        }
    }
    true
}

pub struct SearchHandle {
    stop: Arc<AtomicBool>,
    candidates: Arc<AtomicU64>,
    join: Option<thread::JoinHandle<()>>,
    started: Instant,
}

impl SearchHandle {
    pub fn start(cfg: RuntimeConfig, job: MiningJob) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let candidates = Arc::new(AtomicU64::new(0));
        let stop_c = Arc::clone(&stop);
        let cand_c = Arc::clone(&candidates);
        let intensity = cfg.intensity;

        let join = thread::spawn(move || {
            let mut nonce: u32 = 0;
            // Synthetic easy target if Electrum job not filled yet — for rate plumbing only.
            let target = parse_hex32(&job.target_le_hex).unwrap_or([0xff; 32]);
            while !stop_c.load(Ordering::Relaxed) {
                let batch = 1_000u32.saturating_mul(u32::from(intensity)).max(1);
                for _ in 0..batch {
                    let msg = photon_m1_message(nonce, &target);
                    let digest = hash256(&msg);
                    // Full PHOTON win path is not this M1 hash — keep scanning for rate.
                    let _ = meets_target_le(&digest, &target);
                    nonce = nonce.wrapping_add(1);
                }
                cand_c.fetch_add(u64::from(batch), Ordering::Relaxed);
                // Yield so intensity < 100 does not pin a core as hard.
                if intensity < 100 {
                    thread::sleep(Duration::from_millis(u64::from(100 - intensity).max(1)));
                }
            }
        });

        Self {
            stop,
            candidates,
            join: Some(join),
            started: Instant::now(),
        }
    }

    pub fn stop(mut self) -> SearchStats {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        let candidates = self.candidates.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        SearchStats {
            candidates,
            intensity: 0,
            elapsed_secs: elapsed as u64,
            rate: candidates as f64 / elapsed,
        }
    }

    pub fn snapshot(&self, intensity: u8) -> SearchStats {
        let candidates = self.candidates.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        SearchStats {
            candidates,
            intensity,
            elapsed_secs: elapsed as u64,
            rate: candidates as f64 / elapsed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash256_empty() {
        // HASH256("") = SHA256(SHA256(""))
        let d = hash256(b"");
        assert_eq!(
            hex::encode(d),
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
        );
    }

    #[test]
    fn parse_and_meet_max_target() {
        let t = parse_hex32(&"ff".repeat(32)).unwrap();
        let d = hash256(b"x");
        assert!(meets_target_le(&d, &t));
    }
}
