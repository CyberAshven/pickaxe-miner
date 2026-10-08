//! #### PR #38
//! SV1 firmware adapter over the same authenticated SV2 server. It translates
//! jobs and shares using SRI; it never constructs payouts or accepts a share
//! without the upstream validator. Plain SV1 belongs on a trusted mining LAN.
//!
//! #### PR #40
//! The upstream can also be a remote SV2 pool (`Upstream::remote`): another
//! Pickaxe server, or a BCH SV2 pool such as SoloFury or LoneStrike's stack,
//! so SV1 firmware mines there over an encrypted, pinned link with no local
//! node. A pool may acknowledge shares in batches, so in that mode firmware
//! gets its reply once the adapter has checked and forwarded a share, and the
//! adapter counts the pool's verdicts on the workers page itself.

use super::{
    channel::MAX_ACTIVE_JOBS,
    server::ServerStats,
    telemetry::ShareEvent,
    transport::{Receiver, Sender, Session},
    wire::encoded,
    work_allocation::WorkAllocation,
};
use crate::donation::bch::BchDonation;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2,
    bitcoin::Target,
    codec_sv2::SerializedFrame,
    common_messages_sv2::{
        Protocol, SetupConnection, SetupConnectionSuccess, MESSAGE_TYPE_RECONNECT,
    },
    mining_sv2::*,
    stratum_translation::{sv1_to_sv2, sv2_to_sv1},
    sv1_api::{self as v1, json_rpc::Message, utils::HexU32Be},
};

const VERSION_MASK: u32 = 0x1fffe000;
const MAX_LINE: usize = 64 * 1024;
const MAX_PENDING: usize = 64;
const DEADLINE: Duration = Duration::from_secs(10);
/// A remote pool's verdict on a share is counted if it arrives within this
/// time; later or missing verdicts are dropped without closing the device.
const REMOTE_VERDICT: Duration = Duration::from_secs(120);
/// The rate a device declares when its channel opens at a remote pool, which
/// sets the pool's first difficulty: 1 TH/s, about one Bitaxe. Pools then
/// adjust it from the shares.
const REMOTE_NOMINAL_HASHRATE: f32 = 1e12;
/// #### PR #40
/// At a remote pool the adapter owns the device's extranonce: extranonce1 is
/// four bytes it picks and extranonce2 the next four the device rolls,
/// together the channel's eight miner bytes, while the pool's channel prefix
/// (and zero padding, should a pool grant more) goes into the coinbase part
/// the device receives. One device session can then mine on its own channel
/// or the donation's channel at the same pool by job alone.
const POOL_EXTRANONCE1: usize = 4;
const POOL_EXTRANONCE2: usize = 4;
/// SV1 job numbers of the donation channel's jobs: the pool's number with the
/// top bit set, so the two channels' numbers never meet.
const DONATION_JOBS: u32 = 0x8000_0000;
/// The request ID that opens the donation channel.
const DONATION_REQUEST: u32 = 2;

/// #### PR #40
/// Where the BCH donation's work goes at a remote pool: a second channel at
/// the same pool under the donation address, for the donation's share of
/// mining time. The rate is the Advanced setting, read live.
#[derive(Clone, Debug)]
pub struct DonationRoute {
    pub identity: String,
    pub rate: Arc<RwLock<BchDonation>>,
}

