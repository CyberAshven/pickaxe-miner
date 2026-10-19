//! #### PR #53: P2Pool's configuration from Pickaxe's node and payout, and
//! the supervisor that runs the node binary as a child process, restarts it
//! when it exits and tells whether it is running.
use super::{release::Installed, Settings};
use crate::config::{MiningNetwork, NodeZmq, RuntimeConfig};
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ports of the P2Pool node Pickaxe runs, apart from P2Pool's defaults
/// (3333, 3336, 6884, 46884) so it can sit beside a node run by hand and
/// beside Pickaxe's own ASIC server. Stratum and the API listen on this
/// computer only; Pickaxe's ASIC server is what devices connect to.
pub const SV1_PORT: u16 = 3360;
pub const SV2_PORT: u16 = 3361;
pub const P2P_PORT: u16 = 6860;
pub const API_PORT: u16 = 46860;

/// Pauses before starting P2Pool again after it exits.
const RESTART_DELAYS: [u64; 4] = [5, 10, 30, 60];
/// Running this long resets the pause.
const STABLE: Duration = Duration::from_secs(60);
/// Log lines kept for the screen.
const LOG_LINES: usize = 200;

/// Everything one start of P2Pool needs.
#[derive(Debug, Clone)]
pub struct Plan {
    pub binary: PathBuf,
    pub tag: String,
    pub home: PathBuf,
    pub network: MiningNetwork,
    pub settings: Settings,
    /// The BCH node's RPC URL without a login.
    pub node_url: String,
    pub login: Option<(String, String)>,
    pub zmq: String,
    /// The payout P2Pool's first jobs pay before any share exists.
    pub bootstrap_address: String,
    pub authority_secret: [u8; 32],
}

/// The plan for `cfg`'s network, node and payout with the installed binary.
pub fn plan(
    cfg: &RuntimeConfig,
    home: &Path,
    installed: &Installed,
    settings: &Settings,
) -> Result<Plan, String> {
    let node = cfg
        .node_url
        .as_deref()
        .ok_or("P2Pool needs your BCH node: give --node-rpc http://HOST:PORT")?;
    let (node_url, login) = crate::node::rpc_endpoint_and_login(node)?;
    let zmq = match &cfg.node_zmq {
        NodeZmq::At(url) => url.clone(),
        NodeZmq::Auto => crate::node::zmq_block_url(node).ok_or(
            "P2Pool needs the node's ZMQ block notices: set zmqpubhashblock in bitcoin.conf \
             or give --node-zmq tcp://HOST:PORT",
        )?,
        NodeZmq::Off => {
            return Err(
                "P2Pool needs the node's ZMQ block notices; --node-zmq off cannot run it".into(),
            )
        }
    };
    crate::config::validate_payout_address(cfg.network, &cfg.payout_address)
        .map_err(|_| "P2Pool needs a payout address for the selected network: give --address")?;
    std::fs::create_dir_all(home).map_err(|error| format!("{}: {error}", home.display()))?;
    let authority_secret =
        crate::stratum_v2::command::load_authority(&home.join("sv2-authority.key"))?;
    Ok(Plan {
        binary: installed.binary.clone(),
        tag: installed.tag.clone(),
        home: home.to_owned(),
        network: cfg.network,
        settings: settings.clone(),
        node_url,
        login,
        zmq,
        bootstrap_address: cfg.payout_address.clone(),
        authority_secret,
    })
}

/// The SV2 authority key pair in the reference implementation's text form,
/// as P2Pool reads them: the public key is Base58Check of version 1 and the
/// 32-byte x-only key; the secret is Base58Check of its 32 bytes.
pub fn authority_texts(secret: &[u8; 32]) -> Result<(String, String), String> {
    let public = crate::stratum_v2::server::authority_public(secret)?;
    let mut versioned = vec![1, 0];
    versioned.extend(public);
    Ok((
        stratum_core::bitcoin::base58::encode_check(&versioned),
        stratum_core::bitcoin::base58::encode_check(secret),
    ))
}

impl Plan {
    fn files(&self) -> (PathBuf, PathBuf, PathBuf) {
        let name = format!(
            "{}-{}",
            self.network.as_str(),
            self.settings.share_chain.as_str()
        );
        (
            self.home.join(format!("{}.toml", self.network.as_str())),
            self.home.join(format!("store-{name}.db")),
            self.home.join("logs"),
        )
    }

