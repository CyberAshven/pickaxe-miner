//! Presentation-neutral live PHOTON runtime supervision.
//!
//! The authoritative M67.38 browser reference polls BCH height + the mutable
//! PHOTON baton every 500 ms while GPU batches continue back-to-back. This
//! supervisor keeps network polling independent of the GPU hot path and applies
//! an immutable generation change at the worker's next batch boundary.

use crate::backend::BackendKind;
use crate::config::{JobSource, RuntimeConfig};
use crate::electrum::{ElectrumSession, LiveJob, LiveStateSnapshot};
use crate::reward;
use crate::search::VerifiedWinner;
use crate::search::{
    MiningState, RuntimeCommand as SearchCommand, SearchHandle, SearchPauseHandle, SearchStats,
};
use crate::telemetry::{GpuTelemetry, LiveTelemetrySampler};
use crate::tx;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[path = "source_pool.rs"]
#[allow(dead_code)]
mod source_pool;

use self::source_pool::{
    SourceCapability, SourceCatalog, SourceKind, AUTO_PROBE_LIMIT, DEFAULT_CAPABILITY_TTL_MS,
};

const COMMAND_CAP: usize = 16;
const EVENT_CAP: usize = 32;
const SUPERVISOR_POLL: Duration = Duration::from_millis(10);
const PHOTON_STATE_RECHECK: Duration = Duration::from_millis(500);
const RECONNECT_MIN: Duration = Duration::from_millis(400);
const RECONNECT_MAX: Duration = Duration::from_secs(8);
const REFRESH_RECONNECT_THRESHOLD: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshFailureKind {
    Transient,
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshFailureAction {
    RetainGeneration,
    Reconnect,
}

#[derive(Debug, Default)]
struct RefreshFailureTracker {
    transient_total: u64,
    transport_total: u64,
    consecutive: u8,
}

impl RefreshFailureTracker {
    fn success(&mut self) {
        self.consecutive = 0;
    }

    fn failure(&mut self, kind: RefreshFailureKind) -> RefreshFailureAction {
        match kind {
            RefreshFailureKind::Transient => {
                self.transient_total = self.transient_total.saturating_add(1);
                self.consecutive = self
                    .consecutive
                    .saturating_add(1)
                    .min(REFRESH_RECONNECT_THRESHOLD);
                if self.consecutive >= REFRESH_RECONNECT_THRESHOLD {
                    RefreshFailureAction::Reconnect
                } else {
                    RefreshFailureAction::RetainGeneration
                }
            }
            RefreshFailureKind::Transport => {
                self.transport_total = self.transport_total.saturating_add(1);
                RefreshFailureAction::Reconnect
            }
        }
    }

    fn degraded(&self) -> bool {
        self.consecutive != 0
    }
}

fn classify_refresh_failure(error: &str) -> RefreshFailureKind {
    let error = error.to_ascii_lowercase();
    let transport_failure = error.contains("transport:")
        || error.contains("send:")
        || error.contains("json:")
        || error.contains("socket closed")
        || error.contains("connection closed")
        || error.contains("connection reset")
        || error.contains("forcibly closed")
        || error.contains("broken pipe")
        || error.contains("unexpected eof")
        || error.contains("tls error")
        || error.contains("protocol error");
    if transport_failure {
        RefreshFailureKind::Transport
    } else {
        RefreshFailureKind::Transient
    }
}

fn next_periodic_deadline(previous_deadline: Instant, now: Instant, interval: Duration) -> Instant {
    debug_assert!(!interval.is_zero());
    if previous_deadline > now {
        return previous_deadline;
    }

    let elapsed_ticks = now.duration_since(previous_deadline).as_nanos() / interval.as_nanos();
    let ticks = elapsed_ticks.saturating_add(1).min(u128::from(u32::MAX)) as u32;
    previous_deadline
        .checked_add(interval.saturating_mul(ticks))
        .filter(|deadline| *deadline > now)
        .unwrap_or_else(|| now + interval)
}
const SUBMISSION_JOURNAL_VERSION: u8 = 3;
const SUBMISSION_RESOLUTION_VERSION: u8 = 1;
const PHOTON_TX_BYTES: usize = 615;
const PHOTON_TARGET_OFFSET: usize = 394;
const BATON_LINEAGE_MAX_STEPS: usize = 256;
const VERIFIED_WINNER_DURABILITY_READY: bool = true;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct NativePhotonWorkIdentity {
    height: u32,
    baton_txid: String,
    baton_vout: u32,
    baton_height: u32,
    baton_value_sats: u64,
    commitment_hex: String,
    token_amount: u128,
    age: u32,
    target_le_hex: String,
    reward_raw: u128,
}

impl From<&LiveJob> for NativePhotonWorkIdentity {
    fn from(job: &LiveJob) -> Self {
        Self {
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            baton_height: job.baton_height,
            baton_value_sats: job.baton_value_sats,
            commitment_hex: job.commitment_hex.clone(),
            token_amount: job.token_amount,
            age: job.age,
            target_le_hex: job.target_le_hex.clone(),
            reward_raw: job.reward_raw,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativePhotonEquivalenceProof {
    endpoint: String,
    tip_hash: String,
    proven_work: NativePhotonWorkIdentity,
}

impl NativePhotonEquivalenceProof {
    fn from_verified_snapshot(
        endpoint: &str,
        snapshot: &LiveStateSnapshot,
    ) -> Result<Self, String> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            return Err("native PHOTON equivalence proof requires an endpoint".into());
        }
        if snapshot.tip_hash.len() != 64
            || !snapshot
                .tip_hash
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(
                "native PHOTON equivalence proof requires a valid same-tip block hash".into(),
            );
        }
        Ok(Self {
            endpoint: endpoint.to_string(),
            tip_hash: snapshot.tip_hash.to_ascii_lowercase(),
            proven_work: NativePhotonWorkIdentity::from(&snapshot.job),
        })
    }

    fn validate_continuation(&self, snapshot: &LiveStateSnapshot) -> Result<(), String> {
        if snapshot.job.url.trim() != self.endpoint {
            return Err(format!(
                "native PHOTON proof endpoint changed: proven={} current={}",
                self.endpoint, snapshot.job.url
            ));
        }
        if !snapshot.tip_hash.eq_ignore_ascii_case(&self.tip_hash) {
            return Err(format!(
                "native PHOTON tip changed outside canonical proof: proven={} current={}",
                self.tip_hash, snapshot.tip_hash
            ));
        }
        if self.proven_work != NativePhotonWorkIdentity::from(&snapshot.job) {
            return Err(
                "native PHOTON work changed outside canonical proof; canonical re-proof is required"
                    .into(),
            );
        }
        Ok(())
    }
}

fn source_capability_now_ms(epoch: Instant) -> u64 {
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn eligible_fulcrum_endpoints(
    sources: &SourceCatalog,
    now_ms: u64,
    rotation_key: u64,
    avoid_endpoint: Option<&str>,
) -> Vec<String> {
    let mut endpoints = sources
        .probe_candidates(SourceKind::Fulcrum, now_ms, AUTO_PROBE_LIMIT, rotation_key)
        .into_iter()
        .map(|entry| entry.endpoint.clone())
        .collect::<Vec<_>>();
    if endpoints.len() > 1 {
        if let Some(avoid) = avoid_endpoint {
            endpoints.sort_by_key(|endpoint| endpoint.eq_ignore_ascii_case(avoid));
        }
    }
    endpoints
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettlementState {
    generation_id: u64,
    baton_txid: String,
    baton_vout: u32,
}

impl SettlementState {
    fn new(generation_id: u64, live: &LiveJob) -> Result<Self, String> {
        if live.baton_vout != 0 {
            return Err(format!(
                "PHOTON settlement requires baton output 0, got {}",
                live.baton_vout
            ));
        }
        Ok(Self {
            generation_id,
            baton_txid: live.baton_txid.clone(),
            baton_vout: live.baton_vout,
        })
    }

    fn restamp(&self, generation_id: u64, live: &LiveJob) -> Result<Self, String> {
        self.ensure_current(self.generation_id, live)?;
        Self::new(generation_id, live)
    }

    fn ensure_current(&self, generation_id: u64, live: &LiveJob) -> Result<(), String> {
        if self.generation_id != generation_id
            || self.baton_txid != live.baton_txid
            || self.baton_vout != live.baton_vout
        {
            return Err("settlement state is stale for the current PHOTON generation".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingSubmission {
    version: u8,
    generation_id: u64,
    expected_height: u32,
    expected_baton_txid: String,
    expected_baton_vout: u32,
    parent_txid: String,
    parent_hex: String,
    settlement_txid: String,
    settlement_hex: String,
    resulting_baton_txid: String,
    resulting_baton_vout: u32,
    resulting_baton_value_sats: u64,
    miner_token_amount: u128,
    donation_token_amount: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ResolvedSubmission {
    version: u8,
    journal_version: u8,
    generation_id: u64,
    expected_height: u32,
    expected_baton_txid: String,
    expected_baton_vout: u32,
    parent_txid: String,
    settlement_txid: String,
    parent_attempted: bool,
    parent_accepted: bool,
    observed_height: u32,
    observed_baton_txid: String,
    observed_baton_vout: u32,
    reason: String,
}

impl PendingSubmission {
    fn marker_path(journal_path: &Path, suffix: &str) -> PathBuf {
        let mut path = journal_path.as_os_str().to_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }

    fn parent_attempted_path(journal_path: &Path) -> PathBuf {
        Self::marker_path(journal_path, ".parent-attempted")
    }

    fn parent_accepted_path(journal_path: &Path) -> PathBuf {
        Self::marker_path(journal_path, ".parent-accepted")
    }

    fn marker_present(path: &Path, expected_parent_txid: &str) -> Result<bool, String> {
        let marker = match fs::read_to_string(path) {
            Ok(marker) => marker,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "read pending-submission progress marker {}: {error}",
                    path.display()
                ))
            }
        };
        if marker.trim().eq_ignore_ascii_case(expected_parent_txid) {
            Ok(true)
        } else {
            Err(format!(
                "pending-submission progress marker {} does not match parent txid",
                path.display()
            ))
        }
    }

    fn persist_marker(path: &Path, parent_txid: &str) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or("pending-submission progress marker has no parent directory")?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create pending-submission progress directory {}: {error}",
                parent.display()
            )
        })?;
        if Self::marker_present(path, parent_txid)? {
            return Self::sync_committed(path, parent);
        }

        let temporary = Self::marker_path(path, ".tmp");
        if temporary.exists() {
            fs::remove_file(&temporary).map_err(|error| {
                format!(
                    "remove stale pending-submission progress temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                format!(
                    "create pending-submission progress temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        if let Err(error) = file
            .write_all(parent_txid.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
        {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "write pending-submission progress marker {}: {error}",
                temporary.display()
            ));
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "commit pending-submission progress marker {}: {error}",
                path.display()
            ));
        }
        Self::sync_committed(path, parent)
    }

    fn parent_attempted(&self, journal_path: &Path) -> Result<bool, String> {
        Self::marker_present(
            &Self::parent_attempted_path(journal_path),
            &self.parent_txid,
        )
    }

    fn parent_accepted(&self, journal_path: &Path) -> Result<bool, String> {
        Self::marker_present(&Self::parent_accepted_path(journal_path), &self.parent_txid)
    }

    fn mark_parent_attempted(&self, journal_path: &Path) -> Result<(), String> {
        Self::persist_marker(
            &Self::parent_attempted_path(journal_path),
            &self.parent_txid,
        )
    }

    fn mark_parent_accepted(&self, journal_path: &Path) -> Result<(), String> {
        self.mark_parent_attempted(journal_path)?;
        Self::persist_marker(&Self::parent_accepted_path(journal_path), &self.parent_txid)
    }

    fn from_verified(
        winner: &VerifiedWinner,
        settlement: &reward::PreparedSelfFundedSettlement,
    ) -> Result<Self, String> {
        let parent_txid = reward::transaction_id(&winner.transaction);
        if parent_txid != settlement.parent_txid {
            return Err("settlement parent txid does not match verified PHOTON winner".into());
        }
        let pending = Self {
            version: SUBMISSION_JOURNAL_VERSION,
            generation_id: winner.generation_id,
            expected_height: winner.height,
            expected_baton_txid: winner.baton_txid.clone(),
            expected_baton_vout: winner.baton_vout,
            parent_txid,
            parent_hex: hex::encode(&winner.transaction),
            settlement_txid: settlement.settlement_txid.clone(),
            settlement_hex: hex::encode(&settlement.raw_settlement),
            resulting_baton_txid: settlement.settlement_txid.clone(),
            resulting_baton_vout: 0,
            resulting_baton_value_sats: settlement.baton_output_value_sats,
            miner_token_amount: settlement.miner_token_amount,
            donation_token_amount: settlement.donation_token_amount,
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
        if self.generation_id == 0 {
            return Err("pending submission has invalid generation id".into());
        }
        if self.resulting_baton_vout != 0 {
            return Err("pending submission resulting PHOTON baton must be output 0".into());
        }
        if self.resulting_baton_txid != self.settlement_txid {
            return Err(
                "pending submission resulting baton txid must equal settlement txid".into(),
            );
        }
        let reward_amount = self
            .miner_token_amount
            .checked_add(self.donation_token_amount)
            .ok_or("pending submission reward token amount overflow")?;
        let expected_donation =
            reward_amount.saturating_mul(u128::from(crate::config::DONATION_BPS)) / 10_000;
        if self.donation_token_amount != expected_donation
            || self.miner_token_amount != reward_amount - expected_donation
        {
            return Err("pending submission does not encode the exact 98/2 reward split".into());
        }
        for (label, expected, raw_hex) in [
            (
                "parent",
                self.parent_txid.as_str(),
                self.parent_hex.as_str(),
            ),
            (
                "settlement",
                self.settlement_txid.as_str(),
                self.settlement_hex.as_str(),
            ),
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

    fn expected_baton_is_current(&self, live: &LiveJob) -> bool {
        self.expected_baton_txid
            .eq_ignore_ascii_case(&live.baton_txid)
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
            if Self::load(path)?.as_ref() == Some(self) {
                return Self::sync_committed(path, parent);
            }
            return Err(format!(
                "different pending-submission journal already exists at {}",
                path.display()
            ));
        }
        for marker in [
            Self::parent_attempted_path(path),
            Self::parent_accepted_path(path),
        ] {
            if marker.exists() {
                return Err(format!(
                    "orphan pending-submission progress marker exists at {}",
                    marker.display()
                ));
            }
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
        Self::sync_committed(path, parent)
    }

    fn sync_committed(path: &Path, _parent: &Path) -> Result<(), String> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                format!(
                    "sync committed pending-submission journal {}: {error}",
                    path.display()
                )
            })?;
        #[cfg(unix)]
        fs::File::open(_parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!(
                    "sync pending-submission journal directory {}: {error}",
                    _parent.display()
                )
            })?;
        Ok(())
    }

    fn remove(path: &Path) -> Result<(), String> {
        for marker in [
            Self::parent_accepted_path(path),
            Self::parent_attempted_path(path),
        ] {
            match fs::remove_file(&marker) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "remove pending-submission progress marker {}: {error}",
                        marker.display()
                    ))
                }
            }
        }
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

impl ResolvedSubmission {
    fn path(journal_path: &Path) -> PathBuf {
        let mut path = journal_path.as_os_str().to_os_string();
        path.push(".resolved");
        PathBuf::from(path)
    }

