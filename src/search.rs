//! GPU candidate search (PERFORMANCE CONTRACT).
//! Production hot path = persistent reference-correct native GPU engine.
//! CPU cryptography is limited to job setup and rare returned-winner verification.

use crate::backend::BackendKind;
use crate::config::{MiningNetwork, MiningToken};
use crate::crypto;
use crate::cuda_photon::{CudaPhotonEngine, PhotonCudaBatchResult, PhotonCudaWinner};
use crate::hip_photon::HipPhotonEngine;
use crate::protocol::ProofRule;
#[cfg(test)]
use crate::tx;
#[cfg(feature = "portable-wgpu")]
use crate::wgpu_photon::WgpuPhotonEngine;
use rand::RngExt;
use secp256k1::PublicKey;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// SearchHandle owns an exact persistent native-GPU A -> B -> C candidate path.
/// Higher-level product mining remains gated until submission/TUI and the
/// remaining product backends are complete.
pub const REFERENCE_GPU_PIPELINE_READY: bool = true;

pub(crate) const MAX_BATCH_CANDIDATES: u32 = 65_536;
/// CUDA batches are larger: C1 gives each thread 8 candidates that share
/// one inversion, so a 256K batch keeps every SM busy (35.0M/s against
/// 29.8M/s at 64K on the RTX 5070 Ti).
#[cfg(all(not(feature = "incremental-k"), not(feature = "tail-grind")))]
pub(crate) const CUDA_MAX_BATCH_CANDIDATES: u32 = 262_144;
/// Incremental search uses 16 candidates per C1 thread. This batch fills
/// twelve 64-thread C1 blocks per SM on the measured 46-SM laptop GPU.
#[cfg(all(feature = "incremental-k", not(feature = "tail-grind")))]
pub(crate) const CUDA_MAX_BATCH_CANDIDATES: u32 = 565_248;
#[cfg(feature = "tail-grind")]
pub(crate) const CUDA_MAX_BATCH_CANDIDATES: u32 = 65_536;
const PORTABLE_WGPU_MAX_BATCH_CANDIDATES: u32 = crate::gpu_types::PORTABLE_MAX_BATCH_CANDIDATES;
/// Throttled batches are a quarter of the full batch: small enough for
/// fine duty pacing, large enough to keep the GPU busy during a burst.
const THROTTLED_BATCH_DIVISOR: u32 = 4;
pub(crate) const WINNER_BUFFER_CAP: u32 = 8;
const WINNER_CHANNEL_CAP: usize = WINNER_BUFFER_CAP as usize;
const JOB_UPDATE_CHANNEL_CAP: usize = 2;
const PAUSE_POLL: Duration = Duration::from_millis(25);

pub(crate) enum PhotonEngine {
    Cuda(Box<CudaPhotonEngine>),
    Hip(Box<HipPhotonEngine>),
    #[cfg(feature = "portable-wgpu")]
    Wgpu(Box<WgpuPhotonEngine>),
}

impl PhotonEngine {
    /// Creates a PhotonEngine for the GPU search worker.
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
            BackendKind::Wgpu => {
                #[cfg(feature = "portable-wgpu")]
                {
                    Ok(Self::Wgpu(Box::new(WgpuPhotonEngine::new(
                        device_ordinal,
                        max_batch_candidates,
                        winner_buffer_cap,
                    )?)))
                }
                #[cfg(not(feature = "portable-wgpu"))]
                {
                    let _ = (device_ordinal, max_batch_candidates, winner_buffer_cap);
                    Err(
                        "wgpu fallback is not compiled; rebuild with --features portable-wgpu"
                            .into(),
                    )
                }
            }
            BackendKind::Auto => {
                Err("auto backend must be resolved before GPU engine initialization".into())
            }
        }
    }

    /// Installs validated PHOTON job data into the GPU search worker.
    pub(crate) fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        self.set_job_for_network(template, target, private_key, MiningNetwork::Mainnet)
    }

    pub(crate) fn set_job_for_network(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        private_key: &[u8; 32],
        network: MiningNetwork,
    ) -> Result<(), String> {
        match self {
            Self::Cuda(engine) => {
                engine.set_positive_target_rule(
                    MiningToken::Photon.photon_deployment(network).proof_rule
                        == ProofRule::Positive,
                );
                engine.set_job(template, target, private_key)
            }
            Self::Hip(engine) => {
                engine.set_positive_target_rule(
                    MiningToken::Photon.photon_deployment(network).proof_rule
                        == ProofRule::Positive,
                );
                engine.set_job(template, target, private_key)
            }
            #[cfg(feature = "portable-wgpu")]
            Self::Wgpu(engine) => {
                engine.set_proof_rule(MiningToken::Photon.photon_deployment(network).proof_rule);
                engine.set_job(template, target, private_key)
            }
        }
    }

    /// Searches a bounded batch of PHOTON candidates with the GPU search worker.
    pub(crate) fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        match self {
            Self::Cuda(engine) => engine.search_batch(nonce_base, candidate_count),
            Self::Hip(engine) => engine.search_batch(nonce_base, candidate_count),
            #[cfg(feature = "portable-wgpu")]
            Self::Wgpu(engine) => engine.search_batch(nonce_base, candidate_count),
        }
    }

    /// Returns the size of persistent allocations on the selected GPU.
    pub(crate) fn persistent_device_bytes(&self) -> usize {
        match self {
            Self::Cuda(engine) => engine.persistent_device_bytes(),
            Self::Hip(engine) => engine.persistent_device_bytes(),
            #[cfg(feature = "portable-wgpu")]
            Self::Wgpu(engine) => engine.persistent_device_bytes(),
        }
    }

    /// Returns the origin of the GPU lookup table.
    pub(crate) fn table_source(&self) -> String {
        match self {
            Self::Cuda(engine) => format!("{:?}", engine.table_source()),
            Self::Hip(engine) => format!("{:?}", engine.table_source()),
            #[cfg(feature = "portable-wgpu")]
            Self::Wgpu(engine) => format!("{:?}", engine.table_source()),
        }
    }

    /// Calculates a bounded batch size for the selected intensity.
    pub(crate) fn scheduled_batch_candidates(&self, intensity: u8) -> u32 {
        let capacity = match self {
            Self::Cuda(engine) => {
                #[cfg(feature = "tail-grind")]
                if let Some(capacity) = engine.t2_group_batch_candidates() {
                    return intensity_batch_candidates(capacity, intensity);
                }
                let _ = engine;
                production_max_batch_candidates(BackendKind::Cuda)
            }
            Self::Hip(engine) => {
                #[cfg(feature = "tail-grind")]
                if let Some(capacity) = engine.t2_group_batch_candidates() {
                    return intensity_batch_candidates(capacity, intensity);
                }
                let _ = engine;
                production_max_batch_candidates(BackendKind::Hip)
            }
            #[cfg(feature = "portable-wgpu")]
            Self::Wgpu(engine) => engine.recommended_batch_candidates(),
        };
        intensity_batch_candidates(capacity, intensity)
    }
}

/// Returns the production cap on candidates per GPU batch.
pub(crate) const fn production_max_batch_candidates(backend: BackendKind) -> u32 {
    match backend {
        BackendKind::Wgpu => PORTABLE_WGPU_MAX_BATCH_CANDIDATES,
        BackendKind::Cuda => CUDA_MAX_BATCH_CANDIDATES,
        BackendKind::Auto | BackendKind::Hip => MAX_BATCH_CANDIDATES,
    }
}

