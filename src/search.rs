//! GPU candidate search (PERFORMANCE CONTRACT).
//! Production hot path = persistent reference-correct CudaPhotonEngine.
//! CPU cryptography is limited to job setup and rare returned-winner verification.

use crate::cuda_photon::{CudaPhotonEngine, PhotonCudaWinner};
use crate::{crypto, tx};
use rand::Rng;
use secp256k1::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// SearchHandle now owns the exact persistent CUDA A -> B -> C candidate path.
/// Higher-level product mining remains gated until submission/TUI and the
/// remaining product backends are complete.
pub const REFERENCE_GPU_PIPELINE_READY: bool = true;

const MAX_BATCH_CANDIDATES: u32 = 65_536;
const WINNER_BUFFER_CAP: u32 = 8;
const WINNER_CHANNEL_CAP: usize = 8;
const JOB_UPDATE_CHANNEL_CAP: usize = 2;
const PAUSE_POLL: Duration = Duration::from_millis(25);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedWinner {
    pub generation_id: u64,
    pub height: u32,
    pub baton_txid: String,
    pub baton_vout: u32,
    pub nonce: u32,
    pub digest: [u8; 32],
    pub public_key: [u8; 33],
    pub signature: [u8; 64],
    pub transaction: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub candidates: u64,
    pub batches: u64,
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

struct PreparedJob {
    job: MiningJob,
    template: [u8; 615],
    target: [u8; 32],
}

enum WorkerCommand {
    ReplaceJob {
        job: MiningJob,
        reply: SyncSender<Result<(), String>>,
    },
}

fn validate_job(job: &MiningJob) -> Result<[u8; 32], String> {
    if job.generation_id == 0 {
        return Err("mining job generation_id must be nonzero".into());
    }
    if job.age > 16 {
        return Err(format!(
            "live baton age {} exceeds the fixed 615-byte PHOTON layout (0..=16)",
            job.age
        ));
    }
    if job.payout_address.trim().is_empty() {
        return Err("mining payout address is required".into());
    }
    parse_hex32(&job.target_le_hex)
}

fn prepare_job(
    job: MiningJob,
    sk: &[u8; 32],
    public_key: &[u8; 33],
) -> Result<PreparedJob, String> {
    let target = validate_job(&job)?;
    let payout_locking = tx::cashaddr_to_p2pkh_locking(&job.payout_address)?;
    let message = tx::photon_message_sha256(0, &job.target_le_hex)?;
    let signature = crypto::bch_schnorr_sign(sk, &message)?;
    if !crypto::bch_schnorr_verify(public_key, &message, &signature)? {
        return Err("generated PHOTON setup signature failed verification".into());
    }
    let params = tx::TemplateParams {
        prev_tx_hash_hex: job.baton_txid.clone(),
        prev_index: job.baton_vout,
        age: job.age,
        public_key_hex: hex::encode(public_key),
        target_hex: job.target_le_hex.clone(),
        signature_hex: hex::encode(signature),
        nonce: 0,
        contract_value_sats: job.baton_value_sats,
        contract_token_amount: job.token_amount,
        reward_amount: job.reward_raw,
        payout_locking,
    };
    let bytes = tx::build_photon_template_bytes(&params)?;
    let template: [u8; 615] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "PHOTON live template is {} bytes; CUDA requires 615",
            bytes.len()
        )
    })?;
    if template[394..426] != target[..] {
        return Err("PHOTON template target bytes do not match live target".into());
    }
    Ok(PreparedJob {
        job,
        template,
        target,
    })
}