    /// Where devices of this computer's ASIC server reach P2Pool: SV2 with
    /// the authority key pinned.
    pub fn device_url(&self) -> Result<String, String> {
        let (public, _) = authority_texts(&self.authority_secret)?;
        Ok(format!("stratum2+tcp://127.0.0.1:{SV2_PORT}/{public}"))
    }

    /// P2Pool's config.toml. Secrets stay out of it: the SV2 secret key and
    /// the node's login go in the environment (`environment`).
    pub fn config_toml(&self) -> Result<String, String> {
        let (public, _) = authority_texts(&self.authority_secret)?;
        let (_, store, logs) = self.files();
        let path = |path: &Path| toml_string(&path.display().to_string().replace('\\', "/"));
        let peers = self
            .settings
            .peers
            .iter()
            .map(|peer| toml_string(peer))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "# Written by Pickaxe for P2Pool {tag} at each start; changes here are replaced.
# Pickaxe passes the node's login and the SV2 secret key in the environment.

[network]
listen_address = \"/ip4/0.0.0.0/tcp/{P2P_PORT}\"
dial_peers = [{peers}]
max_pending_incoming = 10
max_pending_outgoing = 10
max_established_incoming = 50
max_established_outgoing = 50
max_established_per_peer = 2
max_workbase_per_second = 10
max_userworkbase_per_second = 10
max_miningshare_per_second = 100
max_inventory_per_second = 100
max_transaction_per_second = 100
max_requests_per_second = 100
dial_timeout_secs = 30

[store]
path = {store}
background_task_frequency_hours = 24
pplns_ttl_days = 7

[stratum]
hostname = \"127.0.0.1\"
port = {SV1_PORT}
start_difficulty = 10000
minimum_difficulty = 100
bootstrap_address = {bootstrap}
zmqpubhashblock = {zmq}
network = \"signet\"
version_mask = \"1fffffe0\"
chain = \"bitcoin-cash\"
bitcoin_cash_network = \"{network}\"
bch_share_chain = \"{chain}\"
wait_for_chain_sync = {wait}
sv2_port = {SV2_PORT}
sv2_authority_public_key = {public}

[bitcoinrpc]
url = {node}

[logging]
file = {log}
console = true
level = \"info\"
stats_dir = {stats}

[api]
hostname = \"127.0.0.1\"
port = {API_PORT}
",
            tag = self.tag,
            store = path(&store),
            bootstrap = toml_string(&self.bootstrap_address),
            zmq = toml_string(&self.zmq),
            network = self.network.as_str(),
            chain = self.settings.share_chain.as_str(),
            // With no peers this node is the share chain; waiting for a
            // current chain would keep its ports closed for good.
            wait = !self.settings.peers.is_empty(),
            public = toml_string(&public),
            node = toml_string(&self.node_url),
            log = path(&logs.join(format!("{}.log", self.network.as_str()))),
            stats = path(&logs.join(format!("stats-{}", self.network.as_str()))),
        ))
    }

    /// The secrets P2Pool reads from its environment: `P2POOL_` and the
    /// key path with `__` between levels, as P2Pool's config loader and its
    /// tests read them (its sample config's `P2POOL__STRATUM__...` is
    /// ignored, which the live check on 2026-10-19 found).
    pub fn environment(&self) -> Result<Vec<(String, String)>, String> {
        let (_, secret) = authority_texts(&self.authority_secret)?;
        let mut env = vec![(
            "P2POOL_STRATUM__SV2_AUTHORITY_SECRET_KEY".to_owned(),
            secret,
        )];
        let (user, password) = self.login.clone().unwrap_or_default();
        env.push(("P2POOL_BITCOINRPC__USERNAME".into(), user));
        env.push(("P2POOL_BITCOINRPC__PASSWORD".into(), password));
        Ok(env)
    }

    /// Writes the configuration and returns how to start P2Pool with it.
    pub fn command(&self) -> Result<Command, String> {
        let (config, _, logs) = self.files();
        std::fs::create_dir_all(&logs).map_err(|error| format!("{}: {error}", logs.display()))?;
        super::write_atomically(&config, self.config_toml()?.as_bytes())?;
        Ok(Command {
            program: self.binary.clone(),
            args: vec!["--config".into(), config.display().to_string()],
            env: self.environment()?,
            dir: self.home.clone(),
        })
    }
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(ch))),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// How to start a child process.
#[derive(Debug, Clone)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub dir: PathBuf,
}

