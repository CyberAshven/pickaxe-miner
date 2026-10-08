//! #### PR #32
//! GPU rigs that mine as one. The coordinator (`mine --rigs-listen`) is the
//! miner's own normal process: it keeps the job source, payout, claim journal
//! and broadcast, and shares its current PHOTON job with rigs over Pickaxe's
//! Stratum V2 Noise transport, authenticated by a key only it holds. Rigs
//! (`mine --coordinator`) mine that job on every local GPU and send winners
//! back; only the coordinator claims, after checking each winner and rebuilding
//! its transaction from its own job and payouts. Every rig signs with its own
//! search key, so no nonce ranges are needed. No outside service is involved.

use crate::{
    config::MiningNetwork,
    mining_job::{MiningJob, VerifiedWinner},
};
use serde::{Deserialize, Serialize};

/// What the coordinator shows about its rigs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RigSummary {
    pub listen: String,
    pub key: String,
    pub connected: usize,
    pub gpus: usize,
    pub rate: f64,
    pub winners: u64,
    pub rejected: u64,
    /// One line per connected rig, in connection order.
    pub rigs: Vec<RigLine>,
}

/// One connected rig as the coordinator shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RigLine {
    pub name: String,
    pub gpus: usize,
    pub rate: f64,
    pub winners: u64,
    pub connected_secs: u64,
}

/// #### PR #32
/// A rig's rate since its previous report, from the candidates its GPUs have
/// tried in all: only the main miner's runtime fills `current_rate`, so a rig
/// measures its own. The first report has no window and says 0.
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
fn window_rate(previous: &mut Option<(std::time::Instant, u64)>, candidates: u64) -> f64 {
    let now = std::time::Instant::now();
    let rate = previous.map_or(0.0, |(at, before)| {
        candidates.saturating_sub(before) as f64 / now.duration_since(at).as_secs_f64().max(0.001)
    });
    *previous = Some((now, candidates));
    rate
}

/// A rig's name as sent, without control characters that could disturb a
/// terminal, and at most 64 characters.
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    if name.trim().is_empty() {
        "rig".into()
    } else {
        name
    }
}

/// A job on its way to a rig. Large integers travel as text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WireJob {
    network: String,
    height: u32,
    baton_txid: String,
    baton_vout: u32,
    baton_height: u32,
    baton_value_sats: u64,
    relay_fee_sats_per_kb: u64,
    age: u32,
    target_le_hex: String,
    token_amount: String,
    reward_raw: String,
    payout_address: String,
    source_identity: String,
    generation_id: u64,
}

impl From<&MiningJob> for WireJob {
    fn from(job: &MiningJob) -> Self {
        Self {
            network: job.network.as_str().to_owned(),
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            baton_height: job.baton_height,
            baton_value_sats: job.baton_value_sats,
            relay_fee_sats_per_kb: job.relay_fee_sats_per_kb,
            age: job.age,
            target_le_hex: job.target_le_hex.clone(),
            token_amount: job.token_amount.to_string(),
            reward_raw: job.reward_raw.to_string(),
            payout_address: job.payout_address.clone(),
            source_identity: job.source_identity.clone(),
            generation_id: job.generation_id,
        }
    }
}

impl TryFrom<WireJob> for MiningJob {
    type Error = String;

    fn try_from(wire: WireJob) -> Result<Self, String> {
        Ok(Self {
            network: MiningNetwork::parse(&wire.network)?,
            height: wire.height,
            baton_txid: wire.baton_txid,
            baton_vout: wire.baton_vout,
            baton_height: wire.baton_height,
            baton_value_sats: wire.baton_value_sats,
            relay_fee_sats_per_kb: wire.relay_fee_sats_per_kb,
            age: wire.age,
            target_le_hex: wire.target_le_hex,
            token_amount: wire
                .token_amount
                .parse()
                .map_err(|_| "rig job has an invalid token amount")?,
            reward_raw: wire
                .reward_raw
                .parse()
                .map_err(|_| "rig job has an invalid reward")?,
            payout_address: wire.payout_address,
            source_identity: wire.source_identity,
            generation_id: wire.generation_id,
        })
    }
}

