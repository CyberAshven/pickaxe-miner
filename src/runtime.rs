//! Presentation-neutral live PHOTON runtime supervision.
//!
//! The authoritative browser reference refreshes BCH height + the mutable
//! PHOTON baton after every GPU batch. This supervisor owns that boundary for
//! both future Ratatui and headless frontends: the CUDA worker cannot start the
//! next supervised batch until fresh Fulcrum state has been checked and any
//! immutable generation change has been applied.

use crate::backend::BackendKind;
use crate::config::RuntimeConfig;
use crate::electrum::{ElectrumSession, LiveJob};
use crate::reward;
use crate::search::VerifiedWinner;
use crate::search::{MiningState, RuntimeCommand as SearchCommand, SearchHandle, SearchStats};
use crate::tx;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const COMMAND_CAP: usize = 16;
const EVENT_CAP: usize = 32;
const SUPERVISOR_POLL: Duration = Duration::from_millis(10);
const RECONNECT_MIN: Duration = Duration::from_millis(400);
const RECONNECT_MAX: Duration = Duration::from_secs(8);
const SUBMISSION_JOURNAL_VERSION: u8 = 1;
const PHOTON_TX_BYTES: usize = 615;
const PHOTON_TARGET_OFFSET: usize = 394;
const VERIFIED_WINNER_DURABILITY_READY: bool = false;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingSubmission {
    version: u8,
    expected_height: u32,
    expected_baton_txid: String,
    expected_baton_vout: u32,
    parent_txid: String,
    parent_hex: String,
    child_txid: String,
    child_hex: String,
}

impl PendingSubmission {
    fn from_verified(
        winner: &VerifiedWinner,
        split: &reward::PreparedRewardSplit,
    ) -> Result<Self, String> {
        let parent_txid = reward::transaction_id(&winner.transaction);
        if parent_txid != split.parent_txid {
            return Err("reward child parent txid does not match verified PHOTON winner".into());
        }
        let pending = Self {
            version: SUBMISSION_JOURNAL_VERSION,
            expected_height: winner.height,
            expected_baton_txid: winner.baton_txid.clone(),
            expected_baton_vout: winner.baton_vout,
            parent_txid,
            parent_hex: hex::encode(&winner.transaction),
            child_txid: split.child_txid.clone(),
            child_hex: hex::encode(&split.raw_child),
        };
        pending.validate()?;
        Ok(pending)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != SUBMISSION_JOURNAL_VERSION {
            return Err(format!(
                "unsupported pending-submission journal version {}",
                self.version
            ));
        }
        if self.expected_baton_txid.len() != 64
            || !self
                .expected_baton_txid
                .chars()
                .all(|value| value.is_ascii_hexdigit())
        {
            return Err("pending submission has invalid PHOTON baton txid".into());
        }
        for (label, expected, raw_hex) in [
            (
                "parent",
                self.parent_txid.as_str(),
                self.parent_hex.as_str(),
            ),
            ("child", self.child_txid.as_str(), self.child_hex.as_str()),
        ] {
            if expected.len() != 64 || !expected.chars().all(|value| value.is_ascii_hexdigit()) {
                return Err(format!("pending submission has invalid {label} txid"));
            }
            let raw = hex::decode(raw_hex)
                .map_err(|error| format!("pending submission {label} hex: {error}"))?;
            if raw.is_empty() {
                return Err(format!("pending submission {label} transaction is empty"));
            }
            let actual = reward::transaction_id(&raw);
            if !actual.eq_ignore_ascii_case(expected) {
                return Err(format!(
                    "pending submission {label} txid does not match journaled bytes"
                ));
            }
        }
        Ok(())
    }

    fn matches_live(&self, live: &LiveJob) -> bool {
        self.expected_height == live.height
            && self.expected_baton_txid == live.baton_txid
            && self.expected_baton_vout == live.baton_vout
    }

    fn load(path: &Path) -> Result<Option<Self>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "read pending-submission journal {}: {error}",
                    path.display()
                ))
            }
        };
        let pending: Self = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "parse pending-submission journal {}: {error}",
                path.display()
            )
        })?;
        pending.validate()?;
        Ok(Some(pending))
    }

    fn persist_new(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let parent = path
            .parent()
            .ok_or("pending-submission journal path has no parent directory")?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create pending-submission journal directory {}: {error}",
                parent.display()
            )
        })?;
        if path.exists() {
            return Err(format!(
                "pending-submission journal already exists at {}",
                path.display()
            ));
        }

        let temporary = path.with_extension("json.tmp");
        if temporary.exists() {
            fs::remove_file(&temporary).map_err(|error| {
                format!(
                    "remove stale pending-submission temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize pending-submission journal: {error}"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                format!(
                    "create pending-submission temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "write pending-submission journal {}: {error}",
                temporary.display()
            ));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "commit pending-submission journal {}: {error}",
                path.display()
            ));
        }
        Ok(())
    }

    fn remove(path: &Path) -> Result<(), String> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove pending-submission journal {}: {error}",
                path.display()
            )),
        }
    }
}

fn submission_journal_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(base)
            .join("Pickaxe Miner")
            .join("pending-reward.json");
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(base) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(base)
                .join("pickaxe-miner")
                .join("pending-reward.json");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("pickaxe-miner")
                .join("pending-reward.json");
        }
    }

    std::env::temp_dir()
        .join("pickaxe-miner")
        .join("pending-reward.json")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmissionDecision {
    Complete,
    BroadcastChild,
    BroadcastParentThenChild,
    StaleUnbroadcast,
}

fn submission_decision(
    parent_known: bool,
    child_known: bool,
    live_matches_expected: bool,
) -> SubmissionDecision {
    if child_known {
        SubmissionDecision::Complete
    } else if parent_known {
        SubmissionDecision::BroadcastChild
    } else if live_matches_expected {
        SubmissionDecision::BroadcastParentThenChild
    } else {
        SubmissionDecision::StaleUnbroadcast
    }
}

enum SubmissionAttempt {
    Complete,
    StaleUnbroadcast(LiveJob),
}

fn ensure_broadcast_txid(label: &str, expected: &str, returned: &str) -> Result<(), String> {
    if returned.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "{label} broadcast returned unexpected txid {returned}; expected {expected}"
        ))
    }
}

