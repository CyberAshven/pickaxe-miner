//! GPU candidate search (PERFORMANCE CONTRACT).
//! Production hot path = persistent reference-correct native GPU engine.
//! CPU cryptography is limited to job setup and rare returned-winner verification.

use crate::backend::BackendKind;
use crate::cuda_photon::{CudaPhotonEngine, PhotonCudaBatchResult, PhotonCudaWinner};
use crate::hip_photon::HipPhotonEngine;
use crate::wgpu_photon::WgpuPhotonEngine;
use crate::{crypto, tx};
use rand::Rng;
use secp256k1::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// SearchHandle owns an exact persistent native-GPU A -> B -> C candidate path.
/// Higher-level product mining remains gated until submission/TUI and the
/// remaining product backends are complete.
pub const REFERENCE_GPU_PIPELINE_READY: bool = true;

pub(crate) const MAX_BATCH_CANDIDATES: u32 = 65_536;
pub(crate) const WINNER_BUFFER_CAP: u32 = 8;
const WINNER_CHANNEL_CAP: usize = WINNER_BUFFER_CAP as usize;
const JOB_UPDATE_CHANNEL_CAP: usize = 2;
const PAUSE_POLL: Duration = Duration::from_millis(25);

pub(crate) enum PhotonEngine {
    Cuda(Box<CudaPhotonEngine>),
    Hip(Box<HipPhotonEngine>),
    Wgpu(Box<WgpuPhotonEngine>),
}

impl PhotonEngine {
    pub(crate) fn new(
        backend: BackendKind,
        device_ordinal: usize,
        max_batch_candidates: u32,
        winner_buffer_cap: u32,
    ) -> Result<Self, String> {
        match backend {
            BackendKind::Cuda => Ok(Self::Cuda(Box::new(CudaPhotonEngine::new(
                device_ordinal,
                max_batch_candidates,
                winner_buffer_cap,
            )?))),
            BackendKind::Hip => Ok(Self::Hip(Box::new(HipPhotonEngine::new(
                device_ordinal,
                max_batch_candidates,
                winner_buffer_cap,
            )?))),
            BackendKind::Wgpu => Ok(Self::Wgpu(Box::new(WgpuPhotonEngine::new(
                device_ordinal,
                max_batch_candidates,
                winner_buffer_cap,
            )?))),
            BackendKind::Auto => {
                Err("auto backend must be resolved before GPU engine initialization".into())
            }
        }
    }

    pub(crate) fn set_job(
        &mut self,
        template: &[u8; 615],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        match self {
            Self::Cuda(engine) => engine.set_job(template, target, private_key),
            Self::Hip(engine) => engine.set_job(template, target, private_key),
            Self::Wgpu(engine) => engine.set_job(template, target, private_key),
        }
    }

    pub(crate) fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        match self {
            Self::Cuda(engine) => engine.search_batch(nonce_base, candidate_count),
            Self::Hip(engine) => engine.search_batch(nonce_base, candidate_count),
            Self::Wgpu(engine) => engine.search_batch(nonce_base, candidate_count),
        }
    }

    pub(crate) fn persistent_device_bytes(&self) -> usize {
        match self {
            Self::Cuda(engine) => engine.persistent_device_bytes(),
            Self::Hip(engine) => engine.persistent_device_bytes(),
            Self::Wgpu(engine) => engine.persistent_device_bytes(),
        }
    }

    pub(crate) fn table_source(&self) -> String {
        match self {
            Self::Cuda(engine) => format!("{:?}", engine.table_source()),
            Self::Hip(engine) => format!("{:?}", engine.table_source()),
            Self::Wgpu(engine) => format!("{:?}", engine.table_source()),
        }
    }

    pub(crate) fn scheduled_batch_candidates(&self) -> u32 {
        match self {
            Self::Cuda(_) | Self::Hip(_) => scheduled_batch_candidates(),
            Self::Wgpu(engine) => engine.recommended_batch_candidates(),
        }
    }
}