impl DonationRoute {
    fn rate(&self) -> BchDonation {
        *self
            .rate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// #### PR #40
/// Where the adapter takes its work from.
#[derive(Clone, Debug)]
pub struct Upstream {
    /// `host:port`, resolved at each connection.
    pub address: String,
    /// The upstream's authority public key; the Noise handshake pins it.
    pub authority: [u8; 32],
    /// The identity each channel opens with: the pool account or payout
    /// address at a remote pool. Never printed.
    pub identity: String,
    /// A remote pool rather than this server's own SV2 listener.
    pub remote: bool,
    /// #### PR #40: where the donation's work goes at this pool.
    pub donation: Option<DonationRoute>,
    /// #### PR #40: this server's own listener running a public pool, where
    /// a device's channel opens at authorize under the device's username.
    pub public: bool,
}

impl Upstream {
    /// This server's own SV2 listener.
    pub fn local(address: SocketAddr, authority: [u8; 32]) -> Self {
        Self {
            address: address.to_string(),
            authority,
            identity: "sv1-device".into(),
            remote: false,
            donation: None,
            public: false,
        }
    }

    /// #### PR #40: this server's own listener running a public pool.
    pub fn local_public(address: SocketAddr, authority: [u8; 32]) -> Self {
        Self {
            public: true,
            ..Self::local(address, authority)
        }
    }

    /// The host part of `address`, for the SV2 setup message.
    fn host(&self) -> &str {
        self.address
            .rsplit_once(':')
            .map_or(self.address.as_str(), |(host, _)| host)
            .trim_start_matches('[')
            .trim_end_matches(']')
    }
}

/// All validation and block submission still pass through the pinned SV2
/// connection, including connections from older SV1-only ASIC firmware.
/// Each device takes the first of `upstreams` that opens a channel for it:
/// this server's own listener, or remote pools in failover order.
pub fn run(
    listener: TcpListener,
    upstreams: Vec<Upstream>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    let upstreams: Arc<[Upstream]> = upstreams.into();
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot configure SV1 listener")?;
    let mut devices: Vec<thread::JoinHandle<()>> = Vec::new();
    let result = (|| {
        while !stop.load(Ordering::Relaxed) {
            let mut active = Vec::new();
            for worker in devices.drain(..) {
                if worker.is_finished() {
                    let _ = worker.join();
                } else {
                    active.push(worker);
                }
            }
            devices = active;
            match listener.accept() {
                Ok((stream, _)) if devices.len() < 64 => {
                    // #### PR #40
                    // On Windows an accepted socket inherits the listener's
                    // non-blocking mode, so reads and writes would fail at
                    // once instead of waiting out their timeouts; Linux does
                    // not inherit it. Blocking again here makes both alike.
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let stop = stop.clone();
                    let stats = stats.clone();
                    let upstreams = upstreams.clone();
                    devices.push(thread::spawn(move || {
                        let _ = serve(stream, &upstreams, &stop, &stats);
                    }));
                }
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(_) => return Err("SV1 listener failed".into()),
            }
        }
        Ok(())
    })();
    // A listener failure stops the entire service rather than leaving the UI
    // claiming an unavailable firmware endpoint is healthy.
    stop.store(true, Ordering::Relaxed);
    for worker in devices {
        let _ = worker.join();
    }
    result
}

/// Connects to the first reachable address `address` resolves to.
fn connect(address: &str) -> Result<TcpStream, String> {
    let addresses = address
        .to_socket_addrs()
        .map_err(|_| "SV2 server unavailable")?;
    addresses
        .into_iter()
        .find_map(|address| TcpStream::connect_timeout(&address, DEADLINE).ok())
        .ok_or_else(|| "SV2 server unavailable".into())
}

/// #### PR #40
/// A channel opened for one device at one upstream.
struct Opened {
    send: Sender,
    receive: Receiver,
    bridge: Bridge,
    /// The adapter's end of the link; this server's own view of the device
    /// uses the same address, which joins the two into one row.
    local: SocketAddr,
}

fn serve(
    stream: TcpStream,
    upstreams: &[Upstream],
    stop: &AtomicBool,
    stats: &Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    // Pools in failover order: the first that completes the handshake, the
    // setup and the channel serves this device.
    let mut failure = String::from("SV2 server unavailable");
    let mut chosen = None;
    for upstream in upstreams {
        match open(upstream) {
            Ok(opened) => {
                chosen = Some((upstream, opened));
                break;
            }
            Err(error) => failure = error,
        }
    }
    let Some((upstream, opened)) = chosen else {
        // The device still gets a row, with the last pool's reason.
        if let Ok(mut stats) = stats.lock() {
            if let Ok(device) = stream.peer_addr() {
                let now = Instant::now();
                let id = stats.device_stats.connect(device, true, now);
                stats.device_stats.set_address(id, device.ip());
                stats.sv1_connection_errors = stats.sv1_connection_errors.saturating_add(1);
                stats.device_stats.close(id, true, Some(&failure), now);
            }
        }
        return Err(failure);
    };
    let id = {
        let mut stats = stats.lock().map_err(|_| "mining statistics unavailable")?;
        let id = stats
            .device_stats
            .connect(opened.local, true, Instant::now());
        // The firmware's own address, for read-only device reports.
        if let Ok(device) = stream.peer_addr() {
            stats.device_stats.set_address(id, device.ip());
        }
        // With a remote pool no local server counts the session.
        if upstream.remote {
            stats.connections = stats.connections.saturating_add(1);
            stats.sessions_started = stats.sessions_started.saturating_add(1);
        }
        id
    };
    let result = serve_session(stream, opened, upstream, stop, stats, id);
    if let Ok(mut stats) = stats.lock() {
        if upstream.remote {
            stats.connections = stats.connections.saturating_sub(1);
        }
        let error = result
            .as_ref()
            .err()
            .map(String::as_str)
            .filter(|_| !stop.load(Ordering::Relaxed));
        if error.is_some() {
            stats.sv1_connection_errors = stats.sv1_connection_errors.saturating_add(1);
        }
        stats.device_stats.close(id, true, error, Instant::now());
    }
    result
}

/// Opens an extended channel for one device at `upstream`: TCP, the pinned
/// Noise handshake, the setup and the channel.
fn open(upstream: &Upstream) -> Result<Opened, String> {
    let socket = connect(&upstream.address)?;
    let local = socket
        .local_addr()
        .map_err(|_| "cannot identify adapter socket")?;
    let peer = socket
        .peer_addr()
        .map_err(|_| "cannot identify adapter upstream")?;
    let (mut send, mut receive) = Session::initiate(socket, upstream.authority)?.split();
    let host = if upstream.remote {
        upstream.host()
    } else {
        "localhost"
    };
    send.send(encoded(
        SetupConnection {
            protocol: Protocol::MiningProtocol,
            min_version: 2,
            max_version: 2,
            flags: 4,
            endpoint_host: host.try_into().map_err(|_| "invalid host")?,
            endpoint_port: peer.port(),
            vendor: "Pickaxe SV1 adapter"
                .try_into()
                .map_err(|_| "invalid vendor")?,
            hardware_version: "".try_into().map_err(|_| "invalid version")?,
            firmware: "".try_into().map_err(|_| "invalid firmware")?,
            device_id: "".try_into().map_err(|_| "invalid device")?,
        },
        0,
        false,
    )?)?;
    let reply = receive.receive(DEADLINE)?.ok_or("SV2 setup timed out")?;
    validate_setup_reply(reply)?;
    // #### PR #40: a public pool opens the device's channel at authorize.
    if upstream.public {
        return Ok(Opened {
            bridge: Bridge::public(),
            send,
            receive,
            local,
        });
    }
    let open = sv1_to_sv2::build_sv2_open_extended_mining_channel(
        1,
        upstream.identity.clone(),
        if upstream.remote {
            REMOTE_NOMINAL_HASHRATE
        } else {
            1.0
        },
        Target::from_le_bytes([255; 32]),
        8,
    )
    .map_err(|_| "cannot open firmware channel")?;
    send.send(encoded(
        open,
        MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL,
        false,
    )?)?;
    let mut reply = receive.receive(DEADLINE)?.ok_or("SV2 channel timed out")?;
    if reply.header().msg_type() != MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS {
        return Err("SV2 channel rejected".into());
    }
    let opened: OpenExtendedMiningChannelSuccess =
        binary_sv2::from_bytes(reply.payload()).map_err(|_| "invalid channel reply")?;
    Ok(Opened {
        bridge: Bridge::new(opened, upstream.remote, upstream.donation.clone())?,
        send,
        receive,
        local,
    })
}

fn serve_session(
    stream: TcpStream,
    opened: Opened,
    upstream: &Upstream,
    stop: &AtomicBool,
    stats: &Arc<Mutex<ServerStats>>,
    device: u64,
) -> Result<(), String> {
    let Opened {
        mut send,
        mut receive,
        mut bridge,
        ..
    } = opened;
    let mut downstream = Lines::new(stream)?;
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if !bridge.ready() && started.elapsed() >= DEADLINE {
            return Err("SV1 setup timed out".into());
        }
        if upstream.remote {
            // Firmware already has its reply; a verdict that never comes is
            // simply not counted.
            for lane in bridge.lanes_mut() {
                lane.pending
                    .retain(|_, pending| pending.sent.elapsed() < REMOTE_VERDICT);
            }
        } else if bridge
            .user
            .pending
            .values()
            .any(|pending| pending.sent.elapsed() >= DEADLINE)
        {
            return Err("SV2 share response timed out".into());
        }
        if let Some(request) = downstream.read()? {
            let (responses, shares) = bridge.request(request)?;
            if let Some(reason) = bridge.local_rejection.take() {
                let mut stats = stats.lock().map_err(|_| "mining statistics unavailable")?;
                stats.shares_rejected = stats.shares_rejected.saturating_add(1);
                stats.sv1_local_rejected = stats.sv1_local_rejected.saturating_add(1);
                stats.device_stats.share(
                    device,
                    ShareEvent::Rejected(reason),
                    true,
                    Instant::now(),
                );
            }
            for response in responses {
                downstream.write(&response)?;
            }
            for share in shares {
                send.send(encoded(share, MESSAGE_TYPE_SUBMIT_SHARES_EXTENDED, true)?)?;
            }
        }
        if let Some(frame) = receive.receive(Duration::from_millis(1))? {
            let (messages, verdicts) = bridge.upstream(frame)?;
            if !verdicts.is_empty() {
                let mut stats = stats.lock().map_err(|_| "mining statistics unavailable")?;
                for verdict in verdicts {
                    match verdict {
                        ShareEvent::Accepted(_) => {
                            stats.shares_accepted = stats.shares_accepted.saturating_add(1)
                        }
                        ShareEvent::Rejected(_) => {
                            stats.shares_rejected = stats.shares_rejected.saturating_add(1)
                        }
                    }
                    stats
                        .device_stats
                        .share(device, verdict, true, Instant::now());
                }
            }
            for message in messages {
                downstream.write(&message)?;
            }
        }
        // #### PR #40: a public pool's channel under the device's username;
        // at a remote pool, the donation channel and the device's switches
        // between it and its own channel.
        if let Some(open) = bridge.channel_request()? {
            send.send(encoded(
                open,
                MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL,
                false,
            )?)?;
        }
        if let Some(open) = bridge.donation_request()? {
            send.send(encoded(
                open,
                MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL,
                false,
            )?)?;
        }
        for message in bridge.tick(Instant::now())? {
            downstream.write(&message)?;
        }
        if let Some(issue) = bridge.donation_issue.take() {
            if let Ok(mut stats) = stats.lock() {
                stats.device_stats.note_adapter(device, issue);
            }
        }
    }
    send.close();
    Ok(())
}

/// #### PR #40
/// One SV2 extended channel a device's work comes from: its own, or at a
/// remote pool the donation's.
struct Lane {
    channel: u32,
    prefix: Vec<u8>,
    /// The channel's miner extranonce size.
    extra_size: usize,
    target: [u8; 32],
    future: BTreeMap<u32, NewExtendedMiningJobOwned>,
    active: BTreeMap<u32, u32>,
    previous_hash: Option<SetNewPrevHashOwned>,
    notify: Option<Value>,
    sequence: u32,
    pending: BTreeMap<u32, Pending>,
}

impl Lane {
    fn new(open: &OpenExtendedMiningChannelSuccess<'_>) -> Result<Self, String> {
        Ok(Self {
            channel: open.channel_id,
            prefix: open.extranonce_prefix.as_ref().to_vec(),
            extra_size: open.extranonce_size as usize,
            target: open
                .target
                .as_ref()
                .try_into()
                .map_err(|_| "invalid target")?,
            future: BTreeMap::new(),
            active: BTreeMap::new(),
            previous_hash: None,
            notify: None,
            sequence: 0,
            pending: BTreeMap::new(),
        })
    }

    /// A job for this channel: kept for its parent, or with the same parent
    /// returned for activation.
    fn job(
        &mut self,
        job: NewExtendedMiningJob<'_>,
    ) -> Result<Option<(SetNewPrevHashOwned, NewExtendedMiningJobOwned)>, String> {
        if !job.version_rolling_allowed {
            return Err("unexpected firmware job".into());
        }
        if self.future.contains_key(&job.job_id) || self.active.contains_key(&job.job_id) {
            return Err("job identifier already in use".into());
        }
        if job.is_future() {
            if self.future.len() >= MAX_ACTIVE_JOBS {
                return Err("too many future jobs".into());
            }
            self.future.insert(job.job_id, job.as_owned());
            return Ok(None);
        }
        let prev = self
            .previous_hash
            .clone()
            .ok_or("job before initial parent")?;
        if job
            .min_ntime
            .clone()
            .into_inner()
            .is_none_or(|time| time < prev.min_ntime)
        {
            return Err("job time precedes active parent".into());
        }
        Ok(Some((prev, job.as_owned())))
    }

