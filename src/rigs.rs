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
    /// #### PR #40: a public GPU pool, where each rig names its payout.
    pub public: bool,
}

/// #### PR #40
/// The command each rig runs to join, once per address other computers can
/// reach this coordinator at (its local network and Tailscale addresses for
/// a wildcard listener). A public pool's rigs add their own payout, which a
/// rig checks against its network, so a Chipnet coordinator's command says
/// `--chipnet`. Then the same addresses as one line,
/// `stratum2+tcp://HOST:PORT/KEY`, which the setup's Join a GPU pool or farm
/// takes whole.
pub fn join_lines(
    summary: &RigSummary,
    network: crate::config::MiningNetwork,
    interfaces: crate::reach::Interfaces,
) -> Vec<(crate::reach::Place, String)> {
    let Ok(listen) = summary.listen.parse() else {
        return Vec::new();
    };
    let addresses = crate::reach::addresses(listen, interfaces);
    let one_line = addresses
        .iter()
        .map(|(place, address)| (*place, format!("stratum2+tcp://{address}/{}", summary.key)))
        .collect::<Vec<_>>();
    addresses
        .into_iter()
        .map(|(place, address)| {
            let network = match network {
                crate::config::MiningNetwork::Mainnet => "",
                crate::config::MiningNetwork::Chipnet => " --chipnet",
            };
            let mut command = format!(
                "pickaxe mine{network} --coordinator {address} --coordinator-key {}",
                summary.key
            );
            if summary.public {
                command.push_str(" --address YOUR_BCH_ADDRESS");
            }
            (place, command)
        })
        .chain(one_line)
        .collect()
}

/// One connected rig as the coordinator shows it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RigLine {
    pub name: String,
    pub gpus: usize,
    pub rate: f64,
    pub winners: u64,
    pub connected_secs: u64,
    /// #### PR #42: how long since the rig was last heard, its Pickaxe
    /// version, and each of its GPUs as its last report gave them.
    pub last_seen_secs: u64,
    pub version: String,
    pub devices: Vec<RigGpu>,
}

// #### PR #42: per-rig GPU health
// What: each rig's report carries its GPUs: name, engine and device, rate,
// temperature, fan, power, status, winners, rejected winners and the last
// error. The coordinator keeps at most 64 per rig, cleaned, and shows them
// under the rig with when it was last heard.
// Why: a farm's operator saw one line per rig and could not tell a hot,
// stopped or failing GPU from a healthy one.
// Look here if: a rig's GPUs are missing or wrong on the coordinator, or a
// rig report is refused as too large.
/// The most GPUs a coordinator keeps for one rig.
pub const MAX_RIG_GPUS: usize = 64;

/// #### PR #42: whether `text` is a coordinator key a rig can pin (a farm
/// system passes it as the pool's password).
pub fn is_coordinator_key(text: &str) -> bool {
    #[cfg(feature = "stratum-v2")]
    {
        net::decode_key(text).is_ok()
    }
    #[cfg(not(feature = "stratum-v2"))]
    {
        let _ = text;
        false
    }
}

/// #### PR #42: a rig's status file, every two seconds, for `mine watch`
/// and the farm systems: its role, version and start, whether it mines,
/// its coordinator, its rate and winners sent, and each GPU; never a key or
/// a payout.
pub fn rig_status_json(
    coordinator: Option<(&str, bool)>,
    mining: bool,
    rate: f64,
    winners_sent: u64,
    started: u64,
    gpus: &[RigGpu],
) -> serde_json::Value {
    serde_json::json!({
        "event": "status",
        "role": "rig",
        "version": env!("CARGO_PKG_VERSION"),
        "started": started,
        "state": match (coordinator, mining) {
            (None, _) => "waiting",
            (Some(_), true) => "mining",
            (Some(_), false) => "paused",
        },
        "coordinator": coordinator.map(|(address, _)| address),
        "backup": coordinator.is_some_and(|(_, backup)| backup),
        "current_rate": rate,
        "verified_winners": winners_sent,
        "rejected_winners": 0,
        "gpus": gpus
            .iter()
            .map(|gpu| serde_json::json!({
                "backend": gpu.backend,
                "device": gpu.device,
                "pci_bus": gpu.pci_bus,
                "name": gpu.name,
                "status": gpu.status,
                "active_rate": gpu.rate,
                "winners": gpu.winners,
                "rejected": gpu.rejected,
                "last_error": gpu.error,
                "gpu_telemetry": {
                    "temperature_c": gpu.temperature_c,
                    "fan_percent": gpu.fan_percent,
                    "power_watts": gpu.power_watts,
                },
            }))
            .collect::<Vec<_>>(),
    })
}
/// A GPU at or above this temperature is shown first.
pub const HOT_GPU_C: f64 = 85.0;