pub(crate) const fn production_max_batch_candidates(backend: BackendKind) -> u32 {
    match backend {
        BackendKind::Wgpu => crate::wgpu_photon::WGPU_REFERENCE_MAX_BATCH,
        BackendKind::Auto | BackendKind::Cuda | BackendKind::Hip => MAX_BATCH_CANDIDATES,
    }
}

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
    pub current_rate: f64,
    pub peak_rate: f64,
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

/// Keep production GPU launches at the tuned full batch size.
///
/// Runtime intensity is applied exactly once by [`duty_rest`]. Shrinking the
/// batch as well would square the requested throttle (for example, 30% work
/// followed by a 30% duty cycle yields roughly 9% throughput) and increases
/// kernel-launch overhead.
pub(crate) const fn scheduled_batch_candidates() -> u32 {
    MAX_BATCH_CANDIDATES
}

pub(crate) fn duty_rest(compute_time: Duration, intensity: u8) -> Duration {
    if intensity >= 100 || compute_time.is_zero() {
        return Duration::ZERO;
    }
    let rest_ns = compute_time
        .as_nanos()
        .saturating_mul(u128::from(100 - intensity))
        / u128::from(intensity);
    Duration::from_nanos(rest_ns.min(u128::from(u64::MAX)) as u64)
}

fn throttle_sleep(rest: Duration) {
    // ponytail: keep the scheduler responsive at reduced intensity. A long
    // sleep makes the GPU look disconnected to telemetry even though the
    // requested duty cycle is active.
    let slice = Duration::from_micros(250);
    let mut remaining = rest;
    while remaining > Duration::ZERO {
        let current = remaining.min(slice);
        thread::park_timeout(current);
        remaining = remaining.saturating_sub(current);
    }
}

fn deliver_verified_batch(
    verified_batch: Vec<VerifiedWinner>,
    paused: &AtomicBool,
    winners: &AtomicU64,
    winner_tx: &SyncSender<VerifiedWinner>,
) -> bool {
    if verified_batch.is_empty() {
        return true;
    }

    paused.store(true, Ordering::SeqCst);
    for verified in verified_batch {
        if winner_tx.send(verified).is_err() {
            return false;
        }
        winners.fetch_add(1, Ordering::Release);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    mut engine: PhotonEngine,
    mut prepared: PreparedJob,
    sk: [u8; 32],
    public_key: [u8; 33],
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    batch_in_flight: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    batches: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    generation_id: Arc<AtomicU64>,
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
        batch_in_flight.store(true, Ordering::SeqCst);
        if paused.load(Ordering::SeqCst) {
            batch_in_flight.store(false, Ordering::SeqCst);
            thread::park_timeout(PAUSE_POLL);
            continue;
        }

        let active_intensity = intensity.load(Ordering::Relaxed).clamp(10, 100);
        let batch_size = engine.scheduled_batch_candidates();
        let batch_started = Instant::now();
        let result = match engine.search_batch(nonce_base, batch_size) {
            Ok(result) => result,
            Err(_) => {
                batch_in_flight.store(false, Ordering::SeqCst);
                stop.store(true, Ordering::Relaxed);
                break;
            }
        };
        let compute_time = batch_started.elapsed();
        candidates.fetch_add(u64::from(result.candidates), Ordering::Relaxed);
        batches.fetch_add(1, Ordering::Release);

        let mut verified_batch = Vec::with_capacity(result.winners.len());
        for gpu_winner in &result.winners {
            let verified = match verify_gpu_winner(&prepared, &sk, &public_key, gpu_winner) {
                Ok(verified) => verified,
                Err(_) => continue,
            };
            verified_batch.push(verified);
        }
        if !deliver_verified_batch(verified_batch, &paused, &winners, &winner_tx) {
            stop.store(true, Ordering::Relaxed);
        }
        batch_in_flight.store(false, Ordering::SeqCst);

        nonce_base = nonce_base.wrapping_add(batch_size);

        let rest = duty_rest(compute_time, active_intensity);
        if !rest.is_zero() {
            throttle_sleep(rest);
        }
    }
}