fn attempt_pending_submission(
    session: &mut ElectrumSession,
    pending: &PendingSubmission,
) -> Result<SubmissionAttempt, String> {
    pending.validate()?;

    let child_known = session.transaction_known(&pending.child_txid)?;
    if child_known {
        return Ok(SubmissionAttempt::Complete);
    }

    let parent_known = session.transaction_known(&pending.parent_txid)?;
    if parent_known {
        let returned = session.broadcast_raw(&pending.child_hex)?;
        ensure_broadcast_txid("reward child", &pending.child_txid, &returned)?;
        return Ok(SubmissionAttempt::Complete);
    }

    let fresh = session.fetch_live_job()?;
    if submission_decision(false, false, pending.matches_live(&fresh))
        == SubmissionDecision::StaleUnbroadcast
    {
        return Ok(SubmissionAttempt::StaleUnbroadcast(fresh));
    }

    let returned_parent = session.broadcast_raw(&pending.parent_hex)?;
    ensure_broadcast_txid("PHOTON parent", &pending.parent_txid, &returned_parent)?;

    let returned_child = session.broadcast_raw(&pending.child_hex)?;
    ensure_broadcast_txid("reward child", &pending.child_txid, &returned_child)?;
    Ok(SubmissionAttempt::Complete)
}

fn prepare_pending_submission(
    session: &mut ElectrumSession,
    winner: &VerifiedWinner,
    cfg: &RuntimeConfig,
    live: &LiveJob,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    journal_path: &Path,
) -> Result<PendingSubmission, String> {
    if !winner_matches_live(winner, cfg.generation_id, live) {
        return Err("verified winner is stale before reward-child preparation".into());
    }
    validate_verified_parent(winner, live, reward_public_key)?;
    let sponsor = session.fetch_sponsor_reserve(&winner.baton_txid)?;
    let split = reward::build_reward_split_child(
        &winner.transaction,
        &winner.baton_txid,
        reward_secret,
        reward_public_key,
        &cfg.payout_address,
        live.reward_raw,
        &sponsor,
    )?;
    let pending = PendingSubmission::from_verified(winner, &split)?;
    pending.persist_new(journal_path)?;
    Ok(pending)
}

fn resolve_pending_before_search(
    session: &mut ElectrumSession,
    journal_path: &Path,
) -> Result<(), String> {
    let Some(pending) = PendingSubmission::load(journal_path)? else {
        return Ok(());
    };

    match attempt_pending_submission(session, &pending)? {
        SubmissionAttempt::Complete | SubmissionAttempt::StaleUnbroadcast(_) => {
            PendingSubmission::remove(journal_path)?;
            Ok(())
        }
    }
}

fn probe_submission_journal(journal_path: &Path) -> Result<(), String> {
    if journal_path.exists() {
        return Err(format!(
            "unresolved previous winner submission exists at {}",
            journal_path.display()
        ));
    }
    let parent = journal_path
        .parent()
        .ok_or("pending-submission journal path has no parent directory")?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "create pending-submission journal directory {}: {error}",
            parent.display()
        )
    })?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("submission journal clock error: {error}"))?
        .as_nanos();
    let stem = format!(".pickaxe-preflight-{}-{stamp}", std::process::id());
    let temporary = parent.join(format!("{stem}.tmp"));
    let committed = parent.join(format!("{stem}.ok"));

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                format!(
                    "create submission-journal preflight file {}: {error}",
                    temporary.display()
                )
            })?;
        file.write_all(b"pickaxe submission journal preflight\n")
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                format!(
                    "write submission-journal preflight file {}: {error}",
                    temporary.display()
                )
            })?;
        drop(file);
        fs::rename(&temporary, &committed).map_err(|error| {
            format!(
                "commit submission-journal preflight file {}: {error}",
                committed.display()
            )
        })?;
        fs::remove_file(&committed).map_err(|error| {
            format!(
                "remove submission-journal preflight file {}: {error}",
                committed.display()
            )
        })?;
        Ok(())
    })();

    let _ = fs::remove_file(&temporary);
    let _ = fs::remove_file(&committed);
    result
}

fn production_preflight(
    session: &mut ElectrumSession,
    cfg: &RuntimeConfig,
    live: &LiveJob,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    mining_payout_address: &str,
    journal_path: &Path,
) -> Result<(), String> {
    let sponsor = session.fetch_sponsor_reserve(&live.baton_txid)?;

    if !session.transaction_known(&live.baton_txid)? {
        return Err(
            "production preflight cannot retrieve the live PHOTON baton transaction".into(),
        );
    }
    if !session.transaction_known(&sponsor.txid)? {
        return Err("production preflight cannot retrieve the sponsor-reserve transaction".into());
    }

    production_preflight_local(
        cfg,
        live,
        reward_secret,
        reward_public_key,
        mining_payout_address,
        &sponsor,
        journal_path,
    )
}

