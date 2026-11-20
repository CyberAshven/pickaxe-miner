//! #### PR #42
//! The client side of Job Declaration. An uplink thread keeps a Job
//! Declaration session and one work-selection channel at the pool,
//! publishes the plan the local server builds its jobs from, declares each
//! local template (Full-Template: with its transactions, which the pool
//! checks and may ask for; Coinbase-only: as a custom job with a token), and
//! forwards the local devices' shares that meet the pool's target; a
//! Full-Template client also pushes its blocks to the pool. When the pool
//! refuses the templates or the link fails, the plan goes and the local
//! server stops offering work, so the SV1 adapter moves devices to the
//! pool's own jobs.

use super::{
    codec::{parse_outputs, serialize_outputs},
    payouts::{exact_coinbase, PayoutAsk, Payouts, PAYOUT_RESTARTS},
    plan::JdPlan,
    token::PxToken,
    ForwardShare, JdMode, JD_COMPACT_ROLLABLE, JD_ROLLABLE, MAX_DECLARED_TXS,
};
use crate::{
    p2pool::jd::Prepared,
    stratum_v2::{
        extensions::{
            pages, DeclareMiningJobTransactions, ExtensionMessage, PayoutOutputsStale,
            ProvideMissingTransactionsPositions, RequestExtensions, RequestExtensionsError,
            RequestExtensionsSuccess, RequestPayoutOutputs, RequestPayoutOutputsError,
            RequestPayoutOutputsSuccess, LARGE_TEMPLATES, NEGOTIATION, PAYOUT_OUTPUTS,
            PAYOUT_OUTPUTS_STALE, PROVIDE_MISSING_TRANSACTIONS_POSITIONS, REQUEST_EXTENSIONS_ERROR,
            REQUEST_EXTENSIONS_SUCCESS, REQUEST_PAYOUT_OUTPUTS_ERROR,
            REQUEST_PAYOUT_OUTPUTS_SUCCESS, STALE_PAYOUT_OUTPUTS,
        },
        sv1::addressed_to,
        template::{meets_target, BchTemplate, Hash},
        transport::{Limits, Receiver, Sender, Session},
        wire::{encoded, mining},
    },
};
use std::{
    collections::{HashMap, VecDeque},
    net::{TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver as Events, RecvTimeoutError, SyncSender},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2,
    codec_sv2::SerializedFrame,
    common_messages_sv2::{self as common, Protocol, SetupConnection, SetupConnectionError},
    job_declaration_sv2::{
        AllocateMiningJobToken, AllocateMiningJobTokenSuccess, DeclareMiningJob,
        DeclareMiningJobError, DeclareMiningJobSuccess, ProvideMissingTransactions,
        ProvideMissingTransactionsSuccess, PushSolution, MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS, MESSAGE_TYPE_DECLARE_MINING_JOB,
        MESSAGE_TYPE_DECLARE_MINING_JOB_ERROR, MESSAGE_TYPE_DECLARE_MINING_JOB_SUCCESS,
        MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS,
        MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS_SUCCESS, MESSAGE_TYPE_PUSH_SOLUTION,
    },
    mining_sv2::*,
    parsers_sv2::Mining,
};

/// Local events the uplink buffers before more are dropped.
const EVENTS: usize = 1_024;
/// Tokens kept in hand, so a new template is declared at once.
const TOKENS_AHEAD: usize = 2;
/// Shares held for a custom job the pool has not confirmed yet.
const CACHED_SHARES: usize = 256;
/// Confirmed custom jobs whose shares are still forwarded, and published
/// templates kept for PushSolution.
const KEPT_JOBS: usize = 16;
/// How long the pool has to answer a setup, a channel, a token or a job.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the pool has to answer a declaration: its node check may wait
/// for a slot and take a while on a large template.
const DECLARE_TIMEOUT: Duration = Duration::from_secs(60);
/// Declarations refused in a row on one parent before the client falls
/// back.
const DECLARE_ATTEMPTS: u8 = 4;
/// The waits before trying again after failures in a row, as multiples of
/// the target's `retry` (30 s in use: 30, 60, 120, then 300 s).
const RETRY_STEPS: [u32; 4] = [1, 2, 4, 10];
/// The forwarded shares whose verdicts are watched, and how many of them the
/// pool may reject before the client falls back.
const REJECT_WINDOW: usize = 20;
const REJECT_LIMIT: usize = 5;

/// The pool a client declares its templates to.
#[derive(Clone)]
pub struct JdTarget {
    /// `host:port` of the pool's SV2 listener.
    pub address: String,
    /// The pool's authority key, pinned on both sessions.
    pub authority: [u8; 32],
    /// The miner's payout address, which the pool pays blocks to.
    pub identity: String,
    /// The wait before trying again after the pool refused or failed.
    pub retry: Duration,
    pub mode: JdMode,
    pub p2pool_prepare_api: Option<String>,
    pub required: bool,
    /// #### merge-mining v2: the miner's own donation setting, named to a
    /// Pickaxe pool as `,pickaxe-donation=` (see `login`); `None` at BCH
    /// P2Pool, when the identity names a donation itself, and once the pool
    /// refused the option.
    pub pickaxe_donation: Option<Arc<RwLock<crate::donation::bch::BchDonation>>>,
}

// #### merge-mining v2: the miner's donation at a Pickaxe pool
// What: a Job Declaration client's login names the miner's own donation
// setting as `,pickaxe-donation=<percent>` (to the 0.5% step below, as of
// each session's start), so a Pickaxe pool's tokens carry it instead of the
// pool's setting. A pool that refuses the channel for the login itself
// (`unknown-user`, as a pool that cannot read options answers, or a Pickaxe
// pool's `invalid-donation`) is asked again without it, and keeps its own
// setting; any other refusal is a failure like any other, and the option
// stays.
// Why: the Pickaxe donation is the miner's choice on any pool; under Job
// Declaration it is paid in the coinbase only, at the token's rate. The key
// is Pickaxe's own: BCH P2Pool reads `donation=` for its own, independent
// donation.
// Look here if: a Job Declaration miner's coinbase donation is the pool's
// setting instead of theirs, or a pool refuses every session.
/// What a session says when the pool refused the channel of a login that
/// named `,pickaxe-donation=`.
const PICKAXE_DONATION_REFUSED: &str =
    "the pool refused the login's donation option; using the pool's own donation setting";

/// Whether a channel refusal is about the login itself, not a passing
/// state of the pool such as `no-template-available`.
fn refuses_login(frame: &mut SerializedFrame) -> bool {
    frame.header().msg_type() == MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR
        && binary_sv2::from_bytes::<OpenMiningChannelError>(frame.payload()).is_ok_and(|error| {
            matches!(
                error.error_code.as_ref(),
                b"unknown-user" | b"invalid-donation"
            )
        })
}

impl JdTarget {
    /// The login the pool sees: the identity, with the miner's donation
    /// setting where `pickaxe_donation` names one.
    fn login(&self) -> String {
        match &self.pickaxe_donation {
            Some(rate) => {
                let bps = u16::from(*rate.read().unwrap_or_else(|poisoned| poisoned.into_inner()));
                let bps = bps - bps % 50;
                format!(
                    "{},pickaxe-donation={}.{:02}",
                    self.identity,
                    bps / 100,
                    bps % 100
                )
            }
            None => self.identity.clone(),
        }
    }
}

/// What the dashboard shows of the client; never an identity or token.
#[derive(Clone, Debug, Default)]
pub struct JdClientSummary {
    /// "connecting", "active" or "fallback".
    pub state: &'static str,
    /// "full-template" or "coinbase-only".
    pub mode: &'static str,
    pub custom_jobs: u64,
    pub refused: u64,
    pub forwarded: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub fallbacks: u64,
    pub last_error: Option<String>,
    /// Full-Template: declarations the pool accepted, rounds of missing
    /// transactions provided, declarations dropped (a tip race, a template
    /// over 65,535 transactions or missing transactions over one frame) and
    /// blocks pushed to the pool.
    pub declared: u64,
    pub provided: u64,
    pub dropped: u64,
    pub pushed: u64,
    /// Requested Payout Outputs (0x5052): coinbases built from the pool's
    /// payout outputs.
    pub payout_outputs: u64,
}

pub struct JdStatus {
    pub plan: RwLock<Option<Arc<JdPlan>>>,
    pub summary: Mutex<JdClientSummary>,
    /// The newest local template generation whose custom job the pool
    /// accepted in this session (0 for none yet).
    pub acknowledged: AtomicU64,
}

impl JdStatus {
    fn update(&self, change: impl FnOnce(&mut JdClientSummary)) {
        if let Ok(mut summary) = self.summary.lock() {
            change(&mut summary);
        }
    }

    fn set_plan(&self, plan: Option<Arc<JdPlan>>) {
        if let Ok(mut current) = self.plan.write() {
            *current = plan;
        }
    }
}

/// What the local server hands the uplink, never waiting on it.
pub enum UplinkEvent {
    /// A template published with the current plan, to declare.
    Published {
        serial: u64,
        template: Arc<BchTemplate>,
        /// Paired sessions admit the event only after both queues accept it.
        admission: Option<Arc<Mutex<bool>>>,
    },
    /// An accepted local share on a declared job.
    Share(ForwardShare),
}

impl UplinkEvent {
    fn published(serial: u64, template: Arc<BchTemplate>) -> Self {
        Self::Published {
            serial,
            template,
            admission: None,
        }
    }
}

fn publication_admitted(admission: Option<Arc<Mutex<bool>>>) -> bool {
    admission.is_none_or(|admission| admission.lock().is_ok_and(|committed| *committed))
}

#[derive(Clone)]
pub struct UplinkHandle {
    pub events: SyncSender<UplinkEvent>,
    pub status: Arc<JdStatus>,
    /// P2Pool credits donation work to a separate authorized share owner.
    /// Both sessions declare the same independently sourced transactions.
    pub donation: Option<Arc<UplinkHandle>>,
}

impl UplinkHandle {
    // #### PR #65: queue personal and donation declarations as one admission.
    // A receiver may dequeue immediately, but cannot declare until both sends
    // succeed. A full/disconnected peer cancels the other event before it can
    // consume a token or replace an accepted job. No network I/O holds the gate.
    // A template without a donation job (the donation at 0%, its session
    // down or its preparation failed) is declared on the personal session
    // alone.
    pub fn publish(&self, serial: u64, template: Arc<BchTemplate>) -> Result<(), &'static str> {
        let (Some(donation), Some(own)) =
            (self.donation.as_ref(), template.p2pool_donation().cloned())
        else {
            return self
                .events
                .try_send(UplinkEvent::published(serial, template))
                .map_err(|_| "personal declaration queue unavailable");
        };
        let admission = Arc::new(Mutex::new(false));
        let mut committed = admission
            .lock()
            .map_err(|_| "declaration admission unavailable")?;
        donation
            .events
            .try_send(UplinkEvent::Published {
                serial,
                template: own,
                admission: Some(admission.clone()),
            })
            .map_err(|_| "donation declaration queue unavailable")?;
        self.events
            .try_send(UplinkEvent::Published {
                serial,
                template,
                admission: Some(admission.clone()),
            })
            .map_err(|_| "personal declaration queue unavailable")?;
        *committed = true;
        Ok(())
    }

    // #### PR #65: each session's acceptance gates its own work
    // What: the miner's own jobs wait only for the personal session's
    // acknowledgement; donation jobs wait for the donation session's, which
    // counts only while that session holds a plan.
    // Why: personal mining waited for min(personal, donation), so a donation
    // session that was down (or slow) stopped the miner's own work.
    // Look here if: devices idle while the personal session is active, or
    // mine donation jobs that session never accepted.
    /// The newest declaration the personal session's pool accepted.
    pub fn acknowledged(&self) -> u64 {
        self.status.acknowledged.load(Ordering::Relaxed)
    }

    /// The donation session's plan serial and the newest declaration its
    /// pool accepted, while it holds a plan.
    pub fn donation_acknowledged(&self) -> Option<(u64, u64)> {
        let donation = self.donation.as_ref()?;
        let plan = donation.status.plan.read().ok()?.as_ref()?.serial;
        Some((plan, donation.status.acknowledged.load(Ordering::Relaxed)))
    }

    pub fn forward(&self, share: ForwardShare) {
        let route = if share.donation {
            self.donation.as_deref().unwrap_or(self)
        } else {
            self
        };
        let _ = route.events.try_send(UplinkEvent::Share(share));
    }
}

