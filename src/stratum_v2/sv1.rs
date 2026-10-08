//! #### PR #38
//! SV1 firmware adapter over the same authenticated SV2 server. It translates
//! jobs and shares using SRI; it never constructs payouts or accepts a share
//! without the upstream validator. Plain SV1 belongs on a trusted mining LAN.

use super::{
    channel::MAX_ACTIVE_JOBS, server::ServerStats, telemetry::ShareEvent, transport::Session,
    wire::encoded,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2,
    bitcoin::Target,
    codec_sv2::SerializedFrame,
    common_messages_sv2::{Protocol, SetupConnection, SetupConnectionSuccess},
    mining_sv2::*,
    stratum_translation::{sv1_to_sv2, sv2_to_sv1},
    sv1_api::{self as v1, json_rpc::Message, utils::HexU32Be},
};

const VERSION_MASK: u32 = 0x1fffe000;
const MAX_LINE: usize = 64 * 1024;
const MAX_PENDING: usize = 64;
const DEADLINE: Duration = Duration::from_secs(10);

/// All validation and block submission still pass through the pinned SV2
/// connection, including connections from older SV1-only ASIC firmware.
pub fn run(
    listener: TcpListener,
    upstream: SocketAddr,
    authority: [u8; 32],
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
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
                    let stop = stop.clone();
                    let stats = stats.clone();
                    devices.push(thread::spawn(move || {
                        let _ = serve(stream, upstream, authority, &stop, &stats);
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

fn serve(
    stream: TcpStream,
    upstream: SocketAddr,
    authority: [u8; 32],
    stop: &AtomicBool,
    stats: &Arc<Mutex<ServerStats>>,
) -> Result<(), String> {
    let socket =
        TcpStream::connect_timeout(&upstream, DEADLINE).map_err(|_| "SV2 server unavailable")?;
    let peer = socket
        .local_addr()
        .map_err(|_| "cannot identify adapter socket")?;
    let id = {
        let mut stats = stats.lock().map_err(|_| "mining statistics unavailable")?;
        let id = stats.device_stats.connect(peer, true, Instant::now());
        // The firmware's own address, for read-only device reports.
        if let Ok(device) = stream.peer_addr() {
            stats.device_stats.set_address(id, device.ip());
        }
        id
    };
    let result = serve_session(stream, socket, authority, stop, stats, id);
    if let Ok(mut stats) = stats.lock() {
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

fn serve_session(
    stream: TcpStream,
    socket: TcpStream,
    authority: [u8; 32],
    stop: &AtomicBool,
    stats: &Arc<Mutex<ServerStats>>,
    device: u64,
) -> Result<(), String> {
    let upstream = socket
        .peer_addr()
        .map_err(|_| "cannot identify adapter upstream")?;
    let (mut send, mut receive) = Session::initiate(socket, authority)?.split();
    send.send(encoded(
        SetupConnection {
            protocol: Protocol::MiningProtocol,
            min_version: 2,
            max_version: 2,
            flags: 4,
            endpoint_host: "localhost".try_into().map_err(|_| "invalid host")?,
            endpoint_port: upstream.port(),
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
    let open = sv1_to_sv2::build_sv2_open_extended_mining_channel(
        1,
        "sv1-device".into(),
        1.0,
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
    let mut bridge = Bridge::new(opened)?;
    let mut downstream = Lines::new(stream)?;
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if !bridge.ready() && started.elapsed() >= DEADLINE {
            return Err("SV1 setup timed out".into());
        }
        if bridge
            .pending
            .values()
            .any(|(_, sent)| sent.elapsed() >= DEADLINE)
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
            for message in bridge.upstream(frame)? {
                downstream.write(&message)?;
            }
        }
    }
    send.close();
    Ok(())
}

struct Bridge {
    channel: u32,
    prefix: Vec<u8>,
    extra_size: usize,
    target: [u8; 32],
    subscribed: bool,
    worker: Option<String>,
    configured: bool,
    mask: Option<HexU32Be>,
    future: BTreeMap<u32, NewExtendedMiningJobOwned>,
    active: BTreeMap<u32, u32>,
    previous_hash: Option<SetNewPrevHashOwned>,
    notify: Option<Value>,
    sequence: u32,
    pending: BTreeMap<u32, (u64, Instant)>,
    local_rejection: Option<&'static str>,
}

impl Bridge {
    fn new(open: OpenExtendedMiningChannelSuccess<'_>) -> Result<Self, String> {
        if open.request_id != 1 || open.extranonce_size != 8 {
            return Err("unexpected SV2 extranonce allocation".into());
        }
        Ok(Self {
            channel: open.channel_id,
            prefix: open.extranonce_prefix.as_ref().to_vec(),
            extra_size: open.extranonce_size as usize,
            target: open
                .target
                .as_ref()
                .try_into()
                .map_err(|_| "invalid target")?,
            subscribed: false,
            worker: None,
            configured: false,
            mask: None,
            future: BTreeMap::new(),
            active: BTreeMap::new(),
            previous_hash: None,
            notify: None,
            sequence: 0,
            pending: BTreeMap::new(),
            local_rejection: None,
        })
    }

    fn ready(&self) -> bool {
        self.subscribed && self.worker.is_some()
    }

    fn notifications(&self) -> Result<Vec<Value>, String> {
        if !self.ready() {
            return Ok(Vec::new());
        }
        let Some(notify) = &self.notify else {
            return Ok(Vec::new());
        };
        let difficulty = sv2_to_sv1::build_sv1_set_difficulty_from_sv2_target(
            Target::from_le_bytes(self.target),
        )
        .map_err(|_| "invalid share target")?;
        Ok(vec![to_json(difficulty)?, notify.clone()])
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
                        .prefix
                        .clone()
                        .try_into()
                        .map_err(|_| "invalid extranonce prefix")?;
                    out.push(
                        serde_json::to_value(subscribe.respond(
                            vec![("mining.notify".into(), format!("{:08x}", self.channel))],
                            prefix,
                            self.extra_size,
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
                if accepted {
                    self.worker = Some(auth.name.clone());
                }
                out.push(
                    serde_json::to_value(auth.respond(accepted))
                        .map_err(|_| "cannot encode authorization")?,
                );
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
                let error =
                    if !self.subscribed {
                        Some((25, "not subscribed"))
                    } else if self.worker.as_deref() != Some(submit.user_name.as_str()) {
                        Some((24, "unauthorized worker"))
                    } else if submit
                        .job_id
                        .parse::<u32>()
                        .ok()
                        .is_none_or(|job| !self.active.contains_key(&job))
                    {
                        Some((21, "stale job"))
                    } else if submit.extra_nonce2.len() != self.extra_size {
                        Some((20, "invalid extranonce size"))
                    } else if self.pending.len() >= MAX_PENDING
                        || self.pending.values().any(|(pending, _)| *pending == id)
                    {
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
                    let version = self.active[&submit
                        .job_id
                        .parse::<u32>()
                        .map_err(|_| "invalid job identifier")?];
                    // A worker may omit version_bits and use the original version.
                    let mask = submit.version_bits.as_ref().and(self.mask.clone());
                    let share = sv1_to_sv2::build_sv2_submit_shares_extended_from_sv1_submit(
                        &submit,
                        self.channel,
                        self.sequence,
                        version,
                        mask,
                    )
                    .map_err(|_| "share translation failed")?;
                    self.pending.insert(self.sequence, (id, Instant::now()));
                    self.sequence = self.sequence.wrapping_add(1);
                    shares.push(share);
                }
            }
            _ => out.push(reject(id, 20, "unsupported method")),
        }
        if !was_ready && self.ready() {
            out.extend(self.notifications()?);
        }
        Ok((out, shares))
    }

    fn upstream(&mut self, mut frame: SerializedFrame) -> Result<Vec<Value>, String> {
        let kind = frame.header().msg_type();
        let mut out = Vec::new();
        match kind {
            MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB => {
                let job: NewExtendedMiningJob =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid mining job")?;
                if job.channel_id != self.channel || !job.version_rolling_allowed {
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
                } else {
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
                    out.extend(self.activate(prev, job.as_owned(), false)?);
                }
            }
            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH => {
                let prev: SetNewPrevHash = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "invalid job activation")?;
                if prev.channel_id != self.channel {
                    return Err("wrong mining channel".into());
                }
                let job = self
                    .future
                    .remove(&prev.job_id)
                    .ok_or("unknown job activation")?;
                self.future.clear();
                self.active.clear();
                self.previous_hash = Some(prev.as_owned());
                out.extend(self.activate(prev.as_owned(), job, true)?);
            }
            MESSAGE_TYPE_SET_TARGET => {
                let target: SetTarget =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid target")?;
                if target.channel_id != self.channel {
                    return Err("wrong mining channel".into());
                }
                self.target = target
                    .maximum_target
                    .as_ref()
                    .try_into()
                    .map_err(|_| "invalid target")?;
            }
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS => {
                let ack: SubmitSharesSuccess = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "invalid share response")?;
                if ack.channel_id != self.channel || ack.new_submits_accepted_count != 1 {
                    return Err("unexpected share acknowledgement".into());
                }
                let (id, _) = self
                    .pending
                    .remove(&ack.last_sequence_number)
                    .ok_or("unknown share acknowledgement")?;
                out.push(json!({"id":id,"result":true,"error":null}));
            }
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR => {
                let error: SubmitSharesError =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "invalid share error")?;
                if error.channel_id != self.channel {
                    return Err("wrong mining channel".into());
                }
                let (id, _) = self
                    .pending
                    .remove(&error.sequence_number)
                    .ok_or("unknown share error")?;
                let code = match error.error_code.as_ref() {
                    b"duplicate-share" => 22,
                    b"difficulty-too-low" => 23,
                    b"stale-share" | b"invalid-job-id" => 21,
                    _ => 20,
                };
                out.push(reject(id, code, "share rejected by validator"));
            }
            _ => return Err("unexpected upstream firmware message".into()),
        }
        Ok(out)
    }

    // #### PR #38
    // Immediate same-parent jobs preserve in-flight shares and use clean=false
    // on SV1. Only SetNewPrevHash flushes old jobs; each share keeps its version.
    fn activate(
        &mut self,
        prev: SetNewPrevHashOwned,
        job: NewExtendedMiningJobOwned,
        clean: bool,
    ) -> Result<Vec<Value>, String> {
        self.active.insert(job.job_id, job.version);
        while self.active.len() > MAX_ACTIVE_JOBS {
            self.active.pop_first();
        }
        let notify = sv2_to_sv1::build_sv1_notify_from_sv2(prev, job, clean)
            .map_err(|_| "job translation failed")?;
        self.notify = Some(to_json(notify.into())?);
        self.notifications()
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

    fn bridge() -> Bridge {
        let target = [255; 32];
        let prefix = [7; 16];
        Bridge::new(OpenExtendedMiningChannelSuccess {
            request_id: 1,
            channel_id: 3,
            group_channel_id: 0,
            target: (&target).into(),
            extranonce_size: 8,
            extranonce_prefix: prefix.as_slice().try_into().unwrap(),
        })
        .unwrap()
    }
    fn ready() -> Bridge {
        let mut bridge = bridge();
        bridge
            .request(json!({"id":1,"method":"mining.subscribe","params":[]}))
            .unwrap();
        bridge
            .request(json!({"id":2,"method":"mining.authorize","params":["worker", "unused"]}))
            .unwrap();
        bridge.active.insert(4, 0x20000000);
        bridge
    }
    fn submit(id: u64) -> Value {
        json!({"id":id,"method":"mining.submit","params":["worker","4","0000000000000000","00000001","00000002"]})
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
        assert!(bridge.upstream(job(5, None)).unwrap().is_empty());
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
        let first = bridge.upstream(parent).unwrap();
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
        assert!(bridge.upstream(set).unwrap().is_empty());
        let next = bridge.upstream(job(6, Some(1700000000))).unwrap();
        assert_eq!(next[0]["method"], "mining.set_difficulty");
        assert!((next[0]["params"][0].as_f64().unwrap() - 4096.0).abs() < 0.01);
        assert_eq!(next[1]["method"], "mining.notify");
        // Work in flight is kept: no clean_jobs, and the older job still counts.
        assert_eq!(next[1]["params"][8], false);
        assert!(bridge.active.contains_key(&5) && bridge.active.contains_key(&6));
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
        let replies = bridge.upstream(ack.clone()).unwrap();
        assert_eq!(replies, vec![json!({"id":9,"result":true,"error":null})]);
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
        let replies = bridge.upstream(reject).unwrap();
        assert_eq!(replies[0]["id"], 10);
        assert_eq!(replies[0]["error"][0], 22);
        assert!(bridge.pending.is_empty());
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
        assert_eq!(bridge.pending.len(), 1);
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
        assert_eq!(bridge.pending.len(), MAX_PENDING);
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