/// A winner on its way back to the coordinator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct WireWinner {
    generation_id: u64,
    height: u32,
    baton_txid: String,
    baton_vout: u32,
    job_reward_raw: String,
    nonce: u32,
    digest: String,
    public_key: String,
    signature: String,
    transaction: String,
}

impl From<&VerifiedWinner> for WireWinner {
    fn from(winner: &VerifiedWinner) -> Self {
        Self {
            generation_id: winner.generation_id,
            height: winner.height,
            baton_txid: winner.baton_txid.clone(),
            baton_vout: winner.baton_vout,
            job_reward_raw: winner.job_reward_raw.to_string(),
            nonce: winner.nonce,
            digest: hex::encode(winner.digest),
            public_key: hex::encode(winner.public_key),
            signature: hex::encode(winner.signature),
            transaction: hex::encode(&winner.transaction),
        }
    }
}

fn fixed_hex<const N: usize>(text: &str, what: &str) -> Result<[u8; N], String> {
    hex::decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("rig winner has an invalid {what}"))
}

impl TryFrom<WireWinner> for VerifiedWinner {
    type Error = String;

    fn try_from(wire: WireWinner) -> Result<Self, String> {
        Ok(Self {
            generation_id: wire.generation_id,
            height: wire.height,
            baton_txid: wire.baton_txid,
            baton_vout: wire.baton_vout,
            job_reward_raw: wire
                .job_reward_raw
                .parse()
                .map_err(|_| "rig winner has an invalid reward")?,
            nonce: wire.nonce,
            digest: fixed_hex(&wire.digest, "digest")?,
            public_key: fixed_hex(&wire.public_key, "public key")?,
            signature: fixed_hex(&wire.signature, "signature")?,
            transaction: hex::decode(&wire.transaction)
                .map_err(|_| "rig winner has an invalid transaction")?,
        })
    }
}

/// A rig introducing itself.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
struct RigHello {
    name: String,
    gpus: usize,
}

/// A rig's regular report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
struct RigStats {
    rate: f64,
    gpus: usize,
    winners: u64,
}

#[cfg(feature = "stratum-v2")]
pub use net::{run_rig, RigHub};