fn require_complete_live_winner_lifecycle() -> Result<(), String> {
    if VERIFIED_WINNER_DURABILITY_READY {
        Ok(())
    } else {
        Err(
            "production mining is disabled until every verified GPU winner is durably recoverable before network-dependent settlement; live GPU search was not started"
                .into(),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn production_preflight_local(
    cfg: &RuntimeConfig,
    live: &LiveJob,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    mining_payout_address: &str,
    sponsor: &reward::SponsorReserve,
    journal_path: &Path,
) -> Result<(), String> {
    if cfg.payout_address.trim().is_empty() {
        return Err("mining payout address is required".into());
    }
    tx::cashaddr_to_p2pkh_locking(&cfg.payout_address)
        .map_err(|error| format!("production payout validation failed: {error}"))?;
    tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)
        .map_err(|error| format!("compiled donation address is invalid: {error}"))?;
    if crate::config::DONATION_BPS != 200 {
        return Err("compiled donation policy must be exactly 200 basis points".into());
    }

    probe_submission_journal(journal_path)?;

    let context = tx::ReferenceJobContext {
        prev_txid: live.baton_txid.clone(),
        prev_vout: live.baton_vout,
        age: live.age,
        target_le_hex: live.target_le_hex.clone(),
        contract_value_sats: live.baton_value_sats,
        contract_token_amount: live.token_amount,
        reward_raw: live.reward_raw,
    };
    let parent_preview = tx::build_unsigned_reference_preview(&context, mining_payout_address)?;
    if parent_preview.len() != PHOTON_TX_BYTES {
        return Err(format!(
            "PHOTON parent builder produced {} bytes; expected {PHOTON_TX_BYTES}",
            parent_preview.len()
        ));
    }
    let target = crate::search::parse_hex32(&live.target_le_hex)?;
    if parent_preview[PHOTON_TARGET_OFFSET..PHOTON_TARGET_OFFSET + 32] != target {
        return Err("PHOTON parent builder target placement disagrees with the live target".into());
    }

    let split = reward::build_reward_split_child(
        &parent_preview,
        &live.baton_txid,
        reward_secret,
        reward_public_key,
        &cfg.payout_address,
        live.reward_raw,
        sponsor,
    )?;
    let (expected_miner, expected_donation) = RuntimeConfig::split_reward(live.reward_raw);
    if split.miner_token_amount != expected_miner
        || split.donation_token_amount != expected_donation
        || split
            .miner_token_amount
            .checked_add(split.donation_token_amount)
            != Some(live.reward_raw)
    {
        return Err("reward-child preflight failed exact 98/2 token conservation".into());
    }

    Ok(())
}

fn validate_verified_parent(
    winner: &VerifiedWinner,
    live: &LiveJob,
    reward_public_key: &[u8; 33],
) -> Result<(), String> {
    let intermediate_payout = reward::p2pkh_cashaddr_from_public_key(reward_public_key)?;
    let context = tx::ReferenceJobContext {
        prev_txid: live.baton_txid.clone(),
        prev_vout: live.baton_vout,
        age: live.age,
        target_le_hex: live.target_le_hex.clone(),
        contract_value_sats: live.baton_value_sats,
        contract_token_amount: live.token_amount,
        reward_raw: live.reward_raw,
    };
    let rebuilt = tx::apply_reference_signature(
        &context,
        &intermediate_payout,
        &hex::encode(winner.public_key),
        winner.nonce,
        &hex::encode(winner.signature),
    )?;
    if rebuilt != winner.transaction {
        return Err("verified PHOTON parent bytes do not match current live job".into());
    }
    if crate::search::hash256(&rebuilt) != winner.digest {
        return Err("verified PHOTON parent digest does not match reconstructed bytes".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorState {
    Mining,
    Paused,
    Reconnecting,
    Error,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    pub state: SupervisorState,
    pub gpu_backend: String,
    pub gpu_device: u32,
    pub generation_id: u64,
    pub payout_address: String,
    pub endpoint: String,
    pub height: u32,
    pub baton_txid: String,
    pub baton_vout: u32,
    pub refreshes: u64,
    pub stale_rebuilds: u64,
    pub reconnects: u64,
    pub stale_winners: u64,
    pub verified_winners: u64,
    pub pending_winners: u64,
    pub last_error: Option<String>,
    pub search: SearchStats,
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    JobRefreshed {
        generation_id: u64,
        height: u32,
        baton_txid: String,
        baton_vout: u32,
        changed: bool,
    },
    Reconnecting(String),
    Reconnected(String),
    StaleWinner {
        winner_generation: u64,
        current_generation: u64,
    },
    VerifiedWinner(VerifiedWinner),
    SubmissionAccepted {
        parent_txid: String,
        child_txid: String,
    },
    Error(String),
}

#[allow(dead_code)]
enum SupervisorCommand {
    SetIntensity(u8, SyncSender<Result<(), String>>),
    Pause(SyncSender<Result<(), String>>),
    Resume(SyncSender<Result<(), String>>),
    SetPayout(String, SyncSender<Result<(), String>>),
    SetFulcrum(Option<String>, SyncSender<Result<(), String>>),
    Reconnect(SyncSender<Result<(), String>>),
    Stop,
}

pub struct RuntimeSupervisor {
    command_tx: SyncSender<SupervisorCommand>,
    event_rx: Receiver<RuntimeEvent>,
    snapshot: Arc<Mutex<RuntimeSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl RuntimeSupervisor {
    #[allow(dead_code)]
    pub fn start_on_device(cfg: RuntimeConfig, device_ordinal: u32) -> Result<Self, String> {
        Self::start_on_backend_device(cfg, BackendKind::Cuda, device_ordinal)
    }

    pub fn start_on_backend_device(
        cfg: RuntimeConfig,
        backend: BackendKind,
        device_ordinal: u32,
    ) -> Result<Self, String> {
        Self::start_inner(cfg, backend, device_ordinal)
    }

    fn start_inner(
        mut cfg: RuntimeConfig,
        backend: BackendKind,
        device_ordinal: u32,
    ) -> Result<Self, String> {
        if cfg.payout_address.trim().is_empty() {
            return Err("mining payout address is required".into());
        }
        require_complete_live_winner_lifecycle()?;

        let endpoints = cfg.electrum_endpoints();
        let mut session = ElectrumSession::connect_failover(&endpoints)?;
        let journal_path = submission_journal_path();
        resolve_pending_before_search(&mut session, &journal_path)?;
        let initial = session.fetch_live_job()?;
        let (reward_secret, reward_public_key, mining_payout_address) =
            reward::new_intermediate_identity()?;
        production_preflight(
            &mut session,
            &cfg,
            &initial,
            &reward_secret,
            &reward_public_key,
            &mining_payout_address,
            &journal_path,
        )?;
        cfg.bump_generation();

        let initial_job = initial.to_mining_job(cfg.generation_id, &mining_payout_address);
        let search = SearchHandle::start_supervised_on_backend_device(
            backend,
            device_ordinal as usize,
            cfg.intensity,
            initial_job,
        )?;
        let initial_search = search.snapshot();
        let initial_snapshot = RuntimeSnapshot {
            state: SupervisorState::Mining,
            gpu_backend: backend.as_str().into(),
            gpu_device: device_ordinal,
            generation_id: cfg.generation_id,
            payout_address: cfg.payout_address.clone(),
            endpoint: initial.url.clone(),
            height: initial.height,
            baton_txid: initial.baton_txid.clone(),
            baton_vout: initial.baton_vout,
            refreshes: 0,
            stale_rebuilds: 0,
            reconnects: 0,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: initial_search,
        };

        let snapshot = Arc::new(Mutex::new(initial_snapshot));
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAP);
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_CAP);
        let worker_snapshot = Arc::clone(&snapshot);

        let worker = thread::Builder::new()
            .name("pickaxe-live-supervisor".into())
            .spawn(move || {
                run_supervisor(
                    cfg,
                    endpoints,
                    initial,
                    session,
                    search,
                    reward_secret,
                    reward_public_key,
                    mining_payout_address,
                    journal_path,
                    None,
                    command_rx,
                    event_tx,
                    worker_snapshot,
                )
            })
            .map_err(|error| format!("start live PHOTON supervisor: {error}"))?;

        Ok(Self {
            command_tx,
            event_rx,
            snapshot,
            worker: Some(worker),
        })
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn drain_events(&self) -> Vec<RuntimeEvent> {
        self.event_rx.try_iter().collect()
    }

    #[allow(dead_code)]
    pub fn set_intensity(&self, value: u8) -> Result<(), String> {
        self.request(|reply| SupervisorCommand::SetIntensity(value, reply))
    }

    #[allow(dead_code)]
    pub fn pause(&self) -> Result<(), String> {
        self.request(SupervisorCommand::Pause)
    }

    #[allow(dead_code)]
    pub fn resume(&self) -> Result<(), String> {
        self.request(SupervisorCommand::Resume)
    }

    #[allow(dead_code)]
    pub fn set_payout(&self, payout: String) -> Result<(), String> {
        self.request(|reply| SupervisorCommand::SetPayout(payout, reply))
    }

    #[allow(dead_code)]
    pub fn set_fulcrum_endpoint(&self, url: String) -> Result<(), String> {
        self.request(|reply| SupervisorCommand::SetFulcrum(Some(url), reply))
    }

    #[allow(dead_code)]
    pub fn clear_fulcrum_endpoint(&self) -> Result<(), String> {
        self.request(|reply| SupervisorCommand::SetFulcrum(None, reply))
    }

    pub fn reconnect(&self) -> Result<(), String> {
        self.request(SupervisorCommand::Reconnect)
    }

    #[allow(dead_code)]
    fn request(
        &self,
        build: impl FnOnce(SyncSender<Result<(), String>>) -> SupervisorCommand,
    ) -> Result<(), String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.command_tx
            .send(build(reply_tx))
            .map_err(|_| "live PHOTON supervisor is not running".to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| "timed out waiting for live PHOTON supervisor".to_string())?
    }

    pub fn stop(mut self) -> RuntimeSnapshot {
        let _ = self.command_tx.send(SupervisorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.snapshot()
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(SupervisorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_supervisor(
    mut cfg: RuntimeConfig,
    mut endpoints: Vec<String>,
    mut live: LiveJob,
    initial_session: ElectrumSession,
    search: SearchHandle,
    mut reward_secret: [u8; 32],
    reward_public_key: [u8; 33],
    mining_payout_address: String,
    journal_path: PathBuf,
    mut pending_submission: Option<PendingSubmission>,
    command_rx: Receiver<SupervisorCommand>,
    event_tx: SyncSender<RuntimeEvent>,
    shared_snapshot: Arc<Mutex<RuntimeSnapshot>>,
) {
    let mut session = Some(initial_session);
    let mut state = if pending_submission.is_some() {
        SupervisorState::Paused
    } else {
        SupervisorState::Mining
    };
    let mut user_paused = false;
    let mut pending_winner: Option<VerifiedWinner> = None;
    let mut refreshes = 0u64;
    let mut stale_rebuilds = 0u64;
    let mut reconnects = 0u64;
    let mut stale_winners = 0u64;
    let mut verified_winners = 0u64;
    let mut pending_winners = u64::from(pending_submission.is_some());
    let mut last_error = None;
    let mut reconnect_backoff = RECONNECT_MIN;
    let mut next_reconnect = Instant::now();
    let mut submission_backoff = RECONNECT_MIN;
    let mut next_submission_retry = Instant::now();
    let mut stop = false;

    while !stop {
        loop {
            match command_rx.try_recv() {
                Ok(SupervisorCommand::SetIntensity(value, reply)) => {
                    let result = cfg
                        .set_intensity(value)
                        .and_then(|()| search.set_intensity(value));
                    let _ = reply.send(result);
                }
                Ok(SupervisorCommand::Pause(reply)) => {
                    user_paused = true;
                    let result = search.apply_control(SearchCommand::Pause).map(|_| ());
                    if result.is_ok() {
                        state = SupervisorState::Paused;
                    }
                    let _ = reply.send(result);
                }
                Ok(SupervisorCommand::Resume(reply)) => {
                    user_paused = false;
                    let result = if session.is_none() {
                        Err("cannot resume while authoritative PHOTON state is disconnected".into())
                    } else if pending_winners > 0 {
                        Err("cannot resume while a verified winner is pending handling".into())
                    } else {
                        search.apply_control(SearchCommand::Resume).map(|_| ())
                    };
                    if result.is_ok() {
                        state = SupervisorState::Mining;
                    }
                    let _ = reply.send(result);
                }
                Ok(SupervisorCommand::SetPayout(payout, reply)) => {
                    if pending_winners != 0 {
                        let _ = reply.send(Err(
                            "payout change unavailable while a verified winner is pending".into(),
                        ));
                        continue;
                    }
                    let before = cfg.generation_id;
                    let result = cfg.set_payout(payout).and_then(|()| {
                        if cfg.generation_id != before {
                            search.replace_job(
                                live.to_mining_job(cfg.generation_id, &mining_payout_address),
                            )?;
                            stale_rebuilds = stale_rebuilds.saturating_add(1);
                        }
                        Ok(())
                    });
                    let _ = reply.send(result);
                }
                Ok(SupervisorCommand::SetFulcrum(endpoint, reply)) => {
                    if pending_winners != 0 {
                        let _ = reply.send(Err(
                            "endpoint change unavailable while runtime work is pending".into(),
                        ));
                        continue;
                    }
                    let result =
                        match prepare_fulcrum_endpoint_change(&cfg, endpoint.as_deref()) {
                            Err(error) => Err(error),
                            Ok(None) => Ok(()),
                            Ok(Some((next_cfg, next_endpoints))) => search
                                .apply_control(SearchCommand::Pause)
                                .map(|_| ())
                                .and_then(|()| {
                                    search.replace_job(live.to_mining_job(
                                        next_cfg.generation_id,
                                        &mining_payout_address,
                                    ))
                                })
                                .map(|()| {
                                    cfg = next_cfg;
                                    endpoints = next_endpoints;
                                    stale_rebuilds = stale_rebuilds.saturating_add(1);
                                    session = None;
                                    state = SupervisorState::Reconnecting;
                                    last_error = None;
                                    reconnect_backoff = RECONNECT_MIN;
                                    next_reconnect = Instant::now();
                                    emit(
                                        &event_tx,
                                        RuntimeEvent::Reconnecting(
                                            "Fulcrum endpoint changed; reconnecting".into(),
                                        ),
                                    );
                                }),
                        };
                    let _ = reply.send(result);
                }
                Ok(SupervisorCommand::Reconnect(reply)) => {
                    let result = if pending_winners != 0 {
                        Err("reconnect unavailable while runtime work is pending".into())
                    } else {
                        search
                            .apply_control(SearchCommand::Pause)
                            .map(|_| ())
                            .map(|()| {
                                session = None;
                                state = SupervisorState::Reconnecting;
                                last_error = None;
                                reconnect_backoff = RECONNECT_MIN;
                                next_reconnect = Instant::now();
                                emit(
                                    &event_tx,
                                    RuntimeEvent::Reconnecting(
                                        "manual Fulcrum reconnect requested".into(),
                                    ),
                                );
                            })
                    };
                    let _ = reply.send(result);
                }
                Ok(SupervisorCommand::Stop) | Err(TryRecvError::Disconnected) => {
                    stop = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if stop {
            break;
        }

        if session.is_none() {
            if Instant::now() >= next_reconnect {
                match ElectrumSession::connect_failover(&endpoints)
                    .and_then(|mut next| next.fetch_live_job().map(|job| (next, job)))
                {
                    Ok((next_session, next_job)) => {
                        reconnects = reconnects.saturating_add(1);
                        reconnect_backoff = RECONNECT_MIN;
                        last_error = None;
                        session = Some(next_session);
                        match apply_refreshed_job(
                            &mut cfg,
                            &mut live,
                            &search,
                            &mining_payout_address,
                            next_job,
                        ) {
                            Ok(changed) => {
                                if changed {
                                    stale_rebuilds = stale_rebuilds.saturating_add(1);
                                }
                                emit(&event_tx, RuntimeEvent::Reconnected(live.url.clone()));
                                if search.refresh_required() {
                                    let _ = search.complete_refresh();
                                }
                                if !user_paused && pending_winners == 0 {
                                    let _ = search.apply_control(SearchCommand::Resume);
                                    state = SupervisorState::Mining;
                                } else {
                                    state = SupervisorState::Paused;
                                }
                            }
                            Err(error) => {
                                last_error = Some(error.clone());
                                state = SupervisorState::Error;
                                emit(&event_tx, RuntimeEvent::Error(error));
                            }
                        }
                    }
                    Err(error) => {
                        last_error = Some(error.clone());
                        state = SupervisorState::Reconnecting;
                        emit(&event_tx, RuntimeEvent::Reconnecting(error));
                        next_reconnect = Instant::now() + reconnect_backoff;
                        reconnect_backoff = reconnect_backoff
                            .checked_mul(2)
                            .unwrap_or(RECONNECT_MAX)
                            .min(RECONNECT_MAX);
                    }
                }
            }
        } else if pending_winner.is_some() {
            if Instant::now() >= next_submission_retry {
                let refreshed = session
                    .as_mut()
                    .expect("checked session above")
                    .fetch_live_job();
                match refreshed {
                    Ok(next_job) => {
                        refreshes = refreshes.saturating_add(1);
                        let winner_is_fresh = pending_winner.as_ref().is_some_and(|winner| {
                            !live_job_changed(&live, &next_job)
                                && winner_matches_live(winner, cfg.generation_id, &next_job)
                        });
                        if !winner_is_fresh {
                            let winner = pending_winner.take().expect("checked above");
                            stale_winners = stale_winners.saturating_add(1);
                            pending_winners = 0;
                            emit(
                                &event_tx,
                                RuntimeEvent::StaleWinner {
                                    winner_generation: winner.generation_id,
                                    current_generation: cfg.generation_id,
                                },
                            );
                            match apply_refreshed_job(
                                &mut cfg,
                                &mut live,
                                &search,
                                &mining_payout_address,
                                next_job,
                            ) {
                                Ok(changed) => {
                                    if changed {
                                        stale_rebuilds = stale_rebuilds.saturating_add(1);
                                    }
                                    last_error = None;
                                    let _ = search.complete_refresh();
                                    if !user_paused {
                                        let _ = search.apply_control(SearchCommand::Resume);
                                        state = SupervisorState::Mining;
                                    }
                                }
                                Err(error) => {
                                    last_error = Some(error.clone());
                                    state = SupervisorState::Error;
                                    emit(&event_tx, RuntimeEvent::Error(error));
                                }
                            }
                        } else {
                            live = next_job;
                            let winner = pending_winner.as_ref().expect("checked above");
                            match prepare_pending_submission(
                                session.as_mut().expect("checked session above"),
                                winner,
                                &cfg,
                                &live,
                                &reward_secret,
                                &reward_public_key,
                                &journal_path,
                            ) {
                                Ok(pending) => {
                                    pending_submission = Some(pending);
                                    pending_winner = None;
                                    last_error = None;
                                    submission_backoff = RECONNECT_MIN;
                                    next_submission_retry = Instant::now();
                                    state = SupervisorState::Paused;
                                }
                                Err(error) => {
                                    last_error = Some(error.clone());
                                    state = SupervisorState::Reconnecting;
                                    emit(
                                        &event_tx,
                                        RuntimeEvent::Reconnecting(format!(
                                            "reward-child preparation retry: {error}"
                                        )),
                                    );
                                    let _ = search.apply_control(SearchCommand::Pause);
                                    session = None;
                                    next_reconnect = Instant::now() + submission_backoff;
                                    submission_backoff = submission_backoff
                                        .checked_mul(2)
                                        .unwrap_or(RECONNECT_MAX)
                                        .min(RECONNECT_MAX);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        last_error = Some(error.clone());
                        state = SupervisorState::Reconnecting;
                        emit(&event_tx, RuntimeEvent::Reconnecting(error));
                        let _ = search.apply_control(SearchCommand::Pause);
                        session = None;
                        next_reconnect = Instant::now() + submission_backoff;
                        submission_backoff = submission_backoff
                            .checked_mul(2)
                            .unwrap_or(RECONNECT_MAX)
                            .min(RECONNECT_MAX);
                    }
                }
            }
        } else if pending_submission.is_some() {
            if Instant::now() >= next_submission_retry {
                let pending = pending_submission.as_ref().expect("checked above").clone();
                let attempt = attempt_pending_submission(
                    session.as_mut().expect("checked session above"),
                    &pending,
                );
                match attempt {
                    Ok(SubmissionAttempt::Complete) => {
                        match PendingSubmission::remove(&journal_path) {
                            Ok(()) => {
                                emit(
                                    &event_tx,
                                    RuntimeEvent::SubmissionAccepted {
                                        parent_txid: pending.parent_txid,
                                        child_txid: pending.child_txid,
                                    },
                                );
                                pending_submission = None;
                                pending_winners = 0;
                                last_error = None;
                                submission_backoff = RECONNECT_MIN;
                                next_submission_retry = Instant::now();

                                let refreshed = session
                                    .as_mut()
                                    .expect("checked session above")
                                    .fetch_live_job();
                                match refreshed.and_then(|next_job| {
                                    apply_refreshed_job(
                                        &mut cfg,
                                        &mut live,
                                        &search,
                                        &mining_payout_address,
                                        next_job,
                                    )
                                }) {
                                    Ok(changed) => {
                                        if changed {
                                            stale_rebuilds = stale_rebuilds.saturating_add(1);
                                        }
                                        let _ = search.complete_refresh();
                                        if user_paused {
                                            state = SupervisorState::Paused;
                                        } else {
                                            let _ = search.apply_control(SearchCommand::Resume);
                                            state = SupervisorState::Mining;
                                        }
                                    }
                                    Err(error) => {
                                        last_error = Some(error.clone());
                                        state = SupervisorState::Reconnecting;
                                        emit(&event_tx, RuntimeEvent::Reconnecting(error));
                                        session = None;
                                        next_reconnect = Instant::now() + reconnect_backoff;
                                    }
                                }
                            }
                            Err(error) => {
                                last_error = Some(error.clone());
                                state = SupervisorState::Error;
                                emit(&event_tx, RuntimeEvent::Error(error));
                                next_submission_retry = Instant::now() + submission_backoff;
                                submission_backoff = submission_backoff
                                    .checked_mul(2)
                                    .unwrap_or(RECONNECT_MAX)
                                    .min(RECONNECT_MAX);
                            }
                        }
                    }
                    Ok(SubmissionAttempt::StaleUnbroadcast(next_job)) => {
                        match PendingSubmission::remove(&journal_path) {
                            Ok(()) => {
                                pending_submission = None;
                                pending_winners = 0;
                                stale_winners = stale_winners.saturating_add(1);
                                last_error = None;
                                match apply_refreshed_job(
                                    &mut cfg,
                                    &mut live,
                                    &search,
                                    &mining_payout_address,
                                    next_job,
                                ) {
                                    Ok(changed) => {
                                        if changed {
                                            stale_rebuilds = stale_rebuilds.saturating_add(1);
                                        }
                                        let _ = search.complete_refresh();
                                        if user_paused {
                                            state = SupervisorState::Paused;
                                        } else {
                                            let _ = search.apply_control(SearchCommand::Resume);
                                            state = SupervisorState::Mining;
                                        }
                                    }
                                    Err(error) => {
                                        last_error = Some(error.clone());
                                        state = SupervisorState::Error;
                                        emit(&event_tx, RuntimeEvent::Error(error));
                                    }
                                }
                            }
                            Err(error) => {
                                last_error = Some(error.clone());
                                state = SupervisorState::Error;
                                emit(&event_tx, RuntimeEvent::Error(error));
                                next_submission_retry = Instant::now() + submission_backoff;
                            }
                        }
                    }
                    Err(error) => {
                        last_error = Some(error.clone());
                        state = SupervisorState::Reconnecting;
                        emit(
                            &event_tx,
                            RuntimeEvent::Reconnecting(format!("winner submission retry: {error}")),
                        );
                        let _ = search.apply_control(SearchCommand::Pause);
                        session = None;
                        next_reconnect = Instant::now() + submission_backoff;
                        submission_backoff = submission_backoff
                            .checked_mul(2)
                            .unwrap_or(RECONNECT_MAX)
                            .min(RECONNECT_MAX);
                    }
                }
            }
        } else if search.refresh_required() {
            let refreshed = session
                .as_mut()
                .expect("checked session above")
                .fetch_live_job();
            match refreshed {
                Ok(next_job) => {
                    refreshes = refreshes.saturating_add(1);
                    match apply_refreshed_job(
                        &mut cfg,
                        &mut live,
                        &search,
                        &mining_payout_address,
                        next_job,
                    ) {
                        Ok(changed) => {
                            if changed {
                                stale_rebuilds = stale_rebuilds.saturating_add(1);
                            }
                            emit(
                                &event_tx,
                                RuntimeEvent::JobRefreshed {
                                    generation_id: cfg.generation_id,
                                    height: live.height,
                                    baton_txid: live.baton_txid.clone(),
                                    baton_vout: live.baton_vout,
                                    changed,
                                },
                            );

                            for winner in search.drain_winners() {
                                if winner_matches_live(&winner, cfg.generation_id, &live) {
                                    verified_winners = verified_winners.saturating_add(1);
                                    pending_winners = 1;
                                    let _ = search.apply_control(SearchCommand::Pause);
                                    state = SupervisorState::Paused;
                                    emit(&event_tx, RuntimeEvent::VerifiedWinner(winner.clone()));
                                    match prepare_pending_submission(
                                        session.as_mut().expect("checked session above"),
                                        &winner,
                                        &cfg,
                                        &live,
                                        &reward_secret,
                                        &reward_public_key,
                                        &journal_path,
                                    ) {
                                        Ok(pending) => {
                                            pending_submission = Some(pending);
                                            submission_backoff = RECONNECT_MIN;
                                            next_submission_retry = Instant::now();
                                            last_error = None;
                                        }
                                        Err(error) => {
                                            pending_winner = Some(winner);
                                            last_error = Some(error.clone());
                                            state = SupervisorState::Reconnecting;
                                            emit(
                                                &event_tx,
                                                RuntimeEvent::Reconnecting(format!(
                                                    "reward-child preparation retry: {error}"
                                                )),
                                            );
                                            session = None;
                                            next_reconnect = Instant::now() + submission_backoff;
                                            submission_backoff = submission_backoff
                                                .checked_mul(2)
                                                .unwrap_or(RECONNECT_MAX)
                                                .min(RECONNECT_MAX);
                                        }
                                    }
                                    break;
                                } else {
                                    stale_winners = stale_winners.saturating_add(1);
                                    emit(
                                        &event_tx,
                                        RuntimeEvent::StaleWinner {
                                            winner_generation: winner.generation_id,
                                            current_generation: cfg.generation_id,
                                        },
                                    );
                                }
                            }

                            let _ = search.complete_refresh();
                            if pending_winners == 0 {
                                last_error = None;
                            }
                            if session.is_some() && !user_paused && pending_winners == 0 {
                                let _ = search.apply_control(SearchCommand::Resume);
                                state = SupervisorState::Mining;
                            }
                        }
                        Err(error) => {
                            last_error = Some(error.clone());
                            state = SupervisorState::Error;
                            let _ = search.apply_control(SearchCommand::Pause);
                            emit(&event_tx, RuntimeEvent::Error(error));
                        }
                    }
                }
                Err(error) => {
                    last_error = Some(error.clone());
                    state = SupervisorState::Reconnecting;
                    let _ = search.apply_control(SearchCommand::Pause);
                    emit(&event_tx, RuntimeEvent::Reconnecting(error));
                    session = None;
                    next_reconnect = Instant::now() + reconnect_backoff;
                }
            }
        }

        write_snapshot(
            &shared_snapshot,
            state,
            &cfg,
            &live,
            &search,
            refreshes,
            stale_rebuilds,
            reconnects,
            stale_winners,
            verified_winners,
            pending_winners,
            last_error.clone(),
        );
        thread::sleep(SUPERVISOR_POLL);
    }

    let final_stats = search.stop();
    reward_secret.fill(0);
    let mut snapshot = shared_snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot.state = SupervisorState::Stopped;
    snapshot.search = final_stats;
}

fn apply_refreshed_job(
    cfg: &mut RuntimeConfig,
    live: &mut LiveJob,
    search: &SearchHandle,
    mining_payout_address: &str,
    next: LiveJob,
) -> Result<bool, String> {
    let changed = live_job_changed(live, &next);
    if changed {
        cfg.bump_generation();
        search.replace_job(next.to_mining_job(cfg.generation_id, mining_payout_address))?;
    }
    *live = next;
    Ok(changed)
}

fn prepare_fulcrum_endpoint_change(
    cfg: &RuntimeConfig,
    endpoint: Option<&str>,
) -> Result<Option<(RuntimeConfig, Vec<String>)>, String> {
    let mut next = cfg.clone();
    match endpoint {
        Some(url) => next.set_fulcrum_url(url)?,
        None => next.clear_fulcrum_url(),
    }
    if next.fulcrum_url == cfg.fulcrum_url {
        return Ok(None);
    }
    let endpoints = next.electrum_endpoints();
    Ok(Some((next, endpoints)))
}

fn live_job_changed(current: &LiveJob, next: &LiveJob) -> bool {
    current.baton_txid != next.baton_txid
        || current.baton_vout != next.baton_vout
        || current.baton_height != next.baton_height
        || current.baton_value_sats != next.baton_value_sats
        || current.height != next.height
        || current.age != next.age
        || current.commitment_hex != next.commitment_hex
        || current.target_le_hex != next.target_le_hex
        || current.token_amount != next.token_amount
        || current.reward_raw != next.reward_raw
        || current.url != next.url
}

fn winner_matches_live(winner: &VerifiedWinner, generation_id: u64, live: &LiveJob) -> bool {
    winner.generation_id == generation_id
        && winner.height == live.height
        && winner.baton_txid == live.baton_txid
        && winner.baton_vout == live.baton_vout
}

#[allow(clippy::too_many_arguments)]
fn write_snapshot(
    shared: &Arc<Mutex<RuntimeSnapshot>>,
    state: SupervisorState,
    cfg: &RuntimeConfig,
    live: &LiveJob,
    search: &SearchHandle,
    refreshes: u64,
    stale_rebuilds: u64,
    reconnects: u64,
    stale_winners: u64,
    verified_winners: u64,
    pending_winners: u64,
    last_error: Option<String>,
) {
    let mut snapshot = shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot.state = state;
    snapshot.generation_id = cfg.generation_id;
    snapshot.payout_address.clone_from(&cfg.payout_address);
    snapshot.endpoint.clone_from(&live.url);
    snapshot.height = live.height;
    snapshot.baton_txid.clone_from(&live.baton_txid);
    snapshot.baton_vout = live.baton_vout;
    snapshot.refreshes = refreshes;
    snapshot.stale_rebuilds = stale_rebuilds;
    snapshot.reconnects = reconnects;
    snapshot.stale_winners = stale_winners;
    snapshot.verified_winners = verified_winners;
    snapshot.pending_winners = pending_winners;
    snapshot.last_error = last_error;
    snapshot.search = search.snapshot();
    if snapshot.state == SupervisorState::Mining && snapshot.search.state == MiningState::Paused {
        snapshot.state = SupervisorState::Paused;
    }
}

fn emit(tx: &SyncSender<RuntimeEvent>, event: RuntimeEvent) {
    match tx.try_send(event) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PAYOUT: &str = "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh";

    fn live_job() -> LiveJob {
        LiveJob {
            url: "wss://one.invalid".into(),
            server_version: serde_json::json!(["Fulcrum", "1.5"]),
            height: 1_000,
            baton_txid: "11".repeat(32),
            baton_vout: 0,
            baton_height: 999,
            baton_value_sats: 15_971_500,
            commitment_hex: "00".repeat(101),
            token_amount: 2_099_905_002_035_715,
            age: 1,
            target_le_hex: "ff".repeat(32),
            reward_raw: 4_999_773_813,
        }
    }

    fn winner(generation_id: u64, job: &LiveJob) -> VerifiedWinner {
        VerifiedWinner {
            generation_id,
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            nonce: 7,
            digest: [0u8; 32],
            public_key: [0u8; 33],
            signature: [0u8; 64],
            transaction: Vec::new(),
        }
    }

    fn preflight_fixture() -> (
        RuntimeConfig,
        LiveJob,
        [u8; 32],
        [u8; 33],
        String,
        reward::SponsorReserve,
        PathBuf,
    ) {
        let mut cfg = RuntimeConfig::default();
        cfg.set_payout(TEST_PAYOUT.into()).unwrap();
        let job = live_job();
        let reward_secret = [2u8; 32];
        let reward_public = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(reward_secret).unwrap(),
        )
        .serialize();
        let mining_payout = reward::p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let sponsor = reward::SponsorReserve {
            txid: "33".repeat(32),
            vout: 1,
            value_sats: 100_000,
            locking_script: reward::build_sponsor_script(&job.baton_txid).unwrap(),
        };
        let unique = format!(
            "pickaxe-preflight-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let journal = std::env::temp_dir().join(unique);
        (
            cfg,
            job,
            reward_secret,
            reward_public,
            mining_payout,
            sponsor,
            journal,
        )
    }

    #[test]
    fn height_or_baton_change_invalidates_generation() {
        let current = live_job();
        let mut next = current.clone();
        assert!(!live_job_changed(&current, &next));

        next.height += 1;
        next.age += 1;
        assert!(live_job_changed(&current, &next));

        let mut baton = current.clone();
        baton.baton_txid = "22".repeat(32);
        assert!(live_job_changed(&current, &baton));
    }

    #[test]
    fn refreshed_state_rejects_old_generation_height_or_baton_winner() {
        let job = live_job();
        let current = winner(4, &job);
        assert!(winner_matches_live(&current, 4, &job));

        let old_generation = winner(3, &job);
        assert!(!winner_matches_live(&old_generation, 4, &job));

        let mut next_height = job.clone();
        next_height.height += 1;
        assert!(!winner_matches_live(&current, 4, &next_height));

        let mut next_baton = job;
        next_baton.baton_vout = 1;
        assert!(!winner_matches_live(&current, 4, &next_baton));
    }

    #[test]
    fn runtime_rebuilds_verified_parent_from_exact_live_state_before_journaling() {
        let job = live_job();
        let mining_secret = [1u8; 32];
        let mining_public = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(mining_secret).unwrap(),
        )
        .serialize();
        let reward_secret = [2u8; 32];
        let reward_public = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(reward_secret).unwrap(),
        )
        .serialize();
        let payout = reward::p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let nonce = 7;
        let message = tx::photon_message_sha256(nonce, &job.target_le_hex).unwrap();
        let signature = crate::crypto::bch_schnorr_sign(&mining_secret, &message).unwrap();
        let context = tx::ReferenceJobContext {
            prev_txid: job.baton_txid.clone(),
            prev_vout: job.baton_vout,
            age: job.age,
            target_le_hex: job.target_le_hex.clone(),
            contract_value_sats: job.baton_value_sats,
            contract_token_amount: job.token_amount,
            reward_raw: job.reward_raw,
        };
        let transaction = tx::apply_reference_signature(
            &context,
            &payout,
            &hex::encode(mining_public),
            nonce,
            &hex::encode(signature),
        )
        .unwrap();
        let winner = VerifiedWinner {
            generation_id: 4,
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            nonce,
            digest: crate::search::hash256(&transaction),
            public_key: mining_public,
            signature,
            transaction,
        };

        validate_verified_parent(&winner, &job, &reward_public).unwrap();

        let mut wrong_parent = winner.clone();
        wrong_parent.transaction[10] ^= 1;
        assert!(validate_verified_parent(&wrong_parent, &job, &reward_public).is_err());

        let mut wrong_live = job;
        wrong_live.reward_raw += 1;
        assert!(validate_verified_parent(&winner, &wrong_live, &reward_public).is_err());
    }

    #[test]
    fn submission_retry_never_rebroadcasts_parent_after_it_is_known() {
        assert_eq!(
            submission_decision(false, true, false),
            SubmissionDecision::Complete
        );
        assert_eq!(
            submission_decision(true, false, false),
            SubmissionDecision::BroadcastChild
        );
        assert_eq!(
            submission_decision(false, false, true),
            SubmissionDecision::BroadcastParentThenChild
        );
        assert_eq!(
            submission_decision(false, false, false),
            SubmissionDecision::StaleUnbroadcast
        );
    }

    #[test]
    fn production_preflight_proves_parent_reward_split_and_journal_readiness() {
        let (cfg, job, secret, public, mining_payout, sponsor, journal) = preflight_fixture();
        production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &sponsor,
            &journal,
        )
        .unwrap();
        assert!(!journal.exists());
    }

    #[test]
    fn production_preflight_refuses_wrong_sponsor_state_before_search() {
        let (cfg, job, secret, public, mining_payout, mut sponsor, journal) = preflight_fixture();
        sponsor.locking_script = reward::build_sponsor_script(&"44".repeat(32)).unwrap();
        let error = production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &sponsor,
            &journal,
        )
        .unwrap_err();
        assert!(error.contains("sponsor reserve does not match"));
        assert!(!journal.exists());
    }

    #[test]
    fn production_preflight_refuses_unresolved_submission_journal() {
        let (cfg, job, secret, public, mining_payout, sponsor, journal) = preflight_fixture();
        fs::write(&journal, b"occupied").unwrap();
        let error = production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &sponsor,
            &journal,
        )
        .unwrap_err();
        assert!(error.contains("unresolved previous winner submission"));
        fs::remove_file(&journal).unwrap();
    }

    #[test]
    fn live_search_remains_gated_until_verified_winner_is_durable() {
        let error = require_complete_live_winner_lifecycle().unwrap_err();
        assert!(error.contains("durably recoverable"));
        assert!(error.contains("live GPU search was not started"));
    }

    #[test]
    fn pending_submission_journal_round_trips_signed_bytes_without_secrets() {
        let parent = vec![0x02, 0x00, 0x00, 0x00, 0x01];
        let child = vec![0x02, 0x00, 0x00, 0x00, 0x02];
        let pending = PendingSubmission {
            version: SUBMISSION_JOURNAL_VERSION,
            expected_height: 1_000,
            expected_baton_txid: "11".repeat(32),
            expected_baton_vout: 0,
            parent_txid: reward::transaction_id(&parent),
            parent_hex: hex::encode(&parent),
            child_txid: reward::transaction_id(&child),
            child_hex: hex::encode(&child),
        };

        let unique = format!(
            "pickaxe-pending-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        pending.persist_new(&path).unwrap();

        let serialized = fs::read_to_string(&path).unwrap();
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("private"));
        assert_eq!(PendingSubmission::load(&path).unwrap(), Some(pending));

        PendingSubmission::remove(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn unbroadcast_pending_pair_requires_exact_height_and_baton() {
        let job = live_job();
        let pending = PendingSubmission {
            version: SUBMISSION_JOURNAL_VERSION,
            expected_height: job.height,
            expected_baton_txid: job.baton_txid.clone(),
            expected_baton_vout: job.baton_vout,
            parent_txid: reward::transaction_id(&[1]),
            parent_hex: "01".into(),
            child_txid: reward::transaction_id(&[2]),
            child_hex: "02".into(),
        };
        assert!(pending.matches_live(&job));

        let mut next_height = job.clone();
        next_height.height += 1;
        assert!(!pending.matches_live(&next_height));

        let mut next_baton = job;
        next_baton.baton_txid = "22".repeat(32);
        assert!(!pending.matches_live(&next_baton));
    }

    #[test]
    fn fulcrum_endpoint_change_is_atomic_and_invalidates_generation() {
        let cfg = RuntimeConfig::default();
        let original_generation = cfg.generation_id;

        assert!(
            prepare_fulcrum_endpoint_change(&cfg, Some("http://wrong-scheme.invalid")).is_err()
        );
        assert_eq!(cfg.generation_id, original_generation);
        assert!(cfg.fulcrum_url.is_none());

        let custom = "wss://unit-test.invalid:50004";
        let (next, endpoints) = prepare_fulcrum_endpoint_change(&cfg, Some(custom))
            .unwrap()
            .expect("new endpoint must require a transition");
        assert_eq!(cfg.generation_id, original_generation);
        assert_eq!(next.generation_id, original_generation + 1);
        assert_eq!(next.fulcrum_url.as_deref(), Some(custom));
        assert_eq!(endpoints.first().map(String::as_str), Some(custom));
        let replacement = live_job().to_mining_job(next.generation_id, "");
        assert_eq!(replacement.generation_id, next.generation_id);

        assert!(prepare_fulcrum_endpoint_change(&next, Some(custom))
            .unwrap()
            .is_none());

        let (cleared, cleared_endpoints) = prepare_fulcrum_endpoint_change(&next, None)
            .unwrap()
            .expect("clearing a custom endpoint must require a transition");
        assert_eq!(cleared.generation_id, next.generation_id + 1);
        assert!(cleared.fulcrum_url.is_none());
        assert!(!cleared_endpoints.iter().any(|value| value == custom));
    }
}