/// One GPU of a rig, as its report gives it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RigGpu {
    pub name: String,
    pub backend: String,
    pub device: u32,
    /// #### PR #42: the GPU's PCI bus, which farm systems match GPUs by.
    pub pci_bus: Option<u32>,
    pub rate: f64,
    pub temperature_c: Option<f64>,
    pub fan_percent: Option<f64>,
    pub power_watts: Option<f64>,
    /// "mining", "recovering" or "stopped".
    pub status: String,
    pub winners: u64,
    pub rejected: u64,
    pub error: Option<String>,
}

impl RigGpu {
    /// The report as a coordinator keeps it: text without control
    /// characters and capped (the name at 64, the engine at 16, the status at
    /// 16, the error at 120), readings finite and within their range.
    pub fn cleaned(self) -> Self {
        let text = |value: &str, limit: usize| -> String {
            value
                .chars()
                .filter(|ch| !ch.is_control())
                .take(limit)
                .collect()
        };
        let reading = |value: Option<f64>, top: f64| {
            value.filter(|value| value.is_finite() && (0.0..=top).contains(value))
        };
        Self {
            name: text(&self.name, 64),
            backend: text(&self.backend, 16),
            device: self.device,
            pci_bus: self.pci_bus.filter(|bus| *bus <= 0xff),
            rate: if self.rate.is_finite() {
                self.rate.max(0.0)
            } else {
                0.0
            },
            temperature_c: reading(self.temperature_c, 200.0),
            fan_percent: reading(self.fan_percent, 100.0),
            power_watts: reading(self.power_watts, 10_000.0),
            status: text(&self.status, 16),
            winners: self.winners,
            rejected: self.rejected,
            error: self
                .error
                .as_deref()
                .map(|error| text(error, 120))
                .filter(|error| !error.is_empty()),
        }
    }

    /// Whether the GPU needs a look: not mining, hot, with rejected winners
    /// or an error.
    pub fn troubled(&self) -> bool {
        self.status != "mining"
            || self
                .temperature_c
                .is_some_and(|degrees| degrees >= HOT_GPU_C)
            || self.rejected > 0
            || self.error.is_some()
    }

    /// Its health on one line, such as "61°C · fan 40% · 120 W".
    pub fn health(&self) -> String {
        let mut parts = Vec::new();
        if let Some(degrees) = self.temperature_c {
            parts.push(format!("{degrees:.0}°C"));
        }
        if let Some(fan) = self.fan_percent {
            parts.push(format!("fan {fan:.0}%"));
        }
        if let Some(watts) = self.power_watts {
            parts.push(format!("{watts:.0} W"));
        }
        if parts.is_empty() {
            "no readings".into()
        } else {
            parts.join(" · ")
        }
    }
}
// #### end PR #42 ####

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
    /// #### PR #32: the coordinator's token donation; a rig never mines
    /// below the token's minimum, whatever it is sent.
    #[serde(default)]
    donation: Option<crate::donation::TokenDonation>,
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
            donation: None,
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
            // The coordinator sets it from the job it gave the rig.
            payout: None,
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
    /// #### PR #32: the rig's own payout, which a public GPU pool pays.
    #[serde(default)]
    payout: Option<String>,
    /// #### PR #42: the rig's Pickaxe version.
    #[serde(default)]
    version: Option<String>,
}

/// #### PR #32
/// A public GPU pool: each rig mines for the payout it sends, and the
/// operator's fee is that share of each rig's mining time (after the
/// donation, which every rig applies itself), paying the fee address. A
/// PHOTON claim cannot split its reward, so the fee is work; nothing is held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicRigs {
    /// The fee in hundredths of a percent of each rig's time; 0 for none.
    pub fee_bps: u16,
    pub address: String,
}

/// The top bit of a job's generation marks the operator's fee window, so the
/// rig takes it as a new job and its winners say which job they solved.
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
const FEE_JOBS: u64 = 1 << 63;

/// A rig's share of time for the operator, on a 10-minute clock counted
/// while it is connected.
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
struct FeeClock {
    position: u64,
    last: std::time::Instant,
}

#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
impl FeeClock {
    const PERIOD_NS: u64 = 600_000_000_000;

    fn new(phase: u64) -> Self {
        Self {
            position: phase % Self::PERIOD_NS,
            last: std::time::Instant::now(),
        }
    }

    /// Advances the clock and says whether this is the fee window.
    fn window(&mut self, now: std::time::Instant, fee_bps: u16) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_nanos() as u64;
        self.last = now;
        self.position = (self.position + elapsed % Self::PERIOD_NS) % Self::PERIOD_NS;
        self.position < (u128::from(Self::PERIOD_NS) * u128::from(fee_bps) / 10_000) as u64
    }
}