pub struct SearchHandle {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    batch_in_flight: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    batches: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    generation_id: Arc<AtomicU64>,
    job_tx: SyncSender<WorkerCommand>,
    winner_rx: Receiver<VerifiedWinner>,
    worker: Option<JoinHandle<()>>,
    started: Instant,
}

#[derive(Clone)]
pub(crate) struct SearchPauseHandle {
    paused: Arc<AtomicBool>,
}

impl SearchPauseHandle {
    pub(crate) fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn from_shared(paused: Arc<AtomicBool>) -> Self {
        Self { paused }
    }
}

impl SearchHandle {
    pub fn start(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_on_device(0, intensity, job)
    }

    pub fn start_on_device(
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_on_backend_device(BackendKind::Cuda, device_ordinal, intensity, job)
    }

    pub fn start_on_backend_device(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_inner(backend, device_ordinal, intensity, job, false)
    }

    /// Start exact GPU search under the live runtime supervisor. The GPU keeps
    /// launching batches back-to-back while the immutable generation is valid;
    /// the supervisor polls PHOTON state independently and applies `ReplaceJob`
    /// between batches when that generation changes.
    pub fn start_supervised(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_supervised_on_device(0, intensity, job)
    }

    pub fn start_supervised_on_device(
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_supervised_on_backend_device(BackendKind::Cuda, device_ordinal, intensity, job)
    }

    pub fn start_supervised_on_backend_device(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_inner(backend, device_ordinal, intensity, job, false)
    }

    /// Start supervised search in a paused state. This is used while a
    /// crash-recovered winner submission is pending, so no GPU batch can begin
    /// before the durable parent/child pair is resolved.
    pub fn start_supervised_paused(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_supervised_paused_on_device(0, intensity, job)
    }

    pub fn start_supervised_paused_on_device(
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_supervised_paused_on_backend_device(
            BackendKind::Cuda,
            device_ordinal,
            intensity,
            job,
        )
    }

    pub fn start_supervised_paused_on_backend_device(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_inner(backend, device_ordinal, intensity, job, true)
    }

    fn start_inner(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
        initially_paused: bool,
    ) -> Result<Self, String> {
        if !(10..=100).contains(&intensity) {
            return Err("intensity must be 10..=100".into());
        }
        let secret = SecretKey::new(&mut rand::rng());
        let sk = secret.to_secret_bytes();
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let prepared = prepare_job(job, &sk, &public_key)?;

        // Fail fast and create exactly one native GPU context. The configured
        // engine is moved into the worker and remains resident across controls.
        let mut engine = PhotonEngine::new(
            backend,
            device_ordinal,
            production_max_batch_candidates(backend),
            WINNER_BUFFER_CAP,
        )?;
        engine.set_job(&prepared.template, &prepared.target, &sk)?;

        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(initially_paused));
        let batch_in_flight = Arc::new(AtomicBool::new(false));
        let intensity_state = Arc::new(AtomicU8::new(intensity));
        let candidates = Arc::new(AtomicU64::new(0));
        let batches = Arc::new(AtomicU64::new(0));
        let winners = Arc::new(AtomicU64::new(0));
        let generation_id = Arc::new(AtomicU64::new(prepared.job.generation_id));
        let (job_tx, job_rx) = mpsc::sync_channel(JOB_UPDATE_CHANNEL_CAP);
        let (winner_tx, winner_rx) = mpsc::sync_channel(WINNER_CHANNEL_CAP);

        let worker = thread::Builder::new()
            .name(format!(
                "pickaxe-photon-{}-{device_ordinal}",
                backend.as_str()
            ))
            .spawn({
                let worker_stop = Arc::clone(&stop);
                let worker_paused = Arc::clone(&paused);
                let worker_batch_in_flight = Arc::clone(&batch_in_flight);
                let worker_intensity = Arc::clone(&intensity_state);
                let worker_candidates = Arc::clone(&candidates);
                let worker_batches = Arc::clone(&batches);
                let worker_winners = Arc::clone(&winners);
                let worker_generation = Arc::clone(&generation_id);
                move || {
                    run_worker(
                        engine,
                        prepared,
                        sk,
                        public_key,
                        worker_stop,
                        worker_paused,
                        worker_batch_in_flight,
                        worker_intensity,
                        worker_candidates,
                        worker_batches,
                        worker_winners,
                        worker_generation,
                        job_rx,
                        winner_tx,
                    )
                }
            })
            .map_err(|error| format!("start PHOTON CUDA worker: {error}"))?;