/// What P2Pool is doing, as its light shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Stopped,
    Starting,
    /// Its BCH node is not ready: still syncing, or no block template.
    WaitingForNode(String),
    /// The share chain is behind its peers; miners wait until it is current.
    SyncingShareChain,
    /// The API answers and the SV2 port accepts miners: green.
    Running {
        tip: Option<u64>,
        peers: Option<usize>,
    },
    /// It exited; Pickaxe starts it again after a pause.
    Restarting {
        reason: String,
        in_secs: u64,
    },
}

/// The light colours of Gupaxx's process tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Green,
    Yellow,
    Red,
    Grey,
}

impl State {
    pub fn light(&self) -> Light {
        match self {
            Self::Running { .. } => Light::Green,
            Self::Starting | Self::WaitingForNode(_) | Self::SyncingShareChain => Light::Yellow,
            Self::Restarting { .. } => Light::Red,
            Self::Stopped => Light::Grey,
        }
    }

    /// The summary without a countdown, so a line per change, not per second.
    pub fn headline(&self) -> String {
        match self {
            Self::Restarting { reason, .. } => format!("stopped ({reason}); starting again"),
            other => other.summary(),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    /// One line for screens.
    pub fn summary(&self) -> String {
        match self {
            Self::Stopped => "stopped".into(),
            Self::Starting => "starting".into(),
            Self::WaitingForNode(reason) => format!("waiting for the BCH node: {reason}"),
            Self::SyncingShareChain => "syncing the share chain from its peers".into(),
            Self::Running { tip, peers } => {
                let mut line = "running".to_owned();
                if let Some(tip) = tip {
                    line.push_str(&format!(" · share chain tip {tip}"));
                }
                if let Some(peers) = peers {
                    line.push_str(&format!(
                        " · {peers} peer{}",
                        if *peers == 1 { "" } else { "s" }
                    ));
                }
                line
            }
            Self::Restarting { reason, in_secs } => {
                format!("stopped ({reason}); starting again in {in_secs}s")
            }
        }
    }
}

/// What P2Pool's log said in this run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seen {
    node_syncing: bool,
    node_failed: Option<String>,
    share_chain_behind: bool,
    sv2_listening: bool,
    template_error: Option<(Instant, String)>,
}

impl Seen {
    /// Notes what one log line says.
    pub(crate) fn line(&mut self, line: &str) {
        if line.contains("Bitcoin node still syncing") {
            self.node_syncing = true;
        } else if let Some((_, error)) = line.split_once("Failed to verify Bitcoin node is ready:")
        {
            self.node_failed = Some(error.trim().to_owned());
        } else if line.contains("Share chain is not current") {
            self.share_chain_behind = true;
        } else if line.contains("Share chain is current") {
            self.share_chain_behind = false;
        } else if line.contains("Stratum V2 listening on") {
            self.sv2_listening = true;
            self.node_syncing = false;
        } else if let Some((_, error)) = line.split_once("Error polling block template:") {
            self.template_error = Some((Instant::now(), error.trim().to_owned()));
        }
    }
}

/// What P2Pool's API answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Probe {
    pub api: bool,
    pub tip: Option<u64>,
    pub peers: Option<usize>,
}

/// Asks P2Pool's API whether it is up, its share-chain tip and its peers.
fn probe(api_port: u16) -> Probe {
    let get = |path: &str| {
        super::http::get(
            &format!("http://127.0.0.1:{api_port}{path}"),
            "application/json",
            1 << 20,
            Duration::from_secs(2),
        )
        .ok()
        .filter(|response| response.status == 200)
    };
    let Some(_) = get("/health") else {
        return Probe::default();
    };
    let json = |path: &str| {
        get(path)
            .and_then(|response| serde_json::from_slice::<serde_json::Value>(&response.body).ok())
    };
    Probe {
        api: true,
        tip: json("/chain_info").and_then(|info| info["chain_tip_height"].as_u64()),
        peers: json("/peers").and_then(|peers| peers.as_array().map(Vec::len)),
    }
}