/// The job a rig of a public pool mines: paying its own payout, or in the fee
/// window the operator's, marked in its generation.
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
fn rig_job(job: &MiningJob, payout: &str, public: &PublicRigs, window: bool) -> MiningJob {
    let mut job = job.clone();
    if window {
        job.payout_address = public.address.clone();
        job.generation_id |= FEE_JOBS;
    } else {
        job.payout_address = payout.to_owned();
    }
    job
}

/// A rig's regular report.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
struct RigStats {
    rate: f64,
    gpus: usize,
    winners: u64,
    /// #### PR #42: each GPU's health; older rigs send none, and older
    /// coordinators ignore it.
    devices: Vec<RigGpu>,
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
        /// The coordinator's token donation, sent with every job.
        donation: Option<crate::donation::TokenDonation>,
        /// #### PR #32: a public GPU pool.
        public: Option<PublicRigs>,
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
        /// #### PR #42: when the rig was last heard, its version and GPUs.
        heard: Instant,
        version: String,
        devices: Vec<RigGpu>,
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

        /// #### PR #32
        /// Makes this coordinator a public GPU pool: each rig mines for the
        /// payout it sends, and the operator's fee is that share of its time.
        pub fn set_public(&self, public: Option<PublicRigs>) {
            let mut state = lock(&self.state);
            state.public = public;
            state.version = state.version.wrapping_add(1);
        }

