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
    plan::JdPlan,
    token::PxToken,
    ForwardShare, JdMode, JD_ROLLABLE, MAX_DECLARED_TXS,
};
use crate::stratum_v2::{
    sv1::addressed_to,
    template::{meets_target, BchTemplate, Hash},
    transport::{Limits, Receiver, Sender, Session},
    wire::{encoded, mining},
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
    },
    /// An accepted local share on a declared job.
    Share(ForwardShare),
}

#[derive(Clone)]
pub struct UplinkHandle {
    pub events: SyncSender<UplinkEvent>,
    pub status: Arc<JdStatus>,
}

/// Starts the uplink to the first of `targets` (the pools in order); it
/// runs until `stop`.
pub fn spawn(
    targets: Vec<JdTarget>,
    stop: Arc<AtomicBool>,
) -> (UplinkHandle, thread::JoinHandle<()>) {
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
        let mut serial = 0u64;
        let mut failures = 0u32;
        let mut next = 0usize;
        while !stop.load(Ordering::Relaxed) {
            let Some(target) = targets.get(next) else {
                return;
            };
            serial += 1;
            let custom_jobs = || {
                shared
                    .summary
                    .lock()
                    .map_or(0, |summary| summary.custom_jobs)
            };
            let before = custom_jobs();
            let result = uplink(target, &shared, &receive, &stop, serial);
            shared.set_plan(None);
            if stop.load(Ordering::Relaxed) {
                break;
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
                summary.state = "fallback";
                summary.fallbacks += 1;
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
    (UplinkHandle { events, status }, worker)
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

/// One pool session: Ok when the server stops, otherwise why it ended.
fn uplink(
    target: &JdTarget,
    status: &JdStatus,
    events: &Events<UplinkEvent>,
    stop: &AtomicBool,
    serial: u64,
) -> Result<(), String> {
    let full = target.mode == JdMode::FullTemplate;
    let (mut jd, mut jd_replies) = connect(target, if full { Limits::JD } else { Limits::DEVICE })?;
    setup(
        target,
        &mut jd,
        &mut jd_replies,
        Protocol::JobDeclarationProtocol,
        u32::from(full),
    )?;
    let (mut pool, mut replies) = connect(target, Limits::DEVICE)?;
    setup(
        target,
        &mut pool,
        &mut replies,
        Protocol::MiningProtocol,
        0b110,
    )?;
    pool.send(mining(Mining::OpenExtendedMiningChannel(
        OpenExtendedMiningChannel {
            request_id: 1,
            user_identity: target
                .identity
                .as_str()
                .try_into()
                .map_err(|_| "identity too long")?,
            nominal_hash_rate: 1e12,
            max_target: (&[0xff; 32]).into(),
            min_extranonce_size: JD_ROLLABLE as u16,
        },
    ))?)?;
    let mut frame = answer(&mut replies, stop)?;
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
        _ => return Err("the pool refused the Job Declaration channel".into()),
    };
    if rollable < JD_ROLLABLE {
        return Err("the pool's extranonce space is too small for Job Declaration".into());
    }
    let mut requests = 0u32;
    for _ in 0..TOKENS_AHEAD {
        allocate(target, &mut jd, &mut requests)?;
    }
    let mut tokens: VecDeque<Vec<u8>> = VecDeque::new();
    let (first, scripts) = token(&mut answer(&mut jd_replies, stop)?)?;
    let plan = Arc::new(JdPlan {
        serial,
        upstream_prefix: prefix,
        pad: vec![0; rollable - JD_ROLLABLE],
        scripts,
        rates: first.1,
        pool_target,
    });
    if !plan.is_valid() {
        return Err("the pool's token names outputs it does not list".into());
    }
    tokens.push_back(first.0);
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
        while let Some(mut frame) = jd_replies.receive(Duration::from_millis(5))? {
            match frame.header().msg_type() {
                MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS => {
                    let ((bytes, rates), scripts) = token(&mut frame)?;
                    if rates != plan.rates || scripts != plan.scripts {
                        return Err("the pool's fee or donation changed".into());
                    }
                    tokens.push_back(bytes);
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
                    let id = ask.request_id;
                    let positions: Vec<u16> =
                        ask.unknown_tx_position_list.iter().copied().collect();
                    let Some(declaration) = declaring.get_mut(&id) else {
                        continue;
                    };
                    match provide(id, &declaration.template, &positions) {
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
                            return Err(
                                "the pool asked for a transaction the template does not have"
                                    .into(),
                            )
                        }
                    }
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
                        &plan,
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
                    status.update(|summary| summary.custom_jobs += 1);
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
                    pool_target = <Hash>::try_from(set.maximum_target.as_ref())
                        .map_err(|_| "malformed target")?;
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
            UplinkEvent::Published { serial, template } => {
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
                // template of the same parent, if any.
                if full && !declarable(&template) {
                    status.update(|summary| summary.dropped += 1);
                    continue;
                }
                let Some(token) = tokens.pop_front() else {
                    status.update(|summary| summary.refused += 1);
                    continue;
                };
                allocate(target, &mut jd, &mut requests)?;
                if full {
                    let request = next_declaration;
                    next_declaration = next_declaration.wrapping_add(1);
                    jd.send(declaration(request, &token, &template)?)?;
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
                    declare(&mut pool, channel, request, &token, &template, &plan)?;
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
                if !meets_target(&share.hash, &pool_target) {
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
/// bits.
fn declarable(template: &BchTemplate) -> bool {
    template.transaction_ids().len() <= MAX_DECLARED_TXS
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

fn allocate(target: &JdTarget, jd: &mut Sender, requests: &mut u32) -> Result<(), String> {
    *requests = requests.wrapping_add(1);
    jd.send(encoded(
        AllocateMiningJobToken {
            user_identifier: target
                .identity
                .as_str()
                .try_into()
                .map_err(|_| "identity too long")?,
            request_id: *requests,
        },
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
        false,
    )?)
}

/// A token's bytes and rates, and the pool's output scripts in its order.
#[allow(clippy::type_complexity)]
fn token(
    frame: &mut SerializedFrame,
) -> Result<((Vec<u8>, super::token::PoolRates), Vec<Vec<u8>>), String> {
    if frame.header().msg_type() != MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS {
        return Err("the pool did not allocate a token".into());
    }
    let success: AllocateMiningJobTokenSuccess =
        binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed token")?;
    let bytes = success.mining_job_token.as_ref().to_vec();
    // A Pickaxe pool's token carries its rates; another pool's is opaque,
    // and its fee and donation cannot be known, so its custom jobs could
    // not pay them.
    let token = PxToken::decode(&bytes)
        .ok_or("the pool's tokens are not a Pickaxe pool's; Job Declaration needs one")?;
    let scripts = parse_outputs(success.coinbase_outputs.as_ref())?
        .into_iter()
        .map(|(_, script)| script)
        .collect();
    Ok(((bytes, token.rates), scripts))
}

/// DeclareMiningJob for `template`: the local jobs' coinbase around the
/// whole extranonce (the pool channel's prefix and the rolled bytes), and
/// the transaction ids in block order.
fn declaration(
    request: u32,
    token: &[u8],
    template: &BchTemplate,
) -> Result<SerializedFrame, String> {
    let parts = template.declared_parts()?;
    let txids: Vec<binary_sv2::U256> = template.transaction_ids().iter().map(Into::into).collect();
    encoded(
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
            excess_data: (&[][..]).try_into().map_err(|_| "excess data too long")?,
        },
        MESSAGE_TYPE_DECLARE_MINING_JOB,
        false,
    )
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
/// one frame.
fn provide(
    request: u32,
    template: &BchTemplate,
    positions: &[u16],
) -> Result<SerializedFrame, Unprovided> {
    let transactions = template.transactions();
    // The header, the request id and the count, then each transaction with
    // its 3-byte length.
    let mut size = 6 + 4 + 2;
    let mut list = Vec::with_capacity(positions.len());
    for position in positions {
        let tx = transactions
            .get(usize::from(*position))
            .ok_or(Unprovided::Unknown)?;
        size += 3 + tx.len();
        if size > Limits::JD.max_out {
            return Err(Unprovided::TooLarge);
        }
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
/// pool channel's prefix and the 16 rolled bytes.
fn declare(
    pool: &mut Sender,
    channel: u32,
    request: u32,
    token: &[u8],
    template: &BchTemplate,
    plan: &JdPlan,
) -> Result<(), String> {
    let head = template.script_head();
    let outputs = serialize_outputs(&plan.outputs(template.coinbase_value));
    let path: Vec<binary_sv2::U256> = template.merkle_path().iter().map(Into::into).collect();
    pool.send(mining(Mining::SetCustomMiningJob(SetCustomMiningJob {
        channel_id: channel,
        request_id: request,
        token: token.try_into().map_err(|_| "token too long")?,
        version: template.version,
        prev_hash: (&template.previous_hash).into(),
        min_ntime: template.current_time,
        nbits: template.bits,
        coinbase_tx_version: 2,
        coinbase_prefix: head.as_slice().try_into().map_err(|_| "prefix too long")?,
        coinbase_tx_input_n_sequence: u32::MAX,
        coinbase_tx_outputs: outputs
            .as_slice()
            .try_into()
            .map_err(|_| "outputs too long")?,
        coinbase_tx_locktime: 0,
        merkle_path: path.try_into().map_err(|_| "merkle path too long")?,
    }))?)
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
    use crate::stratum_v2::template_tests::rpc_template;

    fn template() -> BchTemplate {
        BchTemplate::from_rpc(&rpc_template()).unwrap()
    }

    // #### PR #42
    // What: a template over 65,535 transactions is not declared; one at
    // the limit is.
    // Look here if: declarable or MAX_DECLARED_TXS changes.
    #[test]
    fn templates_over_65535_transactions_are_not_declared() {
        let at_limit = template().with_transactions(vec![vec![0]; MAX_DECLARED_TXS]);
        assert!(declarable(&at_limit));
        let over = template().with_transactions(vec![vec![0]; MAX_DECLARED_TXS + 1]);
        assert!(!declarable(&over));
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
        let mut frame = provide(3, &big, &[1]).unwrap();
        let reply: ProvideMissingTransactionsSuccess =
            binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(reply.request_id, 3);
        assert_eq!(reply.transaction_list.len(), 1);
        assert_eq!(reply.transaction_list[0].as_ref()[0], 2);
        assert_eq!(provide(3, &big, &[0, 1]).err(), Some(Unprovided::TooLarge));
        assert_eq!(provide(3, &big, &[2]).err(), Some(Unprovided::Unknown));
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