        Ok(Self {
            stop,
            paused,
            batch_in_flight,
            intensity: intensity_state,
            candidates,
            batches,
            winners,
            generation_id,
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
        if let Some(worker) = self.worker.as_ref() {
            worker.thread().unpark();
        }
        // A production GPU batch is allowed to finish before a replacement is
        // applied. On slower adapters or a large reference batch, the kernel
        // can legitimately exceed the old fixed 30 second supervisor window.
        // Keep the bounded failure path, but give the worker enough time to
        // reach the command boundary without falsely reporting a dead worker.
        reply_rx
            .recv_timeout(Duration::from_secs(120))
            .map_err(|_| "timed out applying PHOTON job generation".to_string())?
    }

    pub fn generation_id(&self) -> u64 {
        self.generation_id.load(Ordering::Acquire)
    }

    pub fn drain_winners(&self) -> Vec<VerifiedWinner> {
        self.winner_rx.try_iter().collect()
    }

    pub fn batch_in_flight(&self) -> bool {
        self.batch_in_flight.load(Ordering::SeqCst)
    }

    pub(crate) fn pause_handle(&self) -> SearchPauseHandle {
        SearchPauseHandle {
            paused: Arc::clone(&self.paused),
        }
    }

    pub fn apply_control(&self, command: RuntimeCommand) -> Result<SearchStats, String> {
        match command {
            RuntimeCommand::SetIntensity(value) => {
                if !(10..=100).contains(&value) {
                    return Err("intensity must be 10..=100".into());
                }
                self.intensity.store(value, Ordering::Relaxed);
            }
            RuntimeCommand::Pause => self.paused.store(true, Ordering::SeqCst),
            RuntimeCommand::Resume => self.paused.store(false, Ordering::SeqCst),
        }
        if let Some(worker) = self.worker.as_ref() {
            worker.thread().unpark();
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
            current_rate: 0.0,
            peak_rate: 0.0,
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
    fn intensity_uses_one_duty_cycle_throttle_with_full_gpu_batches() {
        assert_eq!(scheduled_batch_candidates(), MAX_BATCH_CANDIDATES);
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
    fn production_batch_envelope_keeps_native_limit_and_reference_wgpu_limit() {
        assert_eq!(production_max_batch_candidates(BackendKind::Cuda), 65_536);
        assert_eq!(production_max_batch_candidates(BackendKind::Hip), 65_536);
        assert_eq!(
            production_max_batch_candidates(BackendKind::Wgpu),
            crate::wgpu_photon::WGPU_REFERENCE_MAX_BATCH
        );
    }

    #[test]
    fn verified_winning_batch_pauses_and_fits_bounded_delivery_queue() {
        assert!(WINNER_CHANNEL_CAP >= WINNER_BUFFER_CAP as usize);

        let paused = AtomicBool::new(false);
        let winners = AtomicU64::new(0);
        let (winner_tx, winner_rx) = mpsc::sync_channel(WINNER_CHANNEL_CAP);
        let batch = (0..WINNER_BUFFER_CAP)
            .map(|nonce| VerifiedWinner {
                generation_id: 1,
                height: 1_000,
                baton_txid: "11".repeat(32),
                baton_vout: 0,
                nonce,
                digest: [0u8; 32],
                public_key: [2u8; 33],
                signature: [0u8; 64],
                transaction: vec![0x02],
            })
            .collect();

        assert!(deliver_verified_batch(batch, &paused, &winners, &winner_tx));
        assert!(paused.load(Ordering::SeqCst));
        assert_eq!(
            winners.load(Ordering::Acquire),
            u64::from(WINNER_BUFFER_CAP)
        );
        assert_eq!(winner_rx.try_iter().count(), WINNER_BUFFER_CAP as usize);
    }

    #[test]
    fn runtime_intensity_change_wakes_parked_worker() {
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let started = Instant::now();
            thread::park_timeout(Duration::from_secs(5));
            let _ = wake_tx.send(started.elapsed());
        });

        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let (_winner_tx, winner_rx) = mpsc::sync_channel(1);
        let handle = SearchHandle {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            batch_in_flight: Arc::new(AtomicBool::new(false)),
            intensity: Arc::new(AtomicU8::new(10)),
            candidates: Arc::new(AtomicU64::new(0)),
            batches: Arc::new(AtomicU64::new(0)),
            winners: Arc::new(AtomicU64::new(0)),
            generation_id: Arc::new(AtomicU64::new(1)),
            job_tx,
            winner_rx,
            worker: Some(worker),
            started: Instant::now(),
        };

        handle
            .apply_control(RuntimeCommand::SetIntensity(100))
            .unwrap();
        let elapsed = wake_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("intensity control must wake a worker parked for duty throttling");
        assert!(
            elapsed < Duration::from_secs(1),
            "worker remained parked after live intensity change: {elapsed:?}"
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
    fn generation_replacement_wakes_parked_worker() {
        let (job_tx, job_rx) = mpsc::sync_channel(1);
        let (_winner_tx, winner_rx) = mpsc::sync_channel(1);
        let generation_id = Arc::new(AtomicU64::new(1));
        let worker_generation = Arc::clone(&generation_id);
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let started = Instant::now();
            thread::park_timeout(Duration::from_secs(5));
            let command = job_rx
                .recv_timeout(Duration::from_millis(500))
                .expect("replacement command must be available after wake");
            match command {
                WorkerCommand::ReplaceJob { job, reply } => {
                    worker_generation.store(job.generation_id, Ordering::Release);
                    let _ = reply.send(Ok(()));
                }
            }
            let _ = wake_tx.send(started.elapsed());
        });

        let handle = SearchHandle {
            stop: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            batch_in_flight: Arc::new(AtomicBool::new(false)),
            intensity: Arc::new(AtomicU8::new(10)),
            candidates: Arc::new(AtomicU64::new(0)),
            batches: Arc::new(AtomicU64::new(0)),
            winners: Arc::new(AtomicU64::new(0)),
            generation_id,
            job_tx,
            winner_rx,
            worker: Some(worker),
            started: Instant::now(),
        };

        handle.replace_job(integration_job(2)).unwrap();
        let elapsed = wake_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("generation replacement must wake a duty-resting worker");
        assert!(
            elapsed < Duration::from_secs(1),
            "worker remained parked after generation replacement: {elapsed:?}"
        );
        assert_eq!(handle.generation_id(), 2);
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
    fn supervised_search_runs_full_back_to_back_batches_if_cuda_present() {
        let handle = match SearchHandle::start_supervised(30, integration_job(1)) {
            Ok(handle) => handle,
            Err(error)
                if error.to_ascii_lowercase().contains("cuda context")
                    || error.to_ascii_lowercase().contains("missing cuda ptx")
                    || error.to_ascii_lowercase().contains("no device")
                    || error.to_ascii_lowercase().contains("not initialized") =>
            {
                eprintln!("skip supervised PHOTON CUDA continuous-batch test: {error}");
                return;
            }
            Err(error) => panic!("supervised PHOTON CUDA start failed: {error}"),
        };

        let deadline = Instant::now() + Duration::from_secs(20);
        while handle.snapshot().batches < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            handle.snapshot().batches >= 2,
            "supervised GPU worker did not run consecutive batches"
        );

        let before = handle.snapshot();
        let second_deadline = Instant::now() + Duration::from_secs(20);
        while handle.snapshot().batches == before.batches && Instant::now() < second_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let after = handle.snapshot();
        assert!(after.batches > before.batches);
        assert!(after.candidates > before.candidates);
        assert_eq!(
            after.candidates,
            after.batches * u64::from(MAX_BATCH_CANDIDATES),
            "30% intensity must throttle between full GPU batches, not shrink each batch as well"
        );
        let _ = handle.stop();
    }
}