use crate::mining_job::{prepare_job, validate_job, verify_gpu_winner, PreparedJob};
pub use crate::mining_job::{MiningJob, VerifiedWinner};
#[derive(Debug, Clone, Default)]
pub struct SearchStats {
    pub candidates: u64,
    /// Completed live candidate hashes for miner, project and collaborator.
    pub work_candidates: [u64; 3],
    pub batches: u64,
    pub intensity: u8,
    pub state: MiningState,
    pub elapsed_secs: u64,
    pub rate: f64,
    pub current_rate: f64,
    pub peak_rate: f64,
    /// Time-weighted throughput of recent GPU batches, excluding host verification.
    /// Retained while settlement pauses GPU work.
    pub active_rate: f64,
    pub winners: u64,
    pub rejected_winners: u64,
    /// Every nonce of the current job has been tried and a fresh signing
    /// key could not be installed, so the GPU idles until the next job.
    pub waiting_for_job: bool,
    /// Fresh signing keys installed after a job's 2^32 nonces ran out.
    pub key_rotations: u64,
    pub last_error: Option<String>,
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

pub use crate::proof::{hash256, meets_target_le, meets_target_le_for_rule, parse_hex32};

#[cfg(test)]
/// Builds the PHOTON M1 signing message for a candidate.
pub fn photon_m1_message(nonce: u32, target32: &[u8; 32]) -> [u8; 36] {
    let mut msg = [0u8; 36];
    msg[0..4].copy_from_slice(&nonce.to_le_bytes());
    msg[4..36].copy_from_slice(target32);
    msg
}

enum WorkerCommand {
    ReplaceJob {
        job: MiningJob,
        reply: SyncSender<Result<(), String>>,
    },
}

/// Installs the current job under a fresh random signing key.
fn rotate_search_identity(
    engine: &mut PhotonEngine,
    job: &MiningJob,
) -> Result<(PreparedJob, [u8; 32], [u8; 33]), String> {
    let secret = crypto::random_secret_key();
    let sk = secret.to_secret_bytes();
    let public_key = PublicKey::from_secret_key(&secret).serialize();
    let prepared = prepare_job(job.clone(), &sk, &public_key)?;
    engine.set_job_for_network(
        &prepared.template,
        &prepared.target,
        &sk,
        prepared.job.network,
    )?;
    Ok((prepared, sk, public_key))
}

/// Scales GPU batch candidates with requested intensity.
pub(crate) const fn intensity_batch_candidates(capacity: u32, intensity: u8) -> u32 {
    if capacity == 0 {
        return 0;
    }
    if intensity < 100 {
        let throttled = capacity / THROTTLED_BATCH_DIVISOR;
        if throttled == 0 {
            1
        } else {
            throttled
        }
    } else {
        capacity
    }
}

fn batch_before_wrap(base: u32, requested: u32) -> u32 {
    u64::from(requested).min((1u64 << 32) - u64::from(base)) as u32
}

/// Tracks how much of a job's 2^32 nonce space has been searched.
///
/// The baseline varies a 4-byte commitment nonce; incremental search varies
/// a 32-bit scalar index. Batches walk either space from a random start with
/// wrapping arithmetic. The last batch is trimmed to prevent repeats, then
/// the worker rotates its unfunded search identity before the next sweep.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct NonceSweep {
    searched: u64,
}

impl NonceSweep {
    const SPACE: u64 = 1 << 32;

    /// Size of the next batch, or None once every nonce has been searched.
    pub(crate) fn next_batch(&self, requested: u32) -> Option<u32> {
        let remaining = Self::SPACE.saturating_sub(self.searched);
        (remaining > 0 && requested > 0)
            .then(|| u32::try_from(remaining.min(u64::from(requested))).unwrap_or(requested))
    }

    /// Counts candidates of a finished batch.
    pub(crate) fn record(&mut self, candidates: u32) {
        self.searched = self.searched.saturating_add(u64::from(candidates));
    }
}

// Keep the native clock adapter small; the browser uses the same pacing core.
#[derive(Debug, Clone)]
pub(crate) struct DutyPacer {
    origin: Instant,
    core: crate::mining_control::DutyPacer,
}
impl DutyPacer {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            origin: now,
            core: crate::mining_control::DutyPacer::new(Duration::ZERO),
        }
    }
    pub(crate) fn reset(&mut self, now: Instant) {
        self.core.reset(now.saturating_duration_since(self.origin));
    }
    pub(crate) fn record_batch(
        &mut self,
        intensity: u8,
        compute: Duration,
        now: Instant,
    ) -> Duration {
        self.core.record_batch(
            intensity,
            compute,
            now.saturating_duration_since(self.origin),
        )
    }
}
#[cfg(test)]
use crate::mining_control::duty_rest;

// #### PR #11: winner delivery never blocks ####
// What: the GPU worker hands host-verified winners to the supervisor through
// a bounded queue (WINNER_CHANNEL_CAP). When the queue is full it stops and
// drops the rest of this batch's winners instead of waiting.
// Why: a blocking send kept `batch_in_flight` set while the supervisor waited
// for `!batch_in_flight` before draining the queue. With a winner in nearly
// every batch the miner froze for good ("WINNER PENDING (GPU PAUSED)"), as in
// the first live Chipnet v3.2 run on 2026-10-03.
// Safe because: winners still queued already cover the job, and only one of
// them becomes a claim. If they belong to an older generation (stale), the
// next batch finds fresh winners again.
// If something looks wrong here: GPU winners found but never claimed, or
// `stale_winners` jumping by up to 8 at once. Review this function together
// with `drain_winners` (search.rs) and `winner_refresh_ready` (runtime.rs).

/// Publishes only GPU batches that pass host verification.
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
        match winner_tx.try_send(verified) {
            Ok(()) => {
                winners.fetch_add(1, Ordering::Release);
            }
            // Full: the queued winners already cover this job (see above).
            Err(TrySendError::Full(_)) => break,
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    true
}
// #### end PR #11 ####

struct WorkerDiagnostics {
    last_error: Mutex<Option<String>>,
    rejected_winners: AtomicU64,
    job_exhausted: AtomicBool,
    key_rotations: AtomicU64,
    active_rate_bits: AtomicU64,
    active_rate: Mutex<crate::mining_control::ActiveRate>,
    work_candidates: [AtomicU64; 3],
}