    fn from_stale(
        pending: &PendingSubmission,
        fresh: &LiveJob,
        journal_path: &Path,
    ) -> Result<Self, String> {
        pending.validate()?;
        let parent_attempted = pending.parent_attempted(journal_path)?;
        let parent_accepted = pending.parent_accepted(journal_path)?;
        if parent_accepted {
            return Err(
                "refusing to resolve a PHOTON winner as stale after parent acceptance".into(),
            );
        }
        if parent_attempted {
            return Err(
                "refusing to resolve a PHOTON winner as stale after a parent broadcast attempt; the durable journal must be retained until the parent outcome is proven"
                    .into(),
            );
        }
        if fresh.baton_txid.eq_ignore_ascii_case(&pending.parent_txid) && fresh.baton_vout == 0 {
            return Err(
                "refusing to resolve a PHOTON winner as stale while its parent baton is live"
                    .into(),
            );
        }
        if pending.expected_baton_is_current(fresh) {
            return Err(
                "refusing to resolve a PHOTON winner as stale while its expected baton is live"
                    .into(),
            );
        }

        Ok(Self {
            version: SUBMISSION_RESOLUTION_VERSION,
            journal_version: pending.version,
            generation_id: pending.generation_id,
            expected_height: pending.expected_height,
            expected_baton_txid: pending.expected_baton_txid.clone(),
            expected_baton_vout: pending.expected_baton_vout,
            parent_txid: pending.parent_txid.clone(),
            settlement_txid: pending.settlement_txid.clone(),
            parent_attempted,
            parent_accepted,
            observed_height: fresh.height,
            observed_baton_txid: fresh.baton_txid.clone(),
            observed_baton_vout: fresh.baton_vout,
            reason: "stale-unbroadcast".into(),
        })
    }

    fn from_confirmed(pending: &PendingSubmission, fresh: &LiveJob) -> Result<Self, String> {
        pending.validate()?;
        Ok(Self {
            version: SUBMISSION_RESOLUTION_VERSION,
            journal_version: pending.version,
            generation_id: pending.generation_id,
            expected_height: pending.expected_height,
            expected_baton_txid: pending.expected_baton_txid.clone(),
            expected_baton_vout: pending.expected_baton_vout,
            parent_txid: pending.parent_txid.clone(),
            settlement_txid: pending.settlement_txid.clone(),
            parent_attempted: true,
            parent_accepted: true,
            observed_height: fresh.height,
            observed_baton_txid: fresh.baton_txid.clone(),
            observed_baton_vout: fresh.baton_vout,
            reason: "confirmed".into(),
        })
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != SUBMISSION_RESOLUTION_VERSION {
            return Err(format!(
                "unsupported resolved-submission version {}",
                self.version
            ));
        }
        if self.journal_version != SUBMISSION_JOURNAL_VERSION {
            return Err(format!(
                "resolved submission references unsupported journal version {}",
                self.journal_version
            ));
        }
        for (label, txid) in [
            ("expected baton", self.expected_baton_txid.as_str()),
            ("parent", self.parent_txid.as_str()),
            ("settlement", self.settlement_txid.as_str()),
            ("observed baton", self.observed_baton_txid.as_str()),
        ] {
            if txid.len() != 64 || !txid.chars().all(|value| value.is_ascii_hexdigit()) {
                return Err(format!("resolved submission has invalid {label} txid"));
            }
        }
        match self.reason.as_str() {
            "stale-unbroadcast" => {
                if self.parent_accepted {
                    return Err("resolved stale submission cannot have an accepted parent".into());
                }
            }
            "confirmed" => {
                if !self.parent_attempted || !self.parent_accepted {
                    return Err(
                        "confirmed submission must record an attempted and accepted parent".into(),
                    );
                }
            }
            _ => return Err("resolved submission has an unsupported resolution reason".into()),
        }
        Ok(())
    }

    fn load(journal_path: &Path) -> Result<Option<Self>, String> {
        let path = Self::path(journal_path);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "read resolved-submission record {}: {error}",
                    path.display()
                ))
            }
        };
        let resolved: Self = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "parse resolved-submission record {}: {error}",
                path.display()
            )
        })?;
        resolved.validate()?;
        Ok(Some(resolved))
    }

    fn persist_latest(&self, journal_path: &Path) -> Result<(), String> {
        self.validate()?;
        let path = Self::path(journal_path);
        let parent = path
            .parent()
            .ok_or("resolved-submission record has no parent directory")?;
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create resolved-submission directory {}: {error}",
                parent.display()
            )
        })?;
        if Self::load(journal_path)?.as_ref() == Some(self) {
            return PendingSubmission::sync_committed(&path, parent);
        }

        let temporary = PendingSubmission::marker_path(&path, ".tmp");
        if temporary.exists() {
            fs::remove_file(&temporary).map_err(|error| {
                format!(
                    "remove stale resolved-submission temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        }
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("serialize resolved-submission record: {error}"))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                format!(
                    "create resolved-submission temporary file {}: {error}",
                    temporary.display()
                )
            })?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "write resolved-submission record {}: {error}",
                temporary.display()
            ));
        }
        drop(file);

        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "replace resolved-submission record {}: {error}",
                    path.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "commit resolved-submission record {}: {error}",
                path.display()
            ));
        }
        PendingSubmission::sync_committed(&path, parent)
    }
}

fn resolve_stale_submission(
    pending: &PendingSubmission,
    fresh: &LiveJob,
    journal_path: &Path,
) -> Result<(), String> {
    let resolved = ResolvedSubmission::from_stale(pending, fresh, journal_path)?;
    resolved.persist_latest(journal_path)?;
    PendingSubmission::remove(journal_path)
}

fn resolve_confirmed_submission(
    pending: &PendingSubmission,
    fresh: &LiveJob,
    journal_path: &Path,
) -> Result<(), String> {
    let resolved = ResolvedSubmission::from_confirmed(pending, fresh)?;
    resolved.persist_latest(journal_path)?;
    PendingSubmission::remove(journal_path)
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
    BroadcastSettlement,
    BroadcastParentThenSettlement,
    AwaitParentResolution,
    StaleUnbroadcast,
}

fn submission_decision(
    parent_known: bool,
    parent_attempted: bool,
    live_matches_expected: bool,
) -> SubmissionDecision {
    if parent_known {
        SubmissionDecision::BroadcastSettlement
    } else if live_matches_expected {
        SubmissionDecision::BroadcastParentThenSettlement
    } else if parent_attempted {
        SubmissionDecision::AwaitParentResolution
    } else {
        SubmissionDecision::StaleUnbroadcast
    }
}

enum SubmissionAttempt {
    Complete(Box<LiveJob>),
    StaleUnbroadcast(Box<LiveJob>),
}

fn resulting_baton_is_authoritative(pending: &PendingSubmission, fresh: &LiveJob) -> bool {
    fresh
        .baton_txid
        .eq_ignore_ascii_case(&pending.resulting_baton_txid)
        && fresh.baton_vout == pending.resulting_baton_vout
}

fn read_compact_size(bytes: &[u8], offset: &mut usize) -> Result<u64, String> {
    let marker = *bytes
        .get(*offset)
        .ok_or("transaction ended before CompactSize")?;
    *offset += 1;
    let (value, width) = match marker {
        0x00..=0xfc => (u64::from(marker), 0usize),
        0xfd => {
            let slice = bytes
                .get(*offset..*offset + 2)
                .ok_or("transaction ended inside CompactSize u16")?;
            (u64::from(u16::from_le_bytes([slice[0], slice[1]])), 2)
        }
        0xfe => {
            let slice = bytes
                .get(*offset..*offset + 4)
                .ok_or("transaction ended inside CompactSize u32")?;
            (
                u64::from(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])),
                4,
            )
        }
        0xff => {
            let slice = bytes
                .get(*offset..*offset + 8)
                .ok_or("transaction ended inside CompactSize u64")?;
            (
                u64::from_le_bytes([
                    slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
                ]),
                8,
            )
        }
    };
    *offset += width;
    Ok(value)
}

fn first_input_outpoint(raw_tx_hex: &str) -> Result<(String, u32), String> {
    let raw = hex::decode(raw_tx_hex.trim())
        .map_err(|error| format!("decode PHOTON lineage transaction: {error}"))?;
    if raw.len() < 5 {
        return Err("PHOTON lineage transaction is too short".into());
    }
    let mut offset = 4usize;
    let input_count = read_compact_size(&raw, &mut offset)?;
    if input_count == 0 {
        return Err("PHOTON lineage transaction has no inputs".into());
    }
    let prev_hash = raw
        .get(offset..offset + 32)
        .ok_or("PHOTON lineage transaction is truncated before input 0 txid")?;
    offset += 32;
    let prev_vout = raw
        .get(offset..offset + 4)
        .ok_or("PHOTON lineage transaction is truncated before input 0 vout")?;
    let mut display_hash = prev_hash.to_vec();
    display_hash.reverse();
    Ok((
        hex::encode(display_hash),
        u32::from_le_bytes([prev_vout[0], prev_vout[1], prev_vout[2], prev_vout[3]]),
    ))
}

fn prove_baton_descends_from<F>(
    current_txid: &str,
    current_vout: u32,
    ancestor_txid: &str,
    ancestor_vout: u32,
    mut fetch_raw: F,
) -> Result<bool, String>
where
    F: FnMut(&str) -> Result<String, String>,
{
    if current_vout != 0 || ancestor_vout != 0 {
        return Ok(false);
    }
    if current_txid.eq_ignore_ascii_case(ancestor_txid) {
        return Ok(true);
    }

    let mut cursor = current_txid.to_string();
    for _ in 0..BATON_LINEAGE_MAX_STEPS {
        let raw = fetch_raw(&cursor)?;
        let raw_bytes = hex::decode(raw.trim())
            .map_err(|error| format!("decode PHOTON lineage transaction: {error}"))?;
        let actual_txid = reward::transaction_id(&raw_bytes);
        if !actual_txid.eq_ignore_ascii_case(&cursor) {
            return Err(format!(
                "PHOTON baton lineage transaction bytes do not match requested txid {cursor}"
            ));
        }
        let (previous_txid, previous_vout) = first_input_outpoint(&raw)?;
        if previous_vout != 0 {
            return Ok(false);
        }
        if previous_txid.eq_ignore_ascii_case(ancestor_txid) {
            return Ok(previous_vout == ancestor_vout);
        }
        if previous_txid.eq_ignore_ascii_case(&cursor) {
            return Err("PHOTON baton lineage contains a transaction cycle".into());
        }
        cursor = previous_txid;
    }

    Err(format!(
        "PHOTON baton lineage exceeded the bounded {BATON_LINEAGE_MAX_STEPS}-transaction recovery window"
    ))
}