    /// A new parent: its future job becomes the only active one.
    fn parent(
        &mut self,
        prev: SetNewPrevHash<'_>,
    ) -> Result<(SetNewPrevHashOwned, NewExtendedMiningJobOwned), String> {
        let job = self
            .future
            .remove(&prev.job_id)
            .ok_or("unknown job activation")?;
        self.future.clear();
        self.active.clear();
        self.previous_hash = Some(prev.as_owned());
        Ok((prev.as_owned(), job))
    }
}

/// #### PR #40
/// The donation's channel at a remote pool.
enum Donation {
    /// None: this server's own listener, a 0% donation, or not asked yet.
    Off,
    Requested,
    Open(Box<Lane>),
    /// The pool refused or broke the donation channel; the device keeps
    /// mining on its own channel and its row shows why.
    Unavailable,
}

struct Bridge {
    /// The device's own channel.
    user: Lane,
    subscribed: bool,
    worker: Option<String>,
    configured: bool,
    mask: Option<HexU32Be>,
    local_rejection: Option<&'static str>,
    /// A remote pool: see `Upstream::remote`.
    remote: bool,
    /// The device's extranonce1: this server's channel prefix, or at a
    /// remote pool the adapter's own four bytes.
    extranonce1: Vec<u8>,
    /// The extranonce2 size the device rolls.
    extranonce2_size: usize,
    /// #### PR #40: the donation at a remote pool.
    route: Option<DonationRoute>,
    donation: Donation,
    allocation: WorkAllocation,
    /// The device mines the donation channel's jobs now.
    on_donation: bool,
    /// A donation problem not yet shown on the device's row.
    donation_issue: Option<&'static str>,
    /// #### PR #40: the adapter owns the device's extranonce, at a remote
    /// pool or in a public pool, and puts the channel's prefix into the
    /// coinbase part the device receives.
    owned: bool,
    /// A public pool's channel, opened under the device's username.
    public: Public,
}

/// #### PR #40
/// A public pool's channel for a device: opened at authorize, under the
/// username, which the server accepts only if it is a payout address.
enum Public {
    /// Not a public pool: the channel opened when the device connected.
    Off,
    /// Waiting for the device's username.
    Waiting,
    /// The channel request to send, and the authorize to answer.
    Asking {
        name: String,
        authorize: u64,
    },
    /// Sent; the authorize is answered when the server replies.
    Opening {
        name: String,
        authorize: u64,
    },
    Open,
}

/// A share forwarded upstream and waiting for its verdict.
struct Pending {
    /// The firmware's request ID.
    id: u64,
    sent: Instant,
    /// The share target the firmware was mining to, for the rate estimate.
    target: [u8; 32],
}

impl Bridge {
    fn new(
        open: OpenExtendedMiningChannelSuccess<'_>,
        remote: bool,
        route: Option<DonationRoute>,
    ) -> Result<Self, String> {
        let size = open.extranonce_size as usize;
        if open.request_id != 1 || size != 8 && !(remote && size > 8) {
            return Err("unexpected SV2 extranonce allocation".into());
        }
        let user = Lane::new(&open)?;
        let (extranonce1, extranonce2_size) = if remote {
            (
                rand::random::<[u8; POOL_EXTRANONCE1]>().to_vec(),
                POOL_EXTRANONCE2,
            )
        } else {
            (user.prefix.clone(), user.extra_size)
        };
        Ok(Self {
            user,
            subscribed: false,
            worker: None,
            configured: false,
            mask: None,
            local_rejection: None,
            remote,
            extranonce1,
            extranonce2_size,
            route: route.filter(|_| remote),
            donation: Donation::Off,
            allocation: WorkAllocation::new(rand::random()),
            on_donation: false,
            donation_issue: None,
            owned: remote,
            public: Public::Off,
        })
    }

    /// #### PR #40
    /// A device at a public pool: no channel until it gives its username.
    fn public() -> Self {
        Self {
            user: Lane {
                channel: 0,
                prefix: Vec::new(),
                extra_size: POOL_EXTRANONCE1 + POOL_EXTRANONCE2,
                target: [255; 32],
                future: BTreeMap::new(),
                active: BTreeMap::new(),
                previous_hash: None,
                notify: None,
                sequence: 0,
                pending: BTreeMap::new(),
            },
            subscribed: false,
            worker: None,
            configured: false,
            mask: None,
            local_rejection: None,
            remote: false,
            extranonce1: rand::random::<[u8; POOL_EXTRANONCE1]>().to_vec(),
            extranonce2_size: POOL_EXTRANONCE2,
            route: None,
            donation: Donation::Off,
            allocation: WorkAllocation::new(rand::random()),
            on_donation: false,
            donation_issue: None,
            owned: true,
            public: Public::Waiting,
        }
    }

    /// #### PR #40: a public pool's channel request, sent once.
    fn channel_request(&mut self) -> Result<Option<OpenExtendedMiningChannelOwned>, String> {
        let Public::Asking { name, authorize } = &self.public else {
            return Ok(None);
        };
        let (name, authorize) = (name.clone(), *authorize);
        self.public = Public::Opening {
            name: name.clone(),
            authorize,
        };
        sv1_to_sv2::build_sv2_open_extended_mining_channel(
            1,
            name,
            1.0,
            Target::from_le_bytes([255; 32]),
            (POOL_EXTRANONCE1 + POOL_EXTRANONCE2) as u16,
        )
        .map(Some)
        .map_err(|_| "cannot open firmware channel".into())
    }

    fn ready(&self) -> bool {
        self.subscribed
            && self.worker.is_some()
            && matches!(self.public, Public::Off | Public::Open)
    }

    fn lane(&self, donation: bool) -> Option<&Lane> {
        match (donation, &self.donation) {
            (false, _) => Some(&self.user),
            (true, Donation::Open(lane)) => Some(lane.as_ref()),
            (true, _) => None,
        }
    }

    fn lane_mut(&mut self, donation: bool) -> Option<&mut Lane> {
        match (donation, &mut self.donation) {
            (false, _) => Some(&mut self.user),
            (true, Donation::Open(lane)) => Some(lane.as_mut()),
            (true, _) => None,
        }
    }

    /// Both channels' shares, for expiring verdicts no longer awaited.
    fn lanes_mut(&mut self) -> impl Iterator<Item = &mut Lane> {
        let donation = match &mut self.donation {
            Donation::Open(lane) => Some(lane.as_mut()),
            _ => None,
        };
        std::iter::once(&mut self.user).chain(donation)
    }

    /// Whether a channel-specific message is the donation channel's; an
    /// unknown channel is an error.
    fn on_lane(&self, channel: u32, error: &'static str) -> Result<bool, String> {
        if self.lane(true).is_some_and(|lane| lane.channel == channel) {
            Ok(true)
        } else if channel == self.user.channel {
            Ok(false)
        } else {
            Err(error.into())
        }
    }

    fn notifications(&self) -> Result<Vec<Value>, String> {
        if !self.ready() {
            return Ok(Vec::new());
        }
        let Some(lane) = self.lane(self.on_donation) else {
            return Ok(Vec::new());
        };
        let Some(notify) = &lane.notify else {
            return Ok(Vec::new());
        };
        let difficulty = sv2_to_sv1::build_sv1_set_difficulty_from_sv2_target(
            Target::from_le_bytes(lane.target),
        )
        .map_err(|_| "invalid share target")?;
        Ok(vec![to_json(difficulty)?, notify.clone()])
    }

    /// The current channel's difficulty and job, with the other channel's
    /// work abandoned: sent when the device switches channels.
    fn switched(&self) -> Result<Vec<Value>, String> {
        let mut out = self.notifications()?;
        if let Some(notify) = out.last_mut() {
            notify["params"][8] = json!(true);
        }
        Ok(out)
    }

    /// #### PR #40
    /// What: at a remote pool, the donation's share of mining time mines on
    /// the donation channel, by a 10-minute cycle per device counted while it
    /// has work, as the server's own donation work is. A switch sends the
    /// channel's difficulty and a clean job; the device never reconnects.
    /// Why: Pickaxe cannot add a coinbase output to a pool's blocks, so the
    /// whole donation is work, in the BCH setting's own 0%..100% range.
    /// Check: at 1.5%, a device's shares at the pool's donation identity are
    /// about 1.5% of its shares there.
    fn tick(&mut self, now: Instant) -> Result<Vec<Value>, String> {
        let Some(route) = &self.route else {
            return Ok(Vec::new());
        };
        let rate = route.rate();
        self.allocation
            .update(now, self.ready() && self.user.notify.is_some());
        let usable = self.lane(true).is_some_and(|lane| lane.notify.is_some());
        let donate = usable && self.allocation.pool_donation_work(rate);
        if donate == self.on_donation {
            return Ok(Vec::new());
        }
        self.on_donation = donate;
        self.switched()
    }