impl WorkerDiagnostics {
    fn new() -> Self {
        Self {
            last_error: Mutex::new(None),
            rejected_winners: AtomicU64::new(0),
            job_exhausted: AtomicBool::new(false),
            key_rotations: AtomicU64::new(0),
            active_rate_bits: AtomicU64::new(0),
            active_rate: Mutex::new(crate::mining_control::ActiveRate::default()),
            work_candidates: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn record_active_batch(&self, candidates: u32, elapsed: Duration) {
        if candidates != 0 {
            let rate = self
                .active_rate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(candidates, elapsed);
            self.active_rate_bits
                .store(rate.to_bits(), Ordering::Relaxed);
        }
    }

    fn record_batch_error(&self, error: String) {
        self.store_error(error);
    }

    fn record_rejected_winner(&self, error: String) {
        self.rejected_winners.fetch_add(1, Ordering::Relaxed);
        self.store_error(format!("GPU winner rejected by host verification: {error}"));
    }

    fn store_error(&self, error: String) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
    }

    fn publish(&self, stats: &mut SearchStats) {
        stats.active_rate = f64::from_bits(self.active_rate_bits.load(Ordering::Relaxed));
        stats.rejected_winners = self.rejected_winners.load(Ordering::Relaxed);
        stats.waiting_for_job = self.job_exhausted.load(Ordering::Relaxed);
        stats.key_rotations = self.key_rotations.load(Ordering::Relaxed);
        stats.last_error = self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
    }
}

struct AcceptedBatch {
    candidates: u32,
    verified: Vec<VerifiedWinner>,
}

enum BatchControl {
    Continue(AcceptedBatch),
    Stop,
}

fn absorb_search_batch(
    diagnostics: &WorkerDiagnostics,
    batch: Result<PhotonCudaBatchResult, String>,
    mut verify: impl FnMut(&PhotonCudaWinner) -> Result<VerifiedWinner, String>,
) -> BatchControl {
    let result = match batch {
        Ok(result) => result,
        Err(error) => {
            diagnostics.record_batch_error(error);
            return BatchControl::Stop;
        }
    };
    let mut verified = Vec::with_capacity(result.winners.len());
    for winner in &result.winners {
        match verify(winner) {
            Ok(accepted) => verified.push(accepted),
            Err(error) => diagnostics.record_rejected_winner(error),
        }
    }
    BatchControl::Continue(AcceptedBatch {
        candidates: result.candidates,
        verified,
    })
}

#[allow(clippy::too_many_arguments)]
/// Runs the GPU worker, processing commands between batches.
fn run_worker(
    mut engine: PhotonEngine,
    mut prepared: PreparedJob,
    mut sk: [u8; 32],
    mut public_key: [u8; 33],
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    batch_in_flight: Arc<AtomicBool>,
    intensity: Arc<AtomicU8>,
    candidates: Arc<AtomicU64>,
    batches: Arc<AtomicU64>,
    winners: Arc<AtomicU64>,
    generation_id: Arc<AtomicU64>,
    diagnostics: Arc<WorkerDiagnostics>,
    job_rx: Receiver<WorkerCommand>,
    winner_tx: SyncSender<VerifiedWinner>,
    work_fee: Option<crate::donation::Policy>,
) {
    let t2_coordinate = cfg!(feature = "tail-grind")
        && matches!(&engine, PhotonEngine::Cuda(_) | PhotonEngine::Hip(_));
    let mut rng = rand::rng();
    let backend = match &engine {
        PhotonEngine::Cuda(_) => BackendKind::Cuda,
        PhotonEngine::Hip(_) => BackendKind::Hip,
        #[cfg(feature = "portable-wgpu")]
        PhotonEngine::Wgpu(_) => BackendKind::Wgpu,
    };
    let quantum = crate::mining_control::work_allocation_quantum(
        backend,
        engine.scheduled_batch_candidates(100),
    );
    let allocation = work_fee
        .map(|policy| {
            let schedule = crate::donation::Schedule::new(policy.scheme, quantum, rng.random())?;
            let payouts = policy.payouts(prepared.job.network, &prepared.job.payout_address)?;
            Ok::<_, String>((schedule, payouts, policy))
        })
        .transpose();
    let mut allocation = match allocation {
        Ok(allocation) => allocation,
        Err(error) => {
            diagnostics.record_batch_error(error);
            stop.store(true, Ordering::Relaxed);
            return;
        }
    };
    let mut nonce_base = if t2_coordinate {
        0
    } else {
        rng.random::<u32>()
    };
    let mut sweep = NonceSweep::default();
    let mut pacer = DutyPacer::new(Instant::now());
    while !stop.load(Ordering::Relaxed) {
        loop {
            match job_rx.try_recv() {
                Ok(WorkerCommand::ReplaceJob { mut job, reply }) => {
                    let next_payouts = if let Some((schedule, _, policy)) = &allocation {
                        match policy.payouts(job.network, &job.payout_address) {
                            Ok(payouts) => {
                                job.payout_address = payouts[schedule.recipient() as usize].clone();
                                Some(payouts)
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error));
                                continue;
                            }
                        }
                    } else {
                        None
                    };
                    let result = prepare_job(job, &sk, &public_key).and_then(|next| {
                        engine.set_job_for_network(
                            &next.template,
                            &next.target,
                            &sk,
                            next.job.network,
                        )?;
                        generation_id.store(next.job.generation_id, Ordering::Release);
                        prepared = next;
                        if let (Some((_, payouts, _)), Some(next)) = (&mut allocation, next_payouts)
                        {
                            *payouts = next;
                        }
                        nonce_base = if t2_coordinate {
                            0
                        } else {
                            rng.random::<u32>()
                        };
                        sweep = NonceSweep::default();
                        diagnostics.job_exhausted.store(false, Ordering::Relaxed);
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
            pacer.reset(Instant::now());
            continue;
        }

        let active_intensity = intensity.load(Ordering::Relaxed).clamp(10, 100);
        if let Some((schedule, payouts, _)) = &allocation {
            let address = &payouts[schedule.recipient() as usize];
            let next = (|| {
                if address == &prepared.job.payout_address {
                    return Ok(None);
                }
                let mut job = prepared.job.clone();
                job.payout_address = address.clone();
                // A fresh identity prevents duplicate work when returning to
                // a recipient before the chain job changes.
                rotate_search_identity(&mut engine, &job).map(Some)
            })();
            match next {
                Ok(Some((next, next_sk, next_public_key))) => {
                    prepared = next;
                    sk.fill(0);
                    sk = next_sk;
                    public_key = next_public_key;
                    nonce_base = if t2_coordinate {
                        0
                    } else {
                        rng.random::<u32>()
                    };
                    sweep = NonceSweep::default();
                    diagnostics.key_rotations.fetch_add(1, Ordering::Relaxed);
                }
                Ok(None) => {}
                Err(error) => {
                    diagnostics.record_batch_error(format!(
                        "mining-work recipient change failed: {error}"
                    ));
                    batch_in_flight.store(false, Ordering::SeqCst);
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        let Some(batch_size) =
            sweep.next_batch(engine.scheduled_batch_candidates(active_intensity))
        else {
            // All 2^32 nonces under this signing key are done. The key only
            // signs the PHOTON commitment and holds no funds, so a fresh key
            // gives a new template and another 2^32 candidates for the same
            // job instead of idling until the next block.
            batch_in_flight.store(false, Ordering::SeqCst);
            match rotate_search_identity(&mut engine, &prepared.job) {
                Ok((next, next_sk, next_public_key)) => {
                    prepared = next;
                    sk.fill(0);
                    sk = next_sk;
                    public_key = next_public_key;
                    nonce_base = if t2_coordinate {
                        0
                    } else {
                        rng.random::<u32>()
                    };
                    sweep = NonceSweep::default();
                    diagnostics.key_rotations.fetch_add(1, Ordering::Relaxed);
                    diagnostics.job_exhausted.store(false, Ordering::Relaxed);
                }
                Err(error) => {
                    diagnostics.record_batch_error(format!("search key rotation failed: {error}"));
                    diagnostics.job_exhausted.store(true, Ordering::Relaxed);
                    thread::park_timeout(PAUSE_POLL);
                    pacer.reset(Instant::now());
                }
            }
            continue;
        };
        // Incremental scalar indexes must not cross u32::MAX inside a batch.
        // The next batch wraps to zero; NonceSweep still counts each index once.
        let batch_size = batch_before_wrap(nonce_base, batch_size);
        let batch_size = allocation.as_ref().map_or(batch_size, |(schedule, _, _)| {
            schedule.limit_batch(batch_size)
        });
        let batch_started = Instant::now();
        let batch = engine.search_batch(nonce_base, batch_size);
        let gpu_elapsed = batch_started.elapsed();
        if let Ok(ref result) = batch {
            diagnostics.record_active_batch(result.candidates, gpu_elapsed);
        }
        let control = absorb_search_batch(&diagnostics, batch, |gpu_winner| {
            verify_gpu_winner(&prepared, &sk, &public_key, gpu_winner)
        });
        let accepted = match control {
            BatchControl::Stop => {
                batch_in_flight.store(false, Ordering::SeqCst);
                stop.store(true, Ordering::Relaxed);
                break;
            }
            BatchControl::Continue(accepted) => accepted,
        };
        let compute_time = batch_started.elapsed();
        if let Some((schedule, _, _)) = &mut allocation {
            let recipient = schedule.recipient();
            if let Err(error) = schedule.record(accepted.candidates) {
                diagnostics.record_batch_error(error);
                batch_in_flight.store(false, Ordering::SeqCst);
                stop.store(true, Ordering::Relaxed);
                break;
            }
            diagnostics.work_candidates[recipient as usize]
                .fetch_add(u64::from(accepted.candidates), Ordering::Relaxed);
        }
        candidates.fetch_add(u64::from(accepted.candidates), Ordering::Relaxed);
        batches.fetch_add(1, Ordering::Release);

        if !deliver_verified_batch(accepted.verified, &paused, &winners, &winner_tx) {
            stop.store(true, Ordering::Relaxed);
        }
        batch_in_flight.store(false, Ordering::SeqCst);

        nonce_base = nonce_base.wrapping_add(accepted.candidates);
        sweep.record(accepted.candidates);
        let rest = pacer.record_batch(active_intensity, compute_time, Instant::now());
        if !rest.is_zero() {
            thread::park_timeout(rest);
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
    diagnostics: Arc<WorkerDiagnostics>,
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
    /// Pauses the GPU search worker at a safe boundary.
    pub(crate) fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    /// Builds a pause handle over shared search state.
    pub(crate) fn from_shared(paused: Arc<AtomicBool>) -> Self {
        Self { paused }
    }
}

impl SearchHandle {
    /// Starts GPU search using the configured backend.
    pub fn start(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_on_device(0, intensity, job)
    }

    /// Starts GPU search on the selected device ordinal.
    pub fn start_on_device(
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_on_backend_device(BackendKind::Cuda, device_ordinal, intensity, job)
    }

    /// Starts GPU search on an explicit backend and device.
    pub fn start_on_backend_device(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_inner(backend, device_ordinal, intensity, job, false, None)
    }

    /// Start exact GPU search under the live runtime supervisor. The GPU keeps
    /// launching batches back-to-back while the immutable generation is valid;
    /// the supervisor polls PHOTON state independently and applies `ReplaceJob`
    /// between batches when that generation changes.
    pub fn start_supervised(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_supervised_on_device(0, intensity, job)
    }

    /// Starts supervised search on a specific GPU device.
    pub fn start_supervised_on_device(
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_supervised_on_backend_device(BackendKind::Cuda, device_ordinal, intensity, job)
    }

    /// Starts supervised search on an explicit GPU backend.
    pub fn start_supervised_on_backend_device(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_inner(backend, device_ordinal, intensity, job, false, None)
    }

    /// Start supervised search in a paused state. This is used while a
    /// crash-recovered winner submission is pending, so no GPU batch can begin
    /// before the durable parent/child pair is resolved.
    pub fn start_supervised_paused(intensity: u8, job: MiningJob) -> Result<Self, String> {
        Self::start_supervised_paused_on_device(0, intensity, job)
    }

    /// Starts supervised, paused search on a specific GPU.
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

    /// Starts supervised, paused search on an explicit backend.
    pub fn start_supervised_paused_on_backend_device(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
    ) -> Result<Self, String> {
        Self::start_inner(backend, device_ordinal, intensity, job, true, None)
    }

    /// Starts production PHOTON mining with direct 96/2/2 work allocation.
    pub fn start_with_work_fee(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
        policy: crate::donation::Policy,
    ) -> Result<Self, String> {
        policy.payouts(job.network, &job.payout_address)?;
        Self::start_inner(backend, device_ordinal, intensity, job, false, Some(policy))
    }

    /// Creates the shared GPU worker and its control channels.
    fn start_inner(
        backend: BackendKind,
        device_ordinal: usize,
        intensity: u8,
        job: MiningJob,
        initially_paused: bool,
        work_fee: Option<crate::donation::Policy>,
    ) -> Result<Self, String> {
        if !(10..=100).contains(&intensity) {
            return Err("intensity must be 10..=100".into());
        }
        let secret = crypto::random_secret_key();
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
        #[cfg(feature = "tail-grind")]
        if let PhotonEngine::Cuda(cuda) = &mut engine {
            // The parent reward BCH output must remain exactly 700 sats.
            cuda.enable_t2_search()?;
        }
        #[cfg(feature = "tail-grind")]
        if let PhotonEngine::Hip(hip) = &mut engine {
            hip.enable_t2_search()?;
        }
        #[cfg(all(feature = "incremental-k", not(feature = "tail-grind")))]
        if let PhotonEngine::Cuda(cuda) = &mut engine {
            cuda.enable_incremental_search()?;
        }
        engine.set_job_for_network(
            &prepared.template,
            &prepared.target,
            &sk,
            prepared.job.network,
        )?;

        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(initially_paused));
        let batch_in_flight = Arc::new(AtomicBool::new(false));
        let intensity_state = Arc::new(AtomicU8::new(intensity));
        let candidates = Arc::new(AtomicU64::new(0));
        let batches = Arc::new(AtomicU64::new(0));
        let winners = Arc::new(AtomicU64::new(0));
        let generation_id = Arc::new(AtomicU64::new(prepared.job.generation_id));
        let diagnostics = Arc::new(WorkerDiagnostics::new());
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
                let worker_diagnostics = Arc::clone(&diagnostics);
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
                        worker_diagnostics,
                        job_rx,
                        winner_tx,
                        work_fee,
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
            diagnostics,
            job_tx,
            winner_rx,
            worker: Some(worker),
            started: Instant::now(),
        })
    }

    /// Replaces search material with a verified new generation.
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

    /// Returns the current search generation identifier.
    pub fn generation_id(&self) -> u64 {
        self.generation_id.load(Ordering::Acquire)
    }

    /// Drains host-verified GPU winners for settlement.
    pub fn drain_winners(&self) -> Vec<VerifiedWinner> {
        self.winner_rx.try_iter().collect()
    }

    /// Reports whether the GPU is currently processing a batch.
    pub fn batch_in_flight(&self) -> bool {
        self.batch_in_flight.load(Ordering::SeqCst)
    }

    /// Returns a handle for pausing the GPU search worker.
    pub(crate) fn pause_handle(&self) -> SearchPauseHandle {
        SearchPauseHandle {
            paused: Arc::clone(&self.paused),
        }
    }

    /// Applies a worker intensity or pause control message.
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

    /// Returns the current GPU worker state.
    fn state(&self) -> MiningState {
        if self.stop.load(Ordering::Relaxed) {
            MiningState::Stopped
        } else if self.paused.load(Ordering::Relaxed) {
            MiningState::Paused
        } else {
            MiningState::Mining
        }
    }

    /// Stops the GPU search worker and waits for outstanding work.
    pub fn stop(mut self) -> SearchStats {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        self.snapshot_final()
    }

    /// Captures a final GPU search snapshot after stopping.
    fn snapshot_final(&self) -> SearchStats {
        let candidates = self.candidates.load(Ordering::Relaxed);
        let elapsed = self.started.elapsed().as_secs_f64().max(0.001);
        let mut stats = SearchStats {
            candidates,
            work_candidates: std::array::from_fn(|i| {
                self.diagnostics.work_candidates[i].load(Ordering::Relaxed)
            }),
            batches: self.batches.load(Ordering::Acquire),
            intensity: self.intensity.load(Ordering::Relaxed),
            state: self.state(),
            elapsed_secs: elapsed as u64,
            rate: candidates as f64 / elapsed,
            current_rate: 0.0,
            peak_rate: 0.0,
            active_rate: 0.0,
            winners: self.winners.load(Ordering::Relaxed),
            rejected_winners: 0,
            waiting_for_job: false,
            key_rotations: 0,
            last_error: None,
        };
        self.diagnostics.publish(&mut stats);
        stats
    }

    /// Captures the current GPU search rates and state.
    pub fn snapshot(&self) -> SearchStats {
        self.snapshot_final()
    }
    /// Updates the intensity used to schedule GPU batches.
    pub fn set_intensity(&self, value: u8) -> Result<(), String> {
        self.apply_control(RuntimeCommand::SetIntensity(value))?;
        Ok(())
    }

    /// Switches GPU search between paused and running states.
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
    /// Releases resources owned by SearchHandle.
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
    use crate::cuda_photon::{PhotonCudaBatchResult, PhotonCudaWinner};
    use secp256k1::SecretKey;

    #[test]
    fn search_batch_error_is_retained_and_stops_the_worker() {
        let diagnostics = WorkerDiagnostics::new();
        let control = absorb_search_batch(&diagnostics, Err("HIP launch failed".into()), |_| {
            unreachable!("a failed batch has no winner to verify")
        });
        assert!(matches!(control, BatchControl::Stop));
        let mut stats = SearchStats::default();
        diagnostics.publish(&mut stats);
        assert_eq!(stats.last_error.as_deref(), Some("HIP launch failed"));
        assert_eq!(stats.rejected_winners, 0);
    }

    #[test]
    fn rejected_gpu_winner_is_counted_and_reported() {
        let diagnostics = WorkerDiagnostics::new();
        let batch = PhotonCudaBatchResult {
            candidates: 4,
            total_winners: 1,
            winners: vec![PhotonCudaWinner {
                nonce: 7,
                digest: [9; 32],
                schnorr_k: None,
                tail_j: None,
                tail_value_sats: None,
            }],
        };
        let control =
            absorb_search_batch(&diagnostics, Ok(batch), |_| Err("HASH256 mismatch".into()));
        match control {
            BatchControl::Continue(accepted) => {
                assert_eq!(accepted.candidates, 4);
                assert!(accepted.verified.is_empty());
            }
            BatchControl::Stop => {
                panic!("host rejection must stay visible without stopping search")
            }
        }
        let mut stats = SearchStats::default();
        diagnostics.publish(&mut stats);
        assert_eq!(stats.rejected_winners, 1);
        let error = stats.last_error.expect("rejection reason");
        assert!(
            error.contains("GPU winner rejected by host verification"),
            "{error}"
        );
        assert!(error.contains("HASH256 mismatch"), "{error}");
    }

    #[test]
    fn active_gpu_rate_is_measured_before_verification_and_retained_while_paused() {
        let diagnostics = WorkerDiagnostics::new();
        diagnostics.record_active_batch(10_000, Duration::from_millis(10));
        let mut paused = SearchStats {
            state: MiningState::Paused,
            current_rate: 0.0,
            ..SearchStats::default()
        };
        diagnostics.publish(&mut paused);
        assert_eq!(paused.current_rate, 0.0);
        assert_eq!(paused.active_rate, 1_000_000.0);
    }

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
        // A live target is a positive script number, so its top bit is clear.
        let mut t = parse_hex32(&"aa".repeat(32)).unwrap();
        t[31] = 0x2a;
        assert!(!meets_target_le(&t, &t));
    }

    #[test]
    fn digest_sign_bit_is_ignored_like_the_covenant() {
        let mut target = [0u8; 32];
        target[28] = 0x02;
        let mut digest = [0u8; 32];
        digest[28] = 0x01;
        assert!(meets_target_le(&digest, &target));
        // ABS(BIN2NUM(..)) drops bit 255, so the same digest with the sign
        // bit set is still a win.
        digest[31] = 0x80;
        assert!(meets_target_le(&digest, &target));
        // Any other high bit keeps it above the target.
        digest[31] = 0x81;
        assert!(!meets_target_le(&digest, &target));
        digest[31] = 0x80;
        digest[28] = 0x02;
        assert!(!meets_target_le(&digest, &target));
    }

    #[test]
    fn parse_and_meet_max_target() {
        let t = parse_hex32(&"ff".repeat(32)).unwrap();
        let d = hash256(b"x");
        assert!(meets_target_le(&d, &t));
    }

    /// Runs the pacer against a simulated clock whose sleeps round up to a
    /// timer tick, and returns the achieved GPU duty cycle.
    fn simulated_duty(intensity: u8, compute: Duration, tick: Duration, total: Duration) -> f64 {
        let start = Instant::now();
        let mut now = start;
        let mut pacer = DutyPacer::new(now);
        let mut busy = Duration::ZERO;
        while now.duration_since(start) < total {
            now += compute;
            busy += compute;
            let rest = pacer.record_batch(intensity, compute, now);
            if !rest.is_zero() {
                let ticks = rest.as_nanos().div_ceil(tick.as_nanos()) as u32;
                now += tick * ticks;
            }
        }
        busy.as_secs_f64() / now.duration_since(start).as_secs_f64()
    }

    #[test]
    fn duty_pacer_holds_intensity_despite_coarse_sleep_ticks() {
        // A throttled CUDA batch is 1-2 ms on the RTX 5070 Ti, and
        // Windows sleeps round up to about 15.6 ms.
        let compute = Duration::from_micros(1_000);
        let tick = Duration::from_micros(15_625);
        for intensity in [10_u8, 25, 30, 50, 75, 90] {
            let duty = simulated_duty(intensity, compute, tick, Duration::from_secs(30));
            let expected = f64::from(intensity) / 100.0;
            assert!(
                (duty - expected).abs() <= 0.02,
                "intensity {intensity}% achieved duty {duty:.3}"
            );
        }
        // Per-batch rest (the old pacing) collapses every level to about 6%.
        let old_duty = compute.as_secs_f64() / (compute + tick).as_secs_f64();
        assert!(old_duty < 0.07);
    }

    #[test]
    fn duty_pacer_works_in_bursts_and_rests_in_whole_periods() {
        let start = Instant::now();
        let mut now = start;
        let mut pacer = DutyPacer::new(now);
        let compute = Duration::from_millis(1);
        let mut burst = Duration::ZERO;
        let mut rests = 0;
        while now.duration_since(start) < Duration::from_secs(5) {
            now += compute;
            burst += compute;
            let rest = pacer.record_batch(25, compute, now);
            if !rest.is_zero() {
                // 25% of a 100 ms period: about 25 ms of work, 75 ms of rest.
                assert!(rest >= Duration::from_millis(75), "short rest {rest:?}");
                assert!(burst >= Duration::from_millis(20), "short burst {burst:?}");
                rests += 1;
                burst = Duration::ZERO;
                now += rest;
            }
        }
        assert!((45..=55).contains(&rests), "{rests} periods in 5 s");
    }

    #[test]
    fn nonce_sweep_covers_each_nonce_once_then_stops() {
        // Include full/throttled batches that do not divide the scalar space.
        for batch in [65_536, 262_144, 565_248, 141_312] {
            for start in [0u32, 0x1234_5678, u32::MAX - 16] {
                let mut sweep = NonceSweep::default();
                let mut base = start;
                let mut total = 0u64;
                while let Some(requested) = sweep.next_batch(batch) {
                    let count = batch_before_wrap(base, requested);
                    assert!(count > 0 && count <= batch);
                    assert!(u64::from(base) + u64::from(count) <= 1u64 << 32);
                    base = base.wrapping_add(count);
                    sweep.record(count);
                    total += u64::from(count);
                }
                assert_eq!(total, 1u64 << 32);
                assert_eq!(base, start);
                assert_eq!(sweep.next_batch(1), None);
            }
        }
        assert_eq!(NonceSweep::default().next_batch(0), None);
    }

    #[test]
    fn duty_pacer_never_rests_at_full_intensity() {
        let now = Instant::now();
        let mut pacer = DutyPacer::new(now);
        for step in 1..=100_u32 {
            let rest = pacer.record_batch(
                100,
                Duration::from_millis(4),
                now + step * 4 * Duration::from_millis(1),
            );
            assert_eq!(rest, Duration::ZERO);
        }
    }

    #[test]
    fn duty_pacer_reset_discards_idle_credit() {
        let start = Instant::now();
        let mut pacer = DutyPacer::new(start);
        let compute = Duration::from_millis(60);
        assert_eq!(pacer.record_batch(50, compute, start + compute), compute);

        // Ten idle seconds without a reset read as credit: no rest at all.
        let resumed = start + Duration::from_secs(10);
        let mut unreset = pacer.clone();
        assert_eq!(
            unreset.record_batch(50, compute, resumed + compute),
            Duration::ZERO
        );

        // After a reset the first batch is paced again.
        pacer.reset(resumed);
        assert_eq!(pacer.record_batch(50, compute, resumed + compute), compute);
    }

    #[test]
    fn intensity_scales_real_gpu_duty() {
        assert_eq!(
            intensity_batch_candidates(CUDA_MAX_BATCH_CANDIDATES, 100),
            CUDA_MAX_BATCH_CANDIDATES
        );
        assert_eq!(
            intensity_batch_candidates(CUDA_MAX_BATCH_CANDIDATES, 50),
            CUDA_MAX_BATCH_CANDIDATES / 4
        );
        assert_eq!(
            intensity_batch_candidates(MAX_BATCH_CANDIDATES, 100),
            65_536
        );
        assert_eq!(intensity_batch_candidates(MAX_BATCH_CANDIDATES, 25), 16_384);
        assert_eq!(intensity_batch_candidates(MAX_BATCH_CANDIDATES, 10), 16_384);
        assert_eq!(intensity_batch_candidates(3, 10), 1);
        let compute = Duration::from_millis(10);
        assert_eq!(duty_rest(compute, 100), Duration::ZERO);
        assert_eq!(duty_rest(compute, 50), Duration::from_millis(10));
        assert_eq!(duty_rest(compute, 25), Duration::from_millis(30));
        assert_eq!(duty_rest(compute, 10), Duration::from_millis(90));

        let mut previous_rest = Duration::MAX;
        for intensity in [10_u8, 25, 50, 75] {
            let rest = duty_rest(compute, intensity);
            assert!(rest < previous_rest);
            previous_rest = rest;
            let active_ns = compute.as_nanos();
            let period_ns = active_ns + rest.as_nanos();
            let occupancy = active_ns as f64 / period_ns as f64;
            let expected = f64::from(intensity) / 100.0;
            assert!(
                (occupancy - expected).abs() < 0.001,
                "intensity {intensity} occupancy {occupancy} != {expected}"
            );
        }
        // Equal batch speed would make 100% at least 10× the 10% candidate rate.
        let low_period = compute + duty_rest(compute, 10);
        let ratio = low_period.as_secs_f64() / compute.as_secs_f64();
        assert!(ratio >= 2.0);
    }

    #[test]
    fn production_batch_envelope_keeps_native_limit_and_reference_wgpu_limit() {
        assert_eq!(
            production_max_batch_candidates(BackendKind::Cuda),
            if cfg!(feature = "tail-grind") {
                65_536
            } else if cfg!(feature = "incremental-k") {
                565_248
            } else {
                262_144
            }
        );
        assert_eq!(production_max_batch_candidates(BackendKind::Hip), 65_536);
        assert_eq!(
            production_max_batch_candidates(BackendKind::Wgpu),
            PORTABLE_WGPU_MAX_BATCH_CANDIDATES
        );
    }

    #[test]
    fn verified_winning_batch_pauses_and_fits_bounded_delivery_queue() {
        assert!(WINNER_CHANNEL_CAP >= WINNER_BUFFER_CAP as usize);

        let paused = AtomicBool::new(false);
        let winners = AtomicU64::new(0);
        let (winner_tx, winner_rx) = mpsc::sync_channel(WINNER_CHANNEL_CAP);
        let batch: Vec<_> = (0..WINNER_BUFFER_CAP)
            .map(|nonce| VerifiedWinner {
                generation_id: 1,
                height: 1_000,
                baton_txid: "11".repeat(32),
                baton_vout: 0,
                job_reward_raw: 4_999_773_813,
                nonce,
                digest: [0u8; 32],
                public_key: [2u8; 33],
                signature: [0u8; 64],
                transaction: vec![0x02],
            })
            .collect();

        let second = batch.clone();
        assert!(deliver_verified_batch(batch, &paused, &winners, &winner_tx));
        assert!(paused.load(Ordering::SeqCst));
        assert_eq!(
            winners.load(Ordering::Acquire),
            u64::from(WINNER_BUFFER_CAP)
        );

        // #### PR #11 test: a second winning batch into a full queue ####
        // The GPU resumed while a claim resolved, before the supervisor
        // drained the queue. The old blocking send hung here forever.
        paused.store(false, Ordering::SeqCst);
        assert!(deliver_verified_batch(
            second, &paused, &winners, &winner_tx
        ));
        assert!(paused.load(Ordering::SeqCst));
        assert_eq!(
            winners.load(Ordering::Acquire),
            u64::from(WINNER_BUFFER_CAP)
        );
        assert_eq!(winner_rx.try_iter().count(), WINNER_BUFFER_CAP as usize);
        // #### end PR #11 test ####
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
            diagnostics: Arc::new(WorkerDiagnostics::new()),
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
    fn prepared_job_uses_identity_and_age_layout() {
        let sk = [1u8; 32];
        let secret = SecretKey::from_secret_bytes(sk).unwrap();
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let job = MiningJob {
            height: 1,
            baton_txid: "42a02ec4f58b50f23df4591dcc999ca1bcae2f378997fe6547ae124712000000".into(),
            baton_vout: 0,
            baton_value_sats: 15_971_500,
            relay_fee_sats_per_kb: 1_000,
            age: 10,
            target_le_hex: "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                .into(),
            token_amount: 2_099_905_002_035_715,
            reward_raw: 4_999_773_813,
            payout_address: "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh".into(),
            generation_id: 1,
            ..MiningJob::default()
        };
        // Mainnet runs the 273-byte v3.2 contract: fourteen bytes past v0.
        for (age, bytes) in [(10u32, 629usize), (17, 630), (128, 631), (32_767, 631)] {
            let prepared = prepare_job(MiningJob { age, ..job.clone() }, &sk, &public_key).unwrap();
            let layout =
                tx::PhotonLayout::for_age_with_deployment(age, &crate::protocol::MAINNET_PHOTON)
                    .unwrap();
            assert_eq!(prepared.template.len(), bytes);
            let n = layout.nonce_offset();
            assert_eq!(&prepared.template[n..n + 4], &[0u8; 4]);
            let t = layout.target_offset();
            assert_eq!(&prepared.template[t..t + 32], &prepared.target);
            assert_eq!(&prepared.template[45..78], &public_key);
        }
        assert!(prepare_job(
            MiningJob {
                age: 32_768,
                ..job.clone()
            },
            &sk,
            &public_key
        )
        .is_err());
        assert!(prepare_job(MiningJob { age: 65_535, ..job }, &sk, &public_key).is_err());
    }

    #[test]
    fn positive_proof_rule_requires_positive_nonzero_digest() {
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        let mut positive = [1; 32];
        positive[31] = 0;
        assert!(meets_target_le_for_rule(
            &positive,
            &target,
            crate::protocol::ProofRule::Positive,
        ));
        let mut negative = positive;
        negative[31] = 0x80;
        assert!(!meets_target_le_for_rule(
            &negative,
            &target,
            crate::protocol::ProofRule::Positive,
        ));
        assert!(!meets_target_le_for_rule(
            &[0; 32],
            &target,
            crate::protocol::ProofRule::Positive,
        ));
        assert!(meets_target_le_for_rule(
            &negative,
            &target,
            crate::protocol::ProofRule::Absolute,
        ));
        let mut invalid_target = target;
        invalid_target[31] = 0xff;
        assert!(!meets_target_le_for_rule(
            &positive,
            &invalid_target,
            crate::protocol::ProofRule::Positive,
        ));
    }

    #[test]
    fn prepared_chipnet_job_uses_confirmed_deployment_layout() {
        let sk = [1u8; 32];
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(sk).unwrap()).serialize();
        let mut job = integration_job(8);
        job.network = MiningNetwork::Chipnet;
        job.age = 38;
        job.baton_value_sats = 49_080_500;
        job.payout_address = crate::config::CHIPNET_DONATION_ADDRESS.into();
        let prepared = prepare_job(job.clone(), &sk, &public_key).unwrap();
        let layout = tx::PhotonLayout::for_age_with_deployment(
            job.age,
            MiningToken::Photon.photon_deployment(job.network),
        )
        .unwrap();
        assert_eq!(prepared.template.len(), 630);
        assert_eq!(layout.shift(), 15);
        assert_eq!(
            &prepared.template[layout.target_offset()..layout.target_offset() + 32],
            &prepared.target,
        );
        let prepared = prepare_job(
            MiningJob {
                age: 128,
                ..job.clone()
            },
            &sk,
            &public_key,
        )
        .unwrap();
        assert_eq!(prepared.template.len(), 631);
        let mut unsupported = job;
        unsupported.age = 32_768;
        assert!(prepare_job(unsupported, &sk, &public_key)
            .err()
            .unwrap()
            .contains("GPU layout shift 17"));
    }

    #[test]
    fn t2_winner_reconstructs_the_actual_reduced_reward() {
        let sk = [0x11; 32];
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(sk).unwrap()).serialize();
        let job = MiningJob {
            target_le_hex: format!("{}7f", "ff".repeat(31)),
            payout_address: "zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3".into(),
            ..integration_job(12)
        };
        let prepared = prepare_job(job.clone(), &sk, &public).unwrap();
        let context = tx::ReferenceJobContext {
            prev_txid: job.baton_txid.clone(),
            prev_vout: job.baton_vout,
            age: job.age,
            target_le_hex: job.target_le_hex.clone(),
            contract_value_sats: job.baton_value_sats,
            relay_fee_sats_per_kb: job.relay_fee_sats_per_kb,
            contract_token_amount: job.token_amount,
            reward_raw: job.reward_raw - 31,
        };
        // The positive proof rule rejects about half the digests.
        let (nonce, signature, tx) = (0..64)
            .find_map(|nonce| {
                let message = tx::photon_message_sha256(nonce, &job.target_le_hex).unwrap();
                let signature = crypto::bch_schnorr_sign(&sk, &message).unwrap();
                tx::apply_reference_signature_for_deployment(
                    &context,
                    &job.payout_address,
                    &hex::encode(public),
                    nonce,
                    &hex::encode(signature),
                    &crate::protocol::MAINNET_PHOTON,
                )
                .ok()
                .map(|tx| (nonce, signature, tx))
            })
            .unwrap();
        let gpu = PhotonCudaWinner {
            nonce,
            digest: hash256(&tx),
            schnorr_k: None,
            tail_j: Some(31),
            tail_value_sats: None,
        };
        let verified = verify_gpu_winner(&prepared, &sk, &public, &gpu).unwrap();
        assert_eq!(verified.transaction, tx);
        assert_eq!(verified.job_reward_raw, job.reward_raw);
        let varied = tx::apply_reference_signature_with_payout_sats(
            &context,
            &job.payout_address,
            &hex::encode(public),
            nonce,
            &hex::encode(signature),
            Some(707),
        )
        .unwrap();
        let gpu_v = PhotonCudaWinner {
            digest: hash256(&varied),
            tail_value_sats: Some(707),
            ..gpu
        };
        let error = verify_gpu_winner(&prepared, &sk, &public, &gpu_v).unwrap_err();
        assert!(error.contains("700"));
    }

    fn integration_job(generation_id: u64) -> MiningJob {
        MiningJob {
            network: MiningNetwork::Mainnet,
            height: 1_000,
            baton_txid: "42a02ec4f58b50f23df4591dcc999ca1bcae2f378997fe6547ae124712000000".into(),
            baton_vout: 0,
            baton_height: 990,
            baton_value_sats: 15_971_500,
            relay_fee_sats_per_kb: 1_000,
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
    fn incremental_search_bounds_and_identity_separation() {
        assert_eq!(batch_before_wrap(u32::MAX - 2, 64), 3);
        assert_eq!(batch_before_wrap(0, 64), 64);
        assert!(crypto::bch_schnorr_sign_search_candidate(&[1; 32], &[2; 32], 0).is_err());
        assert!(
            crypto::bch_schnorr_sign_search_candidate(&[1; 32], &[2; 32], (1u64 << 32) + 1)
                .is_err()
        );
        let sk = [0x11u8; 32];
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(sk).unwrap()).serialize();
        let mut job = integration_job(1);
        job.target_le_hex = format!("{}7f", "ff".repeat(31));
        let mut funded = job.clone();
        funded.payout_address = crate::reward::p2pkh_cashaddr_from_public_key(&public).unwrap();
        assert!(prepare_job(funded, &sk, &public)
            .err()
            .unwrap()
            .contains("separate"));
    }

    #[test]
    fn incremental_live_winner_reconstruction_and_identity_rotation_if_cuda() {
        let sk = [0x11u8; 32];
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(sk).unwrap()).serialize();
        let mut job = integration_job(1);
        job.target_le_hex = format!("{}7f", "ff".repeat(31));
        let mut cuda = match CudaPhotonEngine::new(0, 65, 65) {
            Ok(cuda) => cuda,
            Err(error) if crate::cuda_photon::cuda_unavailable_for_tests(&error) => return,
            Err(error) => panic!("{error}"),
        };
        cuda.enable_incremental_search().unwrap();
        let mut engine = PhotonEngine::Cuda(Box::new(cuda));
        for age in [0, 17, 128, 32_767] {
            job.age = age;
            job.generation_id += 1;
            let prepared = prepare_job(job.clone(), &sk, &public).unwrap();
            engine
                .set_job(&prepared.template, &prepared.target, &sk)
                .unwrap();
            let result = engine.search_batch(u32::MAX - 64, 65).unwrap();
            // Mainnet's positive proof rule admits about half the digests.
            assert!(result.total_winners > 0 && result.total_winners <= 65);
            for winner in &result.winners {
                assert_eq!(winner.nonce, 0);
                assert!(winner.schnorr_k.unwrap() >= (1u64 << 32) - 64);
                let verified = verify_gpu_winner(&prepared, &sk, &public, winner).unwrap();
                assert_eq!(verified.generation_id, job.generation_id);
                let mut wrong = winner.clone();
                wrong.schnorr_k = None;
                assert!(verify_gpu_winner(&prepared, &sk, &public, &wrong).is_err());
            }
            let (next, next_sk, next_public) = rotate_search_identity(&mut engine, &job).unwrap();
            assert_ne!(next_public, public);
            assert!(verify_gpu_winner(&next, &next_sk, &next_public, &result.winners[0]).is_err());
            for winner in engine.search_batch(0, 2).unwrap().winners {
                verify_gpu_winner(&next, &next_sk, &next_public, &winner).unwrap();
            }
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
            diagnostics: Arc::new(WorkerDiagnostics::new()),
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

    // Winners pause the worker. Control/throughput tests must not race them;
    // separate GPU winner tests retain their real targets and verification.
    fn control_job(generation_id: u64) -> MiningJob {
        MiningJob {
            // The smallest positive target: only a zero digest could meet it.
            target_le_hex: format!("01{}", "00".repeat(31)),
            ..integration_job(generation_id)
        }
    }

    #[test]
    fn direct_work_fee_pays_each_recipient_across_controls_if_cuda_present() {
        let _lock = crate::mining_lock::acquire_gpu_lock().unwrap();
        let mut samples = Vec::new();
        for network in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
            let policy = crate::config::MiningToken::Photon.fee_policy(network);
            let mut job = integration_job(1);
            job.network = network;
            job.payout_address =
                crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x42; 20], network).unwrap();
            job.age = 1;
            job.relay_fee_sats_per_kb = if network == MiningNetwork::Mainnet {
                1_100
            } else {
                1_200
            };
            let mut target = [0u8; 32];
            target[28] = 0x80;
            job.target_le_hex = hex::encode(target);
            let payouts = policy.payouts(network, &job.payout_address).unwrap();
            let handle = match SearchHandle::start_with_work_fee(
                BackendKind::Cuda,
                0,
                100,
                job.clone(),
                policy,
            ) {
                Ok(handle) => handle,
                Err(error) if crate::cuda_photon::cuda_unavailable_for_tests(&error) => {
                    eprintln!("skip direct fee CUDA test: {error}");
                    return;
                }
                Err(error) => panic!("{error}"),
            };
            let mut seen = [
                false,
                policy.scheme.work()[0] == 0,
                policy.scheme.work()[1] == 0,
            ];
            let mut changed = false;
            let deadline = Instant::now() + Duration::from_secs(180);
            while Instant::now() < deadline && !seen.iter().all(|seen| *seen) {
                for winner in handle.drain_winners() {
                    let amount = tx::t2_parent_reward_amount(
                        &winner.transaction,
                        job.token_amount,
                        job.reward_raw,
                    )
                    .unwrap();
                    let context = tx::ReferenceJobContext {
                        prev_txid: job.baton_txid.clone(),
                        prev_vout: job.baton_vout,
                        age: job.age,
                        target_le_hex: job.target_le_hex.clone(),
                        contract_value_sats: job.baton_value_sats,
                        relay_fee_sats_per_kb: job.relay_fee_sats_per_kb,
                        contract_token_amount: job.token_amount,
                        reward_raw: amount,
                    };
                    let index = payouts
                        .iter()
                        .position(|payout| {
                            tx::apply_reference_signature_for_deployment(
                                &context,
                                payout,
                                &hex::encode(winner.public_key),
                                winner.nonce,
                                &hex::encode(winner.signature),
                                crate::config::MiningToken::Photon.photon_deployment(network),
                            )
                            .is_ok_and(|raw| raw == winner.transaction)
                        })
                        .expect("GPU winner paid an unauthorized recipient");
                    if !seen[index] {
                        samples.push(serde_json::json!({
                            "raw": hex::encode(&winner.transaction), "network": network.as_str(),
                            "relay_fee_sats_per_kb": job.relay_fee_sats_per_kb,
                            "recipient": format!("{:?}", crate::donation::Recipient::ALL[index]),
                            "payout": payouts[index], "age": 1, "old_target_le": job.target_le_hex,
                            "value": job.baton_value_sats, "amount": job.token_amount.to_string(), "reward": amount.to_string(),
                        }));
                    }
                    seen[index] = true;
                }
                if !changed && handle.snapshot().batches >= 5 {
                    handle.apply_control(RuntimeCommand::Pause).unwrap();
                    let pause_deadline = Instant::now() + Duration::from_secs(20);
                    while handle.batch_in_flight() && Instant::now() < pause_deadline {
                        thread::sleep(Duration::from_millis(5));
                    }
                    assert!(!handle.batch_in_flight());
                    let before = handle.snapshot().work_candidates;
                    job.generation_id += 1;
                    handle.replace_job(job.clone()).unwrap();
                    handle.set_intensity(95).unwrap();
                    assert_eq!(handle.snapshot().work_candidates, before);
                    changed = true;
                }
                handle.apply_control(RuntimeCommand::Resume).unwrap();
                thread::sleep(Duration::from_millis(5));
            }
            let stats = handle.stop();
            assert!(
                seen.iter().all(|seen| *seen),
                "{network:?}: recipients={seen:?} {stats:?}"
            );
            assert_eq!(stats.candidates, stats.work_candidates.iter().sum::<u64>());
            assert_eq!(stats.rejected_winners, 0);
            assert!(stats.last_error.is_none());
            eprintln!(
                "direct fee {network:?}: candidates={} recipient_counts={:?}, rotations={}",
                stats.candidates, stats.work_candidates, stats.key_rotations
            );
        }
        if let Ok(path) = std::env::var("PICKAXE_DIRECT_GPU_PROOF") {
            std::fs::write(path, serde_json::to_vec(&samples).unwrap()).unwrap();
        }
    }

    #[test]
    fn gpu_search_handle_replaces_generation_and_keeps_runtime_controls_if_cuda_present() {
        let handle = match SearchHandle::start(10, control_job(1)) {
            Ok(handle) => handle,
            Err(error) if crate::cuda_photon::cuda_unavailable_for_tests(&error) => {
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

        handle.replace_job(control_job(2)).unwrap();
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
        let handle = match SearchHandle::start_supervised(30, control_job(1)) {
            Ok(handle) => handle,
            Err(error) if crate::cuda_photon::cuda_unavailable_for_tests(&error) => {
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
            "supervised GPU worker did not run consecutive batches: {:?}",
            handle.snapshot()
        );

        let before = handle.snapshot();
        let second_deadline = Instant::now() + Duration::from_secs(20);
        while handle.snapshot().batches == before.batches && Instant::now() < second_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let after = handle.snapshot();
        assert!(
            after.batches > before.batches,
            "supervised worker stalled: before={before:?}, after={after:?}"
        );
        assert!(after.candidates > before.candidates);
        let capacity = if cfg!(feature = "tail-grind") {
            crate::cuda_photon::T2_GROUP_CANDIDATES
        } else {
            CUDA_MAX_BATCH_CANDIDATES
        };
        let expected_batch = u64::from(intensity_batch_candidates(capacity, 30));
        assert_eq!(
            after.candidates,
            after.batches * expected_batch,
            "30% intensity must use bounded work quanta for fine-grained pacing"
        );
        let _ = handle.stop();
    }
}