        /// #### PR #32
        /// The token donation rigs mine with; a change is sent to every rig
        /// at once, with the current job.
        pub fn set_donation(&self, donation: crate::donation::TokenDonation) {
            let mut state = lock(&self.state);
            if state.donation != Some(donation) {
                state.donation = Some(donation);
                state.version = state.version.wrapping_add(1);
            }
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
                        last_seen_secs: rig.heard.elapsed().as_secs(),
                        version: rig.version.clone(),
                        devices: rig.devices.clone(),
                    })
                    .collect(),
                public: state.public.is_some(),
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
                    heard: Instant::now(),
                    version: String::new(),
                    devices: Vec::new(),
                },
            );
            id
        };
        let mut sent = 0u64;
        let mut heard = Instant::now();
        // #### PR #32: a public pool's rig, its payout and its fee clock.
        let mut payout: Option<String> = None;
        let mut clock = FeeClock::new(rand::random());
        let mut sent_window = false;
        let _ = (|| -> Result<(), String> {
            while !stop.load(Ordering::Relaxed) {
                let pending = {
                    let state = lock(&state);
                    let window = match (&state.public, &payout) {
                        (Some(public), Some(_)) => clock.window(Instant::now(), public.fee_bps),
                        _ => false,
                    };
                    // A public pool sends work only once the rig names its
                    // payout, and again when the fee window opens or closes.
                    let ready = state.public.is_none() || payout.is_some();
                    (ready && (state.version != sent || window != sent_window)).then(|| {
                        let job = match (&state.public, &payout, &state.job) {
                            (Some(public), Some(payout), Some(job)) => {
                                Some(rig_job(job, payout, public, window))
                            }
                            (_, _, job) => job.clone(),
                        };
                        (state.version, job, state.donation, window)
                    })
                };
                if let Some((version, job, donation, window)) = pending {
                    if let Some(job) = job {
                        let wire = WireJob {
                            donation,
                            ..WireJob::from(&job)
                        };
                        sender.send(frame(JOB, &wire)?)?;
                    }
                    sent = version;
                    sent_window = window;
                }
                let Some(message) = receiver.receive(Duration::from_millis(200))? else {
                    if heard.elapsed() > QUIET {
                        return Err("rig went quiet".into());
                    }
                    continue;
                };
                heard = Instant::now();
                if let Some(row) = lock(&state).rigs.get_mut(&id) {
                    row.heard = heard;
                }
                let (kind, bytes) = read(message)?;
                match kind {
                    HELLO => {
                        let hello: RigHello =
                            serde_json::from_slice(&bytes).map_err(|_| "malformed rig hello")?;
                        let mut state = lock(&state);
                        // #### PR #32: a public pool pays the rig's own
                        // payout, so a rig without a valid one is turned away.
                        if state.public.is_some() {
                            let network = state
                                .job
                                .as_ref()
                                .map(|job| job.network)
                                .ok_or("the pool has no job yet")?;
                            let named = hello
                                .payout
                                .as_deref()
                                .map(|address| {
                                    crate::config::validate_payout_address(network, address)
                                })
                                .transpose()
                                .ok()
                                .flatten()
                                .ok_or("a public pool's rig needs --address")?;
                            payout = Some(named);
                        }
                        if let Some(row) = state.rigs.get_mut(&id) {
                            row.name = clean_name(&hello.name);
                            row.gpus = hello.gpus.min(64);
                            row.version = hello
                                .version
                                .as_deref()
                                .unwrap_or_default()
                                .chars()
                                .filter(|ch| !ch.is_control())
                                .take(32)
                                .collect();
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
                            row.devices = stats
                                .devices
                                .into_iter()
                                .take(MAX_RIG_GPUS)
                                .map(RigGpu::cleaned)
                                .collect();
                        }
                    }
                    WINNER => {
                        let wire: WireWinner =
                            serde_json::from_slice(&bytes).map_err(|_| "malformed rig winner")?;
                        let mut winner = VerifiedWinner::try_from(wire)?;
                        let mut state = lock(&state);
                        state.received = state.received.saturating_add(1);
                        if let Some(row) = state.rigs.get_mut(&id) {
                            row.winners = row.winners.saturating_add(1);
                        }
                        // #### PR #32: a public pool checks a winner against
                        // the exact job this rig was given, then claims it for
                        // that job's payout, under the shared generation.
                        let job = match (&state.public, &payout, &state.job) {
                            (Some(public), Some(payout), Some(job)) => Some(rig_job(
                                job,
                                payout,
                                public,
                                winner.generation_id & FEE_JOBS != 0,
                            )),
                            (None, _, job) => job.clone(),
                            _ => None,
                        };
                        let checked = job.as_ref().is_some_and(|job| {
                            crate::mining_job::verify_rig_winner(&winner, job).is_ok()
                        });
                        if checked && state.public.is_some() {
                            winner.generation_id &= !FEE_JOBS;
                            winner.payout = job.map(|job| job.payout_address);
                        }
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
    #[allow(clippy::too_many_arguments)]
    pub fn run_rig(
        coordinators: &[(String, String)],
        name: Option<&str>,
        payout: Option<&str>,
        gpus: &[GpuDevice],
        intensity: u8,
        json: bool,
        stop: Arc<AtomicBool>,
        status: Option<&Path>,
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
        // #### PR #42: each GPU's temperature, fan and power for the reports.
        let mut telemetry = crate::telemetry::LiveTelemetrySampler::start(
            crate::telemetry::telemetry_sources(gpus),
        );
        // #### PR #42: the rig's status file, for `mine watch` and farm
        // systems, every two seconds and while it waits.
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let save_status = |coordinator: Option<(&str, bool)>,
                           mining: bool,
                           rate: f64,
                           winners: u64,
                           devices: &[RigGpu]| {
            if let Some(path) = status {
                crate::mine_watch::save(
                    path,
                    &rig_status_json(coordinator, mining, rate, winners, started, devices),
                );
            }
        };
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
                                payout: payout.map(str::to_owned),
                                version: Some(env!("CARGO_PKG_VERSION").into()),
                            },
                        )?)?;
                        let mut last_stats = Instant::now() - STATS_EVERY;
                        let mut last_status = Instant::now();
                        let mut last_saved = Instant::now() - Duration::from_secs(2);
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
                                let donation = wire
                                    .donation
                                    .unwrap_or_else(|| MiningToken::Photon.donation_minimum());
                                let job = MiningJob::try_from(wire)?;
                                let (height, generation) = (job.height, job.generation_id);
                                // #### PR #32: the coordinator's donation,
                                // never below the token's minimum.
                                let policy =
                                    MiningToken::Photon.fee_policy_at(job.network, donation);
                                match search.as_ref() {
                                    None => {
                                        search = Some(SearchHandle::start_devices_with_work_fee(
                                            &devices, intensity, job, policy,
                                        )?);
                                    }
                                    Some(handle) => {
                                        if handle.work_fee().map(|current| current.scheme)
                                            != Some(policy.scheme)
                                        {
                                            handle.set_work_fee(policy)?;
                                        }
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
                                if last_saved.elapsed() >= Duration::from_secs(2) {
                                    save_status(
                                        Some((coordinators[index].0.as_str(), index > 0)),
                                        false,
                                        0.0,
                                        winners_sent,
                                        &rig_gpus(gpus, &[], &telemetry.snapshot_each()),
                                    );
                                    last_saved = Instant::now();
                                }
                                continue;
                            };
                            if last_saved.elapsed() >= Duration::from_secs(2) {
                                let snapshot = handle.snapshot();
                                save_status(
                                    Some((coordinators[index].0.as_str(), index > 0)),
                                    paused_since.is_none(),
                                    rate,
                                    winners_sent,
                                    &rig_gpus(gpus, &snapshot.gpus, &telemetry.snapshot_each()),
                                );
                                last_saved = Instant::now();
                            }
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
                                let snapshot = handle.snapshot();
                                rate = window_rate(&mut rate_window, snapshot.candidates);
                                sender.send(frame(
                                    STATS,
                                    &RigStats {
                                        rate,
                                        gpus: devices.len(),
                                        winners: winners_sent,
                                        devices: rig_gpus(
                                            gpus,
                                            &snapshot.gpus,
                                            &telemetry.snapshot_each(),
                                        ),
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
            save_status(
                None,
                false,
                0.0,
                winners_sent,
                &rig_gpus(gpus, &[], &telemetry.snapshot_each()),
            );
            let until = Instant::now() + backoff;
            while !stop.load(Ordering::Relaxed) && Instant::now() < until {
                thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
        if let Some(handle) = search {
            handle.stop();
        }
        telemetry.stop();
        Ok(())
    }

    /// #### PR #42: each GPU's report: the search's stats matched by engine
    /// and device, the telemetry in `gpus`' order.
    pub(super) fn rig_gpus(
        gpus: &[GpuDevice],
        stats: &[crate::search::GpuSearchStats],
        telemetry: &[crate::telemetry::GpuTelemetry],
    ) -> Vec<RigGpu> {
        gpus.iter()
            .enumerate()
            .map(|(index, gpu)| {
                let search = stats
                    .iter()
                    .find(|stats| stats.backend == gpu.backend && stats.device == gpu.index);
                let sample = telemetry.get(index);
                RigGpu {
                    name: gpu.name.clone(),
                    backend: gpu.backend.as_str().into(),
                    device: gpu.index,
                    pci_bus: gpu.pci.map(|pci| u32::from(pci.bus)),
                    rate: search.map_or(0.0, |stats| stats.active_rate),
                    temperature_c: sample.and_then(|sample| sample.temperature_c),
                    fan_percent: sample.and_then(|sample| sample.fan_percent),
                    power_watts: sample.and_then(|sample| sample.power_watts),
                    status: match search.map(|stats| stats.status) {
                        Some(crate::search::GpuStatus::Mining) => "mining",
                        Some(crate::search::GpuStatus::Recovering) => "recovering",
                        _ => "stopped",
                    }
                    .into(),
                    winners: search.map_or(0, |stats| stats.winners),
                    rejected: search.map_or(0, |stats| stats.rejected_winners),
                    error: search.and_then(|stats| stats.last_error.clone()),
                }
                .cleaned()
            })
            .collect()
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
        pub fn set_donation(&self, _donation: crate::donation::TokenDonation) {}
        pub fn set_public(&self, _public: Option<PublicRigs>) {}
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

    #[allow(clippy::too_many_arguments)]
    pub fn run_rig(
        _coordinators: &[(String, String)],
        _name: Option<&str>,
        _payout: Option<&str>,
        _gpus: &[crate::backend::GpuDevice],
        _intensity: u8,
        _json: bool,
        _stop: Arc<AtomicBool>,
        _status: Option<&std::path::Path>,
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
            payout: None,
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
                        payout: None,
                        version: Some("0.0.4".into()),
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
        // #### PR #32: a raised donation reaches the rig with the same job.
        let raised = crate::donation::TokenDonation::from_bps(600);
        hub.set_donation(raised);
        let resent = loop {
            if let Some(message) = receiver.receive(Duration::from_secs(5)).unwrap() {
                break message;
            }
        };
        let (_, bytes) = net::read(resent).unwrap();
        let wire: WireJob = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire.donation, Some(raised));
        assert_eq!(wire.generation_id, job.generation_id);
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

    /// #### PR #42: a GPU report with every field set.
    fn healthy_gpu(name: &str) -> RigGpu {
        RigGpu {
            name: name.into(),
            backend: "cuda".into(),
            device: 0,
            pci_bus: Some(1),
            rate: 1.5e9,
            temperature_c: Some(61.0),
            fan_percent: Some(40.0),
            power_watts: Some(120.0),
            status: "mining".into(),
            winners: 3,
            rejected: 0,
            error: None,
        }
    }

    // #### PR #42
    // What: a rig's report carries each GPU and reads back unchanged; a
    // report from an older rig (no devices) still parses, and an older
    // coordinator's reading of a new report ignores the devices.
    // Look here if: RigStats or RigGpu changes.
    #[test]
    fn rig_stats_carry_each_gpu_and_old_reports_still_parse() {
        let stats = RigStats {
            rate: 1.5e9,
            gpus: 1,
            winners: 3,
            devices: vec![healthy_gpu("RTX 5070 Ti")],
        };
        let bytes = serde_json::to_vec(&stats).unwrap();
        let back: RigStats = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, stats);
        let old: RigStats =
            serde_json::from_slice(br#"{"rate":1.0,"gpus":2,"winners":0}"#).unwrap();
        assert!(old.devices.is_empty());
        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct OlderCoordinator {
            rate: f64,
            gpus: usize,
            winners: u64,
        }
        let older: OlderCoordinator = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(older.gpus, 1);
    }

    // #### PR #42
    // What: a report from 64 GPUs with the longest names and errors still
    // fits one rig frame (B064K), and cleaning caps text, drops control
    // characters and out-of-range readings.
    // Look here if: RigGpu's limits or the frame size change.
    #[cfg(feature = "stratum-v2")]
    #[test]
    fn a_worst_case_report_fits_one_frame() {
        let worst = RigGpu {
            name: "\u{1b}[2J".repeat(40) + &"N".repeat(200),
            backend: "b".repeat(100),
            device: u32::MAX,
            pci_bus: Some(u32::MAX),
            rate: f64::MAX,
            temperature_c: Some(199.9),
            fan_percent: Some(100.0),
            power_watts: Some(9_999.9),
            status: "s".repeat(100),
            winners: u64::MAX,
            rejected: u64::MAX,
            error: Some("e".repeat(500)),
        }
        .cleaned();
        assert_eq!(worst.name.chars().count(), 64);
        assert!(!worst.name.contains('\u{1b}'));
        assert_eq!(worst.error.as_deref().map(str::len), Some(120));
        let stats = RigStats {
            rate: f64::MAX,
            gpus: MAX_RIG_GPUS,
            winners: u64::MAX,
            devices: vec![worst; MAX_RIG_GPUS],
        };
        assert!(net::frame(4, &stats).is_ok());
        let odd = RigGpu {
            temperature_c: Some(f64::NAN),
            fan_percent: Some(140.0),
            power_watts: Some(-3.0),
            rate: f64::INFINITY,
            error: Some(String::new()),
            ..healthy_gpu("x")
        }
        .cleaned();
        assert_eq!(
            (
                odd.temperature_c,
                odd.fan_percent,
                odd.power_watts,
                odd.rate,
                odd.error
            ),
            (None, None, None, 0.0, None)
        );
    }

    // #### PR #42
    // What: each GPU's report takes the search's stats of the same engine
    // and device (in any order) and the telemetry at its own position; a GPU
    // the search does not list reads as stopped; a hot GPU, one with
    // rejected winners or an error, or one not mining needs a look.
    // Look here if: rig_gpus or RigGpu::troubled changes.
    #[cfg(feature = "stratum-v2")]
    #[test]
    fn rig_gpus_matches_search_and_telemetry_by_device() {
        use crate::backend::{BackendKind, GpuDevice};
        use crate::search::{GpuSearchStats, GpuStatus};
        let device = |backend, index, name: &str| GpuDevice {
            index,
            name: name.into(),
            vendor: "v".into(),
            vram_bytes: None,
            backend,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: None,
        };
        let gpus = [
            device(BackendKind::Cuda, 0, "RTX"),
            device(BackendKind::Wgpu, 1, "Radeon"),
            device(BackendKind::Wgpu, 2, "Gone"),
        ];
        let stats = |backend, device, winners, rejected, status| GpuSearchStats {
            backend,
            device,
            candidates: 1,
            rate: 1.0,
            active_rate: f64::from(device) + 1.0,
            winners,
            rejected_winners: rejected,
            status,
            last_error: (rejected > 0).then(|| "rejected by the host".into()),
        };
        let search = [
            stats(BackendKind::Wgpu, 1, 2, 1, GpuStatus::Recovering),
            stats(BackendKind::Cuda, 0, 5, 0, GpuStatus::Mining),
        ];
        let telemetry = [
            crate::telemetry::GpuTelemetry {
                temperature_c: Some(88.0),
                ..Default::default()
            },
            crate::telemetry::GpuTelemetry {
                fan_percent: Some(30.0),
                ..Default::default()
            },
        ];
        let report = net::rig_gpus(&gpus, &search, &telemetry);
        assert_eq!(report.len(), 3);
        assert_eq!(
            (report[0].name.as_str(), report[0].winners, report[0].rate),
            ("RTX", 5, 1.0)
        );
        assert_eq!(report[0].temperature_c, Some(88.0));
        assert_eq!(report[1].status, "recovering");
        assert_eq!((report[1].rejected, report[1].fan_percent), (1, Some(30.0)));
        assert_eq!(report[2].status, "stopped");
        assert!(
            report.iter().all(RigGpu::troubled),
            "hot, rejected, stopped"
        );
        assert!(!healthy_gpu("ok").troubled());
        assert_eq!(healthy_gpu("ok").health(), "61°C · fan 40% · 120 W");
    }

    // #### PR #42
    // What: the coordinator keeps a rig's version and the GPUs of its last
    // report, at most 64, cleaned, and when it was last heard.
    // Look here if: the coordinator's STATS or HELLO handling changes.
    #[cfg(feature = "stratum-v2")]
    #[test]
    fn the_coordinator_keeps_each_rigs_gpus_and_last_seen() {
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
        hub.publish(sample_job());
        let stream = TcpStream::connect(hub.listen()).unwrap();
        let (mut sender, _receiver) = Session::initiate(stream, authority).unwrap().split();
        sender
            .send(
                net::frame(
                    1,
                    &RigHello {
                        name: "rack".into(),
                        gpus: 70,
                        payout: None,
                        version: Some("0.0.4\u{7}".into()),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let mut devices = vec![healthy_gpu("RTX"); 70];
        devices[0].name = "\u{1b}]0;owned\u{7}RTX".into();
        sender
            .send(
                net::frame(
                    4,
                    &RigStats {
                        rate: 1.0e9,
                        gpus: 70,
                        winners: 0,
                        devices,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hub
            .summary()
            .rigs
            .first()
            .is_none_or(|rig| rig.devices.is_empty())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let rig = hub.summary().rigs[0].clone();
        assert_eq!(rig.devices.len(), MAX_RIG_GPUS);
        assert_eq!(rig.devices[0].name, "]0;ownedRTX");
        assert_eq!(rig.version, "0.0.4");
        assert!(rig.last_seen_secs <= 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    // #### PR #42
    // What: a rig's status names its role, version, start, state,
    // coordinator (and whether it is the backup), rate, winners sent and
    // each GPU with its bus and readings; it never holds a key or a payout.
    // Look here if: rig_status_json changes.
    #[test]
    fn rig_status_json_has_no_key_or_payout() {
        let status = rig_status_json(
            Some(("192.0.2.1:3340", true)),
            true,
            1.5e9,
            4,
            1_700_000_000,
            &[healthy_gpu("RTX 3080")],
        );
        assert_eq!(status["role"], "rig");
        assert_eq!(status["state"], "mining");
        assert_eq!(status["coordinator"], "192.0.2.1:3340");
        assert_eq!(status["backup"], true);
        assert_eq!(status["verified_winners"], 4);
        assert_eq!(status["gpus"][0]["pci_bus"], 1);
        assert_eq!(status["gpus"][0]["gpu_telemetry"]["temperature_c"], 61.0);
        let text = status.to_string();
        assert!(!text.contains("key") && !text.contains("payout"), "{text}");
        assert_eq!(
            rig_status_json(None, false, 0.0, 0, 0, &[])["state"],
            "waiting"
        );
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

    // #### PR #32
    #[test]
    fn a_public_pool_rig_mines_for_itself_and_the_operator_in_its_fee_window() {
        let job = sample_job();
        let public = PublicRigs {
            fee_bps: 200,
            address: "operator".into(),
        };
        let own = rig_job(&job, "rig", &public, false);
        assert_eq!(
            (own.payout_address.as_str(), own.generation_id),
            ("rig", job.generation_id)
        );
        let fee = rig_job(&job, "rig", &public, true);
        assert_eq!(fee.payout_address, "operator");
        assert_eq!(fee.generation_id, job.generation_id | FEE_JOBS);
        // 2% of a 10-minute clock: 12 seconds.
        let start = std::time::Instant::now();
        let mut clock = FeeClock {
            position: 0,
            last: start,
        };
        assert!(clock.window(start, 200));
        assert!(clock.window(start + std::time::Duration::from_secs(11), 200));
        assert!(!clock.window(start + std::time::Duration::from_secs(13), 200));
        assert!(!clock.window(start + std::time::Duration::from_secs(14), 0));
    }

    #[cfg(feature = "stratum-v2")]
    #[test]
    fn a_public_pool_claims_each_rigs_winners_to_the_job_it_was_given() {
        use crate::stratum_v2::transport::Session;
        use std::{net::TcpStream, time::Duration};
        let dir = std::env::temp_dir().join(format!(
            "pickaxe-rigs-public-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let hub = RigHub::start("127.0.0.1:0".parse().unwrap(), &dir.join("rigs-key")).unwrap();
        let authority = net::decode_key(hub.key()).unwrap();
        let job = sample_job();
        let operator = job.payout_address.clone();
        let rig_payout = crate::config::reprefix_p2pkh_payout(
            &crate::reward::p2pkh_cashaddr_from_public_key(
                &secp256k1::PublicKey::from_secret_key(
                    &secp256k1::SecretKey::from_secret_bytes([9; 32]).unwrap(),
                )
                .serialize(),
            )
            .unwrap(),
            job.network,
        )
        .unwrap();
        hub.publish(job.clone());
        // The whole clock is the fee window, so the rig mines for the operator.
        hub.set_public(Some(PublicRigs {
            fee_bps: 10_000,
            address: operator.clone(),
        }));
        let connect = |payout: Option<String>| {
            let stream = TcpStream::connect(hub.listen()).unwrap();
            let (mut sender, receiver) = Session::initiate(stream, authority).unwrap().split();
            sender
                .send(
                    net::frame(
                        1,
                        &RigHello {
                            name: "public".into(),
                            gpus: 1,
                            payout,
                            version: None,
                        },
                    )
                    .unwrap(),
                )
                .unwrap();
            (sender, receiver)
        };
        // A rig that names no payout gets no work and is turned away.
        let (_, mut nameless) = connect(None);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match nameless.receive(Duration::from_millis(100)) {
                Ok(Some(_)) => panic!("a nameless rig got a job"),
                Ok(None) => assert!(std::time::Instant::now() < deadline, "not turned away"),
                Err(_) => break,
            }
        }
        let (mut sender, mut receiver) = connect(Some(rig_payout.clone()));
        let received = loop {
            if let Some(message) = receiver.receive(Duration::from_secs(5)).unwrap() {
                break message;
            }
        };
        let (_, bytes) = net::read(received).unwrap();
        let wire: WireJob = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire.payout_address, operator);
        assert_eq!(wire.generation_id, job.generation_id | FEE_JOBS);
        // A winner for that job is queued for the claim path under the shared
        // generation and the operator's payout.
        let rig_job = MiningJob::try_from(wire).unwrap();
        let winner = crate::mining_job::tests::solved_winner(&rig_job, [3; 32]);
        sender
            .send(net::frame(3, &WireWinner::from(&winner)).unwrap())
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !hub.has_winner() {
            assert!(std::time::Instant::now() < deadline, "winner not queued");
            std::thread::sleep(Duration::from_millis(20));
        }
        let queued = hub.take_winners();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].generation_id, job.generation_id);
        assert_eq!(queued[0].payout.as_deref(), Some(operator.as_str()));
        // Out of the fee window, the rig's work pays the rig.
        hub.set_public(Some(PublicRigs {
            fee_bps: 0,
            address: operator.clone(),
        }));
        let resent = loop {
            if let Some(message) = receiver.receive(Duration::from_secs(5)).unwrap() {
                break message;
            }
        };
        let (_, bytes) = net::read(resent).unwrap();
        let wire: WireJob = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire.payout_address, rig_payout);
        assert_eq!(wire.generation_id, job.generation_id);
        let _ = std::fs::remove_dir_all(dir);
    }

    // #### PR #40
    #[test]
    fn the_coordinator_shows_the_command_each_rig_runs() {
        use crate::reach::{Interfaces, Place};
        let interfaces = Interfaces {
            local: Some("192.168.0.160".parse().unwrap()),
            tailscale: Some("100.101.102.103".parse().unwrap()),
        };
        let mut summary = RigSummary {
            listen: "0.0.0.0:3340".into(),
            key: "KEY".into(),
            ..RigSummary::default()
        };
        let mainnet = crate::config::MiningNetwork::Mainnet;
        assert_eq!(
            join_lines(&summary, mainnet, interfaces),
            [
                (
                    Place::LocalNetwork,
                    "pickaxe mine --coordinator 192.168.0.160:3340 --coordinator-key KEY".into()
                ),
                (
                    Place::Tailscale,
                    "pickaxe mine --coordinator 100.101.102.103:3340 --coordinator-key KEY".into()
                ),
                (
                    Place::LocalNetwork,
                    "stratum2+tcp://192.168.0.160:3340/KEY".into()
                ),
                (
                    Place::Tailscale,
                    "stratum2+tcp://100.101.102.103:3340/KEY".into()
                ),
            ]
        );
        // A public pool's rigs name their own payout, checked against the
        // network; a loopback coordinator takes rigs on this computer only.
        summary.public = true;
        summary.listen = "127.0.0.1:3340".into();
        assert_eq!(
            join_lines(&summary, crate::config::MiningNetwork::Chipnet, interfaces),
            [
                (
                    Place::ThisComputer,
                    "pickaxe mine --chipnet --coordinator 127.0.0.1:3340 --coordinator-key KEY --address YOUR_BCH_ADDRESS".into()
                ),
                (
                    Place::ThisComputer,
                    "stratum2+tcp://127.0.0.1:3340/KEY".into()
                ),
            ]
        );
    }

    #[test]
    fn rig_names_cannot_disturb_the_terminal() {
        assert_eq!(clean_name("rig\u{1b}[2Jone\n"), "rig[2Jone");
        assert_eq!(clean_name("   "), "rig");
        assert_eq!(clean_name(&"x".repeat(100)).len(), 64);
    }
}
