//! The SV2 door of a Non-custodial Pool
//! (`serve --own-p2pool --public`). SV2 devices and other Pickaxes connect
//! to this server's own SV2 port and key, as at a Public Solo Endpoint;
//! each connection is relayed to your own P2Pool node (on this computer
//! only) over its own pinned SV2 link, and each channel opens there under
//! its miner's own payout identity (`sv1::miner_login`), never the
//! owner's, with a second channel under the donation's identity for the
//! Pickaxe donation's share of the device's work (`relay`). P2Pool builds
//! the blocks and checks every share. While P2Pool serves no work, new
//! connections go to the first fallback pool that takes them, and come back
//! as SV1 devices do (`sv1::Preferred`).

mod relay;

use super::{
    server::{self, ServerStats},
    sv1::Upstream,
    telemetry::ShareEvent,
    transport::{Receiver, Sender, Session},
    wire::encoded,
};
use relay::{Out, Relay};
use std::{
    io,
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stratum_core::{
    binary_sv2,
    codec_sv2::SerializedFrame,
    common_messages_sv2::{
        Protocol, SetupConnection, SetupConnectionError, MESSAGE_TYPE_SETUP_CONNECTION,
        MESSAGE_TYPE_SETUP_CONNECTION_ERROR, MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
    },
};

const MAX_CONNECTIONS: usize = 64;
const DEADLINE: Duration = Duration::from_secs(10);
/// SetupConnection's flag for a client that sets its own jobs (Job
/// Declaration): not taken through the door.
const REQUIRES_WORK_SELECTION: u32 = 0b010;
/// Why a session at your own P2Pool node ends while it serves no work: the
/// solo server's own reason, as the SV1 adapter gives it.
const NOT_SERVING: &str = "template source unavailable; reconnect when healthy";
/// How often the relay moves devices between lanes.
const TICK: Duration = Duration::from_millis(250);

/// Serves SV2 devices on `listener` with this server's key
/// (`authority_secret`), relaying each to the first of `upstreams` that
/// takes its connection: your own P2Pool node, then the fallback pools.
pub fn run(
    listener: TcpListener,
    upstreams: Vec<Upstream>,
    authority_secret: [u8; 32],
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    let public = server::authority_public(&authority_secret)?;
    let upstreams: Arc<[Upstream]> = upstreams.into();
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot configure mining listener")?;
    let mut devices: Vec<thread::JoinHandle<()>> = Vec::new();
    let result = (|| {
        while !stop.load(Ordering::Relaxed) {
            devices.retain(|device| !device.is_finished());
            match listener.accept() {
                // An accepted socket blocks again before the handshake (it
                // inherits the listener's mode on Windows).
                Ok((stream, peer))
                    if devices.len() < MAX_CONNECTIONS && stream.set_nonblocking(false).is_ok() =>
                {
                    let id = {
                        let mut stats =
                            stats.lock().map_err(|_| "mining statistics unavailable")?;
                        stats.connections += 1;
                        stats.sessions_started = stats.sessions_started.saturating_add(1);
                        let id = stats.device_stats.connect(peer, false, Instant::now());
                        stats.device_stats.set_address(id, peer.ip());
                        id
                    };
                    let (upstreams, stop, stats) = (upstreams.clone(), stop.clone(), stats.clone());
                    devices.push(thread::spawn(move || {
                        let result = serve(
                            stream,
                            &upstreams,
                            &public,
                            &authority_secret,
                            &stop,
                            &stats,
                            id,
                        );
                        if let Ok(mut stats) = stats.lock() {
                            stats.connections = stats.connections.saturating_sub(1);
                            let error = result
                                .as_ref()
                                .err()
                                .map(String::as_str)
                                .filter(|_| !stop.load(Ordering::Relaxed));
                            if error.is_some() {
                                stats.connection_errors = stats.connection_errors.saturating_add(1);
                            }
                            stats.device_stats.close(id, false, error, Instant::now());
                        }
                    }));
                }
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(_) => return Err("mining listener failed".to_owned()),
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    for device in devices {
        let _ = device.join();
    }
    result
}

fn serve(
    stream: TcpStream,
    upstreams: &[Upstream],
    public: &[u8; 32],
    secret: &[u8; 32],
    stop: &AtomicBool,
    stats: &Mutex<ServerStats>,
    device: u64,
) -> Result<(), String> {
    let (mut down_send, mut down_receive) = Session::accept(stream, public, secret)?.split();
    let setup = down_receive
        .receive(DEADLINE)?
        .ok_or("SV2 setup timed out")?;
    if setup.header().msg_type() != MESSAGE_TYPE_SETUP_CONNECTION || setup.header().channel_msg() {
        return Err("SetupConnection is required first".into());
    }
    let refusal = {
        // Read from a copy: the setup goes upstream as the device sent it.
        let mut copy = setup.clone();
        let request: SetupConnection =
            binary_sv2::from_bytes(copy.payload()).map_err(|_| "malformed setup message")?;
        if request.protocol != Protocol::MiningProtocol {
            Some((0, "unsupported-protocol"))
        } else if request.flags & REQUIRES_WORK_SELECTION != 0 {
            Some((REQUIRES_WORK_SELECTION, "unsupported-feature-flags"))
        } else {
            None
        }
    };
    if let Some((flags, code)) = refusal {
        down_send.send(setup_error(flags, code)?)?;
        down_send.close();
        return Err("SV2 setup rejected".into());
    }
    // The first upstream that takes the connection serves it; the setup
    // goes to each as the device sent it.
    let mut failure = String::from("SV2 server unavailable");
    let mut chosen = None;
    for upstream in upstreams {
        if upstream.down(Instant::now()) {
            failure = NOT_SERVING.into();
            continue;
        }
        match open(upstream, setup.clone()) {
            Ok((send, receive, reply)) => {
                chosen = Some((upstream, send, receive, reply));
                break;
            }
            Err(error) => failure = error,
        }
    }
    let Some((upstream, mut up_send, mut up_receive, reply)) = chosen else {
        down_send.send(setup_error(0, "upstream-unavailable")?)?;
        down_send.close();
        return Err(failure);
    };
    down_send.send(reply)?;
    let mut relay = Relay::new(upstream.miners, upstream.donation.clone());
    let mut ticked = Instant::now();
    let result = (|| {
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            // Back to your own P2Pool node once it has served for 30 s.
            if upstream
                .prefer
                .as_ref()
                .is_some_and(|prefer| prefer.should_return(now))
            {
                return Ok(());
            }
            if upstream.down(now) {
                return Err(NOT_SERVING.to_owned());
            }
            let mut out = Vec::new();
            if let Some(frame) = down_receive.receive(Duration::from_millis(1))? {
                out.extend(relay.device_frame(frame)?);
            }
            if let Some(frame) = up_receive.receive(Duration::from_millis(1))? {
                out.extend(relay.pool_frame(frame, unix_now())?);
            }
            if now.saturating_duration_since(ticked) >= TICK {
                ticked = now;
                out.extend(relay.tick(now)?);
            }
            for frame in out {
                match frame {
                    Out::Up(frame) => up_send.send(frame)?,
                    Out::Down(frame) => down_send.send(frame)?,
                }
            }
            note(&mut relay, stats, device);
        }
        Ok(())
    })();
    // The connection's channels close at P2Pool, each with its donation
    // channel, before the link does.
    for frame in relay.close().unwrap_or_default() {
        if let Out::Up(frame) = frame {
            if up_send.send(frame).is_err() {
                break;
            }
        }
    }
    up_send.close();
    down_send.close();
    result
}

/// Unix time in seconds, for the own donation's buckets.
fn unix_now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| u32::try_from(time.as_secs()).unwrap_or(u32::MAX))
}

/// What the relay learned, onto the workers page.
/// The donation terms come with each tick (`Relay::tick`), the rest as it
/// happens.
fn note(relay: &mut Relay, stats: &Mutex<ServerStats>, device: u64) {
    let noted = std::mem::take(&mut relay.noted);
    if noted.name.is_none() && noted.verdicts.is_empty() && noted.terms.is_none() {
        return;
    }
    let Ok(mut stats) = stats.lock() else {
        return;
    };
    if let Some(name) = &noted.name {
        stats.device_stats.set_worker(device, name);
    }
    if noted.terms.is_some() {
        stats.device_stats.set_donation(device, noted.terms);
    }
    if noted.verdicts.is_empty() {
        return;
    }
    stats.pool_work.merge(noted.work);
    for verdict in noted.verdicts {
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
            .share(device, verdict, false, Instant::now());
    }
}

fn setup_error(flags: u32, code: &str) -> Result<SerializedFrame, String> {
    encoded(
        SetupConnectionError {
            flags,
            error_code: code.try_into().map_err(|_| "invalid setup error")?,
        },
        MESSAGE_TYPE_SETUP_CONNECTION_ERROR,
        false,
    )
}

/// The pinned link to `upstream`, set up with the device's own setup
/// message; its success reply is the device's.
fn open(
    upstream: &Upstream,
    setup: SerializedFrame,
) -> Result<(Sender, Receiver, SerializedFrame), String> {
    let socket = upstream
        .address
        .to_socket_addrs()
        .map_err(|_| "SV2 server unavailable")?
        .find_map(|address| TcpStream::connect_timeout(&address, DEADLINE).ok())
        .ok_or("SV2 server unavailable")?;
    let (mut send, mut receive) = Session::initiate(socket, upstream.authority)?.split();
    send.send(setup)?;
    let reply = receive.receive(DEADLINE)?.ok_or("SV2 setup timed out")?;
    if reply.header().msg_type() != MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS
        || reply.header().channel_msg()
    {
        send.close();
        return Err("SV2 setup rejected".into());
    }
    Ok((send, receive, reply))
}

#[cfg(test)]
mod tests {
    use super::relay::refusal_code;
    use super::*;
    use crate::config::MiningNetwork;
    use crate::stratum_v2::sv1::{miner_login, DonationRoute, MinerLogins, P2PKH_ONLY};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::io::{BufRead, Write};
    use std::net::SocketAddr;
    use std::sync::RwLock;
    use stratum_core::common_messages_sv2::SetupConnectionSuccess;
    use stratum_core::mining_sv2::*;

    /// A Chipnet address of the given CashAddr version: 0 is P2PKH (q), 8
    /// is P2SH (p).
    fn chipnet(version: u8, seed: u8) -> String {
        crate::tx::cashaddr_with_version(version, &[seed; 20], MiningNetwork::Chipnet)
    }

    /// `<payout>[.worker],p2p=<its share address>`, as P2Pool should see it.
    fn p2pool_identity(payout: &str, worker: Option<&str>) -> String {
        let share = crate::tx::p2pool_share_address(payout).unwrap();
        match worker {
            Some(worker) => format!("{payout}.{worker},p2p={share}"),
            None => format!("{payout},p2p={share}"),
        }
    }

    /// The merkle root of the stand-in P2Pool's standard channel `channel`
    /// (extranonce prefix `[channel; 8]`): its coinbase alone.
    fn standard_root(channel: u32) -> [u8; 32] {
        crate::stratum_v2::template::double_sha256(
            &[[1u8; 32].as_slice(), &[channel as u8; 8], &[2; 32]].concat(),
        )
    }

    /// A scripted BCH P2Pool node on an ephemeral port: it answers the
    /// setup, opens every channel it is asked for (keeping the identity it
    /// was asked under), gives each channel a job and its parent, accepts
    /// standard shares, and keeps the shares and closes it got.
    struct ScriptedP2Pool {
        address: SocketAddr,
        authority: [u8; 32],
        identities: Arc<Mutex<Vec<String>>>,
        /// The shares and channel closes it got: message type and channel.
        received: Received,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    type Received = Arc<Mutex<Vec<(u8, u32)>>>;

    /// The group the stand-in P2Pool puts standard channels in.
    const GROUP: u32 = 1000;

    impl ScriptedP2Pool {
        fn start() -> Self {
            let secret = [7u8; 32];
            let authority = server::authority_public(&secret).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let identities = Arc::new(Mutex::new(Vec::new()));
            let received = Received::default();
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let (identities, received, stop) =
                    (identities.clone(), received.clone(), stop.clone());
                thread::spawn(move || {
                    let mut sessions = Vec::new();
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                stream.set_nonblocking(false).unwrap();
                                let (identities, received, stop) =
                                    (identities.clone(), received.clone(), stop.clone());
                                sessions.push(thread::spawn(move || {
                                    let _ = Self::serve(
                                        stream,
                                        &authority,
                                        &secret,
                                        &identities,
                                        &received,
                                        &stop,
                                    );
                                }));
                            }
                            Err(_) => thread::sleep(Duration::from_millis(10)),
                        }
                    }
                    for session in sessions {
                        let _ = session.join();
                    }
                })
            };
            Self {
                address,
                authority,
                identities,
                received,
                stop,
                thread: Some(thread),
            }
        }

        /// One session: standard channels get standard jobs of their own
        /// when the setup requires them, and otherwise go in `GROUP`, which
        /// is sent each new job as an extended job; every share is
        /// accepted.
        fn serve(
            stream: TcpStream,
            authority: &[u8; 32],
            secret: &[u8; 32],
            identities: &Mutex<Vec<String>>,
            received: &Mutex<Vec<(u8, u32)>>,
            stop: &AtomicBool,
        ) -> Result<(), String> {
            let (mut send, mut receive) = Session::accept(stream, authority, secret)?.split();
            let mut channels = 0u32;
            let mut standard_jobs = false;
            while !stop.load(Ordering::Relaxed) {
                let Some(mut frame) = receive.receive(Duration::from_millis(20))? else {
                    continue;
                };
                let kind = frame.header().msg_type();
                if matches!(
                    kind,
                    MESSAGE_TYPE_SUBMIT_SHARES_STANDARD
                        | MESSAGE_TYPE_SUBMIT_SHARES_EXTENDED
                        | MESSAGE_TYPE_CLOSE_CHANNEL
                ) {
                    let channel = frame.payload().first_chunk::<4>().unwrap();
                    received
                        .lock()
                        .unwrap()
                        .push((kind, u32::from_le_bytes(*channel)));
                }
                match kind {
                    MESSAGE_TYPE_SETUP_CONNECTION => {
                        let setup: SetupConnection =
                            binary_sv2::from_bytes(frame.payload()).unwrap();
                        standard_jobs = setup.flags & 1 != 0;
                        send.send(encoded(
                            SetupConnectionSuccess {
                                used_version: 2,
                                flags: 0,
                            },
                            MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
                            false,
                        )?)?
                    }
                    MESSAGE_TYPE_SUBMIT_SHARES_STANDARD => {
                        let share: SubmitSharesStandard =
                            binary_sv2::from_bytes(frame.payload()).unwrap();
                        send.send(encoded(
                            SubmitSharesSuccess {
                                channel_id: share.channel_id,
                                last_sequence_number: share.sequence_number,
                                new_submits_accepted_count: 1,
                                new_shares_sum: 1,
                            },
                            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS,
                            true,
                        )?)?;
                    }
                    MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL => {
                        let open: OpenExtendedMiningChannel =
                            binary_sv2::from_bytes(frame.payload()).unwrap();
                        identities
                            .lock()
                            .unwrap()
                            .push(open.user_identity.as_utf8_or_hex());
                        channels += 1;
                        let channel_id = channels;
                        send.send(encoded(
                            OpenExtendedMiningChannelSuccess {
                                request_id: open.request_id,
                                channel_id,
                                group_channel_id: 0,
                                target: (&[0xffu8; 32]).into(),
                                extranonce_size: 8,
                                extranonce_prefix: [channel_id as u8; 4]
                                    .as_slice()
                                    .try_into()
                                    .unwrap(),
                            },
                            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS,
                            false,
                        )?)?;
                        send.send(encoded(
                            NewExtendedMiningJob {
                                channel_id,
                                job_id: 1,
                                ntime_start: binary_sv2::Sv2Option::new(None),
                                version: 0x2000_0000,
                                version_rolling_allowed: true,
                                merkle_path: Vec::<binary_sv2::U256>::new().try_into().unwrap(),
                                coinbase_tx_prefix: [1u8; 32].as_slice().try_into().unwrap(),
                                coinbase_tx_suffix: [2u8; 32].as_slice().try_into().unwrap(),
                            },
                            MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB,
                            true,
                        )?)?;
                        send.send(encoded(
                            SetNewPrevHash {
                                channel_id,
                                job_id: 1,
                                prev_hash: (&[0u8; 32]).into(),
                                ntime_start: 1_700_000_000,
                                nbits: 0x1d00_ffff,
                            },
                            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH,
                            true,
                        )?)?;
                    }
                    MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL => {
                        let open: OpenStandardMiningChannel =
                            binary_sv2::from_bytes(frame.payload()).unwrap();
                        identities
                            .lock()
                            .unwrap()
                            .push(open.user_identity.as_utf8_or_hex());
                        channels += 1;
                        let channel_id = channels;
                        let prefix = [channel_id as u8; 8];
                        send.send(encoded(
                            OpenStandardMiningChannelSuccess {
                                request_id: open.request_id,
                                channel_id,
                                target: (&[0xffu8; 32]).into(),
                                extranonce_prefix: prefix.as_slice().try_into().unwrap(),
                                group_channel_id: if standard_jobs { 0 } else { GROUP },
                            },
                            MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL_SUCCESS,
                            false,
                        )?)?;
                        // A new job, numbered as the channel: for it alone,
                        // or for the group (each member's own).
                        let (job, addressed) = if standard_jobs {
                            let job = encoded(
                                NewMiningJob {
                                    channel_id,
                                    job_id: channel_id,
                                    ntime_start: binary_sv2::Sv2Option::new(None),
                                    version: 0x2000_0000,
                                    merkle_root: (&standard_root(channel_id)).into(),
                                },
                                MESSAGE_TYPE_NEW_MINING_JOB,
                                true,
                            )?;
                            (job, channel_id)
                        } else {
                            let job = encoded(
                                NewExtendedMiningJob {
                                    channel_id: GROUP,
                                    job_id: channel_id,
                                    ntime_start: binary_sv2::Sv2Option::new(None),
                                    version: 0x2000_0000,
                                    version_rolling_allowed: true,
                                    merkle_path: Vec::<binary_sv2::U256>::new().try_into().unwrap(),
                                    coinbase_tx_prefix: [1u8; 32].as_slice().try_into().unwrap(),
                                    coinbase_tx_suffix: [2u8; 32].as_slice().try_into().unwrap(),
                                },
                                MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB,
                                true,
                            )?;
                            (job, GROUP)
                        };
                        send.send(job)?;
                        send.send(encoded(
                            SetNewPrevHash {
                                channel_id: addressed,
                                job_id: channel_id,
                                prev_hash: (&[0u8; 32]).into(),
                                ntime_start: 1_700_000_000,
                                nbits: 0x1d00_ffff,
                            },
                            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH,
                            true,
                        )?)?;
                    }
                    _ => (),
                }
            }
            Ok(())
        }

        fn identities(&self) -> Vec<String> {
            self.identities.lock().unwrap().clone()
        }

        /// What it got, once `done` holds of it.
        fn received_until(&self, done: impl Fn(&[(u8, u32)]) -> bool) -> Vec<(u8, u32)> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let received = self.received.lock().unwrap().clone();
                if done(&received) {
                    return received;
                }
                assert!(Instant::now() < deadline, "only {received:?}");
                thread::sleep(Duration::from_millis(10));
            }
        }

        /// The identities, once there are `count`.
        fn wait_for(&self, count: usize) -> Vec<String> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let identities = self.identities();
                if identities.len() >= count {
                    return identities;
                }
                assert!(Instant::now() < deadline, "only {identities:?}");
                thread::sleep(Duration::from_millis(10));
            }
        }

        /// This node as an upstream: your own P2Pool node, or a fallback.
        fn upstream(&self, miners: Option<MinerLogins>, owner: &str) -> Upstream {
            Upstream {
                address: self.address.to_string(),
                authority: self.authority,
                identity: owner.to_owned(),
                remote: true,
                donation: None,
                public: miners.is_some(),
                prefer: None,
                fixed_version: false,
                miners,
                serving: None,
            }
        }
    }

    impl Drop for ScriptedP2Pool {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// A server part (the SV1 adapter or the SV2 door) on an ephemeral
    /// port, stopped when dropped.
    struct Listening {
        address: SocketAddr,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<Result<(), String>>>,
    }

    impl Listening {
        fn sv1(upstreams: Vec<Upstream>) -> Self {
            Self::with(|listener, stop, stats| {
                super::super::sv1::run(listener, upstreams, stop, stats)
            })
        }

        fn door(upstreams: Vec<Upstream>, secret: [u8; 32]) -> Self {
            Self::with(move |listener, stop, stats| run(listener, upstreams, secret, stop, stats))
        }

        fn with(
            serve: impl FnOnce(TcpListener, Arc<AtomicBool>, Arc<Mutex<ServerStats>>) -> Result<(), String>
                + Send
                + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let thread = {
                let stop = stop.clone();
                let stats = Arc::new(Mutex::new(ServerStats::default()));
                thread::spawn(move || serve(listener, stop, stats))
            };
            Self {
                address,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Listening {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// An SV1 device: subscribed, then authorized as `name`; the
    /// authorize's answer.
    struct Device {
        write: TcpStream,
        read: std::io::BufReader<TcpStream>,
    }

    impl Device {
        fn authorize(adapter: &Listening, name: &str) -> (Self, Value) {
            let write = TcpStream::connect(adapter.address).unwrap();
            write
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let read = std::io::BufReader::new(write.try_clone().unwrap());
            let mut device = Self { write, read };
            device.send(json!({"id":1,"method":"mining.subscribe","params":["test"]}));
            assert!(device.receive()["result"].is_array());
            device.send(json!({"id":2,"method":"mining.authorize","params":[name, "x"]}));
            let answer = loop {
                let message = device.receive();
                if message["id"] == 2 {
                    break message;
                }
            };
            (device, answer)
        }

        fn send(&mut self, value: Value) {
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            self.write.write_all(&bytes).unwrap();
        }

        fn receive(&mut self) -> Value {
            let mut line = String::new();
            self.read.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "the adapter closed the connection");
            serde_json::from_str(&line).unwrap()
        }

        /// Waits until the adapter ends this connection.
        fn closed(mut self) {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                assert!(Instant::now() < deadline, "the connection stayed open");
                let mut line = String::new();
                match self.read.read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => (),
                }
            }
        }
    }

    /// An SV2 device at the door: the setup, then a channel opened as
    /// `identity`; the reply's message type.
    fn sv2_open(door: &Listening, authority: [u8; 32], identity: &str, extended: bool) -> u8 {
        let (mut send, mut receive) = sv2_device(door, authority, if extended { 4 } else { 5 });
        let maximum = [0xffu8; 32];
        let frame = if extended {
            encoded(
                OpenExtendedMiningChannel {
                    request_id: 1,
                    user_identity: identity.try_into().unwrap(),
                    nominal_hash_rate: 1e12,
                    max_target: (&maximum).into(),
                    min_extranonce_size: 8,
                },
                MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL,
                false,
            )
        } else {
            open_standard(identity)
        };
        send.send(frame.unwrap()).unwrap();
        let reply = receive.receive(DEADLINE).unwrap().unwrap();
        send.close();
        reply.header().msg_type()
    }

    fn open_standard(identity: &str) -> Result<SerializedFrame, String> {
        encoded(
            OpenStandardMiningChannel {
                request_id: 1,
                user_identity: identity.try_into().unwrap(),
                nominal_hash_rate: 1e12,
                max_target: (&[0xffu8; 32]).into(),
            },
            MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL,
            false,
        )
    }

    /// An SV2 device's session at the door, set up with `flags`.
    fn sv2_device(door: &Listening, authority: [u8; 32], flags: u32) -> (Sender, Receiver) {
        let (mut send, mut receive) =
            Session::initiate(TcpStream::connect(door.address).unwrap(), authority)
                .unwrap()
                .split();
        send.send(
            encoded(
                SetupConnection {
                    protocol: Protocol::MiningProtocol,
                    min_version: 2,
                    max_version: 2,
                    flags,
                    endpoint_host: "localhost".try_into().unwrap(),
                    endpoint_port: door.address.port(),
                    vendor: "test".try_into().unwrap(),
                    hardware_version: "".try_into().unwrap(),
                    firmware: "".try_into().unwrap(),
                    device_id: "".try_into().unwrap(),
                },
                MESSAGE_TYPE_SETUP_CONNECTION,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let reply = receive.receive(DEADLINE).unwrap().unwrap();
        assert_eq!(
            reply.header().msg_type(),
            MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS
        );
        (send, receive)
    }

    // A standard-channel device through the door
    // What: an SV2 device on a standard channel, at a P2Pool that sends
    // standard channels their own jobs (the setup requires standard jobs)
    // or puts them in a group sent extended jobs: it gets standard jobs on
    // its own channel, outside any group, then at 100% the donation
    // channel's (the merkle root over that channel's extranonce prefix);
    // its share reaches the donation channel at P2Pool and the verdict comes
    // back under its own channel and sequence number. When the device
    // leaves, both channels close at P2Pool and no share follows.
    // Look here if: the door's standard channels or its closing change.
    #[test]
    fn a_standard_channel_device_mines_the_donation_through_the_door_and_both_channels_close() {
        for flags in [5, 4] {
            let p2pool = ScriptedP2Pool::start();
            let owner = p2pool_identity(&chipnet(0, 1), None);
            let logins = MinerLogins {
                network: MiningNetwork::Chipnet,
                p2pool: true,
            };
            let address = crate::donation::bch::address(MiningNetwork::Chipnet);
            let upstream = Upstream {
                donation: Some(DonationRoute {
                    identity: p2pool_identity(address, None),
                    rate: Arc::new(RwLock::new("100".parse().unwrap())),
                    fallback: Some(address.to_owned()),
                }),
                ..p2pool.upstream(Some(logins), &owner)
            };
            let secret = [9u8; 32];
            let door = Listening::door(vec![upstream], secret);
            let key = server::authority_public(&secret).unwrap();
            let (mut send, mut receive) = sv2_device(&door, key, flags);
            send.send(open_standard(&chipnet(0, 2)).unwrap()).unwrap();
            // P2Pool's channel 1 is the device's own, 2 the donation's.
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut roots = BTreeMap::new();
            let mut donation_job = None;
            while donation_job.is_none() {
                assert!(Instant::now() < deadline, "{flags}: no donation job");
                let Some(mut frame) = receive.receive(Duration::from_millis(50)).unwrap() else {
                    continue;
                };
                match frame.header().msg_type() {
                    MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL_SUCCESS => {
                        let open: OpenStandardMiningChannelSuccess =
                            binary_sv2::from_bytes(frame.payload()).unwrap();
                        assert_eq!((open.channel_id, open.group_channel_id), (1, 0));
                    }
                    MESSAGE_TYPE_NEW_MINING_JOB => {
                        let job: NewMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
                        assert_eq!(job.channel_id, 1);
                        let root: [u8; 32] = job.merkle_root.as_ref().try_into().unwrap();
                        roots.insert(job.job_id, root);
                    }
                    MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH => {
                        let prev: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
                        assert_eq!(prev.channel_id, 1);
                        let root = roots[&prev.job_id];
                        if root == standard_root(2) {
                            donation_job = Some(prev.job_id);
                        } else {
                            assert_eq!(root, standard_root(1), "{flags}: its own job");
                        }
                    }
                    _ => (),
                }
            }
            send.send(
                encoded(
                    SubmitSharesStandard {
                        channel_id: 1,
                        sequence_number: 7,
                        job_id: donation_job.unwrap(),
                        nonce: 1,
                        ntime: 1_700_000_001,
                        version: 0x2000_0000,
                    },
                    MESSAGE_TYPE_SUBMIT_SHARES_STANDARD,
                    true,
                )
                .unwrap(),
            )
            .unwrap();
            let ack = loop {
                assert!(Instant::now() < deadline, "{flags}: no verdict");
                let Some(mut frame) = receive.receive(Duration::from_millis(50)).unwrap() else {
                    continue;
                };
                if frame.header().msg_type() == MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS {
                    let ack: SubmitSharesSuccess = binary_sv2::from_bytes(frame.payload()).unwrap();
                    break ack;
                }
            };
            assert_eq!((ack.channel_id, ack.last_sequence_number), (1, 7));
            // The device leaves: both channels close at P2Pool.
            send.close();
            drop(receive);
            let closes = |received: &[(u8, u32)]| {
                received
                    .iter()
                    .filter(|(kind, _)| *kind == MESSAGE_TYPE_CLOSE_CHANNEL)
                    .map(|(_, channel)| *channel)
                    .collect::<Vec<_>>()
            };
            let received = p2pool.received_until(|received| closes(received).len() >= 2);
            assert_eq!(
                received,
                vec![
                    (MESSAGE_TYPE_SUBMIT_SHARES_STANDARD, 2),
                    (MESSAGE_TYPE_CLOSE_CHANNEL, 1),
                    (MESSAGE_TYPE_CLOSE_CHANNEL, 2),
                ],
                "{flags}"
            );
            drop(door);
        }
    }

    // One upstream channel per miner identity
    // What: at a Non-custodial Pool, two SV1 miners and an SV2 miner with
    // different payouts each reach P2Pool under their own
    // `<payout>[.worker],p2p=<share address>`; the owner's identity and the
    // miners' `,donation=` (the Pickaxe donation) never reach it; a P2SH
    // payout is refused at authorize (SV1) or at the channel (SV2) with
    // the reason.
    // Look here if: `miner_login`, `Bridge::miners` or the door change.
    #[test]
    fn two_miners_reach_p2pool_each_under_their_own_identity() {
        let p2pool = ScriptedP2Pool::start();
        let owner = p2pool_identity(&chipnet(0, 1), None);
        let logins = MinerLogins {
            network: MiningNetwork::Chipnet,
            p2pool: true,
        };
        let upstream = p2pool.upstream(Some(logins), &owner);
        let adapter = Listening::sv1(vec![upstream.clone()]);
        let (alice, bob, carol) = (chipnet(0, 2), chipnet(0, 3), chipnet(0, 4));
        let (_first, answer) = Device::authorize(&adapter, &format!("{alice}.rig1,donation=0.5"));
        assert_eq!(answer["result"], true, "{answer}");
        // The prefix may be left out.
        let bare = bob.split_once(':').unwrap().1.to_owned();
        let (_second, answer) = Device::authorize(&adapter, &bare);
        assert_eq!(answer["result"], true, "{answer}");
        let (_third, answer) = Device::authorize(&adapter, &chipnet(8, 5));
        assert_eq!(answer["error"][1], P2PKH_ONLY, "{answer}");
        // An SV2 miner through the door, on this server's own key.
        let secret = [9u8; 32];
        let door = Listening::door(vec![upstream], secret);
        let key = server::authority_public(&secret).unwrap();
        assert_eq!(
            sv2_open(&door, key, &format!("{carol}.bitaxe"), true),
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS
        );
        assert_eq!(
            sv2_open(&door, key, &carol, false),
            MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL_SUCCESS
        );
        assert_eq!(
            sv2_open(&door, key, &chipnet(8, 6), true),
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR
        );
        let identities = p2pool.wait_for(4);
        assert_eq!(
            identities,
            vec![
                p2pool_identity(&alice, Some("rig1")),
                p2pool_identity(&bob, None),
                p2pool_identity(&carol, Some("bitaxe")),
                p2pool_identity(&carol, None),
            ]
        );
        assert!(identities
            .iter()
            .all(|identity| !identity.contains("donation")));
        assert!(!identities.contains(&owner));
        drop((door, adapter));
    }

    // Your own P2Pool for your own devices
    // What: without --public every device mines at P2Pool under the one
    // owner identity, whatever its username.
    // Look here if: owner-only --own-p2pool upstreams change.
    #[test]
    fn an_owner_only_p2pool_keeps_one_identity() {
        let p2pool = ScriptedP2Pool::start();
        let owner = p2pool_identity(&chipnet(0, 1), None);
        let adapter = Listening::sv1(vec![p2pool.upstream(None, &owner)]);
        let (_first, answer) = Device::authorize(&adapter, &chipnet(0, 2));
        assert_eq!(answer["result"], true);
        let (_second, answer) = Device::authorize(&adapter, "rig-in-the-shed");
        assert_eq!(answer["result"], true);
        assert_eq!(p2pool.wait_for(2), vec![owner.clone(), owner]);
        drop(adapter);
    }

    // Failover for your own P2Pool node
    // What: once P2Pool has served no work for the 3 s grace, a device's
    // session there ends and it reconnects to the fallback pool (P2Pool is
    // skipped while it is down); once P2Pool serves again for the return
    // time (200 ms here, 30 s in use), the session at the fallback ends and
    // the device comes back to P2Pool.
    // Look here if: `Preferred::down`, `Upstream::serving` or the return
    // change.
    #[test]
    fn devices_leave_p2pool_for_a_fallback_while_it_serves_no_work_and_come_back() {
        let p2pool = ScriptedP2Pool::start();
        let fallback = ScriptedP2Pool::start();
        let owner = p2pool_identity(&chipnet(0, 1), None);
        let preferred = Arc::new(super::super::sv1::Preferred::with_return_after(
            Duration::from_millis(200),
        ));
        preferred.set(true, Instant::now());
        let logins = |p2pool| {
            Some(MinerLogins {
                network: MiningNetwork::Chipnet,
                p2pool,
            })
        };
        let adapter = Listening::sv1(vec![
            Upstream {
                serving: Some(preferred.clone()),
                ..p2pool.upstream(logins(true), &owner)
            },
            Upstream {
                prefer: Some(preferred.clone()),
                ..fallback.upstream(logins(false), &owner)
            },
        ]);
        let miner = chipnet(0, 2);
        let (device, answer) = Device::authorize(&adapter, &miner);
        assert_eq!(answer["result"], true);
        assert_eq!(p2pool.wait_for(1).len(), 1);
        // P2Pool stops serving: the session ends after the grace.
        let stopped = Instant::now();
        preferred.set(false, stopped);
        device.closed();
        assert!(stopped.elapsed() >= Duration::from_secs(3));
        let (device, answer) = Device::authorize(&adapter, &miner);
        assert_eq!(answer["result"], true);
        assert_eq!(fallback.wait_for(1), vec![miner.clone()]);
        assert_eq!(p2pool.identities().len(), 1, "P2Pool is skipped while down");
        // P2Pool serves again: back after the return time.
        preferred.set(true, Instant::now());
        device.closed();
        let (_device, answer) = Device::authorize(&adapter, &miner);
        assert_eq!(answer["result"], true);
        assert_eq!(p2pool.wait_for(2).len(), 2);
        drop(adapter);
    }

    #[test]
    fn your_own_p2pool_is_down_only_after_the_grace() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let preferred = super::super::sv1::Preferred::default();
        assert!(!preferred.down(at(60_000)), "never marked");
        preferred.set(false, start);
        preferred.set(false, at(1_000));
        assert!(!preferred.down(at(2_999)));
        assert!(preferred.down(at(3_000)));
        preferred.set(true, at(3_100));
        assert!(!preferred.down(at(10_000)));
        assert!(!preferred.should_return(at(33_099)));
        assert!(preferred.should_return(at(33_100)));
    }

    #[test]
    fn a_miner_login_names_their_own_share_address() {
        let logins = MinerLogins {
            network: MiningNetwork::Chipnet,
            p2pool: true,
        };
        let payout = chipnet(0, 2);
        let share = crate::tx::p2pool_share_address(&payout).unwrap();
        let login = miner_login(logins, &format!("{payout}.rig,donation=2,p2p={share}")).unwrap();
        assert_eq!(login.identity, format!("{payout}.rig,p2p={share}"));
        assert_eq!(login.donation, Some("2".parse().unwrap()));
        let other = crate::tx::p2pool_share_address(&chipnet(0, 3)).unwrap();
        assert_eq!(
            refusal_code(&miner_login(logins, &format!("{payout},p2p={other}")).unwrap_err()),
            "invalid-share-address"
        );
        assert_eq!(
            refusal_code(&miner_login(logins, &format!("{payout},donation=0.3")).unwrap_err()),
            "invalid-donation"
        );
        assert_eq!(
            refusal_code(&miner_login(logins, &chipnet(8, 2)).unwrap_err()),
            "p2pkh-payout-required"
        );
        assert_eq!(
            refusal_code(&miner_login(logins, "worker1").unwrap_err()),
            "unknown-user"
        );
        // At a fallback pool that is not P2Pool: the payout and worker.
        let plain = miner_login(
            MinerLogins {
                p2pool: false,
                ..logins
            },
            &format!("{payout}.rig"),
        )
        .unwrap();
        assert_eq!(plain.identity, format!("{payout}.rig"));
    }
}