fn verify_gpu_winner(
    prepared: &PreparedJob,
    sk: &[u8; 32],
    public_key: &[u8; 33],
    winner: &PhotonCudaWinner,
) -> Result<VerifiedWinner, String> {
    let message = tx::photon_message_sha256(winner.nonce, &prepared.job.target_le_hex)?;
    let signature = crypto::bch_schnorr_sign(sk, &message)?;
    if !crypto::bch_schnorr_verify(public_key, &message, &signature)? {
        return Err("returned GPU winner failed BCH Schnorr verification".into());
    }
    let context = tx::ReferenceJobContext {
        prev_txid: prepared.job.baton_txid.clone(),
        prev_vout: prepared.job.baton_vout,
        age: prepared.job.age,
        target_le_hex: prepared.job.target_le_hex.clone(),
        contract_value_sats: prepared.job.baton_value_sats,
        contract_token_amount: prepared.job.token_amount,
        reward_raw: prepared.job.reward_raw,
    };
    let transaction = tx::apply_reference_signature(
        &context,
        &prepared.job.payout_address,
        &hex::encode(public_key),
        winner.nonce,
        &hex::encode(signature),
    )?;
    let digest = hash256(&transaction);
    if digest != winner.digest {
        return Err(format!(
            "GPU winner HASH256 mismatch: gpu={} host={}",
            hex::encode(winner.digest),
            hex::encode(digest)
        ));
    }
    if !meets_target_le(&digest, &prepared.target) {
        return Err("returned GPU winner failed strict host hash < target verification".into());
    }
    Ok(VerifiedWinner {
        generation_id: prepared.job.generation_id,
        height: prepared.job.height,
        baton_txid: prepared.job.baton_txid.clone(),
        baton_vout: prepared.job.baton_vout,
        nonce: winner.nonce,
        digest,
        public_key: *public_key,
        signature,
        transaction,
    })
}

fn batch_candidates(intensity: u8) -> u32 {
    ((u64::from(MAX_BATCH_CANDIDATES) * u64::from(intensity)) / 100)
        .clamp(1, u64::from(MAX_BATCH_CANDIDATES)) as u32
}