#[cfg(feature = "stratum-v2")]
mod net {
    use super::*;
    use crate::{
        backend::{BackendKind, GpuDevice},
        config::MiningToken,
        search::{RuntimeCommand as SearchCommand, SearchHandle},
        stratum_v2::{server::authority_public, transport::Session},
    };
    use std::{
        collections::{BTreeMap, VecDeque},
        io::Write,
        net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
        path::Path,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex, MutexGuard,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use stratum_core::{
        binary_sv2::{self, B064K},
        codec_sv2::{EncodableFrame, MessageFrame, SerializedFrame},
    };

    /// The Stratum V2 extension type that carries Pickaxe's rig messages.
    const EXTENSION: u16 = 0x5043;
    const HELLO: u8 = 1;
    const JOB: u8 = 2;
    const WINNER: u8 = 3;
    const STATS: u8 = 4;
    /// #### PR #32: room for a farm; each rig is one coordinator thread.
    const MAX_RIGS: usize = 1024;
    const MAX_QUEUED_WINNERS: usize = 16;
    /// A rig reports every few seconds; this long without a word closes it.
    const QUIET: Duration = Duration::from_secs(30);
    const STATS_EVERY: Duration = Duration::from_secs(5);
    /// A rig resumes on its own if no new job follows a winner, so a winner
    /// the coordinator rejected cannot leave its GPUs idle.
    const RESUME_AFTER_WINNER: Duration = Duration::from_secs(20);

    pub(super) fn frame(kind: u8, value: &impl Serialize) -> Result<SerializedFrame, String> {
        let bytes = serde_json::to_vec(value).map_err(|_| "cannot encode a rig message")?;
        let payload: B064K<'_> = bytes
            .as_slice()
            .try_into()
            .map_err(|_| "rig message is too large")?;
        let frame = MessageFrame::from_message(payload, kind, EXTENSION, false)
            .map_err(|_| "rig message exceeds the frame size")?;
        let mut encoded = vec![0; frame.encoded_length()];
        frame
            .encode_into(&mut encoded)
            .map_err(|_| "cannot encode a rig message")?;
        SerializedFrame::from_bytes(encoded).map_err(|_| "cannot encode a rig message".into())
    }

    pub(super) fn read(mut frame: SerializedFrame) -> Result<(u8, Vec<u8>), String> {
        let header = frame.header();
        if header.ext_type_without_channel_msg() != EXTENSION || header.channel_msg() {
            return Err("unexpected message on a rig link".into());
        }
        let kind = header.msg_type();
        let payload: B064K =
            binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed rig message")?;
        Ok((kind, payload.as_ref().to_vec()))
    }

    /// The coordinator's public key as rigs pin it: the same encoding as the
    /// SV2 server's authority key (version 1, x-only key, Base58Check).
    pub(super) fn encode_key(public: &[u8; 32]) -> String {
        let mut encoded = vec![1, 0];
        encoded.extend(public);
        stratum_core::bitcoin::base58::encode_check(&encoded)
    }

    pub(super) fn decode_key(text: &str) -> Result<[u8; 32], String> {
        let bytes = stratum_core::bitcoin::base58::decode_check(text.trim())
            .map_err(|_| "the coordinator key is not valid")?;
        match bytes.split_first_chunk::<2>() {
            Some(([1, 0], key)) => key
                .try_into()
                .map_err(|_| "the coordinator key is not valid".into()),
            _ => Err("the coordinator key is not valid".into()),
        }
    }

    #[derive(Default)]
    struct HubState {
        job: Option<MiningJob>,
        version: u64,
        winners: VecDeque<VerifiedWinner>,
        rigs: BTreeMap<u64, RigRow>,
        next_id: u64,
        received: u64,
        rejected: u64,
    }

    struct RigRow {
        name: String,
        gpus: usize,
        rate: f64,
        winners: u64,
        since: Instant,
    }

    fn lock(state: &Mutex<HubState>) -> MutexGuard<'_, HubState> {
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The coordinator side: shares the current job with every rig and
    /// collects their winners after checking them.
    pub struct RigHub {
        state: Arc<Mutex<HubState>>,
        stop: Arc<AtomicBool>,
        accept: Option<JoinHandle<()>>,
        listen: String,
        key: String,
    }

    impl RigHub {
        /// Listens for rigs. The key file beside the config identifies this
        /// coordinator; it is created on first use and never leaves the machine.
        pub fn start(listen: SocketAddr, key_path: &Path) -> Result<Self, String> {
            let secret = crate::stratum_v2::command::load_authority(key_path)?;
            let public = authority_public(&secret)?;
            let listener = TcpListener::bind(listen).map_err(|_| "cannot bind the rig listener")?;
            let bound = listener
                .local_addr()
                .map_err(|_| "cannot read the rig listener")?;
            listener
                .set_nonblocking(true)
                .map_err(|_| "cannot configure the rig listener")?;
            let state = Arc::new(Mutex::new(HubState::default()));
            let stop = Arc::new(AtomicBool::new(false));
            let accept = {
                let state = Arc::clone(&state);
                let stop = Arc::clone(&stop);
                thread::Builder::new()
                    .name("pickaxe-rig-listener".into())
                    .spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            match listener.accept() {
                                Ok((stream, _)) => {
                                    // #### PR #32: on Windows an accepted
                                    // socket inherits the listener's
                                    // non-blocking mode, and the handshake's
                                    // first read would fail at once whenever
                                    // the rig's bytes came a moment later.
                                    if lock(&state).rigs.len() >= MAX_RIGS
                                        || stream.set_nonblocking(false).is_err()
                                    {
                                        continue;
                                    }
                                    let state = Arc::clone(&state);
                                    let stop = Arc::clone(&stop);
                                    let _ =
                                        thread::Builder::new().name("pickaxe-rig".into()).spawn(
                                            move || serve_rig(stream, public, secret, state, stop),
                                        );
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    thread::sleep(Duration::from_millis(50))
                                }
                                Err(_) => thread::sleep(Duration::from_millis(500)),
                            }
                        }
                    })
                    .map_err(|_| "cannot start the rig listener")?
            };
            Ok(Self {
                state,
                stop,
                accept: Some(accept),
                listen: bound.to_string(),
                key: encode_key(&public),
            })
        }

        /// The key rigs pass as `--coordinator-key`.
        pub fn key(&self) -> &str {
            &self.key
        }

        pub fn listen(&self) -> &str {
            &self.listen
        }

        /// The generation rigs are mining, if any job was shared.
        pub fn published_generation(&self) -> Option<u64> {
            lock(&self.state).job.as_ref().map(|job| job.generation_id)
        }

        /// Shares a job with every rig; a generation already shared is not
        /// sent again.
        pub fn publish(&self, job: MiningJob) {
            let mut state = lock(&self.state);
            if state.job.as_ref().map(|current| current.generation_id) != Some(job.generation_id) {
                state.job = Some(job);
                state.version = state.version.wrapping_add(1);
            }
        }

        pub fn has_winner(&self) -> bool {
            !lock(&self.state).winners.is_empty()
        }

        /// The checked winners rigs sent since the last call.
        pub fn take_winners(&self) -> Vec<VerifiedWinner> {
            lock(&self.state).winners.drain(..).collect()
        }

        pub fn summary(&self) -> RigSummary {
            let state = lock(&self.state);
            RigSummary {
                listen: self.listen.clone(),
                key: self.key.clone(),
                connected: state.rigs.len(),
                gpus: state.rigs.values().map(|rig| rig.gpus).sum(),
                rate: state.rigs.values().map(|rig| rig.rate).sum(),
                winners: state.received,
                rejected: state.rejected,
                rigs: state
                    .rigs
                    .values()
                    .map(|rig| RigLine {
                        name: rig.name.clone(),
                        gpus: rig.gpus,
                        rate: rig.rate,
                        winners: rig.winners,
                        connected_secs: rig.since.elapsed().as_secs(),
                    })
                    .collect(),
            }
        }
    }

    impl Drop for RigHub {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(accept) = self.accept.take() {
                let _ = accept.join();
            }
        }
    }

    fn serve_rig(
        stream: TcpStream,
        public: [u8; 32],
        secret: [u8; 32],
        state: Arc<Mutex<HubState>>,
        stop: Arc<AtomicBool>,
    ) {
        let Ok(session) = Session::accept(stream, &public, &secret) else {
            return;
        };
        let (mut sender, mut receiver) = session.split();
        let id = {
            let mut state = lock(&state);
            state.next_id = state.next_id.wrapping_add(1);
            let id = state.next_id;
            state.rigs.insert(
                id,
                RigRow {
                    name: "rig".into(),
                    gpus: 0,
                    rate: 0.0,
                    winners: 0,
                    since: Instant::now(),
                },
            );
            id
        };
        let mut sent = 0u64;
        let mut heard = Instant::now();
        let _ = (|| -> Result<(), String> {
            while !stop.load(Ordering::Relaxed) {
                let pending = {
                    let state = lock(&state);
                    (state.version != sent).then(|| (state.version, state.job.clone()))
                };
                if let Some((version, job)) = pending {
                    if let Some(job) = job {
                        sender.send(frame(JOB, &WireJob::from(&job))?)?;
                    }
                    sent = version;
                }
                let Some(message) = receiver.receive(Duration::from_millis(200))? else {
                    if heard.elapsed() > QUIET {
                        return Err("rig went quiet".into());
                    }
                    continue;
                };
                heard = Instant::now();
                let (kind, bytes) = read(message)?;
                match kind {
                    HELLO => {
                        let hello: RigHello =
                            serde_json::from_slice(&bytes).map_err(|_| "malformed rig hello")?;
                        if let Some(row) = lock(&state).rigs.get_mut(&id) {
                            row.name = clean_name(&hello.name);
                            row.gpus = hello.gpus.min(64);
                        }
                    }
                    STATS => {
                        let stats: RigStats =
                            serde_json::from_slice(&bytes).map_err(|_| "malformed rig report")?;
                        if let Some(row) = lock(&state).rigs.get_mut(&id) {
                            row.gpus = stats.gpus.min(64);
                            row.rate = if stats.rate.is_finite() {
                                stats.rate.max(0.0)
                            } else {
                                0.0
                            };
                        }
                    }
                    WINNER => {
                        let wire: WireWinner =
                            serde_json::from_slice(&bytes).map_err(|_| "malformed rig winner")?;
                        let winner = VerifiedWinner::try_from(wire)?;
                        let mut state = lock(&state);
                        state.received = state.received.saturating_add(1);
                        if let Some(row) = state.rigs.get_mut(&id) {
                            row.winners = row.winners.saturating_add(1);
                        }
                        let checked = state.job.as_ref().is_some_and(|job| {
                            crate::mining_job::verify_rig_winner(&winner, job).is_ok()
                        });
                        if checked && state.winners.len() < MAX_QUEUED_WINNERS {
                            state.winners.push_back(winner);
                        } else {
                            state.rejected = state.rejected.saturating_add(1);
                        }
                    }
                    _ => return Err("unknown rig message".into()),
                }
            }
            Ok(())
        })();
        sender.close();
        receiver.close();
        lock(&state).rigs.remove(&id);
    }

    type Link = (
        crate::stratum_v2::transport::Sender,
        crate::stratum_v2::transport::Receiver,
    );

    /// Connects to the first reachable coordinator, in the order given: the
    /// first is the main one, the others are backups.
    pub(super) fn connect_first(
        coordinators: &[(String, [u8; 32])],
    ) -> Result<(usize, Link), String> {
        let mut last = String::from("no coordinator given");
        for (index, (address, authority)) in coordinators.iter().enumerate() {
            match connect(address, *authority) {
                Ok(link) => return Ok((index, link)),
                Err(error) => last = format!("{address}: {error}"),
            }
        }
        Err(last)
    }

    fn connect(coordinator: &str, authority: [u8; 32]) -> Result<Link, String> {
        let address = coordinator
            .to_socket_addrs()
            .map_err(|_| "cannot resolve the coordinator address")?
            .next()
            .ok_or("cannot resolve the coordinator address")?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
            .map_err(|_| "cannot reach the coordinator")?;
        Ok(Session::initiate(stream, authority)?.split())
    }

    fn report(json: bool, event: &str, detail: serde_json::Value) {
        if json {
            let mut line = serde_json::json!({"event": event});
            if let (Some(line), Some(detail)) = (line.as_object_mut(), detail.as_object()) {
                line.extend(detail.clone());
            }
            println!("{line}");
        } else {
            println!("rig {event}: {detail}");
        }
        let _ = std::io::stdout().flush();
    }

    /// The rig side: mines the coordinator's jobs on every local GPU and sends
    /// winners back. A rig never talks to the chain and never claims.
    pub fn run_rig(
        coordinators: &[(String, String)],
        name: Option<&str>,
        gpus: &[GpuDevice],
        intensity: u8,
        json: bool,
        stop: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let coordinators = coordinators
            .iter()
            .map(|(address, key)| Ok((address.clone(), decode_key(key)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let devices: Vec<(BackendKind, usize)> = gpus
            .iter()
            .map(|gpu| (gpu.backend, gpu.index as usize))
            .collect();
        if devices.is_empty() {
            return Err("no GPU selected for mining".into());
        }
        // #### PR #32: `--rig-name`, or the computer's name (a Linux
        // service has no HOSTNAME variable, so /etc/hostname too).
        let name = name
            .map(str::to_owned)
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .or_else(|| std::env::var("HOSTNAME").ok())
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_else(|| "rig".into())
            .chars()
            .take(64)
            .collect::<String>();
        // The GPUs this rig mines on, so its operator can check them.
        report(
            json,
            "gpus",
            serde_json::json!({
                "name": name,
                "gpus": gpus
                    .iter()
                    .map(|gpu| format!("{} ({}:{})", gpu.name, gpu.backend.as_str(), gpu.index))
                    .collect::<Vec<_>>(),
            }),
        );
        let mut search: Option<SearchHandle> = None;
        let mut backoff = Duration::from_secs(1);
        let mut winners_sent = 0u64;
        while !stop.load(Ordering::Relaxed) {
            match connect_first(&coordinators) {
                Ok((index, (mut sender, mut receiver))) => {
                    backoff = Duration::from_secs(1);
                    report(
                        json,
                        "connected",
                        serde_json::json!({
                            "coordinator": coordinators[index].0,
                            "backup": index > 0,
                        }),
                    );
                    let link = (|| -> Result<(), String> {
                        sender.send(frame(
                            HELLO,
                            &RigHello {
                                name: name.clone(),
                                gpus: devices.len(),
                            },
                        )?)?;
                        let mut last_stats = Instant::now() - STATS_EVERY;
                        let mut last_status = Instant::now();
                        let mut rate_window = None;
                        let mut rate = 0.0;
                        let mut paused_since: Option<Instant> = None;
                        while !stop.load(Ordering::Relaxed) {
                            if let Some(message) = receiver.receive(Duration::from_millis(200))? {
                                let (kind, bytes) = read(message)?;
                                if kind != JOB {
                                    return Err("unexpected message from the coordinator".into());
                                }
                                let wire: WireJob = serde_json::from_slice(&bytes)
                                    .map_err(|_| "malformed job from the coordinator")?;
                                let job = MiningJob::try_from(wire)?;
                                let (height, generation) = (job.height, job.generation_id);
                                match search.as_ref() {
                                    None => {
                                        let policy = MiningToken::Photon.fee_policy(job.network);
                                        search = Some(SearchHandle::start_devices_with_work_fee(
                                            &devices, intensity, job, policy,
                                        )?);
                                    }
                                    Some(handle) => {
                                        handle.replace_job(job)?;
                                        let _ = handle.apply_control(SearchCommand::Resume);
                                    }
                                }
                                paused_since = None;
                                report(
                                    json,
                                    "job",
                                    serde_json::json!({"height": height, "generation": generation}),
                                );
                            }
                            let Some(handle) = search.as_ref() else {
                                continue;
                            };
                            for winner in handle.drain_winners() {
                                sender.send(frame(WINNER, &WireWinner::from(&winner))?)?;
                                winners_sent = winners_sent.saturating_add(1);
                                paused_since = Some(Instant::now());
                                report(
                                    json,
                                    "winner",
                                    serde_json::json!({"height": winner.height, "nonce": winner.nonce}),
                                );
                            }
                            if paused_since
                                .is_some_and(|since| since.elapsed() >= RESUME_AFTER_WINNER)
                            {
                                let _ = handle.apply_control(SearchCommand::Resume);
                                paused_since = None;
                            }
                            if last_stats.elapsed() >= STATS_EVERY {
                                rate = window_rate(&mut rate_window, handle.snapshot().candidates);
                                sender.send(frame(
                                    STATS,
                                    &RigStats {
                                        rate,
                                        gpus: devices.len(),
                                        winners: winners_sent,
                                    },
                                )?)?;
                                last_stats = Instant::now();
                            }
                            if last_status.elapsed() >= Duration::from_secs(10) {
                                report(
                                    json,
                                    "status",
                                    serde_json::json!({
                                        "rate": crate::telemetry::format_hash_rate(rate),
                                        "gpus": devices.len(),
                                        "winners_sent": winners_sent,
                                    }),
                                );
                                last_status = Instant::now();
                            }
                        }
                        Ok(())
                    })();
                    sender.close();
                    receiver.close();
                    if let Some(handle) = search.as_ref() {
                        // Without the coordinator the job may go stale.
                        let _ = handle.apply_control(SearchCommand::Pause);
                    }
                    if let Err(error) = link {
                        report(json, "disconnected", serde_json::json!({"reason": error}));
                    }
                }
                Err(error) => report(json, "waiting", serde_json::json!({"reason": error})),
            }
            let until = Instant::now() + backoff;
            while !stop.load(Ordering::Relaxed) && Instant::now() < until {
                thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
        if let Some(handle) = search {
            handle.stop();
        }
        Ok(())
    }
}

#[cfg(not(feature = "stratum-v2"))]
pub use stub::{run_rig, RigHub};

#[cfg(not(feature = "stratum-v2"))]
mod stub {
    use super::*;
    use std::{
        net::SocketAddr,
        path::Path,
        sync::{atomic::AtomicBool, Arc},
    };

    const UNAVAILABLE: &str = "GPU rigs need a build with --features stratum-v2";

    /// Without the Stratum V2 transport, rigs are unavailable.
    pub struct RigHub;

    impl RigHub {
        pub fn start(_listen: SocketAddr, _key_path: &Path) -> Result<Self, String> {
            Err(UNAVAILABLE.into())
        }
        pub fn key(&self) -> &str {
            ""
        }
        pub fn listen(&self) -> &str {
            ""
        }
        pub fn published_generation(&self) -> Option<u64> {
            None
        }
        pub fn publish(&self, _job: MiningJob) {}
        pub fn has_winner(&self) -> bool {
            false
        }
        pub fn take_winners(&self) -> Vec<VerifiedWinner> {
            Vec::new()
        }
        pub fn summary(&self) -> RigSummary {
            RigSummary::default()
        }
    }

    pub fn run_rig(
        _coordinators: &[(String, String)],
        _name: Option<&str>,
        _gpus: &[crate::backend::GpuDevice],
        _intensity: u8,
        _json: bool,
        _stop: Arc<AtomicBool>,
    ) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_job() -> MiningJob {
        crate::mining_job::tests::easy_job()
    }

    #[test]
    fn jobs_and_winners_cross_the_wire_unchanged() {
        let job = sample_job();
        let wire: WireJob =
            serde_json::from_slice(&serde_json::to_vec(&WireJob::from(&job)).unwrap()).unwrap();
        let back = MiningJob::try_from(wire).unwrap();
        assert_eq!(format!("{back:?}"), format!("{job:?}"));
        let winner = VerifiedWinner {
            generation_id: 7,
            height: 1_000,
            baton_txid: "11".repeat(32),
            baton_vout: 0,
            job_reward_raw: u128::MAX,
            nonce: 9,
            digest: [3; 32],
            public_key: [2; 33],
            signature: [4; 64],
            transaction: vec![5; 300],
        };
        let wire: WireWinner =
            serde_json::from_slice(&serde_json::to_vec(&WireWinner::from(&winner)).unwrap())
                .unwrap();
        assert_eq!(VerifiedWinner::try_from(wire).unwrap(), winner);
        let mut broken = WireWinner::from(&winner);
        broken.signature.pop();
        assert!(VerifiedWinner::try_from(broken).is_err());
    }

    #[cfg(feature = "stratum-v2")]
    #[test]
    fn a_rig_receives_jobs_and_only_checked_winners_reach_the_claim_path() {
        use crate::stratum_v2::transport::Session;
        use std::{net::TcpStream, time::Duration};
        let dir = std::env::temp_dir().join(format!(
            "pickaxe-rigs-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = RigHub::start("127.0.0.1:0".parse().unwrap(), &dir.join("rigs-key")).unwrap();
        let authority = net::decode_key(hub.key()).unwrap();
        let job = sample_job();
        hub.publish(job.clone());
        let stream = TcpStream::connect(hub.listen()).unwrap();
        let (mut sender, mut receiver) = Session::initiate(stream, authority).unwrap().split();
        sender
            .send(
                net::frame(
                    1,
                    &RigHello {
                        name: "test".into(),
                        gpus: 2,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let received = loop {
            if let Some(message) = receiver.receive(Duration::from_secs(5)).unwrap() {
                break message;
            }
        };
        let (kind, bytes) = net::read(received).unwrap();
        assert_eq!(kind, 2);
        let wire: WireJob = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire, WireJob::from(&job));
        // A real winner for the shared job is queued for the claim path.
        let winner = crate::mining_job::tests::solved_winner(&job, [3; 32]);
        sender
            .send(net::frame(3, &WireWinner::from(&winner)).unwrap())
            .unwrap();
        // A forged one (its digest does not match its transaction) is not.
        let mut forged = winner.clone();
        forged.digest = [0; 32];
        sender
            .send(net::frame(3, &WireWinner::from(&forged)).unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hub.summary().winners < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let summary = hub.summary();
        assert_eq!((summary.connected, summary.gpus), (1, 2));
        assert_eq!((summary.winners, summary.rejected), (2, 1));
        assert_eq!(summary.rigs.len(), 1);
        assert_eq!(summary.rigs[0].name, "test");
        assert_eq!(summary.rigs[0].winners, 2);
        assert_eq!(hub.take_winners(), vec![winner]);
        assert!(!hub.has_winner());
        // A rig pinning another key does not accept this coordinator.
        let other = crate::stratum_v2::server::authority_public(
            &crate::stratum_v2::command::load_authority(&dir.join("other-key")).unwrap(),
        )
        .unwrap();
        let stranger = TcpStream::connect(hub.listen()).unwrap();
        assert!(Session::initiate(stranger, other).is_err());
        // A rig falls back to a backup when the main coordinator is down.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let down = closed.local_addr().unwrap().to_string();
        drop(closed);
        let (index, _) = net::connect_first(&[
            (down.clone(), authority),
            (hub.listen().to_owned(), authority),
        ])
        .unwrap();
        assert_eq!(index, 1);
        assert!(net::connect_first(&[(down, authority)]).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    // #### PR #32
    #[test]
    fn a_rig_measures_its_rate_between_reports() {
        let mut window = None;
        assert_eq!(window_rate(&mut window, 1_000), 0.0);
        let (at, _) = window.unwrap();
        // Pretend the previous report was two seconds ago.
        window = Some((at - std::time::Duration::from_secs(2), 1_000));
        let rate = window_rate(&mut window, 9_000);
        assert!((3_900.0..=4_000.0).contains(&rate), "{rate}");
        // A search restarted with fewer candidates never reads negative.
        assert_eq!(window_rate(&mut window, 10), 0.0);
    }

    #[test]
    fn rig_names_cannot_disturb_the_terminal() {
        assert_eq!(clean_name("rig\u{1b}[2Jone\n"), "rig[2Jone");
        assert_eq!(clean_name("   "), "rig");
        assert_eq!(clean_name(&"x".repeat(100)).len(), 64);
    }
}