/// Whether a session should run now (see `spawn_p2pool`).
type Wanted = Arc<dyn Fn() -> bool + Send + Sync>;

/// A P2Pool beneficiary needs its own channel, token and declared coinbase.
/// Never subtract a software donation from other participants' PPLNS outputs.
// #### PR #65: no donation session at 0%
// What: the donation session connects only while the donation setting is
// above 0%; set to 0% it ends (its plan goes) and waits, without a
// connection, a token or a preparation, until the setting rises again.
// Why: at 0% it still took tokens and helper calls, and its failures
// stopped the miner's own work.
// Look here if: a donation session connects at 0%, or not after the
// setting rises.
pub fn spawn_p2pool(
    targets: Vec<JdTarget>,
    donation_identity: String,
    donation_rate: Arc<RwLock<crate::donation::bch::BchDonation>>,
    stop: Arc<AtomicBool>,
) -> (UplinkHandle, thread::JoinHandle<()>) {
    let donation_targets = targets
        .iter()
        .cloned()
        .map(|mut target| {
            target.identity = donation_identity.clone();
            target
        })
        .collect();
    let (mut personal, personal_worker) = spawn(targets, stop.clone());
    let wanted: Wanted =
        Arc::new(move || donation_rate.read().is_ok_and(|rate| u16::from(*rate) != 0));
    let (donation, donation_worker) = spawn_with(donation_targets, stop, Some(wanted));
    personal.donation = Some(Arc::new(donation));
    let worker = thread::spawn(move || {
        let _ = personal_worker.join();
        let _ = donation_worker.join();
    });
    (personal, worker)
}

/// Starts the uplink to the first of `targets` (the pools in order); it
/// runs until `stop`.
pub fn spawn(
    targets: Vec<JdTarget>,
    stop: Arc<AtomicBool>,
) -> (UplinkHandle, thread::JoinHandle<()>) {
    spawn_with(targets, stop, None)
}

/// `spawn`, connecting only while `wanted` says so (always without it).
fn spawn_with(
    targets: Vec<JdTarget>,
    stop: Arc<AtomicBool>,
    wanted: Option<Wanted>,
) -> (UplinkHandle, thread::JoinHandle<()>) {
    let paused = move || wanted.as_ref().is_some_and(|wanted| !wanted());
    let (events, receive) = mpsc::sync_channel(EVENTS);
    let status = Arc::new(JdStatus {
        plan: RwLock::new(None),
        summary: Mutex::new(JdClientSummary {
            state: "connecting",
            mode: targets
                .first()
                .map_or(JdMode::FullTemplate, |target| target.mode)
                .as_str(),
            ..JdClientSummary::default()
        }),
        acknowledged: AtomicU64::new(0),
    });
    let shared = status.clone();
    let worker = thread::spawn(move || {
        let mut targets = targets;
        let mut serial = 0u64;
        let mut failures = 0u32;
        let mut next = 0usize;
        while !stop.load(Ordering::Relaxed) {
            let Some(target) = targets.get(next).cloned() else {
                return;
            };
            let target = &target;
            // #### PR #65: a paused session waits without a connection (see
            // `spawn_p2pool`).
            if paused() {
                shared.update(|summary| {
                    summary.state = "off";
                    summary.last_error = None;
                });
                if let Err(RecvTimeoutError::Disconnected) =
                    receive.recv_timeout(Duration::from_millis(100))
                {
                    return;
                }
                continue;
            }
            serial += 1;
            let custom_jobs = || {
                shared
                    .summary
                    .lock()
                    .map_or(0, |summary| summary.custom_jobs)
            };
            let before = custom_jobs();
            let result = uplink(target, &shared, &receive, &stop, serial, &paused);
            shared.set_plan(None);
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // #### merge-mining v2: then without the donation option (see
            // `JdTarget::login`).
            if matches!(&result, Err(error) if error == PICKAXE_DONATION_REFUSED) {
                targets[next].pickaxe_donation = None;
                shared.update(|summary| summary.last_error = result.err());
                continue;
            }
            if paused() {
                failures = 0;
                continue;
            }
            // #### PR #42: retries back off across the pools
            // What: after a failure the uplink tries the next pool in order
            // (wrapping), waiting 30, 60, 120, then 300 seconds as failures
            // follow each other; a session whose pool accepted custom jobs
            // starts the count again.
            // Why: a pool that keeps refusing must not be hammered, and a
            // second Pickaxe pool can take the templates while the first is
            // down.
            // Look here if: the uplink retries too often, or never moves to
            // the next pool.
            failures = if custom_jobs() > before {
                1
            } else {
                failures.saturating_add(1)
            };
            next = (next + 1) % targets.len();
            // #### PR #42: fallback
            // What: when the pool refuses a custom job (other than for a tip
            // race), the link fails or the pool's terms change, the plan goes:
            // the local server stops offering work and the SV1 adapter moves
            // devices to the pool's own jobs. The uplink tries again later.
            // Why: a refused template must never cost the miner hash rate.
            // Look here if: devices stay idle while the pool is up, or the
            // dashboard keeps counting fallbacks.
            shared.update(|summary| {
                summary.state = if target.required {
                    "waiting"
                } else {
                    "fallback"
                };
                summary.fallbacks += u64::from(!target.required);
                summary.last_error = result.err();
            });
            let until = Instant::now() + retry_after(target.retry, failures);
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                if let Err(RecvTimeoutError::Disconnected) =
                    receive.recv_timeout(Duration::from_millis(100))
                {
                    return;
                }
            }
            shared.update(|summary| summary.state = "connecting");
        }
    });
    (
        UplinkHandle {
            events,
            status,
            donation: None,
        },
        worker,
    )
}

/// The wait before the next try after `failures` failures in a row.
fn retry_after(base: Duration, failures: u32) -> Duration {
    base * RETRY_STEPS[(failures.max(1) - 1).min(3) as usize]
}

/// The pool's verdicts on the latest forwarded shares.
#[derive(Default)]
struct Verdicts(VecDeque<bool>);

impl Verdicts {
    // #### PR #42: rejected shares fall back
    // What: when the pool rejects 5 of the last 20 shares forwarded on
    // active custom jobs (stale-share and invalid-job-id, races at a new
    // block, not counted), the client falls back to the pool's own jobs.
    // Why: shares the pool rejects earn the miner nothing; the pool's own
    // jobs do.
    // Look here if: the client falls back while the pool accepts its
    // shares, or keeps sending shares the pool rejects.
    /// Records `count` verdicts of one kind; true once 5 of the last 20 are
    /// rejections.
    fn record(&mut self, accepted: bool, count: u32) -> bool {
        for _ in 0..count.min(REJECT_WINDOW as u32) {
            self.0.push_back(accepted);
        }
        while self.0.len() > REJECT_WINDOW {
            self.0.pop_front();
        }
        self.0.iter().filter(|accepted| !**accepted).count() >= REJECT_LIMIT
    }
}

/// A template declared to the pool, waiting for its answer.
struct Declaring {
    serial: u64,
    template: Arc<BchTemplate>,
    /// When the pool last heard of it (the declaration, or transactions).
    sent: Instant,
}