fn duty_rest(compute_time: Duration, intensity: u8) -> Duration {
    if intensity >= 100 || compute_time.is_zero() {
        return Duration::ZERO;
    }
    let rest_ns = compute_time
        .as_nanos()
        .saturating_mul(u128::from(100 - intensity))
        / u128::from(intensity);
    Duration::from_nanos(rest_ns.min(u128::from(u64::MAX)) as u64)
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    mut engine: CudaPhotonEngine,
    mut prepared: PreparedJob,
    sk: [u8; 32],
    public_key: [u8; 33],
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    batches: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    generation_id: Arc<AtomicU64>,
    refresh_required: Option<Arc<AtomicBool>>,
    job_rx: Receiver<WorkerCommand>,
    winner_tx: SyncSender<VerifiedWinner>,
) {
    let mut rng = rand::rng();
    let mut nonce_base = rng.random::<u32>();
    while !stop.load(Ordering::Relaxed) {
        loop {
            match job_rx.try_recv() {
                Ok(WorkerCommand::ReplaceJob { job, reply }) => {
                    let result = prepare_job(job, &sk, &public_key).and_then(|next| {
                        engine.set_job(&next.template, &next.target, &sk)?;
                        generation_id.store(next.job.generation_id, Ordering::Release);
                        prepared = next;
                        nonce_base = rng.random::<u32>();
                        Ok(())
                    });
                    let _ = reply.send(result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }
        if paused.load(Ordering::Relaxed) {
            thread::park_timeout(PAUSE_POLL);
            continue;
        }

        let active_intensity = intensity.load(Ordering::Relaxed).clamp(10, 100);
        let batch_size = batch_candidates(active_intensity);
        let batch_started = Instant::now();
        let result = match engine.search_batch(nonce_base, batch_size) {
            Ok(result) => result,
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                break;
            }
        };
        let compute_time = batch_started.elapsed();
        candidates.fetch_add(u64::from(result.candidates), Ordering::Relaxed);
        batches.fetch_add(1, Ordering::Release);

        for gpu_winner in &result.winners {
            let verified = match verify_gpu_winner(&prepared, &sk, &public_key, gpu_winner) {
                Ok(verified) => verified,
                Err(_) => continue,
            };
            winners.fetch_add(1, Ordering::Relaxed);
            match winner_tx.try_send(verified) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        nonce_base = nonce_base.wrapping_add(batch_size);

        // The authoritative M22 reference rechecks BCH height + PHOTON baton
        // state after every 65,536-candidate batch before the next batch may
        // begin. In supervised product mode, hold this exact batch boundary
        // until the runtime supervisor has refreshed Fulcrum state, applied
        // any immutable generation change through ReplaceJob, and explicitly
        // released the gate. The worker still services ReplaceJob while gated,
        // so a state change cannot deadlock re-jobbing.
        if let Some(required) = refresh_required.as_ref() {
            required.store(true, Ordering::Release);
            while required.load(Ordering::Acquire) && !stop.load(Ordering::Relaxed) {
                match job_rx.try_recv() {
                    Ok(WorkerCommand::ReplaceJob { job, reply }) => {
                        let result = prepare_job(job, &sk, &public_key).and_then(|next| {
                            engine.set_job(&next.template, &next.target, &sk)?;
                            generation_id.store(next.job.generation_id, Ordering::Release);
                            prepared = next;
                            nonce_base = rng.random::<u32>();
                            Ok(())
                        });
                        let _ = reply.send(result);
                    }
                    Err(TryRecvError::Empty) => thread::park_timeout(PAUSE_POLL),
                    Err(TryRecvError::Disconnected) => {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }

        let rest = duty_rest(compute_time, active_intensity);
        if !rest.is_zero() {
            thread::park_timeout(rest);
        }
    }
}

pub struct SearchHandle {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    batches: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    generation_id: Arc<AtomicU64>,
    refresh_required: Option<Arc<AtomicBool>>,
    job_tx: SyncSender<WorkerCommand>,
    winner_rx: Receiver<VerifiedWinner>,
    worker: Option<JoinHandle<()>>,
    started: Instant,
}

impl SearchHandle {
    pub fn start(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_inner(intensity, job, false, false)
    }

    /// Start the exact CUDA search with an authoritative live-state gate after
    /// every GPU batch. The caller must refresh Fulcrum state and call
    /// `complete_refresh` before the next batch can begin.
    pub fn start_supervised(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_inner(intensity, job, true, false)
    }

    /// Start supervised search in a paused state. This is used while a
    /// crash-recovered winner submission is pending, so no GPU batch can begin
    /// before the durable parent/child pair is resolved.
    pub fn start_supervised_paused(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_inner(intensity, job, true, true)
    }

    fn start_inner(
        intensity: u8,
        job: MiningJob,
        supervised: bool,
        initially_paused: bool,
    ) -> Result<Self, String> {
        if !(10..=100).contains(&intensity) {
            return Err("intensity must be 10..=100".into());
        }
        let secret = SecretKey::new(&mut rand::rng());
        let sk = secret.to_secret_bytes();
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let prepared = prepare_job(job, &sk, &public_key)?;

        // Fail fast and create exactly one CUDA context. The configured engine
        // is moved into the worker and remains resident across runtime controls.
        let mut engine = CudaPhotonEngine::new(0, MAX_BATCH_CANDIDATES, WINNER_BUFFER_CAP)?;
        engine.set_job(&prepared.template, &prepared.target, &sk)?;

        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(initially_paused));
        let intensity_state = Arc::new(AtomicU8::new(intensity));
        let candidates = Arc::new(AtomicU64::new(0));
        let batches = Arc::new(AtomicU64::new(0));
        let winners = Arc::new(AtomicU64::new(0));
        let generation_id = Arc::new(AtomicU64::new(prepared.job.generation_id));
        let refresh_required = supervised.then(|| Arc::new(AtomicBool::new(false)));
        let (job_tx, job_rx) = mpsc::sync_channel(JOB_UPDATE_CHANNEL_CAP);
        let (winner_tx, winner_rx) = mpsc::sync_channel(WINNER_CHANNEL_CAP);

        let worker = thread::Builder::new()
            .name("pickaxe-photon-cuda".into())
            .spawn({
                let worker_stop = Arc::clone(&stop);
                let worker_paused = Arc::clone(&paused);
                let worker_intensity = Arc::clone(&intensity_state);
                let worker_candidates = Arc::clone(&candidates);
                let worker_batches = Arc::clone(&batches);
                let worker_winners = Arc::clone(&winners);
                let worker_generation = Arc::clone(&generation_id);
                let worker_refresh_required = refresh_required.as_ref().map(Arc::clone);
                move || {
                    run_worker(
                        engine,
                        prepared,
                        sk,
                        public_key,
                        worker_stop,
                        worker_paused,
                        worker_intensity,
                        worker_candidates,
                        worker_batches,
                        worker_winners,
                        worker_generation,
                        worker_refresh_required,
                        job_rx,
                        winner_tx,
                    )
                }
            })
            .map_err(|error| format!("start PHOTON CUDA worker: {error}"))?;

        Ok(Self {
            stop,
            paused,
            intensity: intensity_state,
            candidates,
            batches,
            winners,
            generation_id,
            refresh_required,
            job_tx,
            winner_rx,
            worker: Some(worker),
            started: Instant::now(),
        })
    }

    pub fn replace_job(&self, job: MiningJob) -> Result<(), String> {
        validate_job(&job)?;
        if job.generation_id == self.generation_id() {
            return Ok(());
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.job_tx
            .send(WorkerCommand::ReplaceJob {
                job,
                reply: reply_tx,
            })
            .map_err(|_| "PHOTON CUDA worker is not running".to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "timed out applying PHOTON job generation".to_string())?
    }

    pub fn generation_id(&self) -> u64 {
        self.generation_id.load(Ordering::Acquire)
    }

    pub fn drain_winners(&self) -> Vec<VerifiedWinner> {
        self.winner_rx.try_iter().collect()
    }

    pub fn refresh_required(&self) -> bool {
        self.refresh_required
            .as_ref()
            .is_some_and(|required| required.load(Ordering::Acquire))
    }

    pub fn complete_refresh(&self) -> Result<(), String> {
        let required = self
            .refresh_required
            .as_ref()
            .ok_or("search was not started in supervised refresh mode")?;
        required.store(false, Ordering::Release);
        if let Some(worker) = self.worker.as_ref() {
            worker.thread().unpark();
        }
        Ok(())
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

    pub fn stop(mut self) -> SearchStats {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(required) = self.refresh_required.as_ref() {
            required.store(false, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        self.snapshot_final()
    }

    fn snapshot_final(&self) -> SearchStats {
        let candidates = self.candidates.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        SearchStats {
            candidates,
            batches: self.batches.load(Ordering::Acquire),
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

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(required) = self.refresh_required.as_ref() {
            required.store(false, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
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
    fn intensity_scales_batch_and_duty_without_recreating_gpu_state() {
        assert_eq!(batch_candidates(100), MAX_BATCH_CANDIDATES);
        assert_eq!(batch_candidates(50), MAX_BATCH_CANDIDATES / 2);
        assert_eq!(batch_candidates(10), MAX_BATCH_CANDIDATES / 10);
        assert_eq!(duty_rest(Duration::from_millis(10), 100), Duration::ZERO);
        assert_eq!(
            duty_rest(Duration::from_millis(10), 50),
            Duration::from_millis(10)
        );
        assert_eq!(
            duty_rest(Duration::from_millis(10), 25),
            Duration::from_millis(30)
        );
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

    #[test]
    fn prepared_job_uses_identity_and_exact_615_byte_layout() {
        let sk = [1u8; 32];
        let secret = SecretKey::from_secret_bytes(sk).unwrap();
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let job = MiningJob {
            height: 1,
            baton_txid: "42a02ec4f58b50f23df4591dcc999ca1bcae2f378997fe6547ae124712000000".into(),
            baton_vout: 0,
            baton_value_sats: 15_971_500,
            age: 10,
            target_le_hex: "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                .into(),
            token_amount: 2_099_905_002_035_715,
            reward_raw: 4_999_773_813,
            payout_address: "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh".into(),
            generation_id: 1,
            ..MiningJob::default()
        };
        let prepared = prepare_job(job, &sk, &public_key).unwrap();
        assert_eq!(prepared.template.len(), 615);
        assert_eq!(&prepared.template[390..394], &[0u8; 4]);
        assert_eq!(&prepared.template[394..426], &prepared.target);
        assert_eq!(&prepared.template[45..78], &public_key);
    }

    fn integration_job(generation_id: u64) -> MiningJob {
        MiningJob {
            height: 1_000,
            baton_txid: "42a02ec4f58b50f23df4591dcc999ca1bcae2f378997fe6547ae124712000000".into(),
            baton_vout: 0,
            baton_height: 990,
            baton_value_sats: 15_971_500,
            age: 10,
            target_le_hex: "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                .into(),
            token_amount: 2_099_905_002_035_715,
            reward_raw: 4_999_773_813,
            payout_address: "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh".into(),
            source_identity: "test".into(),
            generation_id,
        }
    }

    #[test]
    fn gpu_search_handle_replaces_generation_and_keeps_runtime_controls_if_cuda_present() {
        let handle = match SearchHandle::start(10, integration_job(1)) {
            Ok(handle) => handle,
            Err(error)
                if error.to_ascii_lowercase().contains("cuda context")
                    || error.to_ascii_lowercase().contains("missing cuda ptx")
                    || error.to_ascii_lowercase().contains("no device")
                    || error.to_ascii_lowercase().contains("not initialized") =>
            {
                eprintln!("skip integrated PHOTON CUDA SearchHandle test: {error}");
                return;
            }
            Err(error) => panic!("integrated PHOTON CUDA start failed: {error}"),
        };

        let deadline = Instant::now() + Duration::from_secs(20);
        while handle.snapshot().candidates == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            handle.snapshot().candidates > 0,
            "GPU worker produced no candidates"
        );
        assert_eq!(handle.generation_id(), 1);

        handle.replace_job(integration_job(2)).unwrap();
        assert_eq!(handle.generation_id(), 2);
        handle.set_intensity(25).unwrap();
        assert_eq!(handle.snapshot().intensity, 25);
        assert!(handle.toggle_pause());
        assert_eq!(handle.snapshot().state, MiningState::Paused);
        assert!(!handle.toggle_pause());
        assert_eq!(handle.snapshot().state, MiningState::Mining);

        let stats = handle.stop();
        eprintln!(
            "integrated PHOTON CUDA SearchHandle: candidates={} elapsed={}s rate={:.2} candidates/s",
            stats.candidates, stats.elapsed_secs, stats.rate
        );
        assert!(stats.candidates > 0);
    }

    #[test]
    fn supervised_search_holds_exact_batch_boundary_until_refresh_if_cuda_present() {
        let handle = match SearchHandle::start_supervised(100, integration_job(1)) {
            Ok(handle) => handle,
            Err(error)
                if error.to_ascii_lowercase().contains("cuda context")
                    || error.to_ascii_lowercase().contains("missing cuda ptx")
                    || error.to_ascii_lowercase().contains("no device")
                    || error.to_ascii_lowercase().contains("not initialized") =>
            {
                eprintln!("skip supervised PHOTON CUDA refresh-gate test: {error}");
                return;
            }
            Err(error) => panic!("supervised PHOTON CUDA start failed: {error}"),
        };

        let deadline = Instant::now() + Duration::from_secs(20);
        while !handle.refresh_required() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            handle.refresh_required(),
            "GPU worker never reached the supervised refresh boundary"
        );

        let gated = handle.snapshot();
        assert_eq!(gated.batches, 1);
        thread::sleep(Duration::from_millis(150));
        let still_gated = handle.snapshot();
        assert_eq!(still_gated.batches, gated.batches);
        assert_eq!(still_gated.candidates, gated.candidates);

        handle.complete_refresh().unwrap();
        let second_deadline = Instant::now() + Duration::from_secs(20);
        while handle.snapshot().batches == gated.batches && Instant::now() < second_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(handle.snapshot().batches > gated.batches);
        let _ = handle.stop();
    }
}
