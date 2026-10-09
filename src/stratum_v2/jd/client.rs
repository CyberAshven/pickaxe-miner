//! #### PR #42
//! The client side of Job Declaration (Coinbase-only). An uplink thread keeps
//! a Job Declaration session and one work-selection channel at the pool,
//! publishes the plan the local server builds its jobs from, declares each
//! local template as a custom job with a token, and forwards the local
//! devices' shares that meet the pool's target. When the pool refuses the
//! templates or the link fails, the plan goes and the local server stops
//! offering work, so the SV1 adapter moves devices to the pool's own jobs.

use super::{
    codec::{parse_outputs, serialize_outputs},
    plan::JdPlan,
    token::PxToken,
    ForwardShare, JD_ROLLABLE,
};
use crate::stratum_v2::{
    sv1::addressed_to,
    template::{meets_target, BchTemplate, Hash},
    transport::{Receiver, Sender, Session},
    wire::{encoded, mining},
};
use std::{
    collections::{HashMap, VecDeque},
    net::{TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
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
        AllocateMiningJobToken, AllocateMiningJobTokenSuccess,
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN, MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS,
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
/// Confirmed custom jobs whose shares are still forwarded.
const KEPT_JOBS: usize = 16;
/// How long the pool has to answer a setup, a channel, a token or a job.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

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
}

/// What the dashboard shows of the client; never an identity or token.
#[derive(Clone, Debug, Default)]
pub struct JdClientSummary {
    /// "connecting", "active" or "fallback".
    pub state: &'static str,
    pub custom_jobs: u64,
    pub refused: u64,
    pub forwarded: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub fallbacks: u64,
    pub last_error: Option<String>,
}

pub struct JdStatus {
    pub plan: RwLock<Option<Arc<JdPlan>>>,
    pub summary: Mutex<JdClientSummary>,
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

/// Starts the uplink; it runs until `stop`.
pub fn spawn(target: JdTarget, stop: Arc<AtomicBool>) -> (UplinkHandle, thread::JoinHandle<()>) {
    let (events, receive) = mpsc::sync_channel(EVENTS);
    let status = Arc::new(JdStatus {
        plan: RwLock::new(None),
        summary: Mutex::new(JdClientSummary {
            state: "connecting",
            ..JdClientSummary::default()
        }),
    });
    let shared = status.clone();
    let worker = thread::spawn(move || {
        let mut serial = 0u64;
        while !stop.load(Ordering::Relaxed) {
            serial += 1;
            let result = uplink(&target, &shared, &receive, &stop, serial);
            shared.set_plan(None);
            if stop.load(Ordering::Relaxed) {
                break;
            }
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
            let until = Instant::now() + target.retry;
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

/// One pool session: Ok when the server stops, otherwise why it ended.
fn uplink(
    target: &JdTarget,
    status: &JdStatus,
    events: &Events<UplinkEvent>,
    stop: &AtomicBool,
    serial: u64,
) -> Result<(), String> {
    let (mut jd, mut jd_replies) = connect(target)?;
    setup(
        target,
        &mut jd,
        &mut jd_replies,
        Protocol::JobDeclarationProtocol,
        0,
    )?;
    let (mut pool, mut replies) = connect(target)?;
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
    status.set_plan(Some(plan.clone()));
    status.update(|summary| {
        summary.state = "active";
        summary.last_error = None;
    });
    let mut pending: HashMap<u32, (u64, Instant)> = HashMap::new();
    let mut jobs: VecDeque<(u64, u32)> = VecDeque::new();
    let mut cache: HashMap<u64, Vec<ForwardShare>> = HashMap::new();
    let mut sequence = 0u32;
    let mut next_request = 1u32;
    while !stop.load(Ordering::Relaxed) {
        while let Some(mut frame) = jd_replies.receive(Duration::from_millis(5))? {
            if frame.header().msg_type() == MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS {
                let ((bytes, rates), scripts) = token(&mut frame)?;
                if rates != plan.rates || scripts != plan.scripts {
                    return Err("the pool's fee or donation changed".into());
                }
                tokens.push_back(bytes);
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
                Mining::SubmitSharesSuccess(success) => status.update(|summary| {
                    summary.accepted += u64::from(success.new_submits_accepted_count)
                }),
                Mining::SubmitSharesError(_) => status.update(|summary| summary.rejected += 1),
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
                let Some(token) = tokens.pop_front() else {
                    status.update(|summary| summary.refused += 1);
                    continue;
                };
                allocate(target, &mut jd, &mut requests)?;
                let request = next_request;
                next_request = next_request.wrapping_add(1);
                declare(&mut pool, channel, request, &token, &template, &plan)?;
                pending.insert(request, (serial, Instant::now()));
                // Shares of jobs this one replaces on another parent stay
                // forwardable only while the pool keeps the jobs.
                cache.retain(|kept, _| pending.values().any(|(serial, _)| serial == kept));
            }
            UplinkEvent::Share(share) => {
                if !meets_target(&share.hash, &pool_target) {
                    continue;
                }
                if let Some((_, job)) = jobs
                    .iter()
                    .rev()
                    .find(|(serial, _)| *serial == share.serial)
                {
                    forward(&mut pool, channel, *job, &mut sequence, share, status)?;
                } else if pending.values().any(|(serial, _)| *serial == share.serial) {
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

/// The pinned Noise session to the pool.
fn connect(target: &JdTarget) -> Result<(Sender, Receiver), String> {
    let address = target
        .address
        .to_socket_addrs()
        .map_err(|_| "cannot resolve the pool's address")?
        .next()
        .ok_or("cannot resolve the pool's address")?;
    let stream = TcpStream::connect_timeout(&address, ANSWER_TIMEOUT)
        .map_err(|_| "cannot reach the pool")?;
    Ok(Session::initiate(stream, target.authority)?.split())
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