/// The state from what the log said and the API answered.
pub(crate) fn assess(seen: &Seen, probe: &Probe, now: Instant) -> State {
    if let Some((at, error)) = &seen.template_error {
        if now.duration_since(*at) < Duration::from_secs(60) {
            return State::WaitingForNode(format!("no block template ({error})"));
        }
    }
    if seen.sv2_listening && probe.api {
        return State::Running {
            tip: probe.tip,
            peers: probe.peers,
        };
    }
    if seen.node_syncing {
        return State::WaitingForNode("it is still syncing".into());
    }
    if seen.share_chain_behind {
        return State::SyncingShareChain;
    }
    State::Starting
}

/// Removes terminal colour codes from a log line.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !ch.is_control() || ch == '\t' {
            out.push(ch);
        }
    }
    out
}

struct Shared {
    state: State,
    log: VecDeque<String>,
    seen: Seen,
    running_since: Option<Instant>,
    restarts: u32,
}

/// Runs P2Pool, starts it again when it exits, and keeps its state.
pub struct Supervisor {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    restart: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Supervisor {
    /// Starts `command`, reading its state from its output and from its API
    /// on `api_port` (`None`: from its output and process alone).
    pub fn start(command: Command, api_port: Option<u16>) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            state: State::Starting,
            log: VecDeque::new(),
            seen: Seen::default(),
            running_since: None,
            restarts: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let restart = Arc::new(AtomicBool::new(false));
        let worker = {
            let (shared, stop, restart) = (shared.clone(), stop.clone(), restart.clone());
            std::thread::Builder::new()
                .name("pickaxe-p2pool".into())
                .spawn(move || supervise(&command, api_port, &shared, &stop, &restart))
                .ok()
        };
        Self {
            shared,
            stop,
            restart,
            worker,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> State {
        self.lock().state.clone()
    }

    /// How long it has been running (green), if it is.
    pub fn running_for(&self) -> Option<Duration> {
        self.lock().running_since.map(|since| since.elapsed())
    }

    pub fn restarts(&self) -> u32 {
        self.lock().restarts
    }

    /// The newest `count` lines P2Pool printed, oldest first.
    pub fn log(&self, count: usize) -> Vec<String> {
        let shared = self.lock();
        let skip = shared.log.len().saturating_sub(count);
        shared.log.iter().skip(skip).cloned().collect()
    }

    /// Stops P2Pool and starts it again now.
    pub fn restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
    }

    /// Stops P2Pool and waits for it.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

fn set(shared: &Mutex<Shared>, state: State) {
    let mut shared = lock(shared);
    if state.is_running() {
        shared.running_since.get_or_insert_with(Instant::now);
    } else {
        shared.running_since = None;
    }
    shared.state = state;
}

/// Keeps each line `stream` prints: in the screen's log and in what the
/// log said (`Seen`).
fn read_lines(stream: Box<dyn std::io::Read + Send>, shared: &Mutex<Shared>) {
    let mut reader = std::io::BufReader::new(stream);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let line = plain(String::from_utf8_lossy(&bytes).trim_end());
                if line.is_empty() {
                    continue;
                }
                let mut shared = lock(shared);
                shared.seen.line(&line);
                shared.log.push_back(line);
                while shared.log.len() > LOG_LINES {
                    shared.log.pop_front();
                }
            }
        }
    }
}

/// On Windows, puts `child` in a job object that ends its processes when
/// Pickaxe's last handle to it closes, so P2Pool stops with Pickaxe even
/// when Pickaxe crashes or is closed by force, and frees its ports. (On
/// other systems a forced stop of Pickaxe leaves P2Pool running.)
#[cfg(windows)]
fn tie_to_pickaxe(child: &std::process::Child) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    let job = pickaxe_job(|| {
        // SAFETY: plain Win32 calls with null optional arguments and a
        // zeroed, correctly sized limit structure; the handle is checked.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 {
                return 0;
            }
            job as usize
        }
    });
    // SAFETY: the job handle lives for the whole process; the child's
    // handle is valid while `child` is.
    job != 0 && unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) } != 0
}

#[cfg(not(windows))]
fn tie_to_pickaxe(_: &std::process::Child) -> bool {
    false
}

/// The job object every P2Pool joins, made once and never closed: its
/// handle closes only when Pickaxe's process ends.
#[cfg(windows)]
fn pickaxe_job(make: impl FnOnce() -> usize) -> usize {
    static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *JOB.get_or_init(make)
}