fn resulting_baton_is_authoritative_or_descendant(
    session: &mut ElectrumSession,
    pending: &PendingSubmission,
    fresh: &LiveJob,
) -> Result<bool, String> {
    if resulting_baton_is_authoritative(pending, fresh) {
        return Ok(true);
    }
    prove_baton_descends_from(
        &fresh.baton_txid,
        fresh.baton_vout,
        &pending.resulting_baton_txid,
        pending.resulting_baton_vout,
        |txid| {
            match session.rpc(
                "blockchain.transaction.get",
                serde_json::json!([txid, false]),
            )? {
                serde_json::Value::String(raw) => Ok(raw),
                other => Err(format!(
                    "Fulcrum returned non-hex transaction data while proving PHOTON baton lineage: {other}"
                )),
            }
        },
    )
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

fn broadcast_with_preference<F, N>(
    source: JobSource,
    node_configured: bool,
    mut fulcrum: F,
    mut node: N,
) -> Result<String, String>
where
    F: FnMut() -> Result<String, String>,
    N: FnMut() -> Result<String, String>,
{
    match source {
        JobSource::Node if node_configured => match node() {
            Ok(txid) => Ok(txid),
            Err(node_error) => fulcrum().map_err(|fulcrum_error| {
                format!(
                    "native-node broadcast failed: {node_error}; Fulcrum fallback failed: {fulcrum_error}"
                )
            }),
        },
        JobSource::Node => fulcrum(),
        JobSource::Fulcrum => match fulcrum() {
            Ok(txid) => Ok(txid),
            Err(fulcrum_error) if node_configured => node().map_err(|node_error| {
                format!(
                    "Fulcrum broadcast failed: {fulcrum_error}; native-node fallback failed: {node_error}"
                )
            }),
            Err(fulcrum_error) => Err(fulcrum_error),
        },
    }
}

fn broadcast_pending_transaction(
    session: &mut ElectrumSession,
    cfg: &RuntimeConfig,
    raw_tx_hex: &str,
) -> Result<String, String> {
    let node_endpoints = cfg.node_endpoints();
    let node_configured = !node_endpoints.is_empty();
    broadcast_with_preference(
        cfg.source,
        node_configured,
        || session.broadcast_raw(raw_tx_hex),
        || crate::node::broadcast_raw(&node_endpoints, raw_tx_hex).map(|(_, txid)| txid),
    )
}

fn validate_node_mempool_acceptance(
    label: &str,
    expected_txid: &str,
    endpoint: &str,
    acceptance: &crate::node::MempoolAcceptance,
) -> Result<(), String> {
    if !acceptance.txid.eq_ignore_ascii_case(expected_txid) {
        return Err(format!(
            "{label} mempool preflight via {endpoint} returned unexpected txid {}; expected {expected_txid}",
            acceptance.txid
        ));
    }
    if acceptance.allowed {
        return Ok(());
    }

    let reason = acceptance
        .reject_reason
        .as_deref()
        .unwrap_or("node did not provide a rejection reason");
    let details = acceptance
        .reject_details
        .as_deref()
        .map(|value| format!(" ({value})"))
        .unwrap_or_default();
    Err(format!(
        "{label} rejected by native-node mempool preflight via {endpoint}: {reason}{details}"
    ))
}

fn preflight_pending_transaction(
    cfg: &RuntimeConfig,
    label: &str,
    expected_txid: &str,
    raw_tx_hex: &str,
) -> Result<(), String> {
    let endpoints = cfg.node_endpoints();
    if endpoints.is_empty() {
        return Ok(());
    }
    let (endpoint, acceptance) = crate::node::test_mempool_accept(&endpoints, raw_tx_hex)?;
    validate_node_mempool_acceptance(label, expected_txid, &endpoint, &acceptance)
}

fn broadcast_settlement(
    session: &mut ElectrumSession,
    cfg: &RuntimeConfig,
    pending: &PendingSubmission,
) -> Result<SubmissionAttempt, String> {
    preflight_pending_transaction(
        cfg,
        "PHOTON settlement",
        &pending.settlement_txid,
        &pending.settlement_hex,
    )?;
    let returned = broadcast_pending_transaction(session, cfg, &pending.settlement_hex)?;
    ensure_broadcast_txid("PHOTON settlement", &pending.settlement_txid, &returned)?;
    let fresh = session.fetch_live_job()?;
    if resulting_baton_is_authoritative_or_descendant(session, pending, &fresh)? {
        Ok(SubmissionAttempt::Complete(Box::new(fresh)))
    } else {
        Err(
            "settlement broadcast is known but the resulting PHOTON baton is neither authoritative nor a proven ancestor of the live baton".into(),
        )
    }
}

fn attempt_pending_submission(
    session: &mut ElectrumSession,
    cfg: &RuntimeConfig,
    pending: &PendingSubmission,
    journal_path: &Path,
) -> Result<SubmissionAttempt, String> {
    pending.validate()?;

    let settlement_known = session.transaction_known(&pending.settlement_txid)?;
    if settlement_known {
        let fresh = session.fetch_live_job()?;
        if resulting_baton_is_authoritative_or_descendant(session, pending, &fresh)? {
            return Ok(SubmissionAttempt::Complete(Box::new(fresh)));
        }
        return Err(
            "settlement transaction is known but the journaled resulting PHOTON baton is neither authoritative nor a proven ancestor of the live baton".into(),
        );
    }

    if pending.parent_accepted(journal_path)? {
        return broadcast_settlement(session, cfg, pending);
    }

    let parent_known = session.transaction_known(&pending.parent_txid)?;
    if parent_known {
        pending.mark_parent_accepted(journal_path)?;
        return broadcast_settlement(session, cfg, pending);
    }

    let parent_attempted = pending.parent_attempted(journal_path)?;
    let fresh = session.fetch_live_job()?;
    if fresh.baton_txid.eq_ignore_ascii_case(&pending.parent_txid) && fresh.baton_vout == 0 {
        pending.mark_parent_accepted(journal_path)?;
        return broadcast_settlement(session, cfg, pending);
    }
    match submission_decision(
        false,
        parent_attempted,
        pending.expected_baton_is_current(&fresh),
    ) {
        SubmissionDecision::StaleUnbroadcast => {
            return Ok(SubmissionAttempt::StaleUnbroadcast(Box::new(fresh)));
        }
        SubmissionDecision::AwaitParentResolution => {
            return Err(
                "PHOTON parent broadcast was previously attempted but its acceptance is not yet proven; retaining the durable pending submission for retry"
                    .into(),
            );
        }
        SubmissionDecision::BroadcastParentThenSettlement => {}
        SubmissionDecision::BroadcastSettlement => {
            unreachable!("parent-known submissions are handled before live-state refresh")
        }
    }

    if !parent_attempted {
        preflight_pending_transaction(
            cfg,
            "PHOTON parent",
            &pending.parent_txid,
            &pending.parent_hex,
        )?;
        pending.mark_parent_attempted(journal_path)?;
    }
    let returned_parent = broadcast_pending_transaction(session, cfg, &pending.parent_hex)?;
    ensure_broadcast_txid("PHOTON parent", &pending.parent_txid, &returned_parent)?;
    pending.mark_parent_accepted(journal_path)?;

    broadcast_settlement(session, cfg, pending)
}

fn prepare_pending_submission(
    winner: &VerifiedWinner,
    cfg: &RuntimeConfig,
    live: &LiveJob,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    settlement: &SettlementState,
    journal_path: &Path,
) -> Result<PendingSubmission, String> {
    if !winner_matches_live(winner, cfg.generation_id, live) {
        return Err("verified winner is stale before settlement preparation".into());
    }
    settlement.ensure_current(cfg.generation_id, live)?;
    validate_verified_parent(winner, live, reward_public_key)?;
    let relay_fee_sats_per_kb = production_relay_fee_sats_per_kb(cfg)?;
    let split = reward::build_self_funded_settlement_with_relay_fee(
        &winner.transaction,
        reward_secret,
        reward_public_key,
        &cfg.payout_address,
        live.reward_raw,
        relay_fee_sats_per_kb,
    )?;
    let pending = PendingSubmission::from_verified(winner, &split)?;
    pending.persist_new(journal_path)?;
    Ok(pending)
}

fn resolve_pending_before_search(
    session: &mut ElectrumSession,
    cfg: &RuntimeConfig,
    journal_path: &Path,
) -> Result<(), String> {
    let Some(pending) = PendingSubmission::load(journal_path)? else {
        return Ok(());
    };

    match attempt_pending_submission(session, cfg, &pending, journal_path)? {
        SubmissionAttempt::Complete(fresh) => {
            resolve_confirmed_submission(&pending, &fresh, journal_path)
        }
        SubmissionAttempt::StaleUnbroadcast(fresh) => {
            resolve_stale_submission(&pending, &fresh, journal_path)
        }
    }
}

fn production_relay_fee_sats_per_kb(cfg: &RuntimeConfig) -> Result<u64, String> {
    let endpoints = cfg.node_endpoints();
    if endpoints.is_empty() {
        return if cfg.source == JobSource::Node {
            Err("native-node broadcast selected but no node RPC endpoint is configured".into())
        } else {
            Ok(reward::MIN_RELAY_FEE_SATS_PER_KB)
        };
    }
    let (_, policy) = crate::node::fetch_relay_policy(&endpoints)
        .map_err(|error| format!("native-node relay-policy preflight failed: {error}"))?;
    Ok(reward::MIN_RELAY_FEE_SATS_PER_KB.max(policy.mempool_min_fee_sats_per_kb))
}

fn probe_submission_journal(journal_path: &Path) -> Result<(), String> {
    if journal_path.exists() {
        return Err(format!(
            "unresolved previous winner submission exists at {}",
            journal_path.display()
        ));
    }
    for marker in [
        PendingSubmission::parent_attempted_path(journal_path),
        PendingSubmission::parent_accepted_path(journal_path),
    ] {
        if marker.exists() {
            return Err(format!(
                "orphan pending-submission progress marker exists at {}",
                marker.display()
            ));
        }
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
    if !session.transaction_known(&live.baton_txid)? {
        return Err(
            "production preflight cannot retrieve the live PHOTON baton transaction".into(),
        );
    }

    let relay_fee_sats_per_kb = production_relay_fee_sats_per_kb(cfg)?;
    production_preflight_local(
        cfg,
        live,
        reward_secret,
        reward_public_key,
        mining_payout_address,
        journal_path,
        relay_fee_sats_per_kb,
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
    journal_path: &Path,
    relay_fee_sats_per_kb: u64,
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

    let split = reward::build_self_funded_settlement_with_relay_fee(
        &parent_preview,
        reward_secret,
        reward_public_key,
        &cfg.payout_address,
        live.reward_raw,
        relay_fee_sats_per_kb,
    )?;
    let (expected_miner, expected_donation) = RuntimeConfig::split_reward(live.reward_raw);
    if split.miner_token_amount != expected_miner
        || split.donation_token_amount != expected_donation
        || split
            .miner_token_amount
            .checked_add(split.donation_token_amount)
            != Some(live.reward_raw)
    {
        return Err("self-funded settlement preflight failed exact 98/2 token conservation".into());
    }
    let multi_input_max_baton_decrease_sats = reward::photon_multi_input_max_baton_decrease_sats()?;
    if split.fee_sats != split.required_relay_fee_sats
        || split.baton_input_value_sats < split.baton_output_value_sats
        || split.baton_input_value_sats - split.baton_output_value_sats
            > multi_input_max_baton_decrease_sats
    {
        return Err("self-funded settlement preflight failed BCH fee/value accounting".into());
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
    pub state_checks: u64,
    pub transient_refresh_failures: u64,
    pub transport_failures: u64,
    pub consecutive_refresh_failures: u8,
    pub source_degraded: bool,
    pub job_changes: u64,
    pub reconnects: u64,
    pub endpoint_rotations: u64,
    pub stale_winners: u64,
    pub verified_winners: u64,
    pub pending_winners: u64,
    pub last_error: Option<String>,
    pub search: SearchStats,
    pub gpu_telemetry: GpuTelemetry,
}

#[derive(Debug, Clone)]
pub enum RuntimeEvent {
    JobRefreshed {
        generation_id: u64,
        height: u32,
        baton_txid: String,
        baton_vout: u32,
    },
    StateRefreshFailed {
        error: String,
        consecutive: u8,
    },
    Reconnecting(String),
    Reconnected(String),
    EndpointRotated {
        from: String,
        to: String,
    },
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

#[derive(Clone)]
struct ShutdownSignal {
    requested: Arc<AtomicBool>,
    search_pause: SearchPauseHandle,
}

impl ShutdownSignal {
    fn new(search_pause: SearchPauseHandle) -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
            search_pause,
        }
    }

    fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.search_pause.pause();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

pub struct RuntimeSupervisor {
    command_tx: SyncSender<SupervisorCommand>,
    event_rx: Receiver<RuntimeEvent>,
    snapshot: Arc<Mutex<RuntimeSnapshot>>,
    shutdown: ShutdownSignal,
    worker: Option<JoinHandle<()>>,
    telemetry: LiveTelemetrySampler,
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
        crate::backend::require_production_mining_backend(backend)?;
        if cfg.payout_address.trim().is_empty() {
            return Err("mining payout address is required".into());
        }
        require_complete_live_winner_lifecycle()?;

        let sources = SourceCatalog::configured(&cfg)?;
        let rotation_key = u64::from(std::process::id()).wrapping_add(cfg.generation_id);
        let endpoints = eligible_fulcrum_endpoints(&sources, 0, rotation_key, None);
        let mut session = ElectrumSession::connect_failover(&endpoints)?;
        let journal_path = submission_journal_path();
        resolve_pending_before_search(&mut session, &cfg, &journal_path)?;
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
        let initial_settlement = SettlementState::new(cfg.generation_id, &initial)?;

        let initial_job = initial.to_mining_job(cfg.generation_id, &mining_payout_address);
        let search = SearchHandle::start_supervised_on_backend_device(
            backend,
            device_ordinal as usize,
            cfg.intensity,
            initial_job,
        )?;
        let initial_search = search.snapshot();
        let telemetry = LiveTelemetrySampler::start(backend, device_ordinal);
        let shutdown = ShutdownSignal::new(search.pause_handle());
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
            state_checks: 0,
            transient_refresh_failures: 0,
            transport_failures: 0,
            consecutive_refresh_failures: 0,
            source_degraded: false,
            job_changes: 0,
            reconnects: 0,
            endpoint_rotations: 0,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: initial_search,
            gpu_telemetry: telemetry.snapshot(),
        };

        let snapshot = Arc::new(Mutex::new(initial_snapshot));
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAP);
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_CAP);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_shutdown = shutdown.clone();

        let worker = thread::Builder::new()
            .name("pickaxe-live-supervisor".into())
            .spawn(move || {
                run_supervisor(
                    cfg,
                    endpoints,
                    sources,
                    initial,
                    session,
                    None,
                    Instant::now(),
                    search,
                    reward_secret,
                    reward_public_key,
                    mining_payout_address,
                    initial_settlement,
                    journal_path,
                    None,
                    command_rx,
                    event_tx,
                    worker_snapshot,
                    worker_shutdown,
                )
            })
            .map_err(|error| format!("start live PHOTON supervisor: {error}"))?;

        Ok(Self {
            command_tx,
            event_rx,
            snapshot,
            shutdown,
            worker: Some(worker),
            telemetry,
        })
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        snapshot.gpu_telemetry = self.telemetry.snapshot();
        snapshot
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
        self.shutdown.request();
        let _ = self.command_tx.try_send(SupervisorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let final_snapshot = self.snapshot();
        self.telemetry.stop();
        final_snapshot
    }
}

impl Drop for RuntimeSupervisor {
    fn drop(&mut self) {
        self.shutdown.request();
        let _ = self.command_tx.try_send(SupervisorCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.telemetry.stop();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_supervisor(
    mut cfg: RuntimeConfig,
    mut endpoints: Vec<String>,
    mut sources: SourceCatalog,
    mut live: LiveJob,
    initial_session: ElectrumSession,
    mut native_photon_session: Option<crate::node::NativePhotonSession>,
    source_capability_epoch: Instant,
    search: SearchHandle,
    mut reward_secret: [u8; 32],
    reward_public_key: [u8; 33],
    mining_payout_address: String,
    mut settlement: SettlementState,
    journal_path: PathBuf,
    mut pending_submission: Option<PendingSubmission>,
    command_rx: Receiver<SupervisorCommand>,
    event_tx: SyncSender<RuntimeEvent>,
    shared_snapshot: Arc<Mutex<RuntimeSnapshot>>,
    shutdown: ShutdownSignal,
) {
    let mut active_fulcrum_endpoint = initial_session.url.clone();
    let mut session = Some(initial_session);
    let mut state = if pending_submission.is_some() {
        SupervisorState::Paused
    } else {
        SupervisorState::Mining
    };
    let mut user_paused = false;
    let mut pending_winner: Option<VerifiedWinner> = None;
    let mut state_checks = 0u64;
    let mut refresh_failures = RefreshFailureTracker::default();
    let mut job_changes = 0u64;
    let mut reconnects = 0u64;
    let mut endpoint_rotations = 0u64;
    let mut reconnect_attempts = 0u64;
    let mut stale_winners = 0u64;
    let mut verified_winners = 0u64;
    let mut pending_winners = u64::from(pending_submission.is_some());
    let mut last_error = None;
    let mut reconnect_backoff = RECONNECT_MIN;
    let mut next_reconnect = Instant::now();
    let mut next_state_refresh = Instant::now() + PHOTON_STATE_RECHECK;
    let mut submission_backoff = RECONNECT_MIN;
    let mut next_submission_retry = Instant::now();
    let mut stop = false;
    let initial_search_stats = search.snapshot();
    // This SearchHandle is new for this supervisor. Start at zero so a GPU
    // winner produced after SearchHandle::start but before this supervisor
    // thread is scheduled still triggers the authoritative freshness gate.
    let mut observed_search_winners = 0;
    let mut winner_refresh_pending = false;
    let mut throughput = ThroughputTracker::new(
        Instant::now(),
        initial_search_stats.candidates,
        initial_search_stats.state == MiningState::Mining,
    );

    while !stop {
        if shutdown.is_requested() {
            user_paused = true;
            let _ = search.apply_control(SearchCommand::Pause);
            state = SupervisorState::Paused;
        }
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
                    let unacknowledged = search.snapshot().winners > observed_search_winners;
                    let result = resume_search_if_safe(
                        &shutdown,
                        session.is_some(),
                        pending_winners,
                        unacknowledged,
                        &mut user_paused,
                        || search.apply_control(SearchCommand::Resume).map(|_| ()),
                    );
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
                    let mut next_cfg = cfg.clone();
                    let result = next_cfg.set_payout(payout).and_then(|()| {
                        if next_cfg.generation_id == cfg.generation_id {
                            return Ok(());
                        }
                        let relay_fee_sats_per_kb = production_relay_fee_sats_per_kb(&next_cfg)?;
                        production_preflight_local(
                            &next_cfg,
                            &live,
                            &reward_secret,
                            &reward_public_key,
                            &mining_payout_address,
                            &journal_path,
                            relay_fee_sats_per_kb,
                        )?;
                        let next_settlement = settlement.restamp(next_cfg.generation_id, &live)?;
                        search.replace_job(
                            live.to_mining_job(next_cfg.generation_id, &mining_payout_address),
                        )?;
                        cfg = next_cfg;
                        settlement = next_settlement;
                        job_changes = job_changes.saturating_add(1);
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
                    let result = match prepare_fulcrum_endpoint_change(&cfg, endpoint.as_deref()) {
                        Err(error) => Err(error),
                        Ok(None) => Ok(()),
                        Ok(Some((next_cfg, next_endpoints))) => {
                            let relay_fee_sats_per_kb = production_relay_fee_sats_per_kb(&next_cfg);
                            let next_settlement = settlement
                                .restamp(next_cfg.generation_id, &live)
                                .and_then(|next_settlement| {
                                    let relay_fee_sats_per_kb = relay_fee_sats_per_kb?;
                                    production_preflight_local(
                                        &next_cfg,
                                        &live,
                                        &reward_secret,
                                        &reward_public_key,
                                        &mining_payout_address,
                                        &journal_path,
                                        relay_fee_sats_per_kb,
                                    )?;
                                    Ok(next_settlement)
                                });
                            next_settlement.and_then(|next_settlement| {
                                let next_sources = SourceCatalog::configured(&next_cfg)?;
                                search.apply_control(SearchCommand::Pause)?;
                                search.replace_job(live.to_mining_job(
                                    next_cfg.generation_id,
                                    &mining_payout_address,
                                ))?;
                                cfg = next_cfg;
                                sources = next_sources;
                                settlement = next_settlement;
                                endpoints = next_endpoints;
                                job_changes = job_changes.saturating_add(1);
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
                                Ok(())
                            })
                        }
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
                    shutdown.request();
                    user_paused = true;
                    let _ = search.apply_control(SearchCommand::Pause);
                    state = SupervisorState::Paused;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if should_begin_winner_refresh(
            winner_refresh_pending,
            pending_winner.is_some(),
            pending_submission.is_some(),
            observed_search_winners,
            search.snapshot().winners,
        ) {
            // Stop new launches as soon as a host-verified GPU winner is
            // queued. Any batch already in flight is allowed to finish before
            // the authoritative live-state check below.
            winner_refresh_pending = true;
            pending_winners = 1;
            let _ = search.apply_control(SearchCommand::Pause);
            state = SupervisorState::Paused;
        }
        if shutdown.is_requested()
            && shutdown_can_exit(
                pending_winner.is_some() || winner_refresh_pending,
                search.batch_in_flight(),
            )
        {
            stop = true;
        }
        if stop {
            break;
        }
        let force_winner_refresh = winner_refresh_ready(
            winner_refresh_pending,
            search.batch_in_flight(),
            session.is_some(),
        );
        if force_winner_refresh {
            // The worker is now at a safe boundary. Run the existing
            // authoritative refresh immediately rather than waiting for the
            // next 500 ms periodic deadline.
            next_state_refresh = Instant::now();
        }
        if session.is_none() {
            if Instant::now() >= next_reconnect {
                let now_ms = source_capability_now_ms(source_capability_epoch);
                let rotation_key = u64::from(std::process::id())
                    .wrapping_add(cfg.generation_id)
                    .wrapping_add(reconnect_attempts);
                reconnect_attempts = reconnect_attempts.saturating_add(1);
                endpoints = eligible_fulcrum_endpoints(
                    &sources,
                    now_ms,
                    rotation_key,
                    Some(&active_fulcrum_endpoint),
                );
                let reconnect_result = if endpoints.is_empty() {
                    Err(
                        "no eligible Fulcrum source is currently available under source policy"
                            .to_string(),
                    )
                } else {
                    ElectrumSession::connect_failover(&endpoints)
                        .and_then(|mut next| next.fetch_live_job().map(|job| (next, job)))
                };
                match reconnect_result {
                    Ok((next_session, next_job)) => {
                        let connected_endpoint = next_session.url.clone();
                        if !connected_endpoint.eq_ignore_ascii_case(&active_fulcrum_endpoint) {
                            endpoint_rotations = endpoint_rotations.saturating_add(1);
                            emit(
                                &event_tx,
                                RuntimeEvent::EndpointRotated {
                                    from: active_fulcrum_endpoint.clone(),
                                    to: connected_endpoint.clone(),
                                },
                            );
                        }
                        active_fulcrum_endpoint = connected_endpoint;
                        reconnects = reconnects.saturating_add(1);
                        refresh_failures.success();
                        reconnect_backoff = RECONNECT_MIN;
                        last_error = None;
                        session = Some(next_session);
                        match apply_refreshed_job(
                            session.as_mut().expect("session was just installed"),
                            &mut cfg,
                            &mut live,
                            &mut settlement,
                            &search,
                            &reward_secret,
                            &reward_public_key,
                            &mining_payout_address,
                            &journal_path,
                            next_job,
                        ) {
                            Ok(changed) => {
                                if changed {
                                    job_changes = job_changes.saturating_add(1);
                                }
                                emit(&event_tx, RuntimeEvent::Reconnected(live.url.clone()));
                                next_state_refresh = Instant::now() + PHOTON_STATE_RECHECK;
                                if search_resume_allowed(&shutdown, user_paused, pending_winners) {
                                    let _ = search.apply_control(SearchCommand::Resume);
                                    state = SupervisorState::Mining;
                                } else {
                                    state = SupervisorState::Paused;
                                }
                            }
                            Err(error) => {
                                last_error = Some(error.clone());
                                state = SupervisorState::Reconnecting;
                                emit(
                                    &event_tx,
                                    RuntimeEvent::Reconnecting(format!(
                                        "generation settlement preflight retry: {error}"
                                    )),
                                );
                                session = None;
                                next_reconnect = Instant::now() + reconnect_backoff;
                                reconnect_backoff = reconnect_backoff
                                    .checked_mul(2)
                                    .unwrap_or(RECONNECT_MAX)
                                    .min(RECONNECT_MAX);
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
                let winner = pending_winner.as_ref().expect("checked above");
                match prepare_pending_submission(
                    winner,
                    &cfg,
                    &live,
                    &reward_secret,
                    &reward_public_key,
                    &settlement,
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
        } else if pending_submission.is_some() {
            if Instant::now() >= next_submission_retry {
                let pending = pending_submission.as_ref().expect("checked above").clone();
                let attempt = attempt_pending_submission(
                    session.as_mut().expect("checked session above"),
                    &cfg,
                    &pending,
                    &journal_path,
                );
                match attempt {
                    Ok(SubmissionAttempt::Complete(fresh)) => {
                        match resolve_confirmed_submission(&pending, &fresh, &journal_path) {
                            Ok(()) => {
                                emit(
                                    &event_tx,
                                    RuntimeEvent::SubmissionAccepted {
                                        parent_txid: pending.parent_txid,
                                        child_txid: pending.settlement_txid,
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
                                        session.as_mut().expect("checked session above"),
                                        &mut cfg,
                                        &mut live,
                                        &mut settlement,
                                        &search,
                                        &reward_secret,
                                        &reward_public_key,
                                        &mining_payout_address,
                                        &journal_path,
                                        next_job,
                                    )
                                }) {
                                    Ok(changed) => {
                                        if changed {
                                            job_changes = job_changes.saturating_add(1);
                                        }
                                        next_state_refresh = Instant::now() + PHOTON_STATE_RECHECK;
                                        if !search_resume_allowed(
                                            &shutdown,
                                            user_paused,
                                            pending_winners,
                                        ) {
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
                                        reconnect_backoff = reconnect_backoff
                                            .checked_mul(2)
                                            .unwrap_or(RECONNECT_MAX)
                                            .min(RECONNECT_MAX);
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
                        match resolve_stale_submission(&pending, &next_job, &journal_path) {
                            Ok(()) => {
                                pending_submission = None;
                                pending_winners = 0;
                                stale_winners = stale_winners.saturating_add(1);
                                last_error = None;
                                match apply_refreshed_job(
                                    session.as_mut().expect("checked session above"),
                                    &mut cfg,
                                    &mut live,
                                    &mut settlement,
                                    &search,
                                    &reward_secret,
                                    &reward_public_key,
                                    &mining_payout_address,
                                    &journal_path,
                                    *next_job,
                                ) {
                                    Ok(changed) => {
                                        if changed {
                                            job_changes = job_changes.saturating_add(1);
                                        }
                                        next_state_refresh = Instant::now() + PHOTON_STATE_RECHECK;
                                        if !search_resume_allowed(
                                            &shutdown,
                                            user_paused,
                                            pending_winners,
                                        ) {
                                            state = SupervisorState::Paused;
                                        } else {
                                            let _ = search.apply_control(SearchCommand::Resume);
                                            state = SupervisorState::Mining;
                                        }
                                    }
                                    Err(error) => {
                                        last_error = Some(error.clone());
                                        state = SupervisorState::Reconnecting;
                                        emit(
                                            &event_tx,
                                            RuntimeEvent::Reconnecting(format!(
                                                "generation settlement preflight retry: {error}"
                                            )),
                                        );
                                        session = None;
                                        next_reconnect = Instant::now() + reconnect_backoff;
                                        reconnect_backoff = reconnect_backoff
                                            .checked_mul(2)
                                            .unwrap_or(RECONNECT_MAX)
                                            .min(RECONNECT_MAX);
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
        } else if Instant::now() >= next_state_refresh {
            let previous_state_refresh = next_state_refresh;
            let refreshed = refresh_photon_job_on_cadence(
                &cfg,
                session.as_mut().expect("checked session above"),
                &mut sources,
                &mut native_photon_session,
                source_capability_epoch,
            );
            next_state_refresh = next_periodic_deadline(
                previous_state_refresh,
                Instant::now(),
                PHOTON_STATE_RECHECK,
            );
            match refreshed {
                Ok(boundary) => {
                    refresh_failures.success();
                    if let Some(warning) = boundary.route_warning.as_ref() {
                        if last_error.as_deref() != Some(warning.as_str()) {
                            emit(&event_tx, RuntimeEvent::Error(warning.clone()));
                        }
                        last_error = Some(warning.clone());
                    }
                    state_checks = state_checks.saturating_add(1);
                    match apply_refreshed_job(
                        session.as_mut().expect("checked session above"),
                        &mut cfg,
                        &mut live,
                        &mut settlement,
                        &search,
                        &reward_secret,
                        &reward_public_key,
                        &mining_payout_address,
                        &journal_path,
                        boundary.job,
                    ) {
                        Ok(changed) => {
                            if changed {
                                job_changes = job_changes.saturating_add(1);
                            }
                            emit_job_change_if_changed(
                                &event_tx,
                                changed,
                                cfg.generation_id,
                                &live,
                            );

                            if force_winner_refresh {
                                for winner in search.drain_winners() {
                                    if winner_matches_live(&winner, cfg.generation_id, &live) {
                                        verified_winners = verified_winners.saturating_add(1);
                                        pending_winners = 1;
                                        let _ = search.apply_control(SearchCommand::Pause);
                                        state = SupervisorState::Paused;
                                        emit(
                                            &event_tx,
                                            RuntimeEvent::VerifiedWinner(winner.clone()),
                                        );
                                        match prepare_pending_submission(
                                            &winner,
                                            &cfg,
                                            &live,
                                            &reward_secret,
                                            &reward_public_key,
                                            &settlement,
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
                                                        "settlement preparation retry: {error}"
                                                    )),
                                                );
                                                session = None;
                                                next_reconnect =
                                                    Instant::now() + submission_backoff;
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
                                // The worker pauses after a verified winning
                                // batch, so this snapshot acknowledges every
                                // bounded winner queued through that boundary.
                                observed_search_winners = search.snapshot().winners;
                                winner_refresh_pending = false;
                                if pending_winner.is_none() && pending_submission.is_none() {
                                    pending_winners = 0;
                                }
                            }

                            if pending_winners == 0 && boundary.route_warning.is_none() {
                                last_error = None;
                            }
                            if (changed || force_winner_refresh)
                                && session.is_some()
                                && search_resume_allowed(&shutdown, user_paused, pending_winners)
                            {
                                let _ = search.apply_control(SearchCommand::Resume);
                                state = SupervisorState::Mining;
                            }
                        }
                        Err(error) => {
                            last_error = Some(error.clone());
                            state = SupervisorState::Reconnecting;
                            emit(
                                &event_tx,
                                RuntimeEvent::Reconnecting(format!(
                                    "generation settlement preflight retry: {error}"
                                )),
                            );
                            session = None;
                            next_reconnect = Instant::now() + reconnect_backoff;
                            reconnect_backoff = reconnect_backoff
                                .checked_mul(2)
                                .unwrap_or(RECONNECT_MAX)
                                .min(RECONNECT_MAX);
                        }
                    }
                }
                Err(error) => {
                    last_error = Some(error.clone());
                    let failure_kind = classify_refresh_failure(&error);
                    match refresh_failures.failure(failure_kind) {
                        RefreshFailureAction::RetainGeneration => {
                            emit(
                                &event_tx,
                                RuntimeEvent::StateRefreshFailed {
                                    error,
                                    consecutive: refresh_failures.consecutive,
                                },
                            );
                        }
                        RefreshFailureAction::Reconnect => {
                            let now_ms = source_capability_now_ms(source_capability_epoch);
                            let _ = sources.record_failure(
                                SourceKind::Fulcrum,
                                &active_fulcrum_endpoint,
                                now_ms,
                            );
                            state = SupervisorState::Reconnecting;
                            let reason = match failure_kind {
                                RefreshFailureKind::Transport => {
                                    format!("Fulcrum transport lost; reconnecting: {error}")
                                }
                                RefreshFailureKind::Transient => format!(
                                    "Fulcrum PHOTON refresh failed {} consecutive times; reconnecting: {error}",
                                    refresh_failures.consecutive
                                ),
                            };
                            emit(&event_tx, RuntimeEvent::Reconnecting(reason));
                            session = None;
                            next_reconnect = Instant::now() + reconnect_backoff;
                            reconnect_backoff = reconnect_backoff
                                .checked_mul(2)
                                .unwrap_or(RECONNECT_MAX)
                                .min(RECONNECT_MAX);
                        }
                    }
                }
            }
        }

        write_snapshot(
            &shared_snapshot,
            state,
            &cfg,
            &live,
            &search,
            state_checks,
            &refresh_failures,
            job_changes,
            reconnects,
            endpoint_rotations,
            stale_winners,
            verified_winners,
            pending_winners,
            last_error.clone(),
            &mut throughput,
        );
        thread::sleep(SUPERVISOR_POLL);
    }

    let mut final_stats = search.stop();
    reward_secret.fill(0);
    let mut snapshot = shared_snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    final_stats.current_rate = 0.0;
    final_stats.peak_rate = snapshot.search.peak_rate;
    snapshot.state = SupervisorState::Stopped;
    snapshot.search = final_stats;
}

struct PhotonBoundaryRefresh {
    job: LiveJob,
    route_warning: Option<String>,
}

fn select_boundary_photon_job(
    cfg: &RuntimeConfig,
    sources: &SourceCatalog,
    native_snapshot: Option<&LiveStateSnapshot>,
    canonical_snapshot: &LiveStateSnapshot,
    now_ms: u64,
) -> LiveJob {
    let node_endpoint = cfg.node_url.as_deref().map(crate::node::redact_url);
    if let (Some(endpoint), Some(native), Some(selected)) = (
        node_endpoint.as_deref(),
        native_snapshot,
        sources
            .router()
            .select(SourceCapability::PhotonState, now_ms),
    ) {
        if selected.kind == SourceKind::NativeNode
            && selected.endpoint == endpoint
            && native.job.url == endpoint
        {
            return native.job.clone();
        }
    }
    canonical_snapshot.job.clone()
}

fn record_canonical_fulcrum_probe(
    sources: &mut SourceCatalog,
    canonical_snapshot: &LiveStateSnapshot,
    now_ms: u64,
    latency_ms: u32,
) -> Result<(), String> {
    let endpoint = canonical_snapshot.job.url.as_str();
    if !sources
        .entries()
        .iter()
        .any(|entry| entry.kind == SourceKind::Fulcrum && entry.endpoint == endpoint)
    {
        sources.add_user(SourceKind::Fulcrum, endpoint, "Active Fulcrum")?;
    }
    sources.record_success(SourceKind::Fulcrum, endpoint, now_ms, latency_ms)?;
    sources.verify_capability(
        SourceKind::Fulcrum,
        endpoint,
        SourceCapability::PhotonState,
        now_ms,
        DEFAULT_CAPABILITY_TTL_MS,
    )
}

fn record_native_photon_probe(
    sources: &mut SourceCatalog,
    endpoint: &str,
    canonical_snapshot: &LiveStateSnapshot,
    native_result: Result<LiveStateSnapshot, String>,
    now_ms: u64,
    latency_ms: u32,
) -> (Option<LiveStateSnapshot>, Option<String>) {
    let endpoint_identity = crate::node::redact_url(endpoint);
    let verified = native_result.and_then(|snapshot| {
        crate::node::verify_photon_state_equivalence(&snapshot, canonical_snapshot)?;
        let proof =
            NativePhotonEquivalenceProof::from_verified_snapshot(&endpoint_identity, &snapshot)?;
        sources.record_success(
            SourceKind::NativeNode,
            &endpoint_identity,
            now_ms,
            latency_ms,
        )?;
        sources.verify_native_photon_capability(&proof, now_ms, DEFAULT_CAPABILITY_TTL_MS)?;
        Ok(snapshot)
    });
    match verified {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => {
            let _ = sources.revoke_native_photon_capability(&endpoint_identity);
            let _ = sources.record_failure(SourceKind::NativeNode, &endpoint_identity, now_ms);
            (
                None,
                Some(format!(
                    "same-tip BCHN/Fulcrum equivalence proof failed: {error}"
                )),
            )
        }
    }
}

fn refresh_native_with_current_proof(
    sources: &mut SourceCatalog,
    endpoint: &str,
    native_result: Result<LiveStateSnapshot, String>,
    now_ms: u64,
    latency_ms: u32,
) -> Result<LiveStateSnapshot, String> {
    let endpoint_identity = crate::node::redact_url(endpoint);
    let proof = sources
        .native_photon_proof_at(&endpoint_identity, now_ms)
        .cloned()
        .ok_or_else(|| {
            "native-node PHOTON continuity requires a current canonical equivalence proof"
                .to_string()
        })?;

    match native_result {
        Ok(snapshot) => {
            // A native-only refresh proves liveness, not canonical equivalence. Keep the
            // original capability expiry so Fulcrum must be available again before the
            // proof lease can be renewed. The lease remains bound to the exact proven
            // tip/work state; a changed native snapshot requires canonical re-proof.
            if let Err(error) = proof.validate_continuation(&snapshot) {
                let _ = sources.revoke_native_photon_capability(&endpoint_identity);
                let _ = sources.record_failure(SourceKind::NativeNode, &endpoint_identity, now_ms);
                return Err(error);
            }
            sources.record_success(
                SourceKind::NativeNode,
                &endpoint_identity,
                now_ms,
                latency_ms,
            )?;
            Ok(snapshot)
        }
        Err(error) => {
            let _ = sources.revoke_native_photon_capability(&endpoint_identity);
            let _ = sources.record_failure(SourceKind::NativeNode, &endpoint_identity, now_ms);
            Err(format!(
                "proven native-node PHOTON refresh failed during canonical outage: {error}"
            ))
        }
    }
}

fn refresh_photon_job_on_cadence(
    cfg: &RuntimeConfig,
    canonical: &mut ElectrumSession,
    sources: &mut SourceCatalog,
    native_session: &mut Option<crate::node::NativePhotonSession>,
    epoch: Instant,
) -> Result<PhotonBoundaryRefresh, String> {
    let now_ms = source_capability_now_ms(epoch);
    let canonical_started = Instant::now();
    let canonical_result = canonical.fetch_live_snapshot();
    let canonical_latency_ms =
        u32::try_from(canonical_started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let canonical_snapshot = match canonical_result {
        Ok(snapshot) => snapshot,
        Err(canonical_error) => {
            let endpoint = cfg.node_url.as_deref().ok_or_else(|| {
                format!(
                    "canonical Fulcrum PHOTON refresh failed and no native node is configured: {canonical_error}"
                )
            })?;
            let endpoint_identity = crate::node::redact_url(endpoint);
            if !sources.supports_at(
                SourceKind::NativeNode,
                &endpoint_identity,
                SourceCapability::PhotonState,
                now_ms,
            ) {
                return Err(format!(
                    "canonical Fulcrum PHOTON refresh failed and native equivalence proof is unavailable or expired: {canonical_error}"
                ));
            }

            let probe_started = Instant::now();
            let native_result = (|| {
                if native_session.is_none() {
                    *native_session = Some(crate::node::NativePhotonSession::connect_failover(&[
                        endpoint.to_string(),
                    ])?);
                }
                native_session
                    .as_mut()
                    .expect("native PHOTON session was initialized above")
                    .refresh()
            })();
            let latency_ms = u32::try_from(probe_started.elapsed().as_millis()).unwrap_or(u32::MAX);
            let native_snapshot = refresh_native_with_current_proof(
                sources,
                endpoint,
                native_result,
                now_ms,
                latency_ms,
            )?;
            return Ok(PhotonBoundaryRefresh {
                job: native_snapshot.job,
                route_warning: Some(format!(
                    "canonical Fulcrum PHOTON refresh failed; continuing on current proven native-node lease until re-proof or expiry: {canonical_error}"
                )),
            });
        }
    };
    record_canonical_fulcrum_probe(sources, &canonical_snapshot, now_ms, canonical_latency_ms)?;
    let Some(endpoint) = cfg.node_url.as_deref() else {
        return Ok(PhotonBoundaryRefresh {
            job: canonical_snapshot.job,
            route_warning: None,
        });
    };
    let endpoint_identity = crate::node::redact_url(endpoint);
    if !sources.available_at(SourceKind::NativeNode, &endpoint_identity, now_ms) {
        return Ok(PhotonBoundaryRefresh {
            job: canonical_snapshot.job,
            route_warning: None,
        });
    }

    let probe_started = Instant::now();
    let native_result = (|| {
        if native_session.is_none() {
            *native_session = Some(crate::node::NativePhotonSession::connect_failover(&[
                endpoint.to_string(),
            ])?);
        }
        native_session
            .as_mut()
            .expect("native PHOTON session was initialized above")
            .refresh()
    })();
    let latency_ms = u32::try_from(probe_started.elapsed().as_millis()).unwrap_or(u32::MAX);
    let (native_snapshot, native_error) = record_native_photon_probe(
        sources,
        endpoint,
        &canonical_snapshot,
        native_result,
        now_ms,
        latency_ms,
    );
    Ok(PhotonBoundaryRefresh {
        job: select_boundary_photon_job(
            cfg,
            sources,
            native_snapshot.as_ref(),
            &canonical_snapshot,
            now_ms,
        ),
        route_warning: native_error.map(|error| {
            format!("native-node PHOTON route fell back to canonical Fulcrum: {error}")
        }),
    })
}

fn prepare_generation_transition<F>(
    cfg: &RuntimeConfig,
    live: &LiveJob,
    settlement: &SettlementState,
    next: &LiveJob,
    mut preflight_next: F,
) -> Result<Option<(RuntimeConfig, SettlementState)>, String>
where
    F: FnMut(&RuntimeConfig, &LiveJob) -> Result<(), String>,
{
    if !live_job_changed(live, next) {
        return Ok(None);
    }

    settlement.ensure_current(cfg.generation_id, live)?;
    let mut next_cfg = cfg.clone();
    next_cfg.bump_generation();
    preflight_next(&next_cfg, next)?;
    let next_settlement = SettlementState::new(next_cfg.generation_id, next)?;
    Ok(Some((next_cfg, next_settlement)))
}

#[allow(clippy::too_many_arguments)]
fn apply_refreshed_job(
    session: &mut ElectrumSession,
    cfg: &mut RuntimeConfig,
    live: &mut LiveJob,
    settlement: &mut SettlementState,
    search: &SearchHandle,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    mining_payout_address: &str,
    journal_path: &Path,
    next: LiveJob,
) -> Result<bool, String> {
    if live_job_changed(live, &next) {
        // Once authoritative PHOTON work changes, stop launching the old
        // immutable generation. Route metadata such as the verified provider
        // URL may change without rebuilding identical GPU work.
        // The worker sees Pause at its next batch boundary while preflight
        // validates the replacement generation.
        search.apply_control(SearchCommand::Pause)?;
    }
    let staged =
        prepare_generation_transition(cfg, live, settlement, &next, |next_cfg, next_live| {
            production_preflight(
                session,
                next_cfg,
                next_live,
                reward_secret,
                reward_public_key,
                mining_payout_address,
                journal_path,
            )
        })?;
    if let Some((next_cfg, next_settlement)) = staged {
        search.replace_job(next.to_mining_job(next_cfg.generation_id, mining_payout_address))?;
        *cfg = next_cfg;
        *settlement = next_settlement;
        *live = next;
        Ok(true)
    } else {
        *live = next;
        Ok(false)
    }
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
    let sources = SourceCatalog::configured(&next)?;
    let rotation_key = u64::from(std::process::id()).wrapping_add(next.generation_id);
    let endpoints = sources
        .probe_candidates(SourceKind::Fulcrum, 0, AUTO_PROBE_LIMIT, rotation_key)
        .into_iter()
        .map(|entry| entry.endpoint.clone())
        .collect();
    Ok(Some((next, endpoints)))
}

fn live_job_changed(current: &LiveJob, next: &LiveJob) -> bool {
    !current.tip_hash.eq_ignore_ascii_case(&next.tip_hash)
        || current.baton_txid != next.baton_txid
        || current.baton_vout != next.baton_vout
        || current.baton_height != next.baton_height
        || current.baton_value_sats != next.baton_value_sats
        || current.height != next.height
        || current.age != next.age
        || current.commitment_hex != next.commitment_hex
        || current.target_le_hex != next.target_le_hex
        || current.token_amount != next.token_amount
        || current.reward_raw != next.reward_raw
}

fn winner_matches_live(winner: &VerifiedWinner, generation_id: u64, live: &LiveJob) -> bool {
    winner.generation_id == generation_id
        && winner.height == live.height
        && winner.baton_txid == live.baton_txid
        && winner.baton_vout == live.baton_vout
}

fn winner_refresh_ready(
    winner_refresh_pending: bool,
    batch_in_flight: bool,
    session_connected: bool,
) -> bool {
    winner_refresh_pending && !batch_in_flight && session_connected
}

fn should_begin_winner_refresh(
    winner_refresh_pending: bool,
    has_pending_winner: bool,
    has_pending_submission: bool,
    observed_winners: u64,
    current_winners: u64,
) -> bool {
    !winner_refresh_pending
        && !has_pending_winner
        && !has_pending_submission
        && current_winners > observed_winners
}

fn shutdown_can_exit(pending_winner: bool, batch_in_flight: bool) -> bool {
    !pending_winner && !batch_in_flight
}

fn search_resume_allowed(
    shutdown: &ShutdownSignal,
    user_paused: bool,
    pending_winners: u64,
) -> bool {
    !shutdown.is_requested() && !user_paused && pending_winners == 0
}

fn resume_search_if_safe<F>(
    shutdown: &ShutdownSignal,
    session_connected: bool,
    pending_winners: u64,
    unacknowledged_winner: bool,
    user_paused: &mut bool,
    mut resume_search: F,
) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    if shutdown.is_requested() {
        return Err("cannot resume while shutdown is requested".into());
    }
    if !session_connected {
        return Err("cannot resume while authoritative PHOTON state is disconnected".into());
    }
    if pending_winners > 0 {
        return Err("cannot resume while a verified winner is pending handling".into());
    }
    if unacknowledged_winner {
        return Err("cannot resume while a verified winner awaits supervisor refresh".into());
    }

    resume_search()?;
    *user_paused = false;
    Ok(())
}

const THROUGHPUT_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
const THROUGHPUT_CURRENT_WINDOW: Duration = Duration::from_secs(2);
const THROUGHPUT_MIN_HISTORY: Duration = Duration::from_secs(1);
const THROUGHPUT_HISTORY_CAP: usize = 16;

struct ThroughputTracker {
    last_sample: Instant,
    last_progress_at: Instant,
    last_progress_candidates: u64,
    samples: std::collections::VecDeque<(Instant, u64)>,
    current_rate: f64,
    peak_rate: f64,
    was_mining: bool,
}

impl ThroughputTracker {
    fn new(now: Instant, candidates: u64, mining: bool) -> Self {
        let mut samples = std::collections::VecDeque::with_capacity(THROUGHPUT_HISTORY_CAP);
        samples.push_back((now, candidates));
        Self {
            last_sample: now,
            last_progress_at: now,
            last_progress_candidates: candidates,
            samples,
            current_rate: 0.0,
            peak_rate: 0.0,
            was_mining: mining,
        }
    }

    fn observe(&mut self, stats: &mut SearchStats, now: Instant) {
        let mining = stats.state == MiningState::Mining;
        if !mining {
            self.samples.clear();
            self.samples.push_back((now, stats.candidates));
            self.last_sample = now;
            self.last_progress_at = now;
            self.last_progress_candidates = stats.candidates;
            self.current_rate = 0.0;
            self.was_mining = false;
            stats.current_rate = 0.0;
            stats.peak_rate = self.peak_rate;
            return;
        }

        if !self.was_mining {
            self.samples.clear();
            self.samples.push_back((now, stats.candidates));
            self.last_sample = now;
            self.last_progress_at = now;
            self.last_progress_candidates = stats.candidates;
            self.current_rate = 0.0;
            self.was_mining = true;
        } else if now.saturating_duration_since(self.last_sample) >= THROUGHPUT_SAMPLE_INTERVAL {
            let made_progress = stats.candidates > self.last_progress_candidates;
            if made_progress {
                self.last_progress_at = now;
                self.last_progress_candidates = stats.candidates;
            }

            if self.samples.len() == THROUGHPUT_HISTORY_CAP {
                self.samples.pop_front();
            }
            self.samples.push_back((now, stats.candidates));

            while self.samples.len() > 2 {
                let Some((sample_time, _)) = self.samples.front() else {
                    break;
                };
                if now.saturating_duration_since(*sample_time) <= THROUGHPUT_CURRENT_WINDOW {
                    break;
                }
                self.samples.pop_front();
            }

            if let Some((sample_time, sample_candidates)) = self.samples.front() {
                let elapsed = now.saturating_duration_since(*sample_time);
                if made_progress && elapsed >= THROUGHPUT_MIN_HISTORY {
                    let completed = stats.candidates.saturating_sub(*sample_candidates);
                    self.current_rate = completed as f64 / elapsed.as_secs_f64();
                    self.peak_rate = self.peak_rate.max(self.current_rate);
                } else if now.saturating_duration_since(self.last_progress_at)
                    >= THROUGHPUT_CURRENT_WINDOW
                {
                    self.current_rate = 0.0;
                }
            }
            self.last_sample = now;
        }

        stats.current_rate = self.current_rate;
        stats.peak_rate = self.peak_rate;
    }
}

#[allow(clippy::too_many_arguments)]
fn write_snapshot(
    shared: &Arc<Mutex<RuntimeSnapshot>>,
    state: SupervisorState,
    cfg: &RuntimeConfig,
    live: &LiveJob,
    search: &SearchHandle,
    state_checks: u64,
    refresh_failures: &RefreshFailureTracker,
    job_changes: u64,
    reconnects: u64,
    endpoint_rotations: u64,
    stale_winners: u64,
    verified_winners: u64,
    pending_winners: u64,
    last_error: Option<String>,
    throughput: &mut ThroughputTracker,
) {
    let mut search_stats = search.snapshot();
    throughput.observe(&mut search_stats, Instant::now());
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
    snapshot.state_checks = state_checks;
    snapshot.transient_refresh_failures = refresh_failures.transient_total;
    snapshot.transport_failures = refresh_failures.transport_total;
    snapshot.consecutive_refresh_failures = refresh_failures.consecutive;
    snapshot.source_degraded = refresh_failures.degraded();
    snapshot.job_changes = job_changes;
    snapshot.reconnects = reconnects;
    snapshot.endpoint_rotations = endpoint_rotations;
    snapshot.stale_winners = stale_winners;
    snapshot.verified_winners = verified_winners;
    snapshot.pending_winners = pending_winners;
    snapshot.last_error = last_error;
    snapshot.search = search_stats;
    if snapshot.state == SupervisorState::Mining && snapshot.search.state == MiningState::Paused {
        snapshot.state = SupervisorState::Paused;
    }
}

fn emit(tx: &SyncSender<RuntimeEvent>, event: RuntimeEvent) {
    match tx.try_send(event) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

fn emit_job_change_if_changed(
    tx: &SyncSender<RuntimeEvent>,
    changed: bool,
    generation_id: u64,
    live: &LiveJob,
) {
    if changed {
        emit(
            tx,
            RuntimeEvent::JobRefreshed {
                generation_id,
                height: live.height,
                baton_txid: live.baton_txid.clone(),
                baton_vout: live.baton_vout,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::sync_channel;

    const TEST_PAYOUT: &str = "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh";
    static PREFLIGHT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn throughput_stats(candidates: u64, state: MiningState, average_rate: f64) -> SearchStats {
        SearchStats {
            candidates,
            batches: 0,
            intensity: 100,
            state,
            elapsed_secs: 0,
            rate: average_rate,
            current_rate: 0.0,
            peak_rate: 0.0,
            winners: 0,
        }
    }

    #[test]
    fn shared_throughput_tracks_current_average_peak_and_resets_after_pause() {
        let start = Instant::now();
        let mut tracker = ThroughputTracker::new(start, 0, true);

        let mut first = throughput_stats(1_000, MiningState::Mining, 900.0);
        tracker.observe(&mut first, start + Duration::from_secs(1));
        assert!((first.current_rate - 1_000.0).abs() < 0.01);
        assert_eq!(first.rate, 900.0);
        assert!((first.peak_rate - 1_000.0).abs() < 0.01);

        let mut faster = throughput_stats(3_000, MiningState::Mining, 1_400.0);
        tracker.observe(&mut faster, start + Duration::from_secs(2));
        assert!((faster.current_rate - 1_500.0).abs() < 0.01);
        assert!((faster.peak_rate - 1_500.0).abs() < 0.01);

        let mut paused = throughput_stats(3_000, MiningState::Paused, 1_200.0);
        tracker.observe(&mut paused, start + Duration::from_millis(2_100));
        assert_eq!(paused.current_rate, 0.0);
        assert!((paused.peak_rate - 1_500.0).abs() < 0.01);

        let mut resumed = throughput_stats(3_000, MiningState::Mining, 1_100.0);
        tracker.observe(&mut resumed, start + Duration::from_secs(3));
        assert_eq!(resumed.current_rate, 0.0);

        resumed.candidates = 4_000;
        tracker.observe(&mut resumed, start + Duration::from_secs(4));
        assert!((resumed.current_rate - 1_000.0).abs() < 0.01);
        assert!((resumed.peak_rate - 1_500.0).abs() < 0.01);
    }

    #[test]
    fn shared_throughput_rejects_short_batch_spikes_and_holds_between_batches() {
        let start = Instant::now();
        let mut tracker = ThroughputTracker::new(start, 0, true);

        let mut stats = throughput_stats(20_000, MiningState::Mining, 0.0);
        tracker.observe(&mut stats, start + Duration::from_millis(250));
        assert_eq!(stats.current_rate, 0.0);
        assert_eq!(stats.peak_rate, 0.0);

        tracker.observe(&mut stats, start + Duration::from_millis(750));
        assert_eq!(stats.current_rate, 0.0);
        assert_eq!(stats.peak_rate, 0.0);

        stats.candidates = 40_000;
        tracker.observe(&mut stats, start + Duration::from_secs(1));
        assert!((stats.current_rate - 40_000.0).abs() < 0.01);
        assert!((stats.peak_rate - 40_000.0).abs() < 0.01);

        tracker.observe(&mut stats, start + Duration::from_millis(1_250));
        assert!((stats.current_rate - 40_000.0).abs() < 0.01);
        assert!((stats.peak_rate - 40_000.0).abs() < 0.01);
    }

    #[test]
    fn shared_throughput_history_is_bounded_and_real_sustained_rate_updates_peak() {
        let start = Instant::now();
        let mut tracker = ThroughputTracker::new(start, 0, true);
        let mut stats = throughput_stats(0, MiningState::Mining, 0.0);

        for tick in 1..=40_u64 {
            stats.candidates = tick * 250;
            tracker.observe(
                &mut stats,
                start + Duration::from_millis(tick.saturating_mul(250)),
            );
            assert!(tracker.samples.len() <= THROUGHPUT_HISTORY_CAP);
        }
        let baseline_peak = stats.peak_rate;
        assert!(baseline_peak >= 1_000.0);

        for tick in 41..=52_u64 {
            stats.candidates = 10_000 + (tick - 40) * 500;
            tracker.observe(
                &mut stats,
                start + Duration::from_millis(tick.saturating_mul(250)),
            );
            assert!(tracker.samples.len() <= THROUGHPUT_HISTORY_CAP);
        }
        assert!(stats.peak_rate > baseline_peak);
        assert!(stats.current_rate > 1_000.0);
    }

    #[test]
    fn shared_throughput_reports_zero_after_a_real_no_progress_stall() {
        let start = Instant::now();
        let mut tracker = ThroughputTracker::new(start, 0, true);
        let mut stats = throughput_stats(1_000, MiningState::Mining, 0.0);

        tracker.observe(&mut stats, start + Duration::from_secs(1));
        assert!((stats.current_rate - 1_000.0).abs() < 0.01);

        tracker.observe(&mut stats, start + Duration::from_secs(2));
        assert!((stats.current_rate - 1_000.0).abs() < 0.01);

        tracker.observe(&mut stats, start + Duration::from_secs(3));
        assert_eq!(stats.current_rate, 0.0);
        assert!((stats.peak_rate - 1_000.0).abs() < 0.01);
    }

    fn live_job() -> LiveJob {
        LiveJob {
            url: "wss://one.invalid".into(),
            server_version: serde_json::json!(["Fulcrum", "1.5"]),
            height: 1_000,
            tip_hash: "22".repeat(32),
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

    fn live_snapshot(url: &str) -> LiveStateSnapshot {
        let mut job = live_job();
        job.url = url.into();
        LiveStateSnapshot {
            tip_hash: "22".repeat(32),
            job,
        }
    }

    #[test]
    fn unchanged_state_check_emits_no_job_change_event() {
        let job = live_job();
        let (event_tx, event_rx) = sync_channel(1);

        emit_job_change_if_changed(&event_tx, false, 7, &job);
        assert!(matches!(event_rx.try_recv(), Err(TryRecvError::Empty)));

        emit_job_change_if_changed(&event_tx, true, 8, &job);
        match event_rx.try_recv().expect("changed state must emit once") {
            RuntimeEvent::JobRefreshed {
                generation_id,
                height,
                baton_txid,
                baton_vout,
            } => {
                assert_eq!(generation_id, 8);
                assert_eq!(height, job.height);
                assert_eq!(baton_txid, job.baton_txid);
                assert_eq!(baton_vout, job.baton_vout);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(matches!(event_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    fn transaction_spending(txid: &str, vout: u32) -> String {
        let mut previous = hex::decode(txid).unwrap();
        previous.reverse();
        let mut raw = Vec::new();
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&previous);
        raw.extend_from_slice(&vout.to_le_bytes());
        raw.push(0);
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&0u64.to_le_bytes());
        raw.push(0);
        raw.extend_from_slice(&0u32.to_le_bytes());
        hex::encode(raw)
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

    fn signed_winner(
        generation_id: u64,
        job: &LiveJob,
        mining_payout_address: &str,
    ) -> VerifiedWinner {
        let mining_secret = [1u8; 32];
        let mining_public = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(mining_secret).unwrap(),
        )
        .serialize();
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
            mining_payout_address,
            &hex::encode(mining_public),
            nonce,
            &hex::encode(signature),
        )
        .unwrap();
        VerifiedWinner {
            generation_id,
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            nonce,
            digest: crate::search::hash256(&transaction),
            public_key: mining_public,
            signature,
            transaction,
        }
    }

    fn preflight_fixture() -> (RuntimeConfig, LiveJob, [u8; 32], [u8; 33], String, PathBuf) {
        let mut cfg = RuntimeConfig::default();
        cfg.set_payout(TEST_PAYOUT.into()).unwrap();
        let job = live_job();
        let reward_secret = [2u8; 32];
        let reward_public = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(reward_secret).unwrap(),
        )
        .serialize();
        let mining_payout = reward::p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let unique = format!(
            "pickaxe-preflight-{}-{}.json",
            std::process::id(),
            PREFLIGHT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        );
        let journal = std::env::temp_dir().join(unique);
        (
            cfg,
            job,
            reward_secret,
            reward_public,
            mining_payout,
            journal,
        )
    }

    #[test]
    fn photon_work_change_invalidates_generation_but_route_metadata_does_not() {
        let current = live_job();
        let mut next = current.clone();
        assert!(!live_job_changed(&current, &next));

        next.url = "http://equivalent-node.invalid".into();
        next.server_version = serde_json::json!(["BCHN", "28.0"]);
        assert!(
            !live_job_changed(&current, &next),
            "verified route metadata must not rebuild identical immutable PHOTON work"
        );

        next = current.clone();
        next.tip_hash = "33".repeat(32);
        assert!(
            live_job_changed(&current, &next),
            "same-height BCH tip replacement must invalidate the mining generation"
        );

        next = current.clone();
        next.height += 1;
        next.age += 1;
        assert!(live_job_changed(&current, &next));

        let mut baton = current.clone();
        baton.baton_txid = "22".repeat(32);
        assert!(live_job_changed(&current, &baton));
    }

    #[test]
    fn same_height_tip_reorg_stages_one_new_generation() {
        let (cfg, current, _secret, _public, _mining_payout, _journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &current).unwrap();
        let mut reorged = current.clone();
        reorged.tip_hash = "33".repeat(32);
        let mut preflight_calls = 0;

        let staged = prepare_generation_transition(
            &cfg,
            &current,
            &settlement,
            &reorged,
            |_next_cfg, _next_live| {
                preflight_calls += 1;
                Ok(())
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(staged.0.generation_id, cfg.generation_id + 1);
        assert_eq!(preflight_calls, 1);

        let unchanged = prepare_generation_transition(
            &staged.0,
            &reorged,
            &staged.1,
            &reorged,
            |_next_cfg, _next_live| Ok(()),
        )
        .unwrap();
        assert!(unchanged.is_none());
    }

    #[test]
    fn native_boundary_route_requires_current_typed_proof() {
        let endpoint = "http://node.invalid";
        let mut cfg = RuntimeConfig::default();
        cfg.set_node_url(endpoint).unwrap();
        let mut sources = SourceCatalog::configured(&cfg).unwrap();
        let canonical = live_snapshot("wss://fulcrum.invalid");
        let native = live_snapshot(endpoint);
        record_canonical_fulcrum_probe(&mut sources, &canonical, 1_000, 50).unwrap();

        let (accepted, error) = record_native_photon_probe(
            &mut sources,
            endpoint,
            &canonical,
            Ok(native.clone()),
            1_000,
            2,
        );
        assert!(error.is_none());
        assert!(accepted.is_some());
        let selected =
            select_boundary_photon_job(&cfg, &sources, accepted.as_ref(), &canonical, 1_000);
        assert_eq!(selected.url, endpoint);
        assert!(
            !live_job_changed(&canonical.job, &selected),
            "same-tip equivalent Fulcrum -> native routing must not rebuild GPU work"
        );

        let expired = 1_000 + DEFAULT_CAPABILITY_TTL_MS + 1;
        let fallback =
            select_boundary_photon_job(&cfg, &sources, Some(&native), &canonical, expired);
        assert_eq!(fallback.url, canonical.job.url);
        assert!(
            !live_job_changed(&native.job, &fallback),
            "same-tip equivalent native -> Fulcrum fallback must not rebuild GPU work"
        );
    }

    #[test]
    fn native_boundary_route_uses_redacted_endpoint_identity() {
        let endpoint = "http://u:p@node.invalid";
        let endpoint_identity = "http://***@node.invalid";
        let mut cfg = RuntimeConfig::default();
        cfg.set_node_url(endpoint).unwrap();
        let mut sources = SourceCatalog::configured(&cfg).unwrap();
        let canonical = live_snapshot("wss://fulcrum.invalid");
        let native = live_snapshot(endpoint_identity);
        record_canonical_fulcrum_probe(&mut sources, &canonical, 1_000, 50).unwrap();

        let (accepted, error) = record_native_photon_probe(
            &mut sources,
            endpoint,
            &canonical,
            Ok(native.clone()),
            1_000,
            2,
        );
        assert!(error.is_none());
        assert!(accepted.is_some());
        assert!(sources.supports_at(
            SourceKind::NativeNode,
            endpoint_identity,
            SourceCapability::PhotonState,
            1_000
        ));

        let selected = select_boundary_photon_job(&cfg, &sources, Some(&native), &canonical, 1_000);
        assert_eq!(selected.url, endpoint_identity);
    }

    #[test]
    fn native_outage_continuity_is_bounded_by_equivalence_proof() {
        let endpoint = "http://node.invalid";
        let mut cfg = RuntimeConfig::default();
        cfg.set_node_url(endpoint).unwrap();
        let mut sources = SourceCatalog::configured(&cfg).unwrap();
        let canonical = live_snapshot("wss://fulcrum.invalid");
        let native = live_snapshot(endpoint);
        record_canonical_fulcrum_probe(&mut sources, &canonical, 1_000, 50).unwrap();
        let (accepted, error) = record_native_photon_probe(
            &mut sources,
            endpoint,
            &canonical,
            Ok(native.clone()),
            1_000,
            2,
        );
        assert!(error.is_none());
        assert!(accepted.is_some());

        let before_expiry = 1_000 + DEFAULT_CAPABILITY_TTL_MS - 1;
        let continued = refresh_native_with_current_proof(
            &mut sources,
            endpoint,
            Ok(native.clone()),
            before_expiry,
            3,
        )
        .unwrap();
        assert_eq!(continued.job, native.job);
        assert!(!sources.supports_at(
            SourceKind::NativeNode,
            endpoint,
            SourceCapability::PhotonState,
            1_000 + DEFAULT_CAPABILITY_TTL_MS + 1
        ));

        let expired = refresh_native_with_current_proof(
            &mut sources,
            endpoint,
            Ok(native.clone()),
            1_000 + DEFAULT_CAPABILITY_TTL_MS + 1,
            3,
        )
        .unwrap_err();
        assert!(expired.contains("current canonical equivalence proof"));

        let proof =
            NativePhotonEquivalenceProof::from_verified_snapshot(endpoint, &native).unwrap();
        sources
            .verify_native_photon_capability(&proof, 2_000, DEFAULT_CAPABILITY_TTL_MS)
            .unwrap();
        let failure = refresh_native_with_current_proof(
            &mut sources,
            endpoint,
            Err("node disconnected".into()),
            2_001,
            4,
        )
        .unwrap_err();
        assert!(failure.contains("node disconnected"));
        assert!(!sources.supports_at(
            SourceKind::NativeNode,
            endpoint,
            SourceCapability::PhotonState,
            2_001
        ));
    }

    #[test]
    fn native_outage_continuity_rejects_unproven_state_change_inside_lease() {
        let endpoint = "http://node.invalid";
        let mut cfg = RuntimeConfig::default();
        cfg.set_node_url(endpoint).unwrap();
        let mut sources = SourceCatalog::configured(&cfg).unwrap();
        let canonical = live_snapshot("wss://fulcrum.invalid");
        let native = live_snapshot(endpoint);
        record_canonical_fulcrum_probe(&mut sources, &canonical, 1_000, 50).unwrap();
        let (accepted, error) = record_native_photon_probe(
            &mut sources,
            endpoint,
            &canonical,
            Ok(native.clone()),
            1_000,
            2,
        );
        assert!(error.is_none());
        assert!(accepted.is_some());

        let mut advanced_tip = native.clone();
        advanced_tip.tip_hash = "55".repeat(32);
        let error =
            refresh_native_with_current_proof(&mut sources, endpoint, Ok(advanced_tip), 1_001, 2)
                .unwrap_err();
        assert!(error.contains("tip changed outside canonical proof"));
        assert!(!sources.supports_at(
            SourceKind::NativeNode,
            endpoint,
            SourceCapability::PhotonState,
            1_001
        ));

        let (accepted, error) = record_native_photon_probe(
            &mut sources,
            endpoint,
            &canonical,
            Ok(native.clone()),
            2_000,
            2,
        );
        assert!(error.is_none());
        assert!(accepted.is_some());

        let mut advanced_baton = native;
        advanced_baton.job.baton_txid = "66".repeat(32);
        let error =
            refresh_native_with_current_proof(&mut sources, endpoint, Ok(advanced_baton), 2_001, 2)
                .unwrap_err();
        assert!(error.contains("work changed outside canonical proof"));
        assert!(!sources.supports_at(
            SourceKind::NativeNode,
            endpoint,
            SourceCapability::PhotonState,
            2_001
        ));
    }

    #[test]
    fn route_only_refresh_skips_generation_transition_and_preflight() {
        let (cfg, job, _secret, _public, _mining_payout, _journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let mut routed = job.clone();
        routed.url = "http://equivalent-node.invalid".into();
        routed.server_version = serde_json::json!(["BCHN", "28.0"]);
        let mut preflight_calls = 0;

        let staged = prepare_generation_transition(
            &cfg,
            &job,
            &settlement,
            &routed,
            |_next_cfg, _next_live| {
                preflight_calls += 1;
                Ok(())
            },
        )
        .unwrap();

        assert!(staged.is_none());
        assert_eq!(preflight_calls, 0);
        assert_eq!(cfg.generation_id, settlement.generation_id);
    }

    #[test]
    fn divergent_native_boundary_probe_revokes_and_falls_back_to_canonical() {
        let endpoint = "http://node.invalid";
        let mut cfg = RuntimeConfig::default();
        cfg.set_node_url(endpoint).unwrap();
        let mut sources = SourceCatalog::configured(&cfg).unwrap();
        let canonical = live_snapshot("wss://fulcrum.invalid");
        let native = live_snapshot(endpoint);
        record_canonical_fulcrum_probe(&mut sources, &canonical, 1_000, 50).unwrap();

        let (accepted, error) =
            record_native_photon_probe(&mut sources, endpoint, &canonical, Ok(native), 1_000, 2);
        assert!(error.is_none());
        assert!(accepted.is_some());
        assert!(sources.supports_at(
            SourceKind::NativeNode,
            endpoint,
            SourceCapability::PhotonState,
            1_000
        ));

        let mut divergent = live_snapshot(endpoint);
        divergent.job.baton_txid = "33".repeat(32);
        let (accepted, error) =
            record_native_photon_probe(&mut sources, endpoint, &canonical, Ok(divergent), 1_001, 3);
        assert!(accepted.is_none());
        assert!(error
            .as_deref()
            .is_some_and(|message| message.contains("state mismatch for baton_txid")));
        assert!(!sources.supports_at(
            SourceKind::NativeNode,
            endpoint,
            SourceCapability::PhotonState,
            1_001
        ));
        assert_eq!(
            select_boundary_photon_job(&cfg, &sources, None, &canonical, 1_001).url,
            canonical.job.url
        );
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
    fn verified_winner_is_journaled_from_prevalidated_state_before_network_retry() {
        let (cfg, job, reward_secret, reward_public, reward_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let mining_secret = [1u8; 32];
        let mining_public = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(mining_secret).unwrap(),
        )
        .serialize();
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
            &reward_payout,
            &hex::encode(mining_public),
            nonce,
            &hex::encode(signature),
        )
        .unwrap();
        let winner = VerifiedWinner {
            generation_id: cfg.generation_id,
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            nonce,
            digest: crate::search::hash256(&transaction),
            public_key: mining_public,
            signature,
            transaction,
        };

        let first = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &reward_secret,
            &reward_public,
            &settlement,
            &journal,
        )
        .unwrap();
        assert!(journal.exists());
        assert_eq!(
            PendingSubmission::load(&journal).unwrap(),
            Some(first.clone())
        );
        assert_eq!(first.generation_id, cfg.generation_id);
        assert_eq!(first.resulting_baton_txid, first.settlement_txid);
        assert_eq!(first.resulting_baton_vout, 0);
        assert_eq!(
            first.miner_token_amount + first.donation_token_amount,
            job.reward_raw
        );

        let duplicate = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &reward_secret,
            &reward_public,
            &settlement,
            &journal,
        )
        .unwrap();
        assert_eq!(duplicate, first);

        let mut wrong_split = first;
        wrong_split.donation_token_amount += 1;
        assert!(wrong_split.validate().is_err());

        PendingSubmission::remove(&journal).unwrap();
    }

    #[test]
    fn shutdown_waits_for_inflight_or_unpersisted_winner() {
        assert!(shutdown_can_exit(false, false));
        assert!(!shutdown_can_exit(true, false));
        assert!(!shutdown_can_exit(false, true));
        assert!(!shutdown_can_exit(true, true));
    }

    #[test]
    fn shutdown_signal_pauses_search_immediately_and_blocks_resume() {
        let paused = Arc::new(AtomicBool::new(false));
        let shutdown = ShutdownSignal::new(SearchPauseHandle::from_shared(Arc::clone(&paused)));
        assert!(search_resume_allowed(&shutdown, false, 0));
        shutdown.request();
        assert!(shutdown.is_requested());
        assert!(paused.load(Ordering::SeqCst));
        assert!(!search_resume_allowed(&shutdown, false, 0));
    }

    #[test]
    fn rejected_manual_resume_preserves_user_pause_intent() {
        let paused = Arc::new(AtomicBool::new(false));
        let shutdown = ShutdownSignal::new(SearchPauseHandle::from_shared(paused));
        let mut user_paused = true;
        let mut resume_calls = 0;

        let disconnected =
            resume_search_if_safe(&shutdown, false, 0, false, &mut user_paused, || {
                resume_calls += 1;
                Ok(())
            })
            .unwrap_err();
        assert!(disconnected.contains("disconnected"));
        assert!(user_paused);
        assert_eq!(resume_calls, 0);

        let pending = resume_search_if_safe(&shutdown, true, 1, false, &mut user_paused, || {
            resume_calls += 1;
            Ok(())
        })
        .unwrap_err();
        assert!(pending.contains("winner"));
        assert!(user_paused);
        assert_eq!(resume_calls, 0);

        let queued = resume_search_if_safe(&shutdown, true, 0, true, &mut user_paused, || {
            resume_calls += 1;
            Ok(())
        })
        .unwrap_err();
        assert!(queued.contains("supervisor refresh"));
        assert!(user_paused);
        assert_eq!(resume_calls, 0);

        let worker_error =
            resume_search_if_safe(&shutdown, true, 0, false, &mut user_paused, || {
                resume_calls += 1;
                Err("resume worker failure".into())
            })
            .unwrap_err();
        assert_eq!(worker_error, "resume worker failure");
        assert!(user_paused);
        assert_eq!(resume_calls, 1);

        resume_search_if_safe(&shutdown, true, 0, false, &mut user_paused, || {
            resume_calls += 1;
            Ok(())
        })
        .unwrap();
        assert!(!user_paused);
        assert_eq!(resume_calls, 2);
    }

    #[test]
    fn photon_state_recheck_matches_authoritative_m67_cadence() {
        assert_eq!(PHOTON_STATE_RECHECK, Duration::from_millis(500));
    }

    #[test]
    fn photon_state_recheck_deadline_does_not_add_request_latency() {
        let start = Instant::now();
        let first_deadline = start + PHOTON_STATE_RECHECK;

        let request_finished = first_deadline + Duration::from_millis(220);
        let second_deadline =
            next_periodic_deadline(first_deadline, request_finished, PHOTON_STATE_RECHECK);
        assert_eq!(second_deadline, start + Duration::from_secs(1));

        let slow_request_finished = second_deadline + Duration::from_millis(620);
        let next_future_deadline =
            next_periodic_deadline(second_deadline, slow_request_finished, PHOTON_STATE_RECHECK);
        assert_eq!(next_future_deadline, start + Duration::from_secs(2));
    }

    #[test]
    fn transient_refresh_failures_use_hysteresis_and_recover() {
        let mut failures = RefreshFailureTracker::default();
        assert_eq!(
            failures.failure(RefreshFailureKind::Transient),
            RefreshFailureAction::RetainGeneration
        );
        assert_eq!(failures.consecutive, 1);
        assert!(failures.degraded());
        assert_eq!(
            failures.failure(RefreshFailureKind::Transient),
            RefreshFailureAction::RetainGeneration
        );
        assert_eq!(failures.consecutive, 2);
        assert_eq!(
            failures.failure(RefreshFailureKind::Transient),
            RefreshFailureAction::Reconnect
        );
        assert_eq!(failures.consecutive, REFRESH_RECONNECT_THRESHOLD);
        assert_eq!(failures.transient_total, 3);

        failures.success();
        assert_eq!(failures.consecutive, 0);
        assert_eq!(failures.transient_total, 3);
        assert!(!failures.degraded());
    }

    #[test]
    fn transport_failure_reconnects_immediately_and_classification_is_conservative() {
        let mut failures = RefreshFailureTracker::default();
        assert_eq!(
            classify_refresh_failure("timeout: rpc waiting for id=4"),
            RefreshFailureKind::Transient
        );
        assert_eq!(
            classify_refresh_failure(
                "canonical Fulcrum PHOTON refresh failed: read: connection reset by peer"
            ),
            RefreshFailureKind::Transport
        );
        assert_eq!(
            failures.failure(RefreshFailureKind::Transport),
            RefreshFailureAction::Reconnect
        );
        assert_eq!(failures.transport_total, 1);
        assert_eq!(failures.consecutive, 0);
    }

    #[test]
    fn reconnect_candidates_respect_retry_and_ban_policy() {
        let mut sources = SourceCatalog::default();
        sources
            .add_user(SourceKind::Fulcrum, "wss://a.invalid", "a")
            .unwrap();
        sources
            .add_user(SourceKind::Fulcrum, "wss://b.invalid", "b")
            .unwrap();
        sources
            .record_success(SourceKind::Fulcrum, "wss://a.invalid", 0, 5)
            .unwrap();
        sources
            .record_success(SourceKind::Fulcrum, "wss://b.invalid", 0, 5)
            .unwrap();

        sources
            .record_failure(SourceKind::Fulcrum, "wss://a.invalid", 0)
            .unwrap();
        assert_eq!(
            eligible_fulcrum_endpoints(&sources, 0, 0, Some("wss://a.invalid")),
            vec!["wss://b.invalid".to_string()]
        );

        sources
            .set_banned(SourceKind::Fulcrum, "wss://b.invalid", true)
            .unwrap();
        assert!(eligible_fulcrum_endpoints(&sources, 0, 0, None).is_empty());
        assert!(eligible_fulcrum_endpoints(&sources, 399, 0, None).is_empty());
        assert_eq!(
            eligible_fulcrum_endpoints(&sources, 400, 0, None),
            vec!["wss://a.invalid".to_string()]
        );
    }

    #[test]
    fn reconnect_prefers_an_alternate_eligible_source() {
        let mut sources = SourceCatalog::default();
        for endpoint in ["wss://a.invalid", "wss://b.invalid"] {
            sources
                .add_user(SourceKind::Fulcrum, endpoint, endpoint)
                .unwrap();
        }
        let endpoints = eligible_fulcrum_endpoints(&sources, 0, 0, Some("wss://a.invalid"));
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0], "wss://b.invalid");
        assert_eq!(endpoints[1], "wss://a.invalid");
    }

    #[test]
    fn queued_gpu_winner_requires_an_immediate_authoritative_refresh() {
        assert!(!should_begin_winner_refresh(false, false, false, 0, 0));
        assert!(should_begin_winner_refresh(false, false, false, 0, 1));
        assert!(should_begin_winner_refresh(false, false, false, 5, 6));
        assert!(!should_begin_winner_refresh(true, false, false, 0, 1));
        assert!(!should_begin_winner_refresh(false, true, false, 0, 1));
        assert!(!should_begin_winner_refresh(false, false, true, 0, 1));
        assert!(!winner_refresh_ready(false, false, true));
        assert!(!winner_refresh_ready(true, true, true));
        assert!(!winner_refresh_ready(true, false, false));
        assert!(winner_refresh_ready(true, false, true));
    }

    #[test]
    fn submission_retry_never_rebroadcasts_parent_after_it_is_known() {
        assert_eq!(
            submission_decision(true, false, false),
            SubmissionDecision::BroadcastSettlement
        );
        assert_eq!(
            submission_decision(false, false, true),
            SubmissionDecision::BroadcastParentThenSettlement
        );
        assert_eq!(
            submission_decision(false, false, false),
            SubmissionDecision::StaleUnbroadcast
        );
        assert_eq!(
            submission_decision(false, true, false),
            SubmissionDecision::AwaitParentResolution
        );
    }

    #[test]
    fn completed_submission_requires_exact_journaled_resulting_baton() {
        let job = live_job();
        let settlement_txid = reward::transaction_id(&[2]);
        let pending = PendingSubmission {
            version: SUBMISSION_JOURNAL_VERSION,
            generation_id: 1,
            expected_height: job.height,
            expected_baton_txid: job.baton_txid.clone(),
            expected_baton_vout: job.baton_vout,
            parent_txid: reward::transaction_id(&[1]),
            parent_hex: "01".into(),
            settlement_txid: settlement_txid.clone(),
            settlement_hex: "02".into(),
            resulting_baton_txid: settlement_txid,
            resulting_baton_vout: 0,
            resulting_baton_value_sats: 10_000,
            miner_token_amount: 98,
            donation_token_amount: 2,
        };

        let mut exact = job.clone();
        exact.baton_txid = pending.resulting_baton_txid.to_uppercase();
        exact.baton_vout = pending.resulting_baton_vout;
        assert!(resulting_baton_is_authoritative(&pending, &exact));

        let mut unrelated_advance = exact.clone();
        unrelated_advance.baton_txid = "33".repeat(32);
        assert!(!resulting_baton_is_authoritative(
            &pending,
            &unrelated_advance
        ));

        let mut wrong_output = exact;
        wrong_output.baton_vout = 1;
        assert!(!resulting_baton_is_authoritative(&pending, &wrong_output));
    }

    #[test]
    fn restart_recovery_accepts_only_proven_output_zero_baton_descendants() {
        let settlement = "aa".repeat(32);
        let child_raw = transaction_spending(&settlement, 0);
        let child = reward::transaction_id(&hex::decode(&child_raw).unwrap());
        let live_raw = transaction_spending(&child, 0);
        let live = reward::transaction_id(&hex::decode(&live_raw).unwrap());

        let proven = prove_baton_descends_from(&live, 0, &settlement, 0, |txid| {
            if txid.eq_ignore_ascii_case(&live) {
                Ok(live_raw.clone())
            } else if txid.eq_ignore_ascii_case(&child) {
                Ok(child_raw.clone())
            } else {
                Err(format!("unexpected lineage txid {txid}"))
            }
        })
        .unwrap();
        assert!(proven);

        let wrong_link = transaction_spending(&settlement, 1);
        let wrong_live = reward::transaction_id(&hex::decode(&wrong_link).unwrap());
        assert!(
            !prove_baton_descends_from(&wrong_live, 0, &settlement, 0, |_| {
                Ok(wrong_link.clone())
            })
            .unwrap()
        );
        assert!(!prove_baton_descends_from(&live, 1, &settlement, 0, |_| {
            panic!("wrong live baton output must fail before network lookup")
        })
        .unwrap());

        let mismatched_claim = "dd".repeat(32);
        let mismatch = prove_baton_descends_from(&mismatched_claim, 0, &settlement, 0, |_| {
            Ok(live_raw.clone())
        })
        .unwrap_err();
        assert!(mismatch.contains("do not match requested txid"));
    }

    #[test]
    fn lineage_parser_reads_wire_endian_first_input_outpoint() {
        let previous_txid = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let raw = transaction_spending(previous_txid, 7);
        let (parsed_txid, parsed_vout) = first_input_outpoint(&raw).unwrap();
        assert_eq!(parsed_txid, previous_txid);
        assert_eq!(parsed_vout, 7);
    }

    #[test]
    fn broadcast_preference_uses_selected_transport_then_safe_fallback() {
        use std::cell::RefCell;

        let calls = RefCell::new(Vec::new());
        let returned = broadcast_with_preference(
            JobSource::Node,
            true,
            || {
                calls.borrow_mut().push("fulcrum");
                Ok("aa".repeat(32))
            },
            || {
                calls.borrow_mut().push("node");
                Err("node unavailable".into())
            },
        )
        .unwrap();
        assert_eq!(returned, "aa".repeat(32));
        assert_eq!(&*calls.borrow(), &["node", "fulcrum"]);

        calls.borrow_mut().clear();
        let returned = broadcast_with_preference(
            JobSource::Fulcrum,
            true,
            || {
                calls.borrow_mut().push("fulcrum");
                Err("fulcrum unavailable".into())
            },
            || {
                calls.borrow_mut().push("node");
                Ok("bb".repeat(32))
            },
        )
        .unwrap();
        assert_eq!(returned, "bb".repeat(32));
        assert_eq!(&*calls.borrow(), &["fulcrum", "node"]);
    }

    #[test]
    fn native_node_mempool_preflight_requires_exact_txid_and_policy_acceptance() {
        let expected_txid = "aa".repeat(32);
        let allowed = crate::node::MempoolAcceptance {
            txid: expected_txid.clone(),
            allowed: true,
            size: Some(615),
            vsize: Some(615),
            reject_reason: None,
            reject_details: None,
        };
        validate_node_mempool_acceptance("PHOTON parent", &expected_txid, "http://node", &allowed)
            .unwrap();

        let mut mismatch = allowed.clone();
        mismatch.txid = "bb".repeat(32);
        let error = validate_node_mempool_acceptance(
            "PHOTON parent",
            &expected_txid,
            "http://node",
            &mismatch,
        )
        .unwrap_err();
        assert!(error.contains("unexpected txid"));

        let rejected = crate::node::MempoolAcceptance {
            txid: expected_txid.clone(),
            allowed: false,
            size: None,
            vsize: None,
            reject_reason: Some("dust".into()),
            reject_details: Some("policy floor".into()),
        };
        let error = validate_node_mempool_acceptance(
            "PHOTON settlement",
            &expected_txid,
            "http://node",
            &rejected,
        )
        .unwrap_err();
        assert!(error.contains("dust"));
        assert!(error.contains("policy floor"));
    }

    #[test]
    fn native_node_source_refuses_live_search_without_node_endpoint() {
        let cfg = RuntimeConfig {
            source: JobSource::Node,
            ..RuntimeConfig::default()
        };
        let error = production_relay_fee_sats_per_kb(&cfg).unwrap_err();
        assert!(error.contains("no node RPC endpoint"));
    }

    #[test]
    fn configured_node_binds_live_mempool_fee_floor_with_fulcrum_preference() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("\"method\":\"getmempoolinfo\""));
            let body = r#"{"result":{"mempoolminfee":0.00002000},"error":null,"id":"pickaxe"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let cfg = RuntimeConfig {
            node_url: Some(format!("http://{address}")),
            source: JobSource::Fulcrum,
            ..RuntimeConfig::default()
        };
        assert_eq!(production_relay_fee_sats_per_kb(&cfg).unwrap(), 2_000);
        server.join().unwrap();
    }

    #[test]
    fn production_preflight_proves_parent_reward_split_and_journal_readiness() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &journal,
            reward::MIN_RELAY_FEE_SATS_PER_KB,
        )
        .unwrap();
        assert!(!journal.exists());
    }

    #[test]
    fn production_preflight_refuses_insufficient_self_funded_baton_value_before_search() {
        let (cfg, mut job, secret, public, mining_payout, journal) = preflight_fixture();
        job.baton_value_sats = 1_500;
        let error = production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &journal,
            reward::MIN_RELAY_FEE_SATS_PER_KB,
        )
        .unwrap_err();
        assert!(error.contains("baton BCH value is too small"));
        assert!(!journal.exists());
    }

    #[test]
    fn production_preflight_refuses_relay_floor_above_covenant_budget() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let error = production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &journal,
            10_000,
        )
        .unwrap_err();
        assert!(error.contains("covenant permits at most"));
        assert!(!journal.exists());
    }

    #[test]
    fn production_preflight_refuses_unresolved_submission_journal() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        fs::write(&journal, b"occupied").unwrap();
        let error = production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &journal,
            reward::MIN_RELAY_FEE_SATS_PER_KB,
        )
        .unwrap_err();
        assert!(error.contains("unresolved previous winner submission"));
        fs::remove_file(&journal).unwrap();
    }

    #[test]
    fn baton_transition_requires_preflighted_self_funded_settlement() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let mut next = job.clone();
        next.height += 1;
        next.baton_txid = "22".repeat(32);
        next.baton_height = next.height - 1;
        next.age = 1;

        let error = prepare_generation_transition(
            &cfg,
            &job,
            &settlement,
            &next,
            |_next_cfg, _next_live| Err("temporary settlement preflight failure".into()),
        )
        .unwrap_err();
        assert!(error.contains("temporary settlement"));
        assert_eq!(settlement.generation_id, cfg.generation_id);
        assert_eq!(settlement.baton_txid, job.baton_txid);

        let staged =
            prepare_generation_transition(&cfg, &job, &settlement, &next, |next_cfg, next_live| {
                production_preflight_local(
                    next_cfg,
                    next_live,
                    &secret,
                    &public,
                    &mining_payout,
                    &journal,
                    reward::MIN_RELAY_FEE_SATS_PER_KB,
                )
            })
            .unwrap()
            .expect("baton transition must stage a new generation");
        assert_eq!(staged.0.generation_id, cfg.generation_id + 1);
        assert_eq!(staged.1.generation_id, staged.0.generation_id);
        assert_eq!(staged.1.baton_txid, next.baton_txid);
        assert_eq!(settlement.baton_txid, job.baton_txid);
        assert!(!journal.exists());
    }

    #[test]
    fn same_baton_generation_revalidates_self_funded_settlement() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let mut next = job.clone();
        next.height += 1;
        next.age += 1;

        let staged =
            prepare_generation_transition(&cfg, &job, &settlement, &next, |next_cfg, next_live| {
                production_preflight_local(
                    next_cfg,
                    next_live,
                    &secret,
                    &public,
                    &mining_payout,
                    &journal,
                    reward::MIN_RELAY_FEE_SATS_PER_KB,
                )
            })
            .unwrap()
            .expect("height change must publish a new generation");
        assert_eq!(staged.0.generation_id, cfg.generation_id + 1);
        assert_eq!(staged.1.generation_id, staged.0.generation_id);
        assert_eq!(staged.1.baton_txid, job.baton_txid);
        assert!(!journal.exists());
    }

    #[test]
    fn transitioned_generation_journal_survives_restart_and_duplicate_retry() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let mut next = job.clone();
        next.height += 1;
        next.baton_txid = "22".repeat(32);
        next.baton_height = next.height - 1;
        next.age = 1;
        let (next_cfg, next_settlement) =
            prepare_generation_transition(&cfg, &job, &settlement, &next, |next_cfg, next_live| {
                production_preflight_local(
                    next_cfg,
                    next_live,
                    &secret,
                    &public,
                    &mining_payout,
                    &journal,
                    reward::MIN_RELAY_FEE_SATS_PER_KB,
                )
            })
            .unwrap()
            .unwrap();
        let winner = signed_winner(next_cfg.generation_id, &next, &mining_payout);
        let first = prepare_pending_submission(
            &winner,
            &next_cfg,
            &next,
            &secret,
            &public,
            &next_settlement,
            &journal,
        )
        .unwrap();
        let restarted = PendingSubmission::load(&journal).unwrap().unwrap();
        assert_eq!(restarted, first);
        assert_eq!(restarted.expected_baton_txid, next.baton_txid);
        assert_eq!(restarted.generation_id, next_cfg.generation_id);
        assert_eq!(restarted.resulting_baton_txid, restarted.settlement_txid);
        assert_eq!(
            restarted.miner_token_amount + restarted.donation_token_amount,
            next.reward_raw
        );

        let duplicate = prepare_pending_submission(
            &winner,
            &next_cfg,
            &next,
            &secret,
            &public,
            &next_settlement,
            &journal,
        )
        .unwrap();
        assert_eq!(duplicate, restarted);
        PendingSubmission::remove(&journal).unwrap();
    }

    #[test]
    fn live_search_gate_opens_after_generation_bound_winner_durability() {
        require_complete_live_winner_lifecycle().unwrap();
    }

    #[test]
    fn pending_submission_journal_round_trips_signed_bytes_without_secrets() {
        let parent = vec![0x02, 0x00, 0x00, 0x00, 0x01];
        let settlement = vec![0x02, 0x00, 0x00, 0x00, 0x02];
        let settlement_txid = reward::transaction_id(&settlement);
        let pending = PendingSubmission {
            version: SUBMISSION_JOURNAL_VERSION,
            generation_id: 1,
            expected_height: 1_000,
            expected_baton_txid: "11".repeat(32),
            expected_baton_vout: 0,
            parent_txid: reward::transaction_id(&parent),
            parent_hex: hex::encode(&parent),
            settlement_txid: settlement_txid.clone(),
            settlement_hex: hex::encode(&settlement),
            resulting_baton_txid: settlement_txid,
            resulting_baton_vout: 0,
            resulting_baton_value_sats: 10_000,
            miner_token_amount: 98,
            donation_token_amount: 2,
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
        assert!(!serialized.contains("sponsor"));
        assert_eq!(PendingSubmission::load(&path).unwrap(), Some(pending));

        PendingSubmission::remove(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn parent_broadcast_progress_survives_restart_and_is_removed_with_journal() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let winner = signed_winner(cfg.generation_id, &job, &mining_payout);
        let pending = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &secret,
            &public,
            &settlement,
            &journal,
        )
        .unwrap();

        assert!(!pending.parent_attempted(&journal).unwrap());
        assert!(!pending.parent_accepted(&journal).unwrap());
        pending.mark_parent_attempted(&journal).unwrap();
        assert!(pending.parent_attempted(&journal).unwrap());
        assert!(!pending.parent_accepted(&journal).unwrap());

        let restarted = PendingSubmission::load(&journal).unwrap().unwrap();
        assert!(restarted.parent_attempted(&journal).unwrap());
        restarted.mark_parent_accepted(&journal).unwrap();
        assert!(restarted.parent_accepted(&journal).unwrap());

        PendingSubmission::remove(&journal).unwrap();
        assert!(!journal.exists());
        assert!(!PendingSubmission::parent_attempted_path(&journal).exists());
        assert!(!PendingSubmission::parent_accepted_path(&journal).exists());
    }

    #[test]
    fn height_only_advance_cannot_resolve_a_journaled_winner_as_stale() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let winner = signed_winner(cfg.generation_id, &job, &mining_payout);
        let pending = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &secret,
            &public,
            &settlement,
            &journal,
        )
        .unwrap();

        let mut next_height = job.clone();
        next_height.height += 1;
        let error = resolve_stale_submission(&pending, &next_height, &journal).unwrap_err();

        assert!(error.contains("expected baton is live"), "{error}");
        assert!(journal.exists());
        assert!(ResolvedSubmission::load(&journal).unwrap().is_none());

        PendingSubmission::remove(&journal).unwrap();
    }

    #[test]
    fn stale_unbroadcast_winner_keeps_durable_resolution_evidence() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let winner = signed_winner(cfg.generation_id, &job, &mining_payout);
        let pending = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &secret,
            &public,
            &settlement,
            &journal,
        )
        .unwrap();

        let mut conflicting = job.clone();
        conflicting.height += 1;
        conflicting.baton_txid = "44".repeat(32);
        resolve_stale_submission(&pending, &conflicting, &journal).unwrap();

        assert!(!journal.exists());
        assert!(!PendingSubmission::parent_attempted_path(&journal).exists());
        assert!(!PendingSubmission::parent_accepted_path(&journal).exists());
        let resolved = ResolvedSubmission::load(&journal).unwrap().unwrap();
        assert_eq!(resolved.generation_id, cfg.generation_id);
        assert_eq!(resolved.expected_height, job.height);
        assert_eq!(resolved.expected_baton_txid, job.baton_txid);
        assert_eq!(resolved.parent_txid, pending.parent_txid);
        assert_eq!(resolved.settlement_txid, pending.settlement_txid);
        assert!(!resolved.parent_attempted);
        assert!(!resolved.parent_accepted);
        assert_eq!(resolved.observed_height, conflicting.height);
        assert_eq!(resolved.observed_baton_txid, conflicting.baton_txid);
        assert_eq!(resolved.reason, "stale-unbroadcast");

        fs::remove_file(ResolvedSubmission::path(&journal)).unwrap();
    }

    #[test]
    fn confirmed_submission_keeps_durable_resolution_evidence_before_journal_cleanup() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let winner = signed_winner(cfg.generation_id, &job, &mining_payout);
        let pending = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &secret,
            &public,
            &settlement,
            &journal,
        )
        .unwrap();
        pending.mark_parent_accepted(&journal).unwrap();

        let mut confirmed = job.clone();
        confirmed.height += 1;
        confirmed.baton_txid = pending.resulting_baton_txid.clone();
        confirmed.baton_vout = pending.resulting_baton_vout;
        resolve_confirmed_submission(&pending, &confirmed, &journal).unwrap();

        assert!(!journal.exists());
        assert!(!PendingSubmission::parent_attempted_path(&journal).exists());
        assert!(!PendingSubmission::parent_accepted_path(&journal).exists());
        let resolved = ResolvedSubmission::load(&journal).unwrap().unwrap();
        assert_eq!(resolved.generation_id, cfg.generation_id);
        assert_eq!(resolved.expected_height, job.height);
        assert_eq!(resolved.expected_baton_txid, job.baton_txid);
        assert_eq!(resolved.parent_txid, pending.parent_txid);
        assert_eq!(resolved.settlement_txid, pending.settlement_txid);
        assert!(resolved.parent_attempted);
        assert!(resolved.parent_accepted);
        assert_eq!(resolved.observed_height, confirmed.height);
        assert_eq!(resolved.observed_baton_txid, confirmed.baton_txid);
        assert_eq!(resolved.observed_baton_vout, confirmed.baton_vout);
        assert_eq!(resolved.reason, "confirmed");

        fs::remove_file(ResolvedSubmission::path(&journal)).unwrap();
    }

    #[test]
    fn attempted_parent_cannot_be_resolved_as_stale_on_ambiguous_network_evidence() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let winner = signed_winner(cfg.generation_id, &job, &mining_payout);
        let pending = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &secret,
            &public,
            &settlement,
            &journal,
        )
        .unwrap();
        pending.mark_parent_attempted(&journal).unwrap();

        let mut conflicting = job;
        conflicting.height += 1;
        conflicting.baton_txid = "44".repeat(32);
        let error = resolve_stale_submission(&pending, &conflicting, &journal).unwrap_err();

        assert!(
            error.contains("after a parent broadcast attempt"),
            "{error}"
        );
        assert!(journal.exists());
        assert!(pending.parent_attempted(&journal).unwrap());
        assert!(!pending.parent_accepted(&journal).unwrap());
        assert!(ResolvedSubmission::load(&journal).unwrap().is_none());

        PendingSubmission::remove(&journal).unwrap();
    }

    #[test]
    fn stale_resolution_never_discards_an_accepted_parent() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let settlement = SettlementState::new(cfg.generation_id, &job).unwrap();
        let winner = signed_winner(cfg.generation_id, &job, &mining_payout);
        let pending = prepare_pending_submission(
            &winner,
            &cfg,
            &job,
            &secret,
            &public,
            &settlement,
            &journal,
        )
        .unwrap();
        pending.mark_parent_accepted(&journal).unwrap();

        let mut conflicting = job;
        conflicting.height += 1;
        conflicting.baton_txid = "55".repeat(32);
        let error = resolve_stale_submission(&pending, &conflicting, &journal).unwrap_err();
        assert!(error.contains("after parent acceptance"));
        assert!(journal.exists());
        assert!(pending.parent_accepted(&journal).unwrap());
        assert!(ResolvedSubmission::load(&journal).unwrap().is_none());

        PendingSubmission::remove(&journal).unwrap();
    }

    #[test]
    fn production_preflight_refuses_orphan_submission_progress() {
        let (cfg, job, secret, public, mining_payout, journal) = preflight_fixture();
        let marker = PendingSubmission::parent_attempted_path(&journal);
        fs::write(&marker, format!("{}\n", "aa".repeat(32))).unwrap();
        let error = production_preflight_local(
            &cfg,
            &job,
            &secret,
            &public,
            &mining_payout,
            &journal,
            reward::MIN_RELAY_FEE_SATS_PER_KB,
        )
        .unwrap_err();
        assert!(error.contains("orphan pending-submission progress marker"));
        fs::remove_file(marker).unwrap();
    }

    #[test]
    fn journaled_unbroadcast_pair_survives_height_advance_while_baton_is_current() {
        let job = live_job();
        let settlement_txid = reward::transaction_id(&[2]);
        let pending = PendingSubmission {
            version: SUBMISSION_JOURNAL_VERSION,
            generation_id: 1,
            expected_height: job.height,
            expected_baton_txid: job.baton_txid.clone(),
            expected_baton_vout: job.baton_vout,
            parent_txid: reward::transaction_id(&[1]),
            parent_hex: "01".into(),
            settlement_txid: settlement_txid.clone(),
            settlement_hex: "02".into(),
            resulting_baton_txid: settlement_txid,
            resulting_baton_vout: 0,
            resulting_baton_value_sats: 10_000,
            miner_token_amount: 98,
            donation_token_amount: 2,
        };
        assert!(pending.expected_baton_is_current(&job));

        let mut next_height = job.clone();
        next_height.height += 1;
        assert!(pending.expected_baton_is_current(&next_height));
        assert_eq!(
            submission_decision(
                false,
                false,
                pending.expected_baton_is_current(&next_height)
            ),
            SubmissionDecision::BroadcastParentThenSettlement
        );

        let mut uppercase_baton = next_height.clone();
        uppercase_baton.baton_txid = uppercase_baton.baton_txid.to_uppercase();
        assert!(pending.expected_baton_is_current(&uppercase_baton));
        assert_eq!(
            submission_decision(
                false,
                false,
                pending.expected_baton_is_current(&uppercase_baton)
            ),
            SubmissionDecision::BroadcastParentThenSettlement
        );

        let mut next_baton = job;
        next_baton.baton_txid = "22".repeat(32);
        assert!(!pending.expected_baton_is_current(&next_baton));
        assert_eq!(
            submission_decision(false, false, pending.expected_baton_is_current(&next_baton)),
            SubmissionDecision::StaleUnbroadcast
        );
        assert_eq!(
            submission_decision(false, true, pending.expected_baton_is_current(&next_baton)),
            SubmissionDecision::AwaitParentResolution
        );
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