    /// The donation channel to open: at a remote pool, with a donation above
    /// 0%, once the device is ready.
    fn donation_request(&mut self) -> Result<Option<OpenExtendedMiningChannelOwned>, String> {
        let Some(route) = &self.route else {
            return Ok(None);
        };
        if !matches!(self.donation, Donation::Off) || !self.ready() || u16::from(route.rate()) == 0
        {
            return Ok(None);
        }
        let identity = route.identity.clone();
        self.donation = Donation::Requested;
        sv1_to_sv2::build_sv2_open_extended_mining_channel(
            DONATION_REQUEST,
            identity,
            REMOTE_NOMINAL_HASHRATE,
            Target::from_le_bytes([255; 32]),
            (POOL_EXTRANONCE1 + POOL_EXTRANONCE2) as u16,
        )
        .map(Some)
        .map_err(|_| "cannot open the donation channel".into())
    }

    /// Stops donating at this pool: the device goes back to its own channel.
    fn drop_donation(&mut self, reason: &'static str) -> Result<Vec<Value>, String> {
        self.donation = Donation::Unavailable;
        self.donation_issue = Some(reason);
        if self.on_donation {
            self.on_donation = false;
            return self.switched();
        }
        Ok(Vec::new())
    }

    fn request(
        &mut self,
        value: Value,
    ) -> Result<(Vec<Value>, Vec<SubmitSharesExtendedOwned>), String> {
        self.local_rejection = None;
        let id = value["id"]
            .as_u64()
            .ok_or("SV1 request requires a numeric ID")?;
        let method = value["method"]
            .as_str()
            .ok_or("SV1 method missing")?
            .to_owned();
        let mut request = value.clone();
        // Some firmware omits the unused password. It never controls payout.
        if method == "mining.authorize"
            && request["params"].as_array().is_some_and(|p| p.len() == 1)
        {
            request["params"].as_array_mut().unwrap().push(json!(""));
        }
        let request: Message =
            serde_json::from_value(request).map_err(|_| "invalid SV1 request")?;
        let mut out = Vec::new();
        let mut shares = Vec::new();
        let was_ready = self.ready();
        match method.as_str() {
            "mining.configure" => {
                if self.configured || self.ready() {
                    return Ok((vec![reject(id, 20, "already configured")], shares));
                }
                let names = value
                    .pointer("/params/0")
                    .and_then(Value::as_array)
                    .ok_or("invalid configure extensions")?;
                let mut result = serde_json::Map::new();
                for name in names {
                    result.insert(
                        name.as_str().ok_or("invalid extension name")?.to_owned(),
                        json!(false),
                    );
                }
                if names.iter().any(|name| name == "version-rolling") {
                    let mask = match value.pointer("/params/1/version-rolling.mask") {
                        Some(Value::String(s)) => {
                            u32::from_str_radix(s, 16).map_err(|_| "invalid version mask")?
                        }
                        None => VERSION_MASK,
                        _ => return Err("invalid version mask".into()),
                    } & VERSION_MASK;
                    let minimum = match value.pointer("/params/1/version-rolling.min-bit-count") {
                        None => 0,
                        Some(Value::Number(n)) => n
                            .as_u64()
                            .filter(|n| *n <= 32)
                            .ok_or("invalid version bit count")?
                            as u32,
                        Some(Value::String(s)) => {
                            u32::from_str_radix(s, 16).map_err(|_| "invalid version bit count")?
                        }
                        _ => return Err("invalid version bit count".into()),
                    };
                    let enabled = mask != 0 && mask.count_ones() >= minimum;
                    result.insert("version-rolling".into(), json!(enabled));
                    if enabled {
                        result.insert("version-rolling.mask".into(), json!(format!("{mask:08x}")));
                        self.mask = Some(HexU32Be(mask));
                    }
                }
                self.configured = true;
                out.push(json!({"id":id,"result":result,"error":null}));
            }
            "mining.subscribe" => {
                if self.subscribed {
                    out.push(reject(id, 20, "already subscribed"));
                } else {
                    let v1::methods::Client2Server::Subscribe(subscribe) =
                        v1::methods::Client2Server::try_from(request)
                            .map_err(|_| "invalid subscribe")?
                    else {
                        return Err("invalid subscribe".into());
                    };
                    let prefix = self
                        .extranonce1
                        .clone()
                        .try_into()
                        .map_err(|_| "invalid extranonce prefix")?;
                    out.push(
                        serde_json::to_value(subscribe.respond(
                            vec![("mining.notify".into(), format!("{:08x}", self.user.channel))],
                            prefix,
                            self.extranonce2_size,
                        ))
                        .map_err(|_| "cannot encode subscription")?,
                    );
                    self.subscribed = true;
                }
            }
            "mining.authorize" => {
                let v1::methods::Client2Server::Authorize(auth) =
                    v1::methods::Client2Server::try_from(request)
                        .map_err(|_| "invalid authorize")?
                else {
                    return Err("invalid authorize".into());
                };
                let accepted = self.worker.is_none()
                    && !auth.name.is_empty()
                    && auth.name.len() <= 128
                    && !auth.name.chars().any(char::is_control);
                // #### PR #40: in a public pool the username is the payout,
                // so the server decides; the answer waits for its reply.
                if accepted && matches!(self.public, Public::Waiting) {
                    self.public = Public::Asking {
                        name: auth.name.clone(),
                        authorize: id,
                    };
                } else {
                    let accepted = accepted && matches!(self.public, Public::Off);
                    if accepted {
                        self.worker = Some(auth.name.clone());
                    }
                    out.push(
                        serde_json::to_value(auth.respond(accepted))
                            .map_err(|_| "cannot encode authorization")?,
                    );
                }
            }
            "mining.extranonce.subscribe" => out.push(json!({"id":id,"result":true,"error":null})),
            "mining.suggest_difficulty" => out.push(json!({"id":id,"result":false,"error":null})),
            "mining.submit" => {
                // Reject numbers before the reference parser can truncate an oversized u64.
                let params = value["params"].as_array().ok_or("invalid submission")?;
                if params.len() < 5 || !params[3].is_string() || !params[4].is_string() {
                    return Err("share time and nonce must be hex strings".into());
                }
                let v1::methods::Client2Server::Submit(submit) =
                    v1::methods::Client2Server::try_from(request)
                        .map_err(|_| "invalid submission")?
                else {
                    return Err("invalid submission".into());
                };
                // #### PR #40: the donation channel's jobs carry the top bit.
                let number = submit.job_id.parse::<u32>().ok();
                let donation = number.is_some_and(|number| number & DONATION_JOBS != 0);
                let version = number.and_then(|number| {
                    self.lane(donation)?
                        .active
                        .get(&(number & !DONATION_JOBS))
                        .copied()
                });
                let pending_full = self
                    .lane(donation)
                    .is_some_and(|lane| lane.pending.len() >= MAX_PENDING);
                let duplicate = self
                    .lane(false)
                    .into_iter()
                    .chain(self.lane(true))
                    .any(|lane| lane.pending.values().any(|pending| pending.id == id));
                let error =
                    if !self.subscribed {
                        Some((25, "not subscribed"))
                    } else if self.worker.as_deref() != Some(submit.user_name.as_str()) {
                        Some((24, "unauthorized worker"))
                    } else if version.is_none() {
                        Some((21, "stale job"))
                    } else if submit.extra_nonce2.len() != self.extranonce2_size {
                        Some((20, "invalid extranonce size"))
                    } else if (pending_full && !self.remote) || duplicate {
                        Some((20, "too many pending submissions or duplicate request ID"))
                    } else if submit.version_bits.as_ref().is_some_and(|bits| {
                        self.mask.as_ref().is_none_or(|mask| bits.0 & !mask.0 != 0)
                    }) {
                        Some((20, "version bits outside negotiated mask"))
                    } else {
                        None
                    };
                if let Some((code, text)) = error {
                    self.local_rejection = Some(text);
                    out.push(reject(id, code, text));
                } else {
                    let version = version.ok_or("stale job")?;
                    // A worker may omit version_bits and use the original version.
                    let mask = submit.version_bits.as_ref().and(self.mask.clone());
                    let remote = self.remote;
                    let owned = self.owned;
                    let extranonce1 = self.extranonce1.clone();
                    let lane = self.lane_mut(donation).ok_or("stale job")?;
                    let mut share = sv1_to_sv2::build_sv2_submit_shares_extended_from_sv1_submit(
                        &submit,
                        lane.channel,
                        lane.sequence,
                        version,
                        mask,
                    )
                    .map_err(|_| "share translation failed")?;
                    share.job_id &= !DONATION_JOBS;
                    if owned {
                        // The channel's miner bytes: any padding, the
                        // adapter's four, then the device's four.
                        let mut extranonce =
                            vec![0; lane.extra_size - POOL_EXTRANONCE1 - POOL_EXTRANONCE2];
                        extranonce.extend(extranonce1);
                        extranonce.extend(Vec::<u8>::from(submit.extra_nonce2.clone()));
                        share.extranonce = extranonce
                            .try_into()
                            .map_err(|_| "share translation failed")?;
                    }
                    if lane.pending.len() >= MAX_PENDING {
                        // Remote only: the oldest verdict is no longer awaited.
                        lane.pending.pop_first();
                    }
                    lane.pending.insert(
                        lane.sequence,
                        Pending {
                            id,
                            sent: Instant::now(),
                            target: lane.target,
                        },
                    );
                    lane.sequence = lane.sequence.wrapping_add(1);
                    shares.push(share);
                    if remote {
                        out.push(json!({"id":id,"result":true,"error":null}));
                    }
                }
            }
            _ => out.push(reject(id, 20, "unsupported method")),
        }
        if !was_ready && self.ready() {
            out.extend(self.notifications()?);
        }
        Ok((out, shares))
    }

