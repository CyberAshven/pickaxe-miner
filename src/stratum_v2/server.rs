//! #### PR #38
//! Bounded native SV2 server. One worker owns the node/template and submissions;
//! device connections only see immutable jobs and cannot select chain payouts.

use super::{
    journal::Journal,
    provider::{NodeRpc, SubmissionOutcome, TemplateProvider},
    telemetry::Devices,
    template::{BchTemplate, Hash},
    transport::Session,
    wire::MiningSession,
    work_allocation::WorkAllocation,
};
use crate::config::{validate_payout_address, MiningNetwork};
use crate::donation::bch::{BchDonation, BchPayout};
use std::{
    collections::HashMap,
    io,
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stratum_core::bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey};

const MAX_CONNECTIONS: usize = 64;
const TEMPLATE_RECOVERY_GRACE: Duration = Duration::from_secs(3);

// No Debug: contains a local authority secret and payout configuration.
pub struct ServerConfig {
    pub network: MiningNetwork,
    pub payout: String,
    pub authority_secret: [u8; 32],
    pub share_target: Hash,
    pub journal_path: PathBuf,
    pub source_identity: [u8; 32],
    pub donation: Arc<RwLock<BchDonation>>,
    #[cfg(test)]
    pub allocation_phase: Option<u64>,
}

#[derive(Default, Clone, Debug)]
pub struct ServerStats {
    pub connections: usize,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub blocks_accepted: u64,
    pub blocks_pending: usize,
    pub blocks_rejected: u64,
    pub block_retries: u64,
    pub last_block_result: Option<&'static str>,
    pub connection_errors: u64,
    pub sv1_connection_errors: u64,
    pub sv1_local_rejected: u64,
    pub sessions_started: u64,
    pub device_stats: Devices,
    pub template_ready: bool,
    pub template_failures: u64,
    pub last_template_error: Option<&'static str>,
    pub height: Option<u32>,
}

#[derive(Clone)]
struct PublishedJob {
    generation: u64,
    template: Arc<BchTemplate>,
    valid_until: Instant,
}

struct Shared {
    job: RwLock<Option<PublishedJob>>,
    stats: Arc<Mutex<ServerStats>>,
    wake: SyncSender<()>,
    journal: Mutex<Journal>,
    fatal: Mutex<Option<&'static str>>,
    stop: Arc<AtomicBool>,
}