/// One pool session: Ok when the server stops or `paused` says so (see
/// `spawn_p2pool`), otherwise why it ended.
fn uplink(
    target: &JdTarget,
    status: &JdStatus,
    events: &Events<UplinkEvent>,
    stop: &AtomicBool,
    serial: u64,
    paused: &dyn Fn() -> bool,
) -> Result<(), String> {
    let full = target.mode == JdMode::FullTemplate;
    let login = target.login();
    let limits = if full { Limits::JD } else { Limits::DEVICE };
    let (mut jd, mut jd_replies) = connect(target, limits)?;
    setup(
        target,
        &mut jd,
        &mut jd_replies,
        Protocol::JobDeclarationProtocol,
        u32::from(full),
    )?;
    // What: right after the JD setup, the client asks for Requested Payout
    // Outputs (0x5052) and, in Full-Template, BCH large templates (0x4C54),
    // as the Knuth node does. No answer within the answer deadline (or a pool
    // that closes the connection on the request) is not a refusal: that JD
    // connection is closed and one new connection sets up without asking. A
    // refusal leaves both off. A P2Pool session, whose coinbase P2Pool
    // prepares, does not ask.
    // Why: Knuth's fallbacks (docs/sv2.md, "Job Declaration"); a pool that
    // does not know the negotiation must still work.
    // Look here if: a pool's JD connection is opened twice, or a granted
    // extension goes unused.
    let granted = if target.p2pool_prepare_api.is_some() {
        Granted::default()
    } else {
        match negotiate(&mut jd, &mut jd_replies, full, ANSWER_TIMEOUT)? {
            Some(granted) => granted,
            None => {
                jd.close();
                jd_replies.close();
                (jd, jd_replies) = connect(target, limits)?;
                setup(
                    target,
                    &mut jd,
                    &mut jd_replies,
                    Protocol::JobDeclarationProtocol,
                    u32::from(full),
                )?;
                Granted::default()
            }
        }
    };
    let (mut pool, mut replies) = connect(target, Limits::DEVICE)?;
    setup(
        target,
        &mut pool,
        &mut replies,
        Protocol::MiningProtocol,
        0b110,
    )?;
    let open = |pool: &mut Sender, request_id: u32, rollable: usize| -> Result<(), String> {
        pool.send(mining(Mining::OpenExtendedMiningChannel(
            OpenExtendedMiningChannel {
                request_id,
                user_identity: login.as_str().try_into().map_err(|_| "identity too long")?,
                nominal_hash_rate: 1e12,
                max_target: (&[0xff; 32]).into(),
                min_extranonce_size: rollable as u16,
            },
        ))?)
    };
    open(&mut pool, 1, JD_ROLLABLE)?;
    let mut frame = answer(&mut replies, stop)?;
    // #### PR #56: the compact layout
    // What: a pool that refuses 16 rollable bytes is asked once more for 8;
    // granted, the plan is compact: a 2-byte job id, the pad, a 2-byte lane
    // and 4 bytes for each device. Local channels opened on another layout
    // end at their next job and reopen on this one.
    // Why: BCH P2Pool grants at most 8 bytes, the size its share chain
    // stores, so the 16-byte layout cannot mine there.
    // Look here if: a pool refuses both requests, or local devices
    // reconnect when the pool changes.
    if frame.header().msg_type() == MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR {
        open(&mut pool, 2, JD_COMPACT_ROLLABLE)?;
        frame = answer(&mut replies, stop)?;
    }
    let login_refused = target.pickaxe_donation.is_some() && refuses_login(&mut frame);
    let (channel, mut group, mut pool_target, prefix, rollable) = match frame.header().msg_type() {
        MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS => {
            let opened: OpenExtendedMiningChannelSuccess =
                binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed channel")?;
            (
                opened.channel_id,
                opened.group_channel_id,
                <Hash>::try_from(opened.target.as_ref()).map_err(|_| "malformed target")?,
                opened.extranonce_prefix.as_ref().to_vec(),
                usize::from(opened.extranonce_size),
            )
        }
        _ if login_refused => return Err(PICKAXE_DONATION_REFUSED.into()),
        _ => return Err("the pool refused the Job Declaration channel".into()),
    };
    if rollable < JD_COMPACT_ROLLABLE {
        return Err("the pool's extranonce space is too small for Job Declaration".into());
    }
    let compact = rollable < JD_ROLLABLE;
    let mut requests = 0u32;
    for _ in 0..TOKENS_AHEAD {
        allocate(&login, &mut jd, &mut requests)?;
    }
    let mut tokens: VecDeque<Token> = VecDeque::new();
    // A notice (0x5052) before the first token names no token yet.
    let mut first = answer(&mut jd_replies, stop)?;
    while first.header().ext_type_without_channel_msg() != 0 {
        first = answer(&mut jd_replies, stop)?;
    }
    let (first, mut scripts) = token_for(
        &mut first,
        target.p2pool_prepare_api.is_some(),
        granted.payouts,
    )?;
    let local_rates = first.local_rates;
    let (payouts, asks) = if granted.payouts {
        let (door, asks) = Payouts::new(local_rates);
        (Some(Arc::new(door)), Some(asks))
    } else {
        (None, None)
    };
    let p2pool = target
        .p2pool_prepare_api
        .as_ref()
        .map(|api| {
            let (payout, share_owner) = target
                .identity
                .split_once(",p2p=")
                .ok_or("P2Pool own-template mode needs both payout and share-owner identities")?;
            scripts = vec![crate::tx::cashaddr_to_p2pkh_locking(payout)?];
            Ok::<_, String>(Arc::new(crate::p2pool::jd::Plan {
                api: api.clone(),
                payout: payout.to_owned(),
                share_owner: share_owner.to_owned(),
                tokens: std::sync::Mutex::new(VecDeque::from([first.bytes.clone()])),
                consumed: std::sync::atomic::AtomicUsize::new(0),
            }))
        })
        .transpose()?;
    let plan = Arc::new(JdPlan {
        serial,
        upstream_prefix: prefix,
        pad: vec![
            0;
            rollable
                - if compact {
                    JD_COMPACT_ROLLABLE
                } else {
                    JD_ROLLABLE
                }
        ],
        compact,
        scripts,
        rates: first.rates,
        pool_target,
        p2pool,
        payouts,
        full_template: full,
    });
    if !plan.is_valid() {
        return Err("the pool's token names outputs it does not list, or rates over 100%".into());
    }
    if plan.p2pool.is_none() {
        tokens.push_back(first);
    }
    // #### 0x5052: the preparation asked for now, the notices seen (a
    // token's epoch is the count when it arrived), and whether a token
    // since named other rates than the plan's.
    let mut payout: Option<PayoutWork> = None;
    let mut epoch = 0u64;
    let mut next_payout = 1u32;
    let mut moved = false;
    status.acknowledged.store(0, Ordering::Relaxed);
    status.set_plan(Some(plan.clone()));
    status.update(|summary| {
        summary.state = "active";
        summary.last_error = None;
    });
    let mut pending: HashMap<u32, (u64, Instant)> = HashMap::new();
    let mut declaring: HashMap<u32, Declaring> = HashMap::new();
    let mut published: VecDeque<(u64, Arc<BchTemplate>)> = VecDeque::new();
    let mut refusals = Refusals::default();
    let mut jobs: VecDeque<(u64, u32)> = VecDeque::new();
    let mut verdicts = Verdicts::default();
    let mut cache: HashMap<u64, Vec<ForwardShare>> = HashMap::new();
    let mut sequence = 0u32;
    let mut next_request = 1u32;
    let mut next_declaration = 1u32;
    while !stop.load(Ordering::Relaxed) {
        // #### PR #65: the donation set to 0% ends its session.
        if paused() {
            return Ok(());
        }
        if let Some(p2pool) = &plan.p2pool {
            for _ in 0..p2pool.consumed.swap(0, Ordering::Relaxed) {
                allocate(&login, &mut jd, &mut requests)?;
            }
        }
        // #### 0x5052: the next preparation's request (see `PayoutWork`).
        if let Some(asks) = asks.as_ref() {
            if payout.is_none() {
                payout = asks.try_recv().ok().map(PayoutWork::new);
            }
            if let Some(work) = payout.as_mut() {
                if work.inflight.is_none() {
                    if let Some(token) = tokens.pop_front() {
                        allocate(&login, &mut jd, &mut requests)?;
                        let id = next_payout;
                        next_payout = next_payout.wrapping_add(1);
                        jd.send(work.request(id, token)?)?;
                    } else if work.received.elapsed() >= ANSWER_TIMEOUT {
                        work.answer(Err("waiting for a Job Declaration token".into()));
                        payout = None;
                    }
                }
            }
            if payout
                .as_ref()
                .and_then(|work| work.inflight.as_ref())
                .is_some_and(|inflight| inflight.sent.elapsed() >= ANSWER_TIMEOUT)
            {
                return Err("the pool did not answer a payout outputs request".into());
            }
        }
        while let Some(mut frame) = jd_replies.receive(Duration::from_millis(5))? {
            // What: Knuth's extensions' frames on the JD connection: the
            // pool's payout answers and notices (0x5052) once granted, and
            // ProvideMissingTransactions.Positions (0x4C54) in Full-Template
            // once granted. A granted extension's other messages break the
            // protocol; any other extension frame is discarded.
            // Look here if: a payout answer or a Positions request is
            // ignored, or the session ends on one.
            let header = frame.header();
            if header.ext_type_without_channel_msg() != 0 {
                if header.channel_msg() {
                    continue;
                }
                match (header.ext_type_without_channel_msg(), header.msg_type()) {
                    (
                        PAYOUT_OUTPUTS,
                        REQUEST_PAYOUT_OUTPUTS_SUCCESS | REQUEST_PAYOUT_OUTPUTS_ERROR,
                    ) if granted.payouts => {
                        let work = payout
                            .as_mut()
                            .ok_or("the pool answered a payout request never sent")?;
                        if let Some(prepared) = work.receive(&mut frame, epoch)? {
                            payout = None;
                            if prepared {
                                status.update(|summary| summary.payout_outputs += 1);
                            }
                        }
                    }
                    // A notice: every token held is stale; the request in
                    // flight asks again once answered. The next template
                    // refresh declares on new outputs.
                    (PAYOUT_OUTPUTS, PAYOUT_OUTPUTS_STALE) if granted.payouts => {
                        PayoutOutputsStale::decode(frame.payload())
                            .ok_or("malformed PayoutOutputsStale")?;
                        epoch += 1;
                        for _ in tokens.drain(..) {
                            allocate(&login, &mut jd, &mut requests)?;
                        }
                    }
                    (LARGE_TEMPLATES, PROVIDE_MISSING_TRANSACTIONS_POSITIONS)
                        if full && granted.large =>
                    {
                        let ask = ProvideMissingTransactionsPositions::decode(frame.payload())
                            .ok_or("malformed ProvideMissingTransactions.Positions")?;
                        let positions: Vec<usize> =
                            ask.positions.iter().map(|at| *at as usize).collect();
                        supply(
                            &mut jd,
                            &mut declaring,
                            &mut cache,
                            status,
                            ask.request_id,
                            &positions,
                            true,
                        )?;
                    }
                    (PAYOUT_OUTPUTS, _) if granted.payouts => {
                        return Err("the pool sent an unexpected payout outputs message".into())
                    }
                    (LARGE_TEMPLATES, _) if granted.large => {
                        return Err("the pool sent an unexpected large template message".into())
                    }
                    _ => (),
                }
                continue;
            }
            match frame.header().msg_type() {
                MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS => {
                    let (mut token, scripts) =
                        token_for(&mut frame, plan.p2pool.is_some(), granted.payouts)?;
                    if let Some(p2pool) = &plan.p2pool {
                        let mut queued = p2pool
                            .tokens
                            .lock()
                            .map_err(|_| "P2Pool token queue unavailable")?;
                        if queued.len() >= TOKENS_AHEAD {
                            return Err("unsolicited P2Pool job tokens".into());
                        }
                        queued.push_back(token.bytes);
                        continue;
                    }
                    // #### 0x5052: under the pool's own outputs a change of
                    // rates matters only to a template built from them (one
                    // that merge-mines tokens), which then starts a new
                    // session.
                    if token.rates != plan.rates || scripts != plan.scripts {
                        if plan.payouts.is_none() {
                            return Err("the pool's fee or donation changed".into());
                        }
                        moved |= local_rates;
                    }
                    token.epoch = epoch;
                    tokens.push_back(token);
                }
                // #### PR #42: missing transactions
                // What: the pool names transactions of a declaration it does
                // not have by their position; the client sends them from the
                // declared template in one frame. Transactions that cannot
                // fit one frame (16 MiB) drop that declaration, without a
                // fallback.
                // Why: Full-Template lets the pool check and propagate the
                // block, so it needs every transaction once.
                // Look here if: the dropped count grows, or a declaration
                // waits for transactions.
                MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS if full => {
                    let ask: ProvideMissingTransactions =
                        binary_sv2::from_bytes(frame.payload())
                            .map_err(|_| "malformed missing transactions")?;
                    let positions: Vec<usize> = ask
                        .unknown_tx_position_list
                        .iter()
                        .map(|at| usize::from(*at))
                        .collect();
                    supply(
                        &mut jd,
                        &mut declaring,
                        &mut cache,
                        status,
                        ask.request_id,
                        &positions,
                        granted.large,
                    )?;
                }
                MESSAGE_TYPE_DECLARE_MINING_JOB_SUCCESS if full => {
                    let success: DeclareMiningJobSuccess = binary_sv2::from_bytes(frame.payload())
                        .map_err(|_| "malformed declaration answer")?;
                    let Some(declaration) = declaring.remove(&success.request_id) else {
                        continue;
                    };
                    refusals = Refusals::default();
                    status.update(|summary| summary.declared += 1);
                    let request = next_request;
                    next_request = next_request.wrapping_add(1);
                    declare(
                        &mut pool,
                        channel,
                        request,
                        success.new_mining_job_token.as_ref(),
                        &declaration.template,
                    )?;
                    pending.insert(request, (declaration.serial, Instant::now()));
                }
                MESSAGE_TYPE_DECLARE_MINING_JOB_ERROR if full => {
                    let error: DeclareMiningJobError = binary_sv2::from_bytes(frame.payload())
                        .map_err(|_| "malformed declaration error")?;
                    let Some(declaration) = declaring.remove(&error.request_id) else {
                        continue;
                    };
                    cache.remove(&declaration.serial);
                    let code = String::from_utf8_lossy(error.error_code.as_ref()).into_owned();
                    let details = String::from_utf8_lossy(error.error_details.as_ref())
                        .chars()
                        .filter(|c| c.is_ascii_graphic() || *c == ' ')
                        .take(200)
                        .collect::<String>();
                    // A tip race: the next template is declared on the new
                    // parent, without a fallback.
                    if code == "stale-chain-tip" {
                        status.update(|summary| summary.dropped += 1);
                        continue;
                    }
                    status.update(|summary| summary.refused += 1);
                    if refusals.refuse(declaration.template.previous_hash) {
                        return Err(format!(
                            "the pool refused {DECLARE_ATTEMPTS} declarations: {code} ({details})"
                        ));
                    }
                }
                _ => (),
            }
        }
        while let Some(mut frame) = replies.receive(Duration::from_millis(5))? {
            let header = frame.header();
            let Ok(message) = Mining::try_from((header.msg_type(), frame.payload())) else {
                continue;
            };
            match message {
                Mining::SetCustomMiningJobSuccess(success) => {
                    let Some((serial, _)) = pending.remove(&success.request_id) else {
                        continue;
                    };
                    jobs.push_back((serial, success.job_id));
                    while jobs.len() > KEPT_JOBS {
                        jobs.pop_front();
                    }
                    status.acknowledged.fetch_max(serial, Ordering::Relaxed);
                    status.update(|summary| {
                        summary.custom_jobs += 1;
                        summary.state = "active";
                        summary.last_error = None;
                    });
                    for share in cache.remove(&serial).unwrap_or_default() {
                        forward(
                            &mut pool,
                            channel,
                            success.job_id,
                            &mut sequence,
                            share,
                            status,
                        )?;
                    }
                }
                Mining::SetCustomMiningJobError(error) => {
                    let Some((serial, _)) = pending.remove(&error.request_id) else {
                        continue;
                    };
                    cache.remove(&serial);
                    status.update(|summary| summary.refused += 1);
                    let code = String::from_utf8_lossy(error.error_code.as_ref()).into_owned();
                    // A tip race: the next template is declared on the new
                    // parent, without a fallback.
                    if code != "stale-chain-tip" {
                        return Err(format!("the pool refused a custom job: {code}"));
                    }
                }
                Mining::SubmitSharesSuccess(success) => {
                    status.update(|summary| {
                        summary.accepted += u64::from(success.new_submits_accepted_count)
                    });
                    verdicts.record(true, success.new_submits_accepted_count);
                }
                Mining::SubmitSharesError(error) => {
                    status.update(|summary| summary.rejected += 1);
                    let code = String::from_utf8_lossy(error.error_code.as_ref()).into_owned();
                    if !matches!(code.as_str(), "stale-share" | "invalid-job-id")
                        && verdicts.record(false, 1)
                    {
                        return Err(format!(
                            "the pool rejected {REJECT_LIMIT} of the last {REJECT_WINDOW} \
                             shares ({code})"
                        ));
                    }
                }
                Mining::SetTarget(set) if addressed_to(set.channel_id, channel, group) => {
                    pool_target =
                        <Hash>::try_from(set.target.as_ref()).map_err(|_| "malformed target")?;
                }
                Mining::SetGroupChannel(set) => {
                    if set.channel_ids.iter().any(|id| *id == channel) {
                        group = set.group_channel_id;
                    }
                }
                Mining::CloseChannel(close) if addressed_to(close.channel_id, channel, group) => {
                    return Err("the pool closed the Job Declaration channel".into());
                }
                Mining::SetExtranoncePrefix(set) if set.channel_id == channel => {
                    return Err("the pool changed the channel's extranonce".into());
                }
                _ => (),
            }
        }
        if pending
            .values()
            .any(|(_, sent)| sent.elapsed() >= ANSWER_TIMEOUT)
        {
            return Err("the pool did not answer a custom job".into());
        }
        if declaring
            .values()
            .any(|declaration| declaration.sent.elapsed() >= DECLARE_TIMEOUT)
        {
            return Err("the pool did not answer a declaration".into());
        }
        let event = match events.recv_timeout(Duration::from_millis(20)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        match event {
            UplinkEvent::Published {
                serial,
                template,
                admission,
            } => {
                if !publication_admitted(admission) {
                    continue;
                }
                if template.jd_plan().map(|plan| plan.serial) != Some(plan.serial) {
                    continue;
                }
                published.push_back((serial, template.clone()));
                while published.len() > KEPT_JOBS {
                    published.pop_front();
                }
                // #### PR #42: a template over 65,535 transactions is not
                // declared: SV2 counts a declaration's transactions in 16
                // bits. Devices keep the custom job on the previous
                // template of the same parent, if any. #### 0x4C54: a pool
                // that granted BCH large templates takes it in pages.
                if full && !declarable(&template, granted.large) {
                    status.update(|summary| summary.dropped += 1);
                    continue;
                }
                // #### 0x5052: a template built from the plan's rates after
                // the pool named others starts a new session on them.
                if moved && template.prepared_coinbase().is_none() {
                    return Err("the pool's fee or donation changed".into());
                }
                // A prepared coinbase (P2Pool's, or the pool's payout
                // outputs) carries its own token, whose replacement was
                // asked for when it was taken.
                let (token, queued) = match template.p2pool_token() {
                    Some(token) => (token.to_vec(), false),
                    None => match tokens.pop_front() {
                        Some(token) => (token.bytes, true),
                        None => {
                            status.update(|summary| summary.refused += 1);
                            continue;
                        }
                    },
                };
                if queued && plan.p2pool.is_none() {
                    allocate(&login, &mut jd, &mut requests)?;
                }
                if full {
                    let request = next_declaration;
                    next_declaration = next_declaration.wrapping_add(1);
                    for frame in declaration(request, &token, &template, granted.large)? {
                        jd.send(frame)?;
                    }
                    declaring.insert(
                        request,
                        Declaring {
                            serial,
                            template,
                            sent: Instant::now(),
                        },
                    );
                } else {
                    let request = next_request;
                    next_request = next_request.wrapping_add(1);
                    declare(&mut pool, channel, request, &token, &template)?;
                    pending.insert(request, (serial, Instant::now()));
                }
                // Shares of jobs this one replaces on another parent stay
                // forwardable only while the pool keeps the jobs.
                cache.retain(|kept, _| {
                    pending.values().any(|(serial, _)| serial == kept)
                        || declaring.values().any(|declared| declared.serial == *kept)
                });
            }
            UplinkEvent::Share(share) => {
                // #### PR #42: a Full-Template client pushes its blocks to
                // the pool (PushSolution), whatever the pool's target, so
                // the pool's node gets them too.
                if full && share.block {
                    if let Some((_, template)) = published
                        .iter()
                        .rev()
                        .find(|(serial, _)| *serial == share.serial)
                    {
                        jd.send(push_solution(&plan, template, &share)?)?;
                        status.update(|summary| summary.pushed += 1);
                    }
                }
                if !for_pool(&share, &pool_target) {
                    continue;
                }
                if let Some((_, job)) = jobs
                    .iter()
                    .rev()
                    .find(|(serial, _)| *serial == share.serial)
                {
                    forward(&mut pool, channel, *job, &mut sequence, share, status)?;
                } else if pending.values().any(|(serial, _)| *serial == share.serial)
                    || declaring
                        .values()
                        .any(|declared| declared.serial == share.serial)
                {
                    let held = cache.entry(share.serial).or_default();
                    if held.len() < CACHED_SHARES {
                        held.push(share);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Declarations refused in a row on one parent.
#[derive(Default)]
struct Refusals {
    parent: Option<Hash>,
    count: u8,
}

impl Refusals {
    // #### PR #42: four refusals on one template fall back
    // What: a declaration the pool refuses (other than stale-chain-tip) is
    // dropped; the fourth in a row on one parent makes the client fall back
    // to the pool's own jobs. An accepted declaration or a new parent starts
    // the count again.
    // Why: one refusal may be a passing disagreement (a transaction the
    // pool's node has not seen); four mean the pool will not take this
    // node's templates, and devices must not mine work nobody pays for.
    // Look here if: the client falls back after one bad template, or never.
    /// Counts a refusal on `parent`; true on the fourth in a row.
    fn refuse(&mut self, parent: Hash) -> bool {
        if self.parent != Some(parent) {
            self.parent = Some(parent);
            self.count = 0;
        }
        self.count += 1;
        self.count >= DECLARE_ATTEMPTS
    }
}

/// Whether `template` can be declared: SV2 counts its transactions in 16
/// bits; #### 0x4C54: any number when the pool takes pages (`large`).
fn declarable(template: &BchTemplate, large: bool) -> bool {
    large || template.transaction_ids().len() <= MAX_DECLARED_TXS
}

/// Knuth's extensions a pool granted this session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Granted {
    /// Requested Payout Outputs (0x5052).
    payouts: bool,
    /// BCH large templates (0x4C54), asked for in Full-Template only.
    large: bool,
}

/// Asks for Knuth's extensions right after the JD setup: what the pool
/// granted, or `None` when it did not answer in time or closed the
/// connection (a pool that does not know the negotiation).
fn negotiate(
    jd: &mut Sender,
    replies: &mut Receiver,
    full: bool,
    wait: Duration,
) -> Result<Option<Granted>, String> {
    let mut requested = vec![PAYOUT_OUTPUTS];
    if full {
        requested.push(LARGE_TEMPLATES);
    }
    jd.send(
        RequestExtensions {
            request_id: 1,
            requested: requested.clone(),
        }
        .frame()?,
    )?;
    let deadline = Instant::now() + wait;
    let mut reply = loop {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        match replies.receive(Duration::from_millis(100)) {
            Ok(Some(frame)) => break frame,
            Ok(None) => (),
            Err(_) => return Ok(None),
        }
    };
    granted_by(&mut reply, &requested).map(Some)
}

// What: the answer to RequestExtensions: Success grants the requested
// extensions it lists; Error grants none, unless it requires one this client
// does not implement, which ends the session; any other answer breaks the
// protocol.
// Why: Knuth's client reads the answer the same way.
// Look here if: a pool's grant is not used, or its refusal ends the session.
/// What `reply` grants of `requested`.
fn granted_by(reply: &mut SerializedFrame, requested: &[u16]) -> Result<Granted, String> {
    let header = reply.header();
    if header.ext_type_without_channel_msg() != NEGOTIATION || header.channel_msg() {
        return Err("the pool answered RequestExtensions with another message".into());
    }
    let supported = match header.msg_type() {
        REQUEST_EXTENSIONS_SUCCESS => {
            RequestExtensionsSuccess::decode(reply.payload())
                .filter(|success| success.request_id == 1)
                .ok_or("malformed RequestExtensions.Success")?
                .supported
        }
        REQUEST_EXTENSIONS_ERROR => {
            let error = RequestExtensionsError::decode(reply.payload())
                .filter(|error| error.request_id == 1)
                .ok_or("malformed RequestExtensions.Error")?;
            if let Some(missing) = error
                .required
                .iter()
                .find(|extension| !requested.contains(extension))
            {
                return Err(format!(
                    "the pool requires SV2 extension 0x{missing:04x}, which this client does not implement"
                ));
            }
            Vec::new()
        }
        _ => return Err("the pool answered RequestExtensions with another message".into()),
    };
    let has = |extension: u16| requested.contains(&extension) && supported.contains(&extension);
    Ok(Granted {
        payouts: has(PAYOUT_OUTPUTS),
        large: has(LARGE_TEMPLATES),
    })
}

/// An allocated token: its bytes, its outputs' serialized size (the
/// reservation a 0x5052 answer must fit), its rates when it is a Pickaxe
/// pool's, and the PayoutOutputsStale notices seen when it arrived.
struct Token {
    bytes: Vec<u8>,
    reservation: usize,
    rates: super::token::PoolRates,
    local_rates: bool,
    epoch: u64,
}

/// A RequestPayoutOutputs in flight.
struct Inflight {
    id: u32,
    token: Vec<u8>,
    reservation: usize,
    epoch: u64,
    sent: Instant,
}

// What: one preparation's payout request (0x5052), served by the uplink: it
// takes a held token (allocating its replacement), sends RequestPayoutOutputs
// with the template's revenue, height, parent, bits and script head, and
// answers the preparation with the coinbase the pool's answer makes
// (`payouts::exact_coinbase`). One request is in flight at a time. A token
// from before a PayoutOutputsStale (an answer or stale-payout-outputs on
// it) asks again with a fresh token, at most 8 times. stale-payout-outputs
// on a fresh token means the pool's node is on another block: that
// preparation fails and is offered again shortly, without a fallback. Any
// other refusal, a malformed answer or no answer within 10 s ends the
// session (the fallback).
// Why: Knuth's client follows these rules (docs/sv2.md, "Job Declaration").
// Look here if: payout requests repeat, or the client falls back on one.
/// A preparation's payout request the uplink serves.
struct PayoutWork {
    ask: PayoutAsk,
    received: Instant,
    restarts: u8,
    inflight: Option<Inflight>,
}

impl PayoutWork {
    fn new(ask: PayoutAsk) -> Self {
        Self {
            ask,
            received: Instant::now(),
            restarts: 0,
            inflight: None,
        }
    }

    /// The request for `token`, now in flight as request `id`.
    fn request(&mut self, id: u32, token: Token) -> Result<SerializedFrame, String> {
        let frame = RequestPayoutOutputs {
            request_id: id,
            token: token.bytes.clone(),
            available_payout_value: self.ask.value,
            height: self.ask.height,
            prev_hash: self.ask.previous_hash,
            nbits: self.ask.bits,
            coinbase_prefix: self.ask.head.clone(),
        }
        .frame()?;
        self.inflight = Some(Inflight {
            id,
            token: token.bytes,
            reservation: token.reservation,
            epoch: token.epoch,
            sent: Instant::now(),
        });
        Ok(frame)
    }

    fn answer(&self, result: Result<Prepared, String>) {
        let _ = self.ask.reply.try_send(result);
    }

    /// Asks again with a fresh token, within the restarts allowed.
    fn restart(&mut self) -> Result<Option<bool>, String> {
        self.restarts += 1;
        if self.restarts > PAYOUT_RESTARTS {
            let why = "the pool's payouts kept changing".to_owned();
            self.answer(Err(why.clone()));
            return Err(why);
        }
        Ok(None)
    }

    /// Takes the pool's answer `frame`; `Some(prepared)` once the
    /// preparation has its answer (`None` while it asks again). `epoch` is
    /// the notices seen so far.
    fn receive(&mut self, frame: &mut SerializedFrame, epoch: u64) -> Result<Option<bool>, String> {
        let inflight = self
            .inflight
            .take()
            .ok_or("the pool answered a payout request never sent")?;
        let stale = inflight.epoch < epoch;
        if frame.header().msg_type() == REQUEST_PAYOUT_OUTPUTS_SUCCESS {
            let success = RequestPayoutOutputsSuccess::decode(frame.payload())
                .filter(|success| success.request_id == inflight.id)
                .ok_or("malformed RequestPayoutOutputs.Success")?;
            if stale {
                return self.restart();
            }
            return match exact_coinbase(&success, &self.ask, inflight.reservation) {
                Ok(coinbase) => {
                    self.answer(Ok(Prepared {
                        token: inflight.token,
                        coinbase,
                    }));
                    Ok(Some(true))
                }
                Err(why) => {
                    self.answer(Err(why.clone()));
                    Err(why)
                }
            };
        }
        let error = RequestPayoutOutputsError::decode(frame.payload())
            .filter(|error| error.request_id == inflight.id)
            .ok_or("malformed RequestPayoutOutputs.Error")?;
        if error.code == STALE_PAYOUT_OUTPUTS {
            if stale {
                return self.restart();
            }
            self.answer(Err(
                "the pool's node is on another block; declaring again shortly".into(),
            ));
            return Ok(Some(false));
        }
        let code: String = error
            .code
            .chars()
            .filter(|c| c.is_ascii_graphic())
            .take(64)
            .collect();
        let why = format!("the pool refused the payout outputs: {code}");
        self.answer(Err(why.clone()));
        Err(why)
    }
}

/// Answers the pool's request for declaration `id`'s transactions at
/// `positions` (`provide`), or drops the declaration when they cannot go.
#[allow(clippy::too_many_arguments)]
fn supply(
    jd: &mut Sender,
    declaring: &mut HashMap<u32, Declaring>,
    cache: &mut HashMap<u64, Vec<ForwardShare>>,
    status: &JdStatus,
    id: u32,
    positions: &[usize],
    large: bool,
) -> Result<(), String> {
    let Some(declaration) = declaring.get_mut(&id) else {
        return Ok(());
    };
    match provide(id, &declaration.template, positions, large) {
        Ok(frame) => {
            jd.send(frame)?;
            declaration.sent = Instant::now();
            status.update(|summary| summary.provided += 1);
        }
        Err(Unprovided::TooLarge) => {
            if let Some(dropped) = declaring.remove(&id) {
                cache.remove(&dropped.serial);
            }
            status.update(|summary| summary.dropped += 1);
        }
        Err(Unprovided::Unknown) => {
            return Err("the pool asked for a transaction the template does not have".into())
        }
    }
    Ok(())
}

/// The pinned Noise session to the pool.
fn connect(target: &JdTarget, limits: Limits) -> Result<(Sender, Receiver), String> {
    let address = target
        .address
        .to_socket_addrs()
        .map_err(|_| "cannot resolve the pool's address")?
        .next()
        .ok_or("cannot resolve the pool's address")?;
    let stream = TcpStream::connect_timeout(&address, ANSWER_TIMEOUT)
        .map_err(|_| "cannot reach the pool")?;
    Ok(Session::initiate_with(stream, target.authority, limits)?.split())
}

fn setup(
    target: &JdTarget,
    sender: &mut Sender,
    receiver: &mut Receiver,
    protocol: Protocol,
    flags: u32,
) -> Result<(), String> {
    let (host, port) = target
        .address
        .rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse::<u16>().ok()?)))
        .ok_or("the pool's address has no port")?;
    let firmware = format!("pickaxe {}", env!("CARGO_PKG_VERSION"));
    sender.send(encoded(
        SetupConnection {
            protocol,
            min_version: 2,
            max_version: 2,
            flags,
            endpoint_host: host.try_into().map_err(|_| "pool host too long")?,
            endpoint_port: port,
            vendor: "Pickaxe JDC".try_into().map_err(|_| "vendor too long")?,
            hardware_version: "".try_into().map_err(|_| "version too long")?,
            firmware: firmware
                .as_str()
                .try_into()
                .map_err(|_| "firmware too long")?,
            device_id: "".try_into().map_err(|_| "device too long")?,
        },
        common::MESSAGE_TYPE_SETUP_CONNECTION,
        false,
    )?)?;
    let stop = AtomicBool::new(false);
    let mut reply = answer(receiver, &stop)?;
    match reply.header().msg_type() {
        common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS => Ok(()),
        common::MESSAGE_TYPE_SETUP_CONNECTION_ERROR => {
            let error: SetupConnectionError =
                binary_sv2::from_bytes(reply.payload()).map_err(|_| "malformed setup error")?;
            Err(format!(
                "the pool refused the setup: {}",
                String::from_utf8_lossy(error.error_code.as_ref())
            ))
        }
        _ => Err("the pool answered the setup with something else".into()),
    }
}

/// The pool's next frame within `ANSWER_TIMEOUT`.
fn answer(receiver: &mut Receiver, stop: &AtomicBool) -> Result<SerializedFrame, String> {
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        if let Some(frame) = receiver.receive(Duration::from_millis(100))? {
            return Ok(frame);
        }
    }
    Err("the pool did not answer".into())
}

fn allocate(login: &str, jd: &mut Sender, requests: &mut u32) -> Result<(), String> {
    *requests = requests.wrapping_add(1);
    jd.send(encoded(
        AllocateMiningJobToken {
            user_identifier: login.try_into().map_err(|_| "identity too long")?,
            request_id: *requests,
        },
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
        false,
    )?)
}

/// A token, and the pool's output scripts in its order. #### 0x5052: with
/// the pool's own outputs (`payouts`) another pool's opaque token serves
/// too.
fn token_for(
    frame: &mut SerializedFrame,
    p2pool: bool,
    payouts: bool,
) -> Result<(Token, Vec<Vec<u8>>), String> {
    if frame.header().msg_type() != MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS {
        return Err("the pool did not allocate a token".into());
    }
    let success: AllocateMiningJobTokenSuccess =
        binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed token")?;
    let bytes = success.mining_job_token.as_ref().to_vec();
    let none = super::token::PoolRates {
        donation_bps: 0,
        fee_bps: 0,
        donation_output: None,
        fee_output: None,
    };
    // A Pickaxe pool's token carries its rates; another pool's is opaque,
    // and its fee and donation cannot be known, so its custom jobs could
    // not pay them, unless the pool names the outputs (0x5052).
    let (rates, local_rates) = if p2pool {
        if bytes.first() != Some(&1) || !(65..=255).contains(&bytes.len()) {
            return Err("unsupported BCH P2Pool token format".into());
        }
        (none, false)
    } else {
        match PxToken::decode(&bytes) {
            Some(token) => (token.rates, true),
            None if payouts => (none, false),
            None => {
                return Err(
                    "the pool's tokens are not a Pickaxe pool's; Job Declaration needs one".into(),
                )
            }
        }
    };
    let scripts = parse_outputs(success.coinbase_outputs.as_ref())?
        .into_iter()
        .map(|(_, script)| script)
        .collect();
    Ok((
        Token {
            bytes,
            reservation: success.coinbase_outputs.as_ref().len(),
            rates,
            local_rates,
            epoch: 0,
        },
        scripts,
    ))
}

/// DeclareMiningJob for `template`: the local jobs' coinbase around the
/// whole extranonce (the pool channel's prefix and the rolled bytes), and
/// the transaction ids in block order. #### 0x4C54: with BCH large
/// templates (`large`), the ids past one SEQ0_64K list go first as
/// DeclareMiningJob.Transactions pages (`extensions::pages`), each in its
/// own frame before the DeclareMiningJob.
fn declaration(
    request: u32,
    token: &[u8],
    template: &BchTemplate,
    large: bool,
) -> Result<Vec<SerializedFrame>, String> {
    let parts = template.declared_parts()?;
    let (paged, own) = if large {
        pages(template.transaction_ids())
    } else {
        (Vec::new(), template.transaction_ids())
    };
    let mut frames = paged
        .into_iter()
        .map(|page| {
            DeclareMiningJobTransactions {
                request_id: request,
                txids: page.to_vec(),
            }
            .frame()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let txids: Vec<binary_sv2::U256> = own.iter().map(Into::into).collect();
    let excess = template
        .declared_aux()?
        .map(|aux| crate::stratum_v2::merge::disclosure::Disclosure::from_job(&aux).encode())
        .unwrap_or_default();
    frames.push(encoded(
        DeclareMiningJob {
            request_id: request,
            mining_job_token: token.try_into().map_err(|_| "token too long")?,
            version: template.version,
            coinbase_tx_prefix: parts
                .prefix
                .as_slice()
                .try_into()
                .map_err(|_| "coinbase prefix too long")?,
            coinbase_tx_suffix: parts
                .suffix
                .as_slice()
                .try_into()
                .map_err(|_| "coinbase suffix too long")?,
            wtxid_list: txids.try_into().map_err(|_| "too many transactions")?,
            // #### merge-mining v2: the token leaves behind output 0, for
            // the pool to check (`merge::disclosure`); none without tokens.
            excess_data: excess
                .as_slice()
                .try_into()
                .map_err(|_| "excess data too long")?,
        },
        MESSAGE_TYPE_DECLARE_MINING_JOB,
        false,
    )?);
    Ok(frames)
}

/// Why missing transactions cannot be provided.
#[derive(Debug, PartialEq, Eq)]
enum Unprovided {
    /// A position the template does not have.
    Unknown,
    /// More than one frame takes.
    TooLarge,
}

/// ProvideMissingTransactions.Success for `positions` of `template`, in
/// one frame. #### 0x4C54: with BCH large templates (`large`), when they do
/// not all fit, the longest prefix that does goes, and the pool asks again
/// for the rest (a BCH transaction is at most 1,000,000 bytes, so it holds
/// one at least).
fn provide(
    request: u32,
    template: &BchTemplate,
    positions: &[usize],
    large: bool,
) -> Result<SerializedFrame, Unprovided> {
    let transactions = template.transactions();
    // The header, the request id and the count, then each transaction with
    // its 3-byte length.
    let mut size = 6 + 4 + 2;
    let mut list = Vec::with_capacity(positions.len());
    for position in positions {
        let tx = transactions.get(*position).ok_or(Unprovided::Unknown)?;
        if size + 3 + tx.len() > Limits::JD.max_out {
            if large && !list.is_empty() {
                break;
            }
            return Err(Unprovided::TooLarge);
        }
        size += 3 + tx.len();
        list.push(binary_sv2::B016M::try_from(&tx[..]).map_err(|_| Unprovided::TooLarge)?);
    }
    encoded(
        ProvideMissingTransactionsSuccess {
            request_id: request,
            transaction_list: list.try_into().map_err(|_| Unprovided::TooLarge)?,
        },
        MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS_SUCCESS,
        false,
    )
    .map_err(|_| Unprovided::TooLarge)
}

/// PushSolution for a local block on `template`: the whole extranonce is
/// the pool channel's prefix and the share's rolled bytes.
fn push_solution(
    plan: &JdPlan,
    template: &BchTemplate,
    share: &ForwardShare,
) -> Result<SerializedFrame, String> {
    let mut extranonce = plan.upstream_prefix.clone();
    extranonce.extend_from_slice(&share.extranonce);
    encoded(
        PushSolution {
            extranonce: extranonce
                .as_slice()
                .try_into()
                .map_err(|_| "extranonce too long")?,
            prev_hash: (&template.previous_hash).into(),
            nonce: share.nonce,
            ntime: share.ntime,
            nbits: template.bits,
            version: share.version,
        },
        MESSAGE_TYPE_PUSH_SOLUTION,
        false,
    )
}

/// SetCustomMiningJob for `template`: the local jobs' coinbase around the
/// pool channel's prefix and the 16 rolled bytes, with the merge-mining
/// outputs when tokens are merge-mined.
fn declare(
    pool: &mut Sender,
    channel: u32,
    request: u32,
    token: &[u8],
    template: &BchTemplate,
) -> Result<(), String> {
    let head = template.script_head();
    // #### PR #42: the outputs of the template's own plan (the session's:
    // templates on another plan are never declared), commitment and tickets
    // included (`BchTemplate::declared_outputs`).
    let outputs = serialize_outputs(&template.declared_outputs()?);
    let path: Vec<binary_sv2::U256> = template.merkle_path().iter().map(Into::into).collect();
    // A prepared coinbase (P2Pool's, or the pool's payout outputs) carries
    // its own version, sequence and lock time; the local one is 2,
    // 0xffffffff and 0.
    let (version, sequence, locktime) = template
        .prepared_coinbase()
        .map_or((2, u32::MAX, 0), |shape| {
            (shape.tx_version, shape.sequence, shape.locktime)
        });
    pool.send(mining(Mining::SetCustomMiningJob(SetCustomMiningJob {
        channel_id: channel,
        request_id: request,
        token: token.try_into().map_err(|_| "token too long")?,
        version: template.version,
        prev_hash: (&template.previous_hash).into(),
        ntime_start: template.current_time,
        nbits: template.bits,
        coinbase_tx_version: version,
        coinbase_prefix: head.as_slice().try_into().map_err(|_| "prefix too long")?,
        coinbase_tx_input_n_sequence: sequence,
        coinbase_tx_outputs: outputs
            .as_slice()
            .try_into()
            .map_err(|_| "outputs too long")?,
        coinbase_tx_locktime: locktime,
        merkle_path: path.try_into().map_err(|_| "merkle path too long")?,
    }))?)
}

// #### merge-mining v2: only the pool's shares go to the pool
// What: a local share goes to the pool only when its hash meets the pool's
// current share target. A share that meets only a merge-mined token's
// target (the local device target is eased toward it, `Channel::settle`) is
// a token-only hit: it stays local, journaled and claimed by this server,
// and is never sent as a share.
// Why: the pool credits shares at its own target; token-only hits are work
// the pool did not ask for, and sending them would leak the local token
// easing upstream.
// Look here if: the pool rejects low-difficulty shares while tokens are
// merge-mined, or a token win reaches the pool as a share.
/// Whether `share` is one for the pool: its hash meets the pool's target.
pub(crate) fn for_pool(share: &ForwardShare, pool_target: &Hash) -> bool {
    meets_target(&share.hash, pool_target)
}

// #### PR #42: shares forwarded to the pool without blocking
// What: an accepted local share on a declared job that meets the pool's
// target goes to the pool with the pool's job id and the 16 rolled bytes;
// until the pool confirms the job, up to 256 wait. The local server hands
// shares over with try_send, so a slow pool never delays a device.
// Why: the pool credits the miner's work by these shares.
// Look here if: the pool counts fewer shares than the local server.
fn forward(
    pool: &mut Sender,
    channel: u32,
    job: u32,
    sequence: &mut u32,
    share: ForwardShare,
    status: &JdStatus,
) -> Result<(), String> {
    *sequence = sequence.wrapping_add(1);
    pool.send(mining(Mining::SubmitSharesExtended(
        SubmitSharesExtended {
            channel_id: channel,
            sequence_number: *sequence,
            job_id: job,
            nonce: share.nonce,
            ntime: share.ntime,
            version: share.version,
            extranonce: share
                .extranonce
                .as_slice()
                .try_into()
                .map_err(|_| "extranonce too long")?,
        },
    ))?)?;
    status.update(|summary| summary.forwarded += 1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### merge-mining v2: the miner's donation at a Pickaxe pool
    #[test]
    fn a_login_names_the_miners_donation_to_the_step_below() {
        use crate::donation::bch::BchDonation;
        let target = |donation: Option<u16>| JdTarget {
            address: String::new(),
            authority: [0; 32],
            identity: "bchtest:qq.rig".into(),
            retry: Duration::from_secs(1),
            mode: JdMode::FullTemplate,
            p2pool_prepare_api: None,
            required: false,
            pickaxe_donation: donation
                .map(|bps| Arc::new(RwLock::new(BchDonation::try_from(bps).unwrap()))),
        };
        assert_eq!(target(None).login(), "bchtest:qq.rig");
        assert_eq!(
            target(Some(150)).login(),
            "bchtest:qq.rig,pickaxe-donation=1.50"
        );
        assert_eq!(
            target(Some(75)).login(),
            "bchtest:qq.rig,pickaxe-donation=0.50"
        );
        assert_eq!(
            target(Some(0)).login(),
            "bchtest:qq.rig,pickaxe-donation=0.00"
        );
        // The pool reads it back as the donation.
        let login = crate::stratum_v2::payout::identity_login(
            crate::config::MiningNetwork::Chipnet,
            &format!(
                "{},pickaxe-donation=1.50",
                crate::stratum_v2::template_tests::payout()
            ),
        )
        .unwrap();
        assert_eq!(login.donation, Some("1.5".parse().unwrap()));
        // Only a refusal of the login drops the option.
        let refusal = |code: &'static str| {
            mining(Mining::OpenMiningChannelError(OpenMiningChannelError {
                request_id: 1,
                error_code: code.try_into().unwrap(),
            }))
            .unwrap()
        };
        assert!(refuses_login(&mut refusal("unknown-user")));
        assert!(refuses_login(&mut refusal("invalid-donation")));
        assert!(!refuses_login(&mut refusal("no-template-available")));
        assert!(!refuses_login(&mut refusal("max-target-out-of-range")));
    }
    use crate::stratum_v2::template_tests::rpc_template;

    fn template() -> BchTemplate {
        BchTemplate::from_rpc(&rpc_template()).unwrap()
    }

    #[test]
    fn paired_declarations_are_all_admitted_or_all_cancelled_and_retry_the_same_id() {
        for (personal_full, donation_full, disconnected) in [
            (true, false, None),
            (false, true, None),
            (false, false, Some(false)),
            (false, false, Some(true)),
        ] {
            let status = || {
                Arc::new(JdStatus {
                    plan: RwLock::new(None),
                    summary: Mutex::default(),
                    acknowledged: AtomicU64::new(0),
                })
            };
            let (personal_events, personal_receive) = mpsc::sync_channel(1);
            let (donation_events, donation_receive) = mpsc::sync_channel(1);
            let donation = Arc::new(UplinkHandle {
                events: donation_events,
                status: status(),
                donation: None,
            });
            let handle = UplinkHandle {
                events: personal_events,
                status: status(),
                donation: Some(donation.clone()),
            };
            let own = Arc::new(template());
            let mut personal = template();
            personal.set_p2pool_donation((*own).clone());
            let personal = Arc::new(personal);
            if personal_full {
                handle
                    .events
                    .send(UplinkEvent::published(0, personal.clone()))
                    .unwrap();
            }
            if donation_full {
                donation
                    .events
                    .send(UplinkEvent::published(0, own.clone()))
                    .unwrap();
            }
            let personal_receive = (disconnected != Some(false)).then_some(personal_receive);
            let donation_receive = (disconnected != Some(true)).then_some(donation_receive);
            assert!(handle.publish(9, personal.clone()).is_err());
            for receive in [&personal_receive, &donation_receive].into_iter().flatten() {
                while let Ok(event) = receive.try_recv() {
                    let UplinkEvent::Published {
                        serial, admission, ..
                    } = event
                    else {
                        panic!("unexpected share");
                    };
                    if serial == 9 {
                        assert!(
                            !publication_admitted(admission),
                            "an unmatched declaration must be discarded before touching the pool"
                        );
                    }
                }
            }
            assert_eq!(handle.acknowledged(), 0);
            if disconnected.is_some() {
                continue;
            }
            handle.publish(9, personal.clone()).unwrap();
            for (receive, expected) in [
                (personal_receive.unwrap(), &personal),
                (
                    donation_receive.unwrap(),
                    personal.p2pool_donation().unwrap(),
                ),
            ] {
                match receive.try_recv().unwrap() {
                    UplinkEvent::Published {
                        serial,
                        template,
                        admission,
                    } => {
                        assert_eq!(serial, 9, "queue failures must allow the same ID to retry");
                        assert!(Arc::ptr_eq(&template, expected));
                        assert!(publication_admitted(admission));
                    }
                    _ => panic!("expected matching declarations"),
                }
            }
        }
    }

    #[test]
    fn dequeued_publication_cannot_reach_the_pool_until_pair_admission_finishes() {
        for committed in [false, true] {
            let admission = Arc::new(Mutex::new(false));
            let mut gate = admission.lock().unwrap();
            let receive_gate = admission.clone();
            let (started, start) = mpsc::channel();
            let (finished, finish) = mpsc::channel();
            let receiver = thread::spawn(move || {
                started.send(()).unwrap();
                finished
                    .send(publication_admitted(Some(receive_gate)))
                    .unwrap();
            });
            start.recv().unwrap();
            assert!(matches!(finish.try_recv(), Err(mpsc::TryRecvError::Empty)));
            *gate = committed;
            drop(gate);
            assert_eq!(finish.recv().unwrap(), committed);
            receiver.join().unwrap();
        }
    }

    // #### PR #65
    // What: the personal session's acknowledgement stands on its own; the
    // donation session's counts only while it holds a plan, and a
    // reconnecting donation session revokes only donation readiness. Block
    // and donation shares keep their route.
    // Look here if: UplinkHandle::acknowledged or donation_acknowledged
    // changes.
    #[test]
    fn each_recipient_acknowledges_its_own_work_and_block_only_donation_work_keeps_its_route() {
        let status = || {
            Arc::new(JdStatus {
                plan: RwLock::new(None),
                summary: Mutex::new(JdClientSummary::default()),
                acknowledged: AtomicU64::new(0),
            })
        };
        let (personal_events, personal) = mpsc::sync_channel(2);
        let (donation_events, donation) = mpsc::sync_channel(2);
        let donor = Arc::new(UplinkHandle {
            events: donation_events,
            status: status(),
            donation: None,
        });
        let handle = UplinkHandle {
            events: personal_events,
            status: status(),
            donation: Some(donor.clone()),
        };
        handle.status.acknowledged.store(7, Ordering::Relaxed);
        assert_eq!(handle.acknowledged(), 7, "a down donation never gates");
        assert_eq!(handle.donation_acknowledged(), None);
        donor.status.set_plan(Some(Arc::new(JdPlan {
            serial: 4,
            upstream_prefix: vec![0; 4],
            pad: Vec::new(),
            compact: true,
            scripts: Vec::new(),
            rates: super::super::token::PoolRates {
                donation_bps: 0,
                fee_bps: 0,
                donation_output: None,
                fee_output: None,
            },
            pool_target: [255; 32],
            p2pool: None,
            payouts: None,
            full_template: true,
        })));
        donor.status.acknowledged.store(6, Ordering::Relaxed);
        assert_eq!(handle.acknowledged(), 7);
        assert_eq!(handle.donation_acknowledged(), Some((4, 6)));
        let share = ForwardShare {
            donation: true,
            serial: 7,
            version: 2,
            ntime: 1,
            nonce: 2,
            extranonce: vec![0; 8],
            hash: [0; 32],
            block: true,
        };
        handle.forward(share.clone());
        assert!(personal.try_recv().is_err());
        match donation.try_recv().unwrap() {
            UplinkEvent::Share(found) => assert_eq!(found, share),
            _ => panic!("share route"),
        };
        donor.status.set_plan(None);
        assert_eq!(
            handle.donation_acknowledged(),
            None,
            "a reconnecting donation session revokes donation readiness"
        );
        assert_eq!(handle.acknowledged(), 7, "and only that");
        let personal_share = ForwardShare {
            donation: false,
            ..share
        };
        handle.forward(personal_share.clone());
        match personal.try_recv().unwrap() {
            UplinkEvent::Share(found) => assert_eq!(found, personal_share),
            _ => panic!("share route"),
        };
        assert!(donation.try_recv().is_err());
    }

    // #### PR #65
    // What: at 0% the donation session never connects (only the personal
    // one does) and shows "off"; raised above 0% it connects at once.
    // Look here if: spawn_p2pool's gate changes.
    #[test]
    fn the_donation_session_connects_only_above_zero_percent() {
        use crate::donation::bch::BchDonation;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicU64::new(0));
        let count = accepted.clone();
        thread::spawn(move || {
            // Silent: each handshake waits for an answer that never comes.
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                count.fetch_add(1, Ordering::SeqCst);
                held.push(stream);
            }
        });
        let rate = Arc::new(RwLock::new(BchDonation::try_from(0).unwrap()));
        let stop = Arc::new(AtomicBool::new(false));
        let target = JdTarget {
            address: address.to_string(),
            authority: crate::stratum_v2::server::authority_public(&[7; 32]).unwrap(),
            identity: "personal,p2p=owner".into(),
            retry: Duration::from_secs(30),
            mode: JdMode::CoinbaseOnly,
            p2pool_prepare_api: Some("http://127.0.0.1:1/jd_prepare".into()),
            required: true,
            pickaxe_donation: None,
        };
        let (handle, _worker) = spawn_p2pool(
            vec![target],
            "donation,p2p=owner".into(),
            rate.clone(),
            stop.clone(),
        );
        let reach = |connections: u64| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while accepted.load(Ordering::SeqCst) < connections {
                assert!(Instant::now() < deadline, "{connections} connections");
                thread::sleep(Duration::from_millis(10));
            }
        };
        reach(1);
        thread::sleep(Duration::from_millis(600));
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "no donation session at 0%"
        );
        let donor = handle.donation.as_ref().unwrap();
        assert_eq!(donor.status.summary.lock().unwrap().state, "off");
        assert!(donor.status.plan.read().unwrap().is_none());
        *rate.write().unwrap() = BchDonation::default();
        reach(2);
        stop.store(true, Ordering::Relaxed);
    }

    // #### PR #65
    // What: a template without a donation job is declared on the personal
    // session alone, even where a donation session exists.
    // Look here if: UplinkHandle::publish changes.
    #[test]
    fn a_template_without_a_donation_job_is_declared_on_the_personal_session_alone() {
        let status = || {
            Arc::new(JdStatus {
                plan: RwLock::new(None),
                summary: Mutex::default(),
                acknowledged: AtomicU64::new(0),
            })
        };
        let (personal_events, personal) = mpsc::sync_channel(1);
        let (donation_events, donation) = mpsc::sync_channel(1);
        let handle = UplinkHandle {
            events: personal_events,
            status: status(),
            donation: Some(Arc::new(UplinkHandle {
                events: donation_events,
                status: status(),
                donation: None,
            })),
        };
        handle.publish(3, Arc::new(template())).unwrap();
        match personal.try_recv().unwrap() {
            UplinkEvent::Published {
                serial, admission, ..
            } => {
                assert_eq!(serial, 3);
                assert!(admission.is_none());
            }
            UplinkEvent::Share(_) => panic!("expected a declaration"),
        }
        assert!(donation.try_recv().is_err());
    }

    /// Reads only a locally supplied private config; does not hash or broadcast.
    #[test]
    #[ignore = "requires an isolated updated P2Pool node and a synced BCHN on Chipnet"]
    fn live_p2pool_prepares_both_recipients_on_own_bchn_templates() {
        use crate::config::MiningNetwork;
        use crate::stratum_v2::provider::{NativeNodeRpc, TemplateProvider};
        let path =
            std::env::var("PICKAXE_P2POOL_JD_TEST_CONFIG").expect("private test config path");
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("private test config"))
                .expect("valid test config");
        let authority: [u8; 32] =
            serde_json::from_value(config["authority"].clone()).expect("public authority");
        let payout = config["payout"].as_str().expect("payout");
        let share = crate::tx::p2pool_share_address(payout).expect("Chipnet payout");
        let mut source = TemplateProvider::new(
            NativeNodeRpc::new(config["node"].as_str().expect("saved RPC").to_owned()),
            MiningNetwork::Chipnet,
        );
        let (_, original) = source.refresh().expect("synced Chipnet full template");
        let original = original.clone();
        let donation = crate::donation::bch::address(MiningNetwork::Chipnet);
        let donation_share = crate::tx::p2pool_share_address(donation).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let _stop = Stop(stop.clone());
        for mode in [JdMode::FullTemplate, JdMode::CoinbaseOnly] {
            let target = JdTarget {
                address: config["pool"].as_str().unwrap().into(),
                authority,
                identity: format!("{payout},p2p={share}"),
                retry: Duration::from_secs(2),
                mode,
                p2pool_prepare_api: Some(config["api"].as_str().unwrap().into()),
                required: true,
                pickaxe_donation: None,
            };
            let session_stop = Arc::new(AtomicBool::new(false));
            let guard = Stop(session_stop.clone());
            let (handles, worker) = spawn_p2pool(
                vec![target],
                format!("{donation},p2p={donation_share}"),
                Arc::new(RwLock::new(crate::donation::bch::BchDonation::default())),
                session_stop.clone(),
            );
            let donor = handles.donation.as_ref().unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let (personal_plan, donation_plan) = loop {
                let personal = handles.status.plan.read().unwrap().clone();
                let donation = donor.status.plan.read().unwrap().clone();
                if let (Some(personal), Some(donation)) = (personal, donation) {
                    break (personal, donation);
                }
                assert!(
                    Instant::now() < deadline,
                    "both native P2Pool channels must authorize"
                );
                thread::sleep(Duration::from_millis(20));
            };
            let mut personal = original.clone();
            personal.tag(b"Pickaxe JD test");
            personal.declare(personal_plan);
            personal
                .prepare_p2pool()
                .expect("personal PPLNS bytes must prepare");
            let mut donation = original.clone();
            donation.tag(b"Pickaxe JD test");
            donation.declare(donation_plan);
            donation
                .prepare_p2pool()
                .expect("donation PPLNS bytes must prepare");
            assert_eq!(personal.transaction_ids(), donation.transaction_ids());
            assert_ne!(
                personal.declared_outputs().unwrap(),
                donation.declared_outputs().unwrap(),
                "share ownership must be committed to the recipient"
            );
            personal.set_p2pool_donation(donation);
            handles.publish(1, Arc::new(personal)).unwrap();
            while handles.acknowledged() < 1
                || handles
                    .donation_acknowledged()
                    .is_none_or(|(_, acknowledged)| acknowledged < 1)
            {
                assert!(
                    Instant::now() < deadline,
                    "BCHN-validated custom jobs must both be acknowledged"
                );
                thread::sleep(Duration::from_millis(20));
            }
            let personal = handles.status.summary.lock().unwrap().clone();
            let donation = donor.status.summary.lock().unwrap().clone();
            assert_eq!((personal.refused, donation.refused), (0, 0));
            assert_eq!((personal.custom_jobs, donation.custom_jobs), (1, 1));
            println!("mode={} Chipnet height={} personal_custom_jobs={} donation_custom_jobs={} rejected=0",mode.as_str(),original.height,personal.custom_jobs,donation.custom_jobs);
            drop(guard);
            worker.join().unwrap();
        }
    }

    // #### PR #42
    // What: a template over 65,535 transactions is not declared; one at
    // the limit is.
    // Look here if: declarable or MAX_DECLARED_TXS changes.
    #[test]
    fn templates_over_65535_transactions_are_not_declared() {
        let at_limit = template().with_transactions(vec![vec![0]; MAX_DECLARED_TXS]);
        assert!(declarable(&at_limit, false));
        let over = template().with_transactions(vec![vec![0]; MAX_DECLARED_TXS + 1]);
        assert!(!declarable(&over, false));
        // #### 0x4C54: a pool that takes pages takes it.
        assert!(declarable(&over, true));
    }

    // #### PR #42
    // What: missing transactions go back in one frame of at most 16 MiB in
    // the order asked for; more than one frame takes is TooLarge (the
    // declaration is dropped), and a position the template does not have
    // is Unknown (the pool is broken).
    // Look here if: provide or Limits::JD changes.
    #[test]
    fn a_missing_transactions_request_that_cannot_fit_one_frame_drops_the_declaration() {
        let big = template().with_transactions(vec![vec![1; 9_000_000], vec![2; 9_000_000]]);
        let mut frame = provide(3, &big, &[1], false).unwrap();
        let reply: ProvideMissingTransactionsSuccess =
            binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(reply.request_id, 3);
        assert_eq!(reply.transaction_list.len(), 1);
        assert_eq!(reply.transaction_list[0].as_ref()[0], 2);
        assert_eq!(
            provide(3, &big, &[0, 1], false).err(),
            Some(Unprovided::TooLarge)
        );
        assert_eq!(
            provide(3, &big, &[2], false).err(),
            Some(Unprovided::Unknown)
        );
        // #### 0x4C54: with BCH large templates the first that fits goes,
        // and the pool asks again for the rest (Knuth's supply).
        let mut frame = provide(3, &big, &[1, 0], true).unwrap();
        let reply: ProvideMissingTransactionsSuccess =
            binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(reply.transaction_list.len(), 1);
        assert_eq!(reply.transaction_list[0].as_ref()[0], 2);
        let huge = template().with_transactions(vec![vec![1; 17_000_000]]);
        assert_eq!(
            provide(3, &huge, &[0], true).err(),
            Some(Unprovided::TooLarge),
            "a transaction over one frame cannot go even with pages"
        );
    }

    // #### 0x4C54
    // What: with BCH large templates a template of 65,537 transactions is
    // declared as one DeclareMiningJob.Transactions page of the first 65,535
    // ids, then the DeclareMiningJob with the same request id and the last 2;
    // without the grant (or at 65,535) there is no page.
    // Look here if: `declaration` or `extensions::pages` changes.
    #[test]
    fn a_large_template_is_declared_in_pages_before_its_declaration() {
        use crate::stratum_v2::extensions::{DECLARE_MINING_JOB_TRANSACTIONS, SEQ_MAX};
        let mut large = template().with_transactions(vec![vec![0]; SEQ_MAX + 2]);
        large.declare(Arc::new(JdPlan {
            serial: 1,
            upstream_prefix: vec![9; 4],
            pad: Vec::new(),
            compact: false,
            scripts: vec![vec![0x51]],
            rates: super::super::token::PoolRates {
                donation_bps: 0,
                fee_bps: 0,
                donation_output: None,
                fee_output: None,
            },
            pool_target: [0xff; 32],
            p2pool: None,
            payouts: None,
            full_template: true,
        }));
        let mut frames = declaration(7, &[1; 26], &large, true).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].header().ext_type(), LARGE_TEMPLATES);
        assert_eq!(
            frames[0].header().msg_type(),
            DECLARE_MINING_JOB_TRANSACTIONS
        );
        let page = DeclareMiningJobTransactions::decode(frames[0].payload()).unwrap();
        assert_eq!(page.request_id, 7);
        assert_eq!(page.txids, large.transaction_ids()[..SEQ_MAX]);
        assert_eq!(frames[1].header().ext_type(), 0);
        let declared: DeclareMiningJob = binary_sv2::from_bytes(frames[1].payload()).unwrap();
        assert_eq!(declared.request_id, 7);
        let own: Vec<Hash> = declared
            .wtxid_list
            .iter()
            .map(|txid| Hash::try_from(txid.as_ref()).unwrap())
            .collect();
        assert_eq!(own, large.transaction_ids()[SEQ_MAX..]);
        let mut fits = template().with_transactions(vec![vec![0]; SEQ_MAX]);
        fits.declare(large.jd_plan().unwrap().clone());
        assert_eq!(declaration(8, &[1; 26], &fits, true).unwrap().len(), 1);
    }

    // #### 0x5052, 0x4C54
    // What: the answer to RequestExtensions: Success grants what it lists of
    // what was asked; Error with nothing required grants nothing; Error
    // requiring an extension this client lacks ends the session; any other
    // answer, or one for another request id, breaks the protocol.
    // Look here if: `granted_by` changes.
    #[test]
    fn the_negotiation_answer_decides_the_extensions() {
        let asked = [PAYOUT_OUTPUTS, LARGE_TEMPLATES];
        let success = |supported: Vec<u16>| {
            RequestExtensionsSuccess {
                request_id: 1,
                supported,
            }
            .frame()
            .unwrap()
        };
        assert_eq!(
            granted_by(&mut success(vec![LARGE_TEMPLATES, PAYOUT_OUTPUTS]), &asked),
            Ok(Granted {
                payouts: true,
                large: true
            })
        );
        assert_eq!(
            granted_by(
                &mut success(vec![PAYOUT_OUTPUTS, 0x0002]),
                &[PAYOUT_OUTPUTS]
            ),
            Ok(Granted {
                payouts: true,
                large: false
            })
        );
        assert_eq!(
            granted_by(&mut success(vec![LARGE_TEMPLATES]), &[PAYOUT_OUTPUTS]),
            Ok(Granted::default()),
            "a grant never asked for is not taken"
        );
        let error = |required: Vec<u16>| {
            RequestExtensionsError {
                request_id: 1,
                unsupported: asked.to_vec(),
                required,
            }
            .frame()
            .unwrap()
        };
        assert_eq!(
            granted_by(&mut error(vec![]), &asked),
            Ok(Granted::default())
        );
        assert!(granted_by(&mut error(vec![0x0003]), &asked)
            .unwrap_err()
            .contains("0x0003"));
        let other = RequestExtensionsSuccess {
            request_id: 2,
            supported: vec![PAYOUT_OUTPUTS],
        };
        assert!(granted_by(&mut other.frame().unwrap(), &asked).is_err());
        let core = encoded(
            AllocateMiningJobToken {
                user_identifier: "x".try_into().unwrap(),
                request_id: 1,
            },
            MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
            false,
        )
        .unwrap();
        let mut core = core;
        assert!(granted_by(&mut core, &asked).is_err());
    }

    // #### 0x5052, 0x4C54
    // What: over a Noise session, the client's RequestExtensions asks for
    // 0x5052 and, in Full-Template, 0x4C54 (request id 1); a grant turns them
    // on, a refusal leaves them off, and a pool that closes the connection
    // or stays silent past the wait gives `None` (the uplink then sets up a
    // new JD connection without asking).
    // Look here if: `negotiate` changes.
    #[test]
    fn the_client_negotiates_and_takes_silence_as_no_answer() {
        let secret = [7; 32];
        let public = crate::stratum_v2::server::authority_public(&secret).unwrap();
        for (case, full) in [
            ("granted", true),
            ("granted", false),
            ("refused", true),
            ("closed", true),
            ("silent", true),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let (mut send, mut receive) =
                    Session::accept_with(stream, &public, &secret, Limits::JD)
                        .unwrap()
                        .split();
                let mut ask = loop {
                    if let Some(frame) = receive.receive(Duration::from_millis(50)).unwrap() {
                        break frame;
                    }
                };
                assert_eq!(ask.header().ext_type(), NEGOTIATION);
                let request = RequestExtensions::decode(ask.payload()).unwrap();
                match case {
                    "granted" => send
                        .send(
                            RequestExtensionsSuccess {
                                request_id: request.request_id,
                                supported: request.requested.clone(),
                            }
                            .frame()
                            .unwrap(),
                        )
                        .unwrap(),
                    "refused" => send
                        .send(
                            RequestExtensionsError {
                                request_id: request.request_id,
                                unsupported: request.requested.clone(),
                                required: Vec::new(),
                            }
                            .frame()
                            .unwrap(),
                        )
                        .unwrap(),
                    "closed" => {
                        send.close();
                        receive.close();
                    }
                    _ => thread::sleep(Duration::from_millis(600)),
                }
                request
            });
            let stream = TcpStream::connect(address).unwrap();
            let (mut jd, mut replies) = Session::initiate_with(stream, public, Limits::JD)
                .unwrap()
                .split();
            let granted =
                negotiate(&mut jd, &mut replies, full, Duration::from_millis(300)).unwrap();
            let request = server.join().unwrap();
            assert_eq!(request.request_id, 1);
            let expected: &[u16] = if full {
                &[PAYOUT_OUTPUTS, LARGE_TEMPLATES]
            } else {
                &[PAYOUT_OUTPUTS]
            };
            assert_eq!(request.requested, expected, "{case}");
            assert_eq!(
                granted,
                match case {
                    "granted" => Some(Granted {
                        payouts: true,
                        large: full
                    }),
                    "refused" => Some(Granted::default()),
                    _ => None,
                },
                "{case}"
            );
        }
    }

    // #### 0x5052
    // What: the uplink's payout request for a preparation: it goes as
    // RequestPayoutOutputs with the token and the template's revenue,
    // height, parent, bits and head; the pool's exact answer answers the
    // preparation with that token and coinbase; an answer on a token from
    // before a notice asks again (up to 8 times, then the session ends);
    // stale-payout-outputs on a fresh token fails only the preparation; any
    // other code fails both.
    // Look here if: `PayoutWork` changes.
    #[test]
    fn a_payout_request_answers_its_preparation() {
        let (door, asks) = Payouts::new(true);
        let preparation = thread::spawn(move || {
            door.prepare(
                101,
                0x207f_ffff,
                [3; 32],
                1_000,
                vec![0x01, 0x65],
                16,
                1_700_000_000,
            )
        });
        let ask = asks.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut work = PayoutWork::new(ask);
        let token = |epoch: u64| Token {
            bytes: vec![0xaa; 26],
            reservation: 64,
            rates: super::super::token::PoolRates {
                donation_bps: 0,
                fee_bps: 0,
                donation_output: None,
                fee_output: None,
            },
            local_rates: true,
            epoch,
        };
        let mut sent = work.request(4, token(0)).unwrap();
        assert_eq!(sent.header().ext_type(), PAYOUT_OUTPUTS);
        assert_eq!(
            RequestPayoutOutputs::decode(sent.payload()).unwrap(),
            RequestPayoutOutputs {
                request_id: 4,
                token: vec![0xaa; 26],
                available_payout_value: 1_000,
                height: 101,
                prev_hash: [3; 32],
                nbits: 0x207f_ffff,
                coinbase_prefix: vec![0x01, 0x65],
            }
        );
        let outputs = serialize_outputs(&[(1_000, vec![0x51])]);
        let answer = |id: u32| {
            RequestPayoutOutputsSuccess {
                request_id: id,
                flags: crate::stratum_v2::extensions::EXACT_OUTPUTS,
                version: 2,
                sequence: u32::MAX,
                locktime: 0,
                outputs: outputs.clone(),
            }
            .frame()
            .unwrap()
        };
        // A notice came since the token: the answer is consumed and asked
        // again on a fresh token.
        assert_eq!(work.receive(&mut answer(4), 1), Ok(None));
        assert!(work.inflight.is_none());
        work.request(5, token(1)).unwrap();
        assert!(work.receive(&mut answer(4), 1).is_err(), "another request");
        work.request(6, token(1)).unwrap();
        assert_eq!(work.receive(&mut answer(6), 1), Ok(Some(true)));
        let prepared = preparation.join().unwrap().unwrap();
        assert_eq!(prepared.token, vec![0xaa; 26]);
        assert_eq!(prepared.coinbase.outputs, vec![(1_000, vec![0x51])]);
        assert_eq!(prepared.coinbase.head, [0x01, 0x65]);
        // stale-payout-outputs: on a fresh token only the preparation
        // fails; any other code ends the session.
        let error = |id: u32, code: &str| {
            RequestPayoutOutputsError {
                request_id: id,
                code: code.into(),
            }
            .frame()
            .unwrap()
        };
        let (reply, answered) = mpsc::sync_channel(1);
        let ask = |reply| PayoutAsk {
            height: 101,
            bits: 0x207f_ffff,
            previous_hash: [3; 32],
            value: 1_000,
            head: vec![0x01, 0x65],
            extranonce: 16,
            min_time: 1_700_000_000,
            reply,
        };
        let mut work = PayoutWork::new(ask(reply));
        work.request(7, token(1)).unwrap();
        assert_eq!(
            work.receive(&mut error(7, STALE_PAYOUT_OUTPUTS), 1),
            Ok(Some(false))
        );
        assert!(answered.try_recv().unwrap().is_err());
        let (reply, answered) = mpsc::sync_channel(1);
        let mut work = PayoutWork::new(ask(reply));
        work.request(8, token(1)).unwrap();
        assert!(work
            .receive(&mut error(8, "invalid-mining-job-token"), 1)
            .unwrap_err()
            .contains("invalid-mining-job-token"));
        assert!(answered.try_recv().unwrap().is_err());
        // Eight restarts at most.
        let (reply, _answered) = mpsc::sync_channel(1);
        let mut work = PayoutWork::new(ask(reply));
        for restart in 0..=PAYOUT_RESTARTS {
            work.request(9, token(0)).unwrap();
            let result = work.receive(&mut error(9, STALE_PAYOUT_OUTPUTS), 1);
            assert_eq!(result.is_err(), restart == PAYOUT_RESTARTS, "{restart}");
        }
    }

    // #### PR #42
    // What: retries wait 30, 60, 120, then 300 seconds as failures follow
    // each other.
    // Look here if: retry_after or RETRY_STEPS changes.
    #[test]
    fn retries_back_off_across_pools() {
        let base = Duration::from_secs(30);
        let waits: Vec<u64> = (1..=6)
            .map(|failures| retry_after(base, failures).as_secs())
            .collect();
        assert_eq!(waits, [30, 60, 120, 300, 300, 300]);
    }

    // #### PR #42
    // What: the fifth rejection among the last 20 verdicts falls back;
    // accepted shares push old rejections out of the window.
    // Look here if: Verdicts changes.
    #[test]
    fn rejections_on_active_jobs_fall_back_after_5_of_20() {
        let mut verdicts = Verdicts::default();
        for _ in 0..4 {
            assert!(!verdicts.record(false, 1));
        }
        assert!(verdicts.record(false, 1), "the fifth of 5");
        let mut spread = Verdicts::default();
        for _ in 0..4 {
            assert!(!spread.record(false, 1));
            assert!(!spread.record(true, 4));
        }
        assert!(
            !spread.record(true, 1),
            "the oldest rejection left the window"
        );
        assert!(!spread.record(false, 1), "4 of the last 20");
        assert!(spread.record(false, 1), "5 of the last 20");
    }

    // #### PR #42
    // What: the fourth refusal in a row on one parent falls back; a new
    // parent starts the count again.
    // Look here if: Refusals changes.
    #[test]
    fn four_refusals_on_one_template_fall_back() {
        let mut refusals = Refusals::default();
        assert!(!refusals.refuse([1; 32]));
        assert!(!refusals.refuse([1; 32]));
        assert!(!refusals.refuse([1; 32]));
        assert!(!refusals.refuse([2; 32]), "a new parent");
        assert!(!refusals.refuse([2; 32]));
        assert!(!refusals.refuse([2; 32]));
        assert!(refusals.refuse([2; 32]));
    }
}