    /// Handles one upstream message: the SV1 messages it produces, and with
    /// a remote pool the pool's verdicts on shares, for the workers page.
    fn upstream(
        &mut self,
        mut frame: SerializedFrame,
    ) -> Result<(Vec<Value>, Vec<ShareEvent>), String> {
        let kind = frame.header().msg_type();
        let channel_message = frame.header().channel_msg();
        let mut out = Vec::new();
        let mut verdicts = Vec::new();
        match kind {
            // A common-protocol Reconnect: the device reconnects, and the
            // adapter with it.
            MESSAGE_TYPE_RECONNECT if !channel_message => {
                return Err("SV2 upstream asked to reconnect".into())
            }
            // #### PR #40: a public pool's answer to the device's channel.
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS
                if matches!(self.public, Public::Opening { .. }) =>
            {
                let opened: OpenExtendedMiningChannelSuccess =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid channel reply")?;
                let Public::Opening { name, authorize } =
                    std::mem::replace(&mut self.public, Public::Open)
                else {
                    return Err("unexpected SV2 channel".into());
                };
                if opened.request_id != 1
                    || (opened.extranonce_size as usize) < POOL_EXTRANONCE1 + POOL_EXTRANONCE2
                {
                    return Err("unexpected SV2 extranonce allocation".into());
                }
                self.user = Lane::new(&opened)?;
                self.worker = Some(name);
                out.push(json!({"id":authorize,"result":true,"error":null}));
            }
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR
                if matches!(self.public, Public::Opening { .. }) =>
            {
                let error: OpenMiningChannelError =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid channel error")?;
                let Public::Opening { authorize, .. } =
                    std::mem::replace(&mut self.public, Public::Waiting)
                else {
                    return Err("unexpected SV2 channel error".into());
                };
                if error.request_id != 1 {
                    return Err("unexpected SV2 channel error".into());
                }
                self.local_rejection = Some("username is not a payout address");
                out.push(reject(
                    authorize,
                    24,
                    "the username must be your payout address on this network",
                ));
            }
            // #### PR #40: the pool's answer to the donation channel.
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS if self.remote => {
                let opened: OpenExtendedMiningChannelSuccess =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid channel reply")?;
                if opened.request_id != DONATION_REQUEST
                    || !matches!(self.donation, Donation::Requested)
                {
                    return Err("unexpected SV2 channel".into());
                }
                if (opened.extranonce_size as usize) < POOL_EXTRANONCE1 + POOL_EXTRANONCE2
                    || opened.channel_id == self.user.channel
                {
                    out.extend(self.drop_donation("the pool's donation channel cannot be used")?);
                } else {
                    self.donation = Donation::Open(Box::new(Lane::new(&opened)?));
                }
            }
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR if self.remote => {
                let error: OpenMiningChannelError =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid channel error")?;
                if error.request_id != DONATION_REQUEST
                    || !matches!(self.donation, Donation::Requested)
                {
                    return Err("unexpected SV2 channel error".into());
                }
                out.extend(self.drop_donation("the pool refused the donation channel")?);
            }
            MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB => {
                let job: NewExtendedMiningJob =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid mining job")?;
                let donation = self.on_lane(job.channel_id, "unexpected firmware job")?;
                if donation && job.job_id & DONATION_JOBS != 0 {
                    out.extend(
                        self.drop_donation(
                            "the pool's job numbers do not fit the donation channel",
                        )?,
                    );
                } else {
                    let lane = self.lane_mut(donation).ok_or("unexpected firmware job")?;
                    if let Some((prev, job)) = lane.job(job)? {
                        out.extend(self.activate(donation, prev, job, false)?);
                    }
                }
            }
            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH => {
                let prev: SetNewPrevHash = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "invalid job activation")?;
                let donation = self.on_lane(prev.channel_id, "wrong mining channel")?;
                let lane = self.lane_mut(donation).ok_or("wrong mining channel")?;
                let (prev, job) = lane.parent(prev)?;
                out.extend(self.activate(donation, prev, job, true)?);
            }
            MESSAGE_TYPE_SET_TARGET => {
                let target: SetTarget =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid target")?;
                let donation = self.on_lane(target.channel_id, "wrong mining channel")?;
                let lane = self.lane_mut(donation).ok_or("wrong mining channel")?;
                lane.target = target
                    .maximum_target
                    .as_ref()
                    .try_into()
                    .map_err(|_| "invalid target")?;
            }
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS => {
                let ack: SubmitSharesSuccess = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "invalid share response")?;
                if self.remote {
                    let donation =
                        self.on_lane(ack.channel_id, "unexpected share acknowledgement")?;
                    let lane = self
                        .lane_mut(donation)
                        .ok_or("unexpected share acknowledgement")?;
                    // Pools may acknowledge a batch: every share up to the
                    // last sequence number that has no error is accepted.
                    while let Some(entry) = lane.pending.first_entry() {
                        if *entry.key() > ack.last_sequence_number {
                            break;
                        }
                        verdicts.push(ShareEvent::Accepted(entry.remove().target));
                    }
                } else {
                    // This server acknowledges each share on its own.
                    if ack.channel_id != self.user.channel || ack.new_submits_accepted_count != 1 {
                        return Err("unexpected share acknowledgement".into());
                    }
                    let pending = self
                        .user
                        .pending
                        .remove(&ack.last_sequence_number)
                        .ok_or("unknown share acknowledgement")?;
                    out.push(json!({"id":pending.id,"result":true,"error":null}));
                }
            }
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR => {
                let error: SubmitSharesError =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid share error")?;
                let donation = self.on_lane(error.channel_id, "wrong mining channel")?;
                let pending = self
                    .lane_mut(donation)
                    .and_then(|lane| lane.pending.remove(&error.sequence_number));
                let (code, reason) = match error.error_code.as_ref() {
                    b"duplicate-share" => (22, "duplicate-share"),
                    b"difficulty-too-low" => (23, "difficulty-too-low"),
                    b"stale-share" => (21, "stale-share"),
                    b"invalid-job-id" => (21, "invalid-job-id"),
                    _ => (20, "rejected-by-pool"),
                };
                match (pending, self.remote) {
                    (Some(_), true) => verdicts.push(ShareEvent::Rejected(reason)),
                    // A verdict no longer awaited.
                    (None, true) => (),
                    (Some(pending), false) => {
                        out.push(reject(pending.id, code, "share rejected by validator"))
                    }
                    (None, false) => return Err("unknown share error".into()),
                }
            }
            // The device's extranonce cannot change under SV1 firmware, and
            // a closed channel has no work: the device reconnects for a new
            // channel. The donation channel just stops.
            MESSAGE_TYPE_SET_EXTRANONCE_PREFIX => {
                let changed: SetExtranoncePrefix = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "SV2 upstream changed the extranonce")?;
                if self
                    .lane(true)
                    .is_none_or(|lane| lane.channel != changed.channel_id)
                {
                    return Err("SV2 upstream changed the extranonce".into());
                }
                out.extend(self.drop_donation("the pool changed the donation channel")?);
            }
            MESSAGE_TYPE_CLOSE_CHANNEL => {
                let closed: CloseChannel = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "SV2 upstream closed the channel")?;
                if self
                    .lane(true)
                    .is_none_or(|lane| lane.channel != closed.channel_id)
                {
                    return Err("SV2 upstream closed the channel".into());
                }
                out.extend(self.drop_donation("the pool closed the donation channel")?);
            }
            // Group channels only matter for standard channels.
            MESSAGE_TYPE_SET_GROUP_CHANNEL if self.remote => (),
            _ => return Err("unexpected upstream firmware message".into()),
        }
        Ok((out, verdicts))
    }

    // #### PR #38
    // Immediate same-parent jobs preserve in-flight shares and use clean=false
    // on SV1. Only SetNewPrevHash flushes old jobs; each share keeps its version.
    fn activate(
        &mut self,
        donation: bool,
        prev: SetNewPrevHashOwned,
        job: NewExtendedMiningJobOwned,
        clean: bool,
    ) -> Result<Vec<Value>, String> {
        let owned = self.owned;
        let lane = self.lane_mut(donation).ok_or("unknown mining channel")?;
        lane.active.insert(job.job_id, job.version);
        while lane.active.len() > MAX_ACTIVE_JOBS {
            lane.active.pop_first();
        }
        let number = job.job_id;
        let mut notify = sv2_to_sv1::build_sv1_notify_from_sv2(prev, job, clean)
            .map_err(|_| "job translation failed")?;
        // #### PR #40: when the adapter owns the extranonce, the channel's
        // prefix (and any padding) is part of the coinbase the device gets.
        if owned {
            let mut coinbase: Vec<u8> = notify.coin_base1.clone().into();
            coinbase.extend(&lane.prefix);
            coinbase.resize(
                coinbase.len() + lane.extra_size - POOL_EXTRANONCE1 - POOL_EXTRANONCE2,
                0,
            );
            notify.coin_base1 = coinbase.into();
        }
        if donation {
            notify.job_id = (number | DONATION_JOBS).to_string();
        }
        lane.notify = Some(to_json(notify.into())?);
        if donation == self.on_donation {
            self.notifications()
        } else {
            Ok(Vec::new())
        }
    }
}