/// Bind the listener before calling this function. The caller owns the stop
/// flag and display, so native TUI and CPU tests use the same server lifecycle.
pub fn run<R: NodeRpc + Send + 'static>(
    listener: TcpListener,
    rpc: R,
    config: ServerConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    validate_payout_address(config.network, &config.payout)
        .map_err(|_| "invalid payout for selected network")?;
    if config.share_target == [0; 32] {
        return Err("share target cannot be zero".into());
    }
    let public = authority_public(&config.authority_secret)?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot configure mining listener")?;
    let journal = Journal::open(
        &config.journal_path,
        config.network,
        &config.payout,
        config.source_identity,
    )?;
    let (wake, receive_blocks) = mpsc::sync_channel::<()>(1);
    let shared = Arc::new(Shared {
        job: RwLock::new(None),
        stats: stats.clone(),
        wake,
        journal: Mutex::new(journal),
        fatal: Mutex::new(None),
        stop: stop.clone(),
    });
    update_journal_stats(&shared)?;
    let node_shared = shared.clone();
    let network = config.network;
    let node = thread::spawn(move || -> Result<(), String> {
        let mut provider = TemplateProvider::new(rpc, network);
        let mut refreshed = Instant::now() - Duration::from_secs(60);
        let mut retries = RetrySchedule::default();
        while !node_shared.stop.load(Ordering::Relaxed) {
            match receive_blocks.recv_timeout(Duration::from_millis(250)) {
                Ok(()) => (),
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // #### PR #38
            // The journal is authoritative; a coalesced wake cannot lose work.
            // Release its lock before RPC so another device can persist a block
            // while a reply is delayed. Retry old blocks fairly with backoff.
            let pending = {
                let journal = node_shared
                    .journal
                    .lock()
                    .map_err(|_| "block journal unavailable")?;
                retries
                    .next(&journal.pending_hashes(), Instant::now())
                    .and_then(|hash| journal.pending(&hash))
            };
            if let Some(pending) = pending {
                if retries.is_retry(&pending.hash) {
                    if let Ok(mut stats) = node_shared.stats.lock() {
                        stats.block_retries = stats.block_retries.saturating_add(1);
                    }
                }
                let outcome = provider.submit_saved(&pending);
                let result = match outcome {
                    SubmissionOutcome::Accepted => Some(true),
                    SubmissionOutcome::Rejected(_) => Some(false),
                    SubmissionOutcome::Pending(_) => None,
                };
                if let Some(accepted) = result {
                    node_shared
                        .journal
                        .lock()
                        .map_err(|_| "block journal unavailable")?
                        .finish(&pending.hash, accepted)?;
                    retries.remove(&pending.hash);
                } else {
                    retries.defer(&pending.hash, Instant::now());
                }
                update_journal_stats(&node_shared)?;
                if let Ok(mut stats) = node_shared.stats.lock() {
                    stats.last_block_result = Some(match outcome {
                        SubmissionOutcome::Accepted => "accepted",
                        SubmissionOutcome::Rejected(reason)
                        | SubmissionOutcome::Pending(reason) => reason,
                    });
                }
                // Refresh even when RPC acceptance is unknown. Saved bytes
                // survive tip changes; no response is treated as acceptance.
                refreshed = Instant::now() - Duration::from_secs(60);
            }
            let current = provider.tip_is_current().unwrap_or(false);
            if !current || refreshed.elapsed() >= Duration::from_secs(15) {
                match provider.refresh() {
                    Ok((generation, template)) => {
                        publish(
                            &node_shared,
                            Some(PublishedJob {
                                generation,
                                template: Arc::new(template.clone()),
                                valid_until: Instant::now() + Duration::from_secs(30),
                            }),
                        );
                        refreshed = Instant::now();
                    }
                    Err(error) => {
                        if let Ok(mut stats) = node_shared.stats.lock() {
                            stats.template_failures = stats.template_failures.saturating_add(1);
                            stats.last_template_error = Some(template_reason(&error));
                        }
                        publish(&node_shared, None);
                    }
                }
            }
        }
        publish(&node_shared, None);
        Ok(())
    });
    let config = Arc::new(config);
    let mut devices: Vec<(u64, thread::JoinHandle<()>)> = Vec::new();
    let mut listener_error = None;
    while !stop.load(Ordering::Relaxed) {
        if node.is_finished() {
            listener_error = Some("template worker stopped unexpectedly".to_owned());
            break;
        }
        let mut remaining = Vec::new();
        for (id, device) in devices.drain(..) {
            if device.is_finished() {
                if device.join().is_err() {
                    device_ended(&shared, id, Some("device worker panicked"));
                }
            } else {
                remaining.push((id, device));
            }
        }
        devices = remaining;
        match listener.accept() {
            Ok((stream, peer)) => {
                if devices.len() >= MAX_CONNECTIONS {
                    drop(stream);
                    continue;
                }
                let shared = shared.clone();
                let config = config.clone();
                let id = if let Ok(mut stats) = shared.stats.lock() {
                    stats.connections += 1;
                    stats.sessions_started = stats.sessions_started.saturating_add(1);
                    stats.device_stats.connect(peer, false, Instant::now())
                } else {
                    0
                };
                devices.push((
                    id,
                    thread::spawn(move || {
                        let result = serve_device(stream, public, &config, &shared, id);
                        device_ended(&shared, id, result.as_ref().err().map(String::as_str));
                    }),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(_) => {
                listener_error = Some("mining listener failed".to_owned());
                break;
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    for (id, device) in devices {
        if device.join().is_err() {
            device_ended(&shared, id, Some("device worker panicked"));
        }
    }
    node.join()
        .map_err(|_| "template worker stopped unexpectedly")??;
    if let Some(reason) = *shared
        .fatal
        .lock()
        .map_err(|_| "server failure state unavailable")?
    {
        return Err(reason.into());
    }
    if let Some(error) = listener_error {
        Err(error)
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct RetrySchedule(HashMap<String, (u32, Instant)>);

impl RetrySchedule {
    fn next(&mut self, pending: &[String], now: Instant) -> Option<String> {
        self.0.retain(|hash, _| pending.contains(hash));
        pending
            .iter()
            .find(|hash| self.0.get(*hash).is_none_or(|(_, due)| now >= *due))
            .cloned()
    }
    fn is_retry(&self, hash: &str) -> bool {
        self.0.contains_key(hash)
    }
    fn defer(&mut self, hash: &str, now: Instant) {
        let attempts = self.0.get(hash).map_or(0, |(n, _)| *n).saturating_add(1);
        let delay = (1u64 << attempts.saturating_sub(1).min(5)).min(30);
        self.0.insert(
            hash.to_owned(),
            (attempts, now + Duration::from_secs(delay)),
        );
    }
    fn remove(&mut self, hash: &str) {
        self.0.remove(hash);
    }
}

fn update_journal_stats(shared: &Shared) -> Result<(), String> {
    let journal = shared
        .journal
        .lock()
        .map_err(|_| "block journal unavailable")?;
    let (pending, accepted, rejected) = journal.counts();
    let mut stats = shared
        .stats
        .lock()
        .map_err(|_| "mining statistics unavailable")?;
    stats.blocks_pending = pending;
    stats.blocks_accepted = accepted;
    stats.blocks_rejected = rejected;
    Ok(())
}

pub fn authority_public(secret: &[u8; 32]) -> Result<[u8; 32], String> {
    let key = SecretKey::from_slice(secret).map_err(|_| "invalid SV2 authority secret")?;
    Ok(Keypair::from_secret_key(&Secp256k1::new(), &key)
        .x_only_public_key()
        .0
        .serialize())
}

fn device_ended(shared: &Shared, id: u64, error: Option<&str>) {
    if let Ok(mut stats) = shared.stats.lock() {
        stats.connections = stats.connections.saturating_sub(1);
        // Closing sockets during the operator's shutdown is expected.
        let error = error.filter(|_| !shared.stop.load(Ordering::Relaxed));
        if error.is_some() {
            stats.connection_errors = stats.connection_errors.saturating_add(1);
        }
        stats.device_stats.close(id, false, error, Instant::now());
    }
}

fn publish(shared: &Shared, job: Option<PublishedJob>) {
    let ready = job.is_some();
    let height = job.as_ref().map(|job| job.template.height);
    if let Ok(mut current) = shared.job.write() {
        *current = job;
    }
    if let Ok(mut stats) = shared.stats.lock() {
        stats.template_ready = ready;
        stats.height = height;
    }
}

// #### PR #38
// A brief template failure is not a failed device transport. Revoke all work
// immediately, reject submissions while unavailable, then cleanly activate a
// fresh job on the same channel. The grace is bounded; never extend a job's
// original freshness lease just to keep a connection alive.
struct JobAvailability {
    generation: Option<u64>,
    unavailable_since: Option<Instant>,
    payout: Option<BchPayout>,
    next_id: u32,
    allocation: WorkAllocation,
}

impl JobAvailability {
    fn new(phase: u64) -> Self {
        Self {
            generation: None,
            unavailable_since: None,
            payout: None,
            next_id: 0,
            allocation: WorkAllocation::new(phase),
        }
    }

    fn update(
        &mut self,
        mining: &mut MiningSession,
        current: Option<PublishedJob>,
        now: Instant,
        donation: BchDonation,
    ) -> Result<Vec<stratum_core::codec_sv2::SerializedFrame>, String> {
        if let Some(job) = current.filter(|job| now < job.valid_until) {
            self.allocation.update(now, !mining.channels.is_empty());
            let payout = BchPayout {
                donation,
                donation_work: self.allocation.donation_work(donation),
            };
            self.unavailable_since = None;
            if self.generation == Some(job.generation) && self.payout == Some(payout) {
                return Ok(Vec::new());
            }
            // Policy rotations need unique search space even with the same
            // template generation. The provider still receives its original
            // generation, while the wire ID commits a distinct coinbase.
            self.next_id = self
                .next_id
                .checked_add(1)
                .ok_or("job identifiers exhausted")?;
            let frames =
                mining.set_job_with_payout(self.next_id, job.generation, job.template, payout)?;
            self.generation = Some(job.generation);
            self.payout = Some(payout);
            return Ok(frames);
        }
        self.allocation.update(now, false);
        if self.generation.take().is_some() {
            mining.revoke_job();
        }
        let since = *self.unavailable_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= TEMPLATE_RECOVERY_GRACE {
            return Err("template source unavailable; reconnect when healthy".into());
        }
        Ok(Vec::new())
    }
}

fn template_reason(error: &str) -> &'static str {
    match error {
        "node tip changed while fetching the template" => "tip changed during refresh",
        "node is on the wrong network" => "wrong network",
        "node is not fully synchronized" => "node not synchronized",
        "node omitted height" | "node omitted tip" | "node returned an invalid tip hash" => {
            "invalid node tip"
        }
        "invalid block template" => "invalid block template",
        "template generation exhausted" => "template generation exhausted",
        _ => "node RPC unavailable",
    }
}

fn serve_device(
    stream: TcpStream,
    public: [u8; 32],
    config: &ServerConfig,
    shared: &Shared,
    device: u64,
) -> Result<(), String> {
    let session = Session::accept(stream, &public, &config.authority_secret)?;
    let (mut sender, mut receiver) = session.split();
    let result = (|| {
        let mut mining = MiningSession::new(
            config.network,
            config.payout.clone(),
            rand::random(),
            config.share_target,
        )?;
        let phase = rand::random();
        #[cfg(test)]
        let phase = config.allocation_phase.unwrap_or(phase);
        let mut availability = JobAvailability::new(phase);
        let mut accepted = 0u64;
        let mut rejected = 0u64;
        while !shared.stop.load(Ordering::Relaxed) {
            let current_job = || {
                shared
                    .job
                    .read()
                    .map(|job| job.clone())
                    .map_err(|_| "template state unavailable")
            };
            let donation = || {
                config
                    .donation
                    .read()
                    .map(|value| *value)
                    .map_err(|_| "donation setting unavailable")
            };
            for frame in
                availability.update(&mut mining, current_job()?, Instant::now(), donation()?)?
            {
                sender.send(frame)?;
            }
            if let Some(frame) = receiver.receive(Duration::from_millis(100))? {
                // A node failure or tip change may have occurred while waiting
                // for a device frame; resample before validating that frame.
                for update in
                    availability.update(&mut mining, current_job()?, Instant::now(), donation()?)?
                {
                    sender.send(update)?;
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "invalid system clock")?
                    .as_secs();
                let now = u32::try_from(now).map_err(|_| "system time exceeds header range")?;
                let responses = mining.receive(frame, now)?;
                // Persist the complete solved block before acknowledging it.
                // Storage failure stops the service rather than acknowledging
                // work which would disappear on a restart.
                for block in responses.blocks {
                    let saved = shared
                        .journal
                        .lock()
                        .map_err(|_| "block journal unavailable")?
                        .enqueue(&block);
                    if saved.is_err() {
                        if let Ok(mut fatal) = shared.fatal.lock() {
                            *fatal = Some("cannot persist solved block; mining stopped");
                        }
                        shared.stop.store(true, Ordering::Relaxed);
                        return Err("cannot persist solved block; mining stopped".into());
                    }
                    update_journal_stats(shared)?;
                    // A full wake slot already guarantees the worker wakes;
                    // it also scans pending disk work on its bounded timeout.
                    let _ = shared.wake.try_send(());
                }
                let next_accepted = mining.accepted;
                let next_rejected = mining.rejected;
                if let Ok(mut stats) = shared.stats.lock() {
                    stats.shares_accepted = stats
                        .shares_accepted
                        .saturating_add(next_accepted.saturating_sub(accepted));
                    stats.shares_rejected = stats
                        .shares_rejected
                        .saturating_add(next_rejected.saturating_sub(rejected));
                    stats.device_stats.channels(device, mining.channels.len());
                    if let Some(event) = responses.share_event {
                        stats
                            .device_stats
                            .share(device, event, false, Instant::now());
                    }
                }
                accepted = next_accepted;
                rejected = next_rejected;
                // #### PR #38
                // Publish validation counters before the peer can observe its
                // acknowledgement. The node worker may already have accepted
                // the block; delayed socket scheduling must not leave the
                // acknowledged share missing from the dashboard snapshot.
                for frame in responses.frames {
                    sender.send(frame)?;
                }
            }
        }
        Ok(())
    })();
    sender.close();
    receiver.close();
    result
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn payout_rotation_issues_unique_jobs_without_changing_template_generation() {
        use super::super::channel::{Channel, ChannelKind};
        let start = Instant::now();
        let payout = super::super::template_tests::payout();
        let mut mining =
            MiningSession::new(MiningNetwork::Chipnet, payout.clone(), [17; 12], [255; 32])
                .unwrap();
        mining.channels.insert(
            1,
            Channel::new(
                1,
                ChannelKind::Standard,
                [255; 32],
                [17; 12],
                MiningNetwork::Chipnet,
                &payout,
            )
            .unwrap(),
        );
        let job = PublishedJob {
            generation: 99,
            template: Arc::new(
                BchTemplate::from_rpc(&super::super::template_tests::rpc_template()).unwrap(),
            ),
            valid_until: start + Duration::from_secs(30),
        };
        let mut availability = JobAvailability::new(0);
        for (seconds, rate, expected_work, expected_id) in [
            (0, "1.5", true, 1),
            (3, "1.5", false, 2),
            (5, "2", false, 3),
        ] {
            availability
                .update(
                    &mut mining,
                    Some(job.clone()),
                    start + Duration::from_secs(seconds),
                    rate.parse().unwrap(),
                )
                .unwrap();
            let current = mining.channels[&1].job().unwrap();
            assert_eq!(current.id, expected_id);
            assert_eq!(current.generation, 99);
            assert_eq!(current.payout.donation_work, expected_work);
            assert_eq!(current.payout.donation, rate.parse().unwrap());
        }
        assert!(availability
            .update(
                &mut mining,
                Some(job),
                start + Duration::from_secs(6),
                "2".parse().unwrap()
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn expired_template_is_revoked_and_repeated_failures_do_not_extend_grace() {
        let start = Instant::now();
        let mut mining = MiningSession::new(
            MiningNetwork::Chipnet,
            super::super::template_tests::payout(),
            [17; 12],
            [255; 32],
        )
        .unwrap();
        let job = PublishedJob {
            generation: 1,
            template: Arc::new(
                BchTemplate::from_rpc(&super::super::template_tests::rpc_template()).unwrap(),
            ),
            valid_until: start + Duration::from_secs(30),
        };
        let mut availability = JobAvailability::new(300_000_000_000);
        availability
            .update(
                &mut mining,
                Some(job.clone()),
                start,
                BchDonation::default(),
            )
            .unwrap();
        assert_eq!(availability.generation, Some(1));
        availability
            .update(
                &mut mining,
                Some(job.clone()),
                job.valid_until,
                BchDonation::default(),
            )
            .unwrap();
        assert_eq!(availability.generation, None);
        availability
            .update(
                &mut mining,
                None,
                job.valid_until + Duration::from_secs(2),
                BchDonation::default(),
            )
            .unwrap();
        assert!(availability
            .update(
                &mut mining,
                Some(job.clone()),
                job.valid_until + TEMPLATE_RECOVERY_GRACE,
                BchDonation::default()
            )
            .is_err());
        assert_eq!(
            template_reason("private://user:secret@node/?key=secret"),
            "node RPC unavailable"
        );
        assert_eq!(
            template_reason("node tip changed while fetching the template"),
            "tip changed during refresh"
        );
    }

    #[test]
    fn pending_retry_backoff_is_bounded_and_does_not_block_new_work() {
        let mut schedule = RetrySchedule::default();
        let mut now = Instant::now();
        let hashes = vec!["old".to_owned(), "new".to_owned()];
        assert_eq!(schedule.next(&hashes, now).as_deref(), Some("old"));
        for expected in [1, 2, 4, 8, 16, 30, 30] {
            schedule.defer("old", now);
            assert_eq!(schedule.next(&hashes, now).as_deref(), Some("new"));
            assert_eq!(
                schedule.0["old"].1.duration_since(now),
                Duration::from_secs(expected)
            );
            now += Duration::from_secs(expected);
            assert_eq!(schedule.next(&hashes, now).as_deref(), Some("old"));
        }
        schedule.next(&["new".to_owned()], now);
        assert!(!schedule.is_retry("old"));
    }
}
