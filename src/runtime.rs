//! Presentation-neutral live PHOTON runtime supervision.
//!
//! The authoritative browser reference refreshes BCH height + the mutable
//! PHOTON baton after every GPU batch. This supervisor owns that boundary for
//! both future Ratatui and headless frontends: the CUDA worker cannot start the
//! next supervised batch until fresh Fulcrum state has been checked and any
//! immutable generation change has been applied.

use crate::config::RuntimeConfig;
use crate::electrum::{ElectrumSession, LiveJob};
use crate::search::VerifiedWinner;
use crate::search::{MiningState, RuntimeCommand as SearchCommand, SearchHandle, SearchStats};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const COMMAND_CAP: usize = 16;
const EVENT_CAP: usize = 32;
const SUPERVISOR_POLL: Duration = Duration::from_millis(10);
const RECONNECT_MIN: Duration = Duration::from_millis(400);
const RECONNECT_MAX: Duration = Duration::from_secs(8);

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
    Error(String),
}

#[allow(dead_code)]
enum SupervisorCommand {
    SetIntensity(u8, SyncSender<Result<(), String>>),
    Pause(SyncSender<Result<(), String>>),
    Resume(SyncSender<Result<(), String>>),
    SetPayout(String, SyncSender<Result<(), String>>),
    Stop,
}

pub struct RuntimeSupervisor {
    command_tx: SyncSender<SupervisorCommand>,
    event_rx: Receiver<RuntimeEvent>,
    snapshot: Arc<Mutex<RuntimeSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl RuntimeSupervisor {
    pub fn start(mut cfg: RuntimeConfig) -> Result<Self, String> {
        if cfg.payout_address.trim().is_empty() {
            return Err("mining payout address is required".into());
        }

        let endpoints = cfg.electrum_endpoints();
        let mut session = ElectrumSession::connect_failover(&endpoints)?;
        let initial = session.fetch_live_job()?;
        cfg.bump_generation();

        let search = SearchHandle::start_supervised(
            cfg.intensity,
            initial.to_mining_job(cfg.generation_id, &cfg.payout_address),
        )?;
        let initial_search = search.snapshot();
        let initial_snapshot = RuntimeSnapshot {
            state: SupervisorState::Mining,
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
    endpoints: Vec<String>,
    mut live: LiveJob,
    initial_session: ElectrumSession,
    search: SearchHandle,
    command_rx: Receiver<SupervisorCommand>,
    event_tx: SyncSender<RuntimeEvent>,
    shared_snapshot: Arc<Mutex<RuntimeSnapshot>>,
) {
    let mut session = Some(initial_session);
    let mut state = SupervisorState::Mining;
    let mut user_paused = false;
    let mut refreshes = 0u64;
    let mut stale_rebuilds = 0u64;
    let mut reconnects = 0u64;
    let mut stale_winners = 0u64;
    let mut verified_winners = 0u64;
    let mut pending_winners = 0u64;
    let mut last_error = None;
    let mut reconnect_backoff = RECONNECT_MIN;
    let mut next_reconnect = Instant::now();
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
                    let before = cfg.generation_id;
                    let result = cfg.set_payout(payout).and_then(|()| {
                        if cfg.generation_id != before {
                            search.replace_job(
                                live.to_mining_job(cfg.generation_id, &cfg.payout_address),
                            )?;
                            stale_rebuilds = stale_rebuilds.saturating_add(1);
                        }
                        Ok(())
                    });
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
                        match apply_refreshed_job(&mut cfg, &mut live, &search, next_job) {
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
        } else if search.refresh_required() {
            let refreshed = session
                .as_mut()
                .expect("checked session above")
                .fetch_live_job();
            match refreshed {
                Ok(next_job) => {
                    refreshes = refreshes.saturating_add(1);
                    match apply_refreshed_job(&mut cfg, &mut live, &search, next_job) {
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
                                    pending_winners = pending_winners.saturating_add(1);
                                    user_paused = true;
                                    let _ = search.apply_control(SearchCommand::Pause);
                                    state = SupervisorState::Paused;
                                    emit(&event_tx, RuntimeEvent::VerifiedWinner(winner));
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

                            last_error = None;
                            let _ = search.complete_refresh();
                            if !user_paused && pending_winners == 0 {
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
    next: LiveJob,
) -> Result<bool, String> {
    let changed = live_job_changed(live, &next);
    if changed {
        cfg.bump_generation();
        search.replace_job(next.to_mining_job(cfg.generation_id, &cfg.payout_address))?;
    }
    *live = next;
    Ok(changed)
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
}
