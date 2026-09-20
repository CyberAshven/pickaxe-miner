//! GPU candidate search (PERFORMANCE CONTRACT).
//! Production hot path = persistent CudaMiner. CPU hash256 is reference/tests only.

use crate::cuda_miner::{CudaMiner, Winner};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_BATCH: u32 = 65_536;

#[derive(Debug, Clone, Default)]
pub struct MiningJob {
    pub height: u32,
    pub target_le_hex: String,
    pub baton_txid: String,
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

/// Double SHA-256 (HASH256) Ã¢â‚¬â€ tests / rare winner verify only.
pub fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

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
    paused: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    join: Option<thread::JoinHandle<()>>,
    started: Instant,
    winner_rx: Receiver<Winner>,
}

impl SearchHandle {
    pub fn start(intensity: u8, job: MiningJob) -> Result<Self, String> {
        if !(10..=100).contains(&intensity) {
            return Err("intensity must be 10..=100".into());
        }
        let target = parse_hex32(&job.target_le_hex)?;
        // Prove GPU once at start (persistent miner).
        let mut probe = CudaMiner::new(0, 256)?;
        probe.set_target(&target)?;
        let _ = probe.mine_batch(0)?;

        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let intensity_a = Arc::new(AtomicU8::new(intensity));
        let candidates = Arc::new(AtomicU64::new(0));
        let winners = Arc::new(AtomicU64::new(0));
        let (winner_tx, winner_rx) = mpsc::channel::<Winner>();

        let stop_c = Arc::clone(&stop);
        let paused_c = Arc::clone(&paused);
        let intensity_c = Arc::clone(&intensity_a);
        let cand_c = Arc::clone(&candidates);
        let win_c = Arc::clone(&winners);

        let join = thread::spawn(move || {
            let mut miner = match CudaMiner::new(0, DEFAULT_BATCH) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("CUDA miner init failed: {e}");
                    return;
                }
            };
            if let Err(e) = miner.set_target(&target) {
                eprintln!("CUDA set_target: {e}");
                return;
            }
            let mut nonce: u32 = 0;
            while !stop_c.load(Ordering::Relaxed) {
                if paused_c.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(25));
                    continue;
                }
                let pct = intensity_c.load(Ordering::Relaxed).clamp(10, 100);
                // Intensity as GPU batch scale (scheduler), not CPU hash+sleep.
                let batch = ((DEFAULT_BATCH as u64) * (pct as u64) / 100).max(256) as u32;
                miner.set_batch(batch);

                let active = Instant::now();
                match miner.mine_batch(nonce) {
                    Ok((hashes, found)) => {
                        nonce = nonce.wrapping_add(hashes as u32);
                        cand_c.fetch_add(hashes, Ordering::Relaxed);
                        for w in found {
                            win_c.fetch_add(1, Ordering::Relaxed);
                            let _ = winner_tx.send(w);
                        }
                    }
                    Err(e) => {
                        eprintln!("CUDA mine error: {e}");
                        stop_c.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                // Mild cadence throttle only below 100% after GPU work (duty), not CPU hashing.
                if pct < 100 {
                    let work = active.elapsed();
                    let sleep = work.mul_f64((100 - pct) as f64 / pct as f64);
                    if !sleep.is_zero() {
                        thread::sleep(sleep);
                    }
                }
            }
        });

        Ok(Self {
            stop,
            paused,
            intensity: intensity_a,
            candidates,
            winners,
            join: Some(join),
            started: Instant::now(),
            winner_rx,
        })
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

    pub fn drain_winners(&self) -> Vec<Winner> {
        let mut out = Vec::new();
        while let Ok(w) = self.winner_rx.try_recv() {
            out.push(w);
        }
        out
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

    pub fn stop(mut self) -> SearchStats {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
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
    fn parse_and_meet_max_target() {
        let t = parse_hex32(&"ff".repeat(32)).unwrap();
        let d = hash256(b"x");
        assert!(meets_target_le(&d, &t));
    }

    #[test]
    fn gpu_live_intensity_and_pause_if_cuda_present() {
        let job = MiningJob {
            height: 1,
            target_le_hex: "ff".repeat(32),
            baton_txid: "00".repeat(32),
            generation_id: 1,
        };
        let Ok(handle) = SearchHandle::start(100, job) else {
            return;
        };
        std::thread::sleep(Duration::from_millis(200));
        assert!(handle.snapshot().candidates > 0);
        handle.apply_control(RuntimeCommand::SetIntensity(10)).unwrap();
        assert_eq!(handle.snapshot().intensity, 10);
        handle.apply_control(RuntimeCommand::Pause).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let paused = handle.snapshot().candidates;
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(handle.snapshot().candidates, paused);
        handle.apply_control(RuntimeCommand::Resume).unwrap();
        let _ = handle.stop();
    }
}