// #### PR #38
// SetupConnection.Success uses bit 0 for fixed version and bit 1 for requiring
// extended channels. The adapter requires version rolling and opens an extended
// channel, so only the latter requirement is compatible. Fail closed on unknown
// requirements; see Mining Protocol section 5.3.1.
fn validate_setup_reply(mut reply: SerializedFrame) -> Result<(), String> {
    let header = reply.header();
    if header.msg_type() != 1 || header.channel_msg() || header.ext_type_without_channel_msg() != 0
    {
        return Err("SV2 setup rejected".into());
    }
    let setup: SetupConnectionSuccess =
        binary_sv2::from_bytes(reply.payload()).map_err(|_| "invalid setup reply")?;
    if setup.used_version != 2 || setup.flags & !0b10 != 0 {
        return Err("SV2 setup incompatible".into());
    }
    Ok(())
}

fn to_json(message: Message) -> Result<Value, String> {
    serde_json::to_value(message).map_err(|_| "SV1 encoding failed".into())
}
fn reject(id: u64, code: i32, text: &str) -> Value {
    json!({"id":id,"result":null,"error":[code,text,null]})
}

struct Lines {
    stream: TcpStream,
    bytes: Vec<u8>,
    started: Option<Instant>,
}
impl Lines {
    fn new(stream: TcpStream) -> Result<Self, String> {
        stream
            .set_nodelay(true)
            .map_err(|_| "cannot configure SV1 socket")?;
        stream
            .set_read_timeout(Some(Duration::from_millis(1)))
            .map_err(|_| "cannot configure SV1 read")?;
        stream
            .set_write_timeout(Some(DEADLINE))
            .map_err(|_| "cannot configure SV1 write")?;
        Ok(Self {
            stream,
            bytes: Vec::new(),
            started: None,
        })
    }
    fn read(&mut self) -> Result<Option<Value>, String> {
        if self.started.is_some_and(|at| at.elapsed() >= DEADLINE) {
            return Err("SV1 line timed out".into());
        }
        if !self.bytes.contains(&b'\n') {
            let mut incoming = [0; 4096];
            match self.stream.read(&mut incoming) {
                Ok(0) => return Err("SV1 disconnected".into()),
                Ok(n) => {
                    self.started.get_or_insert_with(Instant::now);
                    self.bytes.extend(&incoming[..n]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(None)
                }
                Err(_) => return Err("SV1 read failed".into()),
            }
        }
        if let Some(end) = self.bytes.iter().position(|b| *b == b'\n') {
            if end > MAX_LINE {
                return Err("SV1 line too large".into());
            }
            let result =
                serde_json::from_slice(&self.bytes[..end]).map_err(|_| "invalid SV1 JSON")?;
            self.bytes.drain(..=end);
            self.started = if self.bytes.is_empty() {
                None
            } else {
                Some(Instant::now())
            };
            return Ok(Some(result));
        }
        if self.bytes.len() > MAX_LINE {
            return Err("SV1 line too large".into());
        }
        Ok(None)
    }
    fn write(&mut self, value: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| "SV1 encoding failed")?;
        if bytes.len() > MAX_LINE {
            return Err("SV1 reply too large".into());
        }
        bytes.push(b'\n');
        self.stream
            .write_all(&bytes)
            .map_err(|_| "SV1 write failed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_reply_accepts_extended_channels_but_rejects_fixed_version() {
        for (version, flags, message_type, channel, extension, expected) in [
            (2, 0, 1, false, 0, true),
            (2, 2, 1, false, 0, true),
            (2, 1, 1, false, 0, false),
            (2, 3, 1, false, 0, false),
            (2, 1 << 31, 1, false, 0, false),
            (3, 0, 1, false, 0, false),
            (2, 0, 2, false, 0, false),
            (2, 0, 1, true, 0, false),
            (2, 0, 1, false, 42, false),
        ] {
            use stratum_core::codec_sv2::{EncodableFrame, MessageFrame};
            let frame = MessageFrame::from_message(
                SetupConnectionSuccess {
                    used_version: version,
                    flags,
                },
                message_type,
                extension,
                channel,
            )
            .unwrap();
            let mut bytes = vec![0; frame.encoded_length()];
            frame.encode_into(&mut bytes).unwrap();
            let frame = SerializedFrame::from_bytes(bytes).unwrap();
            assert_eq!(validate_setup_reply(frame).is_ok(), expected, "version={version}, flags={flags}, type={message_type}, channel={channel}, extension={extension}");
        }
    }

    fn bridge_for(remote: bool) -> Bridge {
        let target = [255; 32];
        let prefix = [7; 16];
        Bridge::new(
            OpenExtendedMiningChannelSuccess {
                request_id: 1,
                channel_id: 3,
                group_channel_id: 0,
                target: (&target).into(),
                extranonce_size: 8,
                extranonce_prefix: prefix.as_slice().try_into().unwrap(),
            },
            remote,
            None,
        )
        .unwrap()
    }
    fn bridge() -> Bridge {
        bridge_for(false)
    }
    fn ready_for(remote: bool) -> Bridge {
        let mut bridge = bridge_for(remote);
        bridge
            .request(json!({"id":1,"method":"mining.subscribe","params":[]}))
            .unwrap();
        bridge
            .request(json!({"id":2,"method":"mining.authorize","params":["worker", "unused"]}))
            .unwrap();
        bridge.user.active.insert(4, 0x20000000);
        bridge
    }
    fn ready() -> Bridge {
        ready_for(false)
    }
    fn submit(id: u64) -> Value {
        json!({"id":id,"method":"mining.submit","params":["worker","4","0000000000000000","00000001","00000002"]})
    }
    /// #### PR #40: at a remote pool the device rolls four extranonce2 bytes.
    fn remote_submit(id: u64) -> Value {
        json!({"id":id,"method":"mining.submit","params":["worker","4","00000000","00000001","00000002"]})
    }

    // #### PR #40
    #[test]
    fn at_a_pool_the_device_switches_to_the_donation_channel_by_job_alone() {
        let job = |channel_id: u32, job_id: u32| {
            encoded(
                NewExtendedMiningJob {
                    channel_id,
                    job_id,
                    min_ntime: binary_sv2::Sv2Option::new(None),
                    version: 0x20000000,
                    version_rolling_allowed: true,
                    merkle_path: Vec::<binary_sv2::U256>::new().try_into().unwrap(),
                    coinbase_tx_prefix: [1u8; 32].as_slice().try_into().unwrap(),
                    coinbase_tx_suffix: [2u8; 32].as_slice().try_into().unwrap(),
                },
                MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB,
                true,
            )
            .unwrap()
        };
        let parent = |channel_id: u32, job_id: u32| {
            encoded(
                SetNewPrevHash {
                    channel_id,
                    job_id,
                    prev_hash: (&[0u8; 32]).into(),
                    min_ntime: 1700000000,
                    nbits: 0x1d00ffff,
                },
                MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH,
                true,
            )
            .unwrap()
        };
        let rate = Arc::new(RwLock::new("100".parse::<BchDonation>().unwrap()));
        let mut bridge = Bridge::new(
            OpenExtendedMiningChannelSuccess {
                request_id: 1,
                channel_id: 3,
                group_channel_id: 0,
                target: (&[255u8; 32]).into(),
                extranonce_size: 8,
                extranonce_prefix: [7u8; 4].as_slice().try_into().unwrap(),
            },
            true,
            Some(DonationRoute {
                identity: "donation".into(),
                rate: rate.clone(),
            }),
        )
        .unwrap();
        // The device's extranonce is the adapter's: four bytes, four to roll.
        let (subscribed, _) = bridge
            .request(json!({"id":1,"method":"mining.subscribe","params":[]}))
            .unwrap();
        let extranonce1 = hex::decode(subscribed[0]["result"][1].as_str().unwrap()).unwrap();
        assert_eq!(extranonce1.len(), 4);
        assert_eq!(subscribed[0]["result"][2], 4);
        assert!(
            bridge.donation_request().unwrap().is_none(),
            "not before authorize"
        );
        bridge
            .request(json!({"id":2,"method":"mining.authorize","params":["worker", ""]}))
            .unwrap();
        // Its own channel's prefix is in the coinbase part the device gets.
        bridge.upstream(job(3, 5)).unwrap();
        let own = bridge.upstream(parent(3, 5)).unwrap().0;
        assert_eq!(
            own[1]["params"][2],
            format!("{}{}", "01".repeat(32), "07".repeat(4))
        );
        // The donation channel opens once the device is ready.
        let open = bridge
            .donation_request()
            .unwrap()
            .expect("donation channel");
        assert_eq!(open.request_id, DONATION_REQUEST);
        assert!(bridge.donation_request().unwrap().is_none());
        let opened = encoded(
            OpenExtendedMiningChannelSuccess {
                request_id: DONATION_REQUEST,
                channel_id: 9,
                group_channel_id: 0,
                target: (&[255u8; 32]).into(),
                extranonce_size: 10,
                extranonce_prefix: [9u8; 2].as_slice().try_into().unwrap(),
            },
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
            false,
        )
        .unwrap();
        assert!(bridge.upstream(opened).unwrap().0.is_empty());
        bridge.upstream(job(9, 5)).unwrap();
        // Its jobs are kept while the device mines its own channel.
        assert!(bridge.upstream(parent(9, 5)).unwrap().0.is_empty());
        // At 100% the work clock switches the device at once, with a clean
        // job of its own number; the pool granted ten bytes, so two pad.
        let switched = bridge.tick(Instant::now()).unwrap();
        assert_eq!(switched[0]["method"], "mining.set_difficulty");
        assert_eq!(switched[1]["params"][0], (5 | DONATION_JOBS).to_string());
        assert_eq!(switched[1]["params"][8], true);
        assert_eq!(
            switched[1]["params"][2],
            format!("{}{}{}", "01".repeat(32), "09".repeat(2), "00".repeat(2))
        );
        // A share for that job goes to the donation channel.
        let (replies, shares) = bridge
            .request(json!({"id":3,"method":"mining.submit","params":["worker",(5 | DONATION_JOBS).to_string(),"0a0b0c0d","00000001","00000002"]}))
            .unwrap();
        assert_eq!(replies, vec![json!({"id":3,"result":true,"error":null})]);
        assert_eq!((shares[0].channel_id, shares[0].job_id), (9, 5));
        let mut expected = vec![0, 0];
        expected.extend(&extranonce1);
        expected.extend([0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(shares[0].extranonce.as_ref(), expected.as_slice());
        // At 0% it goes back to its own channel.
        *rate.write().unwrap() = "0".parse().unwrap();
        let back = bridge.tick(Instant::now()).unwrap();
        assert_eq!(back[1]["params"][0], "5");
        assert_eq!(back[1]["params"][8], true);
        // A closed donation channel stops the donation and tells the row.
        *rate.write().unwrap() = "100".parse().unwrap();
        assert!(!bridge.tick(Instant::now()).unwrap().is_empty());
        let close = encoded(
            CloseChannel {
                channel_id: 9,
                reason_code: "bye".try_into().unwrap(),
            },
            MESSAGE_TYPE_CLOSE_CHANNEL,
            true,
        )
        .unwrap();
        let after = bridge.upstream(close).unwrap().0;
        assert_eq!(after[1]["params"][0], "5");
        assert_eq!(
            bridge.donation_issue,
            Some("the pool closed the donation channel")
        );
        assert!(bridge.tick(Instant::now()).unwrap().is_empty());
        assert!(bridge.donation_request().unwrap().is_none());
    }

    // #### PR #40
    #[test]
    fn a_public_pool_opens_the_device_channel_under_its_username() {
        let mut bridge = Bridge::public();
        let (subscribed, _) = bridge
            .request(json!({"id":1,"method":"mining.subscribe","params":[]}))
            .unwrap();
        assert_eq!(subscribed[0]["result"][2], 4);
        // The answer waits for the server, which checks the payout.
        let (replies, _) = bridge
            .request(json!({"id":2,"method":"mining.authorize","params":["bchtest:qq.rig"]}))
            .unwrap();
        assert!(replies.is_empty());
        assert!(!bridge.ready());
        let open = bridge.channel_request().unwrap().expect("channel request");
        assert_eq!(open.request_id, 1);
        assert_eq!(open.user_identity.as_utf8_or_hex(), "bchtest:qq.rig");
        assert!(bridge.channel_request().unwrap().is_none());
        // Refused: the device hears why and may try again.
        let refused = encoded(
            OpenMiningChannelError {
                request_id: 1,
                error_code: "unknown-user".try_into().unwrap(),
            },
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR,
            false,
        )
        .unwrap();
        let replies = bridge.upstream(refused).unwrap().0;
        assert_eq!(replies[0]["id"], 2);
        assert_eq!(replies[0]["error"][0], 24);
        bridge
            .request(json!({"id":3,"method":"mining.authorize","params":["bchtest:qq"]}))
            .unwrap();
        assert!(bridge.channel_request().unwrap().is_some());
        let opened = encoded(
            OpenExtendedMiningChannelSuccess {
                request_id: 1,
                channel_id: 4,
                group_channel_id: 0,
                target: (&[255u8; 32]).into(),
                extranonce_size: 8,
                extranonce_prefix: [5u8; 16].as_slice().try_into().unwrap(),
            },
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
            false,
        )
        .unwrap();
        let replies = bridge.upstream(opened).unwrap().0;
        assert_eq!(replies, vec![json!({"id":3,"result":true,"error":null})]);
        assert!(bridge.ready());
        assert_eq!(bridge.user.channel, 4);
    }

    #[test]
    fn a_pool_that_refuses_the_donation_channel_keeps_the_device_mining() {
        let rate = Arc::new(RwLock::new(BchDonation::default()));
        let mut bridge = Bridge::new(
            OpenExtendedMiningChannelSuccess {
                request_id: 1,
                channel_id: 3,
                group_channel_id: 0,
                target: (&[255u8; 32]).into(),
                extranonce_size: 8,
                extranonce_prefix: [7u8; 4].as_slice().try_into().unwrap(),
            },
            true,
            Some(DonationRoute {
                identity: "donation".into(),
                rate,
            }),
        )
        .unwrap();
        bridge
            .request(json!({"id":1,"method":"mining.subscribe","params":[]}))
            .unwrap();
        bridge
            .request(json!({"id":2,"method":"mining.authorize","params":["worker", ""]}))
            .unwrap();
        assert!(bridge.donation_request().unwrap().is_some());
        let refused = encoded(
            OpenMiningChannelError {
                request_id: DONATION_REQUEST,
                error_code: "unknown-user".try_into().unwrap(),
            },
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR,
            false,
        )
        .unwrap();
        assert!(bridge.upstream(refused).unwrap().0.is_empty());
        assert_eq!(
            bridge.donation_issue,
            Some("the pool refused the donation channel")
        );
        assert!(bridge.tick(Instant::now()).unwrap().is_empty());
        // This server's own listener never gets a donation channel.
        let mut local = ready();
        assert!(local.donation_request().unwrap().is_none());
        assert!(local.tick(Instant::now()).unwrap().is_empty());
    }

    #[test]
    fn a_new_target_reaches_firmware_with_the_next_job_without_flushing_work() {
        let job = |job_id: u32, min_ntime: Option<u32>| {
            encoded(
                NewExtendedMiningJob {
                    channel_id: 3,
                    job_id,
                    min_ntime: binary_sv2::Sv2Option::new(min_ntime),
                    version: 0x20000000,
                    version_rolling_allowed: true,
                    merkle_path: Vec::<binary_sv2::U256>::new().try_into().unwrap(),
                    coinbase_tx_prefix: [1u8; 32].as_slice().try_into().unwrap(),
                    coinbase_tx_suffix: [2u8; 32].as_slice().try_into().unwrap(),
                },
                MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB,
                true,
            )
            .unwrap()
        };
        let mut bridge = ready();
        assert!(bridge.upstream(job(5, None)).unwrap().0.is_empty());
        let parent = encoded(
            SetNewPrevHash {
                channel_id: 3,
                job_id: 5,
                prev_hash: (&[0u8; 32]).into(),
                min_ntime: 1700000000,
                nbits: 0x1d00ffff,
            },
            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH,
            true,
        )
        .unwrap();
        let first = bridge.upstream(parent).unwrap().0;
        assert_eq!(first[0]["method"], "mining.set_difficulty");
        assert_eq!(first[1]["params"][8], true);
        // Vardiff: a new target, then an immediate job on the same block.
        let target = super::super::template::compact_target(0x1b0ffff0).unwrap();
        let set = encoded(
            SetTarget {
                channel_id: 3,
                maximum_target: (&target).into(),
            },
            MESSAGE_TYPE_SET_TARGET,
            true,
        )
        .unwrap();
        assert!(bridge.upstream(set).unwrap().0.is_empty());
        let next = bridge.upstream(job(6, Some(1700000000))).unwrap().0;
        assert_eq!(next[0]["method"], "mining.set_difficulty");
        assert!((next[0]["params"][0].as_f64().unwrap() - 4096.0).abs() < 0.01);
        assert_eq!(next[1]["method"], "mining.notify");
        // Work in flight is kept: no clean_jobs, and the older job still counts.
        assert_eq!(next[1]["params"][8], false);
        assert!(bridge.user.active.contains_key(&5) && bridge.user.active.contains_key(&6));
    }

    #[test]
    fn acknowledgement_requires_upstream_validation_and_maps_exact_request() {
        let mut bridge = ready();
        let (replies, shares) = bridge.request(submit(9)).unwrap();
        assert!(replies.is_empty());
        assert_eq!(shares.len(), 1);
        let ack = encoded(
            SubmitSharesSuccess {
                channel_id: 3,
                last_sequence_number: 0,
                new_submits_accepted_count: 1,
                new_shares_sum: 0,
            },
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS,
            true,
        )
        .unwrap();
        let (replies, verdicts) = bridge.upstream(ack.clone()).unwrap();
        assert_eq!(replies, vec![json!({"id":9,"result":true,"error":null})]);
        // This server counts its own verdicts.
        assert!(verdicts.is_empty());
        assert!(bridge.upstream(ack).is_err());
        bridge.request(submit(10)).unwrap();
        let reject = encoded(
            SubmitSharesError {
                channel_id: 3,
                sequence_number: 1,
                error_code: "duplicate-share".try_into().unwrap(),
            },
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR,
            true,
        )
        .unwrap();
        let replies = bridge.upstream(reject).unwrap().0;
        assert_eq!(replies[0]["id"], 10);
        assert_eq!(replies[0]["error"][0], 22);
        assert!(bridge.user.pending.is_empty());
    }

    #[test]
    fn a_remote_pool_gets_replies_at_once_and_its_batched_verdicts_are_counted() {
        let mut bridge = ready_for(true);
        // Firmware gets its reply as soon as the share is forwarded.
        for id in 20..23 {
            let (replies, shares) = bridge.request(remote_submit(id)).unwrap();
            assert_eq!(replies, vec![json!({"id":id,"result":true,"error":null})]);
            assert_eq!(shares.len(), 1);
        }
        // The pool rejects the second and acknowledges the batch.
        let error = encoded(
            SubmitSharesError {
                channel_id: 3,
                sequence_number: 1,
                error_code: "stale-share".try_into().unwrap(),
            },
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR,
            true,
        )
        .unwrap();
        let (replies, verdicts) = bridge.upstream(error).unwrap();
        assert!(replies.is_empty());
        assert!(matches!(
            verdicts[..],
            [ShareEvent::Rejected("stale-share")]
        ));
        let batch = encoded(
            SubmitSharesSuccess {
                channel_id: 3,
                last_sequence_number: 2,
                new_submits_accepted_count: 2,
                new_shares_sum: 0,
            },
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS,
            true,
        )
        .unwrap();
        let (replies, verdicts) = bridge.upstream(batch.clone()).unwrap();
        assert!(replies.is_empty());
        assert_eq!(verdicts.len(), 2);
        assert!(verdicts.iter().all(
            |verdict| matches!(verdict, ShareEvent::Accepted(target) if *target == [255; 32])
        ));
        assert!(bridge.user.pending.is_empty());
        // A repeated or late verdict is not an error.
        assert!(bridge.upstream(batch).unwrap().1.is_empty());
        // Pending verdicts stay bounded: the oldest is dropped, never the share.
        for id in 0..(MAX_PENDING as u64 + 5) {
            assert_eq!(bridge.request(remote_submit(100 + id)).unwrap().1.len(), 1);
        }
        assert_eq!(bridge.user.pending.len(), MAX_PENDING);
        // The firmware cannot follow a new extranonce, so it reconnects.
        let prefix = encoded(
            SetExtranoncePrefix {
                channel_id: 3,
                extranonce_prefix: [1u8; 16].as_slice().try_into().unwrap(),
            },
            MESSAGE_TYPE_SET_EXTRANONCE_PREFIX,
            true,
        )
        .unwrap();
        assert!(bridge.upstream(prefix).is_err());
    }

    #[test]
    fn firmware_cannot_change_worker_extranonce_mask_or_submit_before_setup() {
        let mut bridge = bridge();
        assert_eq!(bridge.request(submit(1)).unwrap().0[0]["error"][0], 25);
        bridge.subscribed = true;
        assert_eq!(bridge.request(submit(1)).unwrap().0[0]["error"][0], 24);
        let mut bridge = ready();
        let mut request = submit(5);
        request["params"][0] = json!("other-worker");
        assert_eq!(bridge.request(request).unwrap().0[0]["error"][0], 24);
        let mut request = submit(5);
        request["params"][1] = json!("3");
        assert_eq!(bridge.request(request).unwrap().0[0]["error"][0], 21);
        let mut request = submit(5);
        request["params"][2] = json!("00");
        assert_eq!(bridge.request(request).unwrap().0[0]["error"][0], 20);
        let mut request = submit(5);
        request["params"]
            .as_array_mut()
            .unwrap()
            .push(json!("00002000"));
        assert_eq!(
            bridge.request(request.clone()).unwrap().0[0]["error"][0],
            20
        );
        bridge.mask = Some(HexU32Be(0x2000));
        let (response, shares) = bridge.request(request.clone()).unwrap();
        assert!(response.is_empty());
        assert_eq!(shares[0].version, 0x20002000);
        request["params"][5] = json!("20000000");
        request["id"] = json!(6);
        assert_eq!(bridge.request(request).unwrap().0[0]["error"][0], 20);
        let mut request = submit(7);
        request["params"][4] = json!(u64::MAX);
        assert!(bridge.request(request).is_err());
        assert_eq!(bridge.user.pending.len(), 1);
    }

    #[test]
    fn configure_intersects_mask_and_rejects_unavailable_bit_count() {
        for (requested, minimum, expected) in [
            ("ffffffff", 2, true),
            ("00002000", 2, false),
            ("e0000000", 0, false),
        ] {
            let mut bridge = bridge();
            let request = json!({"id":1,"method":"mining.configure","params":[["version-rolling","unknown"],{"version-rolling.mask":requested,"version-rolling.min-bit-count":minimum}]});
            let replies = bridge.request(request).unwrap().0;
            assert_eq!(replies[0]["result"]["version-rolling"], expected);
            assert_eq!(replies[0]["result"]["unknown"], false);
            if expected {
                assert_eq!(replies[0]["result"]["version-rolling.mask"], "1fffe000");
            }
        }
    }

    #[test]
    fn pending_work_and_request_ids_are_bounded() {
        let mut bridge = ready();
        for id in 0..MAX_PENDING as u64 {
            assert_eq!(bridge.request(submit(id)).unwrap().1.len(), 1);
        }
        let (replies, shares) = bridge.request(submit(100)).unwrap();
        assert!(shares.is_empty());
        assert_eq!(replies[0]["error"][0], 20);
        assert_eq!(bridge.user.pending.len(), MAX_PENDING);
    }

    #[test]
    fn json_lines_handle_fragmentation_and_reject_oversized_or_slow_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let mut lines = Lines::new(listener.accept().unwrap().0).unwrap();
        peer.write_all(b"{\"id\":").unwrap();
        assert!(lines.read().unwrap().is_none());
        peer.write_all(b"1}\n{\"id\":2}\n").unwrap();
        assert_eq!(lines.read().unwrap().unwrap()["id"], 1);
        assert_eq!(lines.read().unwrap().unwrap()["id"], 2);
        lines.bytes = vec![b' '; MAX_LINE + 1];
        lines.bytes.push(b'\n');
        assert!(lines.read().is_err());
        lines.bytes.clear();
        lines.started = Some(Instant::now() - DEADLINE);
        assert!(lines.read().is_err());
    }
}