fn supervise(
    command: &Command,
    api_port: Option<u16>,
    shared: &Arc<Mutex<Shared>>,
    stop: &AtomicBool,
    restart: &AtomicBool,
) {
    let mut failures = 0usize;
    while !stop.load(Ordering::SeqCst) {
        restart.store(false, Ordering::SeqCst);
        lock(shared).seen = Seen::default();
        set(shared, State::Starting);
        let spawned = std::process::Command::new(&command.program)
            .args(&command.args)
            .envs(command.env.iter().map(|(key, value)| (key, value)))
            .current_dir(&command.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => {
                tie_to_pickaxe(&child);
                child
            }
            Err(error) => {
                let reason = format!("cannot start {}: {error}", command.program.display());
                if !pause(&reason, &mut failures, shared, stop) {
                    break;
                }
                continue;
            }
        };
        let streams: [Option<Box<dyn std::io::Read + Send>>; 2] = [
            child.stdout.take().map(|out| Box::new(out) as _),
            child.stderr.take().map(|err| Box::new(err) as _),
        ];
        let readers: Vec<_> = streams
            .into_iter()
            .flatten()
            .map(|stream| {
                let shared = Arc::clone(shared);
                std::thread::spawn(move || read_lines(stream, &shared))
            })
            .collect();
        let mut probed: Option<Instant> = None;
        let exit = loop {
            if stop.load(Ordering::SeqCst) || restart.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            match child.try_wait() {
                Ok(Some(status)) => break Some(status.to_string()),
                Ok(None) => {}
                Err(error) => break Some(error.to_string()),
            }
            if probed.is_none_or(|at| at.elapsed() >= Duration::from_secs(2)) {
                probed = Some(Instant::now());
                let answer = api_port.map(probe).unwrap_or_default();
                let state = assess(&lock(shared).seen, &answer, Instant::now());
                set(shared, state);
                if lock(shared)
                    .running_since
                    .is_some_and(|since| since.elapsed() >= STABLE)
                {
                    failures = 0;
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        // Stopped by Pickaxe: the readers end when the pipes close; an
        // exit of its own: wait for its last lines, which say why.
        let Some(exit) = exit else {
            continue;
        };
        for reader in readers {
            let _ = reader.join();
        }
        let reason = {
            let shared = lock(shared);
            match (&shared.seen.node_failed, shared.log.back()) {
                (Some(error), _) => format!("the BCH node is not ready: {error}"),
                (None, Some(line)) => {
                    format!("{exit}: {}", line.chars().take(160).collect::<String>())
                }
                (None, None) => exit,
            }
        };
        if !pause(&reason, &mut failures, shared, stop) {
            break;
        }
    }
    set(shared, State::Stopped);
}

/// Waits before the next start, showing why and for how long; false when
/// stopped meanwhile.
fn pause(reason: &str, failures: &mut usize, shared: &Mutex<Shared>, stop: &AtomicBool) -> bool {
    let delay = RESTART_DELAYS[(*failures).min(RESTART_DELAYS.len() - 1)];
    *failures += 1;
    lock(shared).restarts += 1;
    let until = Instant::now() + Duration::from_secs(delay);
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        set(
            shared,
            State::Restarting {
                reason: reason.to_owned(),
                in_secs: left.as_secs() + 1,
            },
        );
        std::thread::sleep(left.min(Duration::from_millis(200)));
    }
}

#[cfg(test)]
mod tests {
    use super::super::ShareChain;
    use super::*;

    fn plan_for(home: &Path, peers: Vec<String>, login: Option<(String, String)>) -> Plan {
        Plan {
            binary: home.join(super::super::release::BINARY),
            tag: "v0.0.1-alpha".into(),
            home: home.to_owned(),
            network: MiningNetwork::Chipnet,
            settings: Settings {
                auto_update: true,
                share_chain: ShareChain::Mini,
                peers,
            },
            node_url: "https://node.example:48332".into(),
            login,
            zmq: "tcp://node.example:28332".into(),
            bootstrap_address: "bchtest:qrzq5f9ltv70u4su7d40agd4nlnp8qlgqcma6x2tvp".into(),
            authority_secret: [7; 32],
        }
    }

    #[test]
    fn the_config_names_the_node_ports_and_chain_and_keeps_secrets_out() {
        let home = PathBuf::from("C:\\Users\\miner\\Pickaxe\\p2pool");
        let plan = plan_for(&home, vec![], Some(("p2pool".into(), "s3cret".into())));
        let toml = plan.config_toml().unwrap();
        let (public, secret) = authority_texts(&plan.authority_secret).unwrap();
        for expected in [
            "listen_address = \"/ip4/0.0.0.0/tcp/6860\"",
            "dial_peers = []",
            "path = \"C:/Users/miner/Pickaxe/p2pool/store-chipnet-mini.db\"",
            "hostname = \"127.0.0.1\"\nport = 3360",
            "bootstrap_address = \"bchtest:qrzq5f9ltv70u4su7d40agd4nlnp8qlgqcma6x2tvp\"",
            "zmqpubhashblock = \"tcp://node.example:28332\"",
            "chain = \"bitcoin-cash\"",
            "bitcoin_cash_network = \"chipnet\"",
            "bch_share_chain = \"mini\"",
            "wait_for_chain_sync = false",
            "sv2_port = 3361",
            &format!("sv2_authority_public_key = \"{public}\""),
            "url = \"https://node.example:48332\"",
            "file = \"C:/Users/miner/Pickaxe/p2pool/logs/chipnet.log\"",
            "port = 46860",
        ] {
            assert!(toml.contains(expected), "{expected}:\n{toml}");
        }
        assert!(!toml.contains(&secret) && !toml.contains("s3cret") && !toml.contains("p2pool\""));
        let env = plan.environment().unwrap();
        assert!(env.contains(&("P2POOL_STRATUM__SV2_AUTHORITY_SECRET_KEY".into(), secret)));
        assert!(env.contains(&("P2POOL_BITCOINRPC__USERNAME".into(), "p2pool".into())));
        assert!(env.contains(&("P2POOL_BITCOINRPC__PASSWORD".into(), "s3cret".into())));
        // Joining peers waits for their share chain.
        let joined = plan_for(&home, vec!["/ip4/203.0.113.7/tcp/6884".into()], None)
            .config_toml()
            .unwrap();
        assert!(joined.contains("dial_peers = [\"/ip4/203.0.113.7/tcp/6884\"]"));
        assert!(joined.contains("wait_for_chain_sync = true"));
        assert_eq!(toml_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn the_authority_keys_are_in_the_form_p2pool_reads() {
        let secret = [7u8; 32];
        let (public, secret_text) = authority_texts(&secret).unwrap();
        let public_bytes = stratum_core::bitcoin::base58::decode_check(&public).unwrap();
        assert_eq!(public_bytes.len(), 34);
        assert_eq!(public_bytes[..2], [1, 0]);
        assert_eq!(
            public_bytes[2..],
            crate::stratum_v2::server::authority_public(&secret).unwrap()
        );
        assert_eq!(
            stratum_core::bitcoin::base58::decode_check(&secret_text).unwrap(),
            secret
        );
        let plan = plan_for(Path::new("/tmp/p"), vec![], None);
        assert_eq!(
            plan.device_url().unwrap(),
            format!("stratum2+tcp://127.0.0.1:3361/{public}")
        );
        // No login: empty, so P2Pool's config needs none either.
        assert!(plan
            .environment()
            .unwrap()
            .contains(&("P2POOL_BITCOINRPC__USERNAME".into(), String::new())));
    }

    #[test]
    fn the_light_follows_the_log_and_the_api() {
        let now = Instant::now();
        let up = Probe {
            api: true,
            tip: Some(42),
            peers: Some(1),
        };
        let mut seen = Seen::default();
        assert_eq!(assess(&seen, &Probe::default(), now), State::Starting);
        seen.line("2026-10-19 INFO Bitcoin node still syncing, checking again in 30s...");
        assert!(matches!(assess(&seen, &up, now), State::WaitingForNode(_)));
        seen.line(
            "INFO Share chain is not current, waiting before accepting stratum connections...",
        );
        seen.line("INFO Stratum V2 listening on 127.0.0.1:3361");
        assert_eq!(
            assess(&seen, &up, now),
            State::Running {
                tip: Some(42),
                peers: Some(1)
            }
        );
        assert_eq!(assess(&seen, &up, now).light(), Light::Green);
        assert_eq!(
            assess(&seen, &up, now).summary(),
            "running · share chain tip 42 · 1 peer"
        );
        // The API down is not running, whatever the log said.
        assert_eq!(assess(&seen, &Probe::default(), now).light(), Light::Yellow);
        seen.line("ERROR Error polling block template: connection refused");
        let waiting = assess(&seen, &up, now);
        assert!(waiting
            .summary()
            .contains("no block template (connection refused)"));
        assert_eq!(waiting.light(), Light::Yellow);
        assert_eq!(
            assess(&seen, &up, now + Duration::from_secs(61)).light(),
            Light::Green
        );
        seen.line("ERROR Failed to verify Bitcoin node is ready: 401 Unauthorized");
        assert_eq!(seen.node_failed.as_deref(), Some("401 Unauthorized"));
        assert_eq!(
            plain("\u{1b}[32mINFO\u{1b}[0m Share chain is current"),
            "INFO Share chain is current"
        );
        assert_eq!(
            State::Restarting {
                reason: "x".into(),
                in_secs: 5
            }
            .light(),
            Light::Red
        );
    }

    /// One process (as P2Pool is) that prints `lines` and then exits, or
    /// keeps running.
    fn script(lines: &[&str], keep_running: bool) -> Command {
        let dir = std::env::temp_dir();
        if cfg!(windows) {
            let mut body: Vec<String> = lines
                .iter()
                .map(|line| format!("Write-Output '{line}'"))
                .collect();
            body.push(if keep_running {
                "Start-Sleep -Seconds 30".into()
            } else {
                "exit 3".into()
            });
            Command {
                program: "powershell".into(),
                args: vec!["-NoProfile".into(), "-Command".into(), body.join("; ")],
                env: vec![],
                dir,
            }
        } else {
            let mut body: Vec<String> = lines.iter().map(|line| format!("echo '{line}'")).collect();
            body.push(if keep_running {
                "exec sleep 30".into()
            } else {
                "exit 3".into()
            });
            Command {
                program: "sh".into(),
                args: vec!["-c".into(), body.join("; ")],
                env: vec![],
                dir,
            }
        }
    }

    fn wait_for(supervisor: &Supervisor, what: &str, test: impl Fn(&State) -> bool) -> State {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let state = supervisor.state();
            if test(&state) {
                return state;
            }
            assert!(Instant::now() < deadline, "{what}: still {state:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_p2pool_joins_the_job_that_ends_with_pickaxe() {
        use std::os::windows::io::AsRawHandle;
        let command = script(&[], true);
        let mut child = std::process::Command::new(&command.program)
            .args(&command.args)
            .spawn()
            .unwrap();
        assert!(tie_to_pickaxe(&child));
        let job = pickaxe_job(|| 0);
        assert_ne!(job, 0);
        let mut inside = 0;
        // SAFETY: valid process and job handles and an out-parameter.
        let asked = unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                child.as_raw_handle() as _,
                job as _,
                &mut inside,
            )
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(asked != 0 && inside != 0);
    }

    #[test]
    fn an_exiting_node_is_started_again_after_a_pause_and_says_why() {
        let supervisor = Supervisor::start(
            script(
                &["Failed to verify Bitcoin node is ready: no answer"],
                false,
            ),
            None,
        );
        let state = wait_for(&supervisor, "a restart", |state| {
            matches!(state, State::Restarting { .. })
        });
        let State::Restarting { reason, in_secs } = state else {
            unreachable!()
        };
        assert_eq!(reason, "the BCH node is not ready: no answer");
        assert!((1..=5).contains(&in_secs), "{in_secs}");
        assert_eq!(supervisor.restarts(), 1);
        assert_eq!(
            supervisor.log(5),
            vec!["Failed to verify Bitcoin node is ready: no answer".to_string()]
        );
        supervisor.stop();
    }

    #[test]
    fn a_node_whose_api_answers_after_sv2_opens_is_green_and_stops_on_request() {
        let port = super::super::http::tests::serve(
            (0..30)
                .flat_map(|_| {
                    [
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK".to_vec(),
                        b"HTTP/1.1 200 OK\r\nContent-Length: 26\r\n\r\n{\"chain_tip_height\": 1234}"
                            .to_vec(),
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n[{}]".to_vec(),
                    ]
                })
                .collect(),
        );
        let supervisor = Supervisor::start(
            script(&["Stratum V2 listening on 127.0.0.1:3361"], true),
            Some(port),
        );
        let state = wait_for(&supervisor, "green", State::is_running);
        assert_eq!(
            state,
            State::Running {
                tip: Some(1234),
                peers: Some(1)
            }
        );
        assert!(supervisor.running_for().is_some());
        let started = Instant::now();
        supervisor.stop();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
