use crate::{
    backend::{BackendKind, DeviceSelection, GpuDevice},
    config::{
        ConnectionKind, MiningNetwork, MiningProfiles, MiningToken, RuntimeConfig, SavedConfig,
        SharedSources,
    },
    protocol::ProofRule,
    runtime::{RuntimeEvent, RuntimeSnapshot, RuntimeSupervisor, SupervisorState},
    search::GpuStatus,
};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, Gauge, GraphType, List, ListItem, Paragraph,
        Wrap,
    },
    Frame, Terminal, TerminalOptions, Viewport,
};
use std::{
    collections::VecDeque,
    io::{self, Stdout},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

const DRAW_INTERVAL: Duration = Duration::from_millis(200);
const STATUS_LOG_INTERVAL: Duration = Duration::from_secs(10);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const EVENT_HISTORY_CAP: usize = 96;
/// Spacing of chart history samples.
const HISTORY_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// One hour of chart history.
const HISTORY_CAP: usize = 720;
/// Height of one stacked chart window: borders plus three plot rows.
const CHART_MIN_ROWS: u16 = 5;
/// Extra rows the bottom chart needs for the shared time axis.
const CHART_AXIS_ROWS: u16 = 2;
/// Height of the note shown for a metric the GPU does not report.
const CHART_NOTE_ROWS: u16 = 3;
/// Runtime rows (with borders) kept when charts need the space.
const RUNTIME_MIN_ROWS: u16 = 5;
const COMMAND_HISTORY_CAP: usize = 32;
const BENCHMARK_TERMINAL_WIDTH: u16 = 120;
const BENCHMARK_TERMINAL_HEIGHT: u16 = 40;

type PickaxeTerminal = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug, Clone)]
pub struct SetupResult {
    pub config: RuntimeConfig,
    /// The GPUs to mine on.
    pub gpus: Vec<GpuDevice>,
    pub profile_name: String,
    /// #### PR #40: the BCH ASIC server to start instead of GPU mining.
    pub server: Option<ServerSetup>,
}

/// #### PR #40
/// The server setup starts: the ASIC server solo on the miner's own node,
/// SV1 devices at someone's pool, a public ASIC pool, or a public GPU pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerSetup {
    Solo,
    JoinPool {
        address: String,
        key: String,
    },
    Public {
        fee: crate::donation::bch::BchDonation,
        mode: crate::donation::bch::FeeMode,
        /// `None` is the payout address.
        address: Option<String>,
        /// The pool's name in its blocks' coinbase; `None` writes none.
        tag: Option<String>,
    },
    /// #### PR #40: this computer's GPUs join someone's GPU pool or farm as
    /// a rig of its coordinator (`HOST:PORT` and the coordinator's key).
    JoinGpuPool {
        address: String,
        key: String,
    },
    /// A public GPU pool: this computer coordinates other people's rigs,
    /// each mining for its own address; the fee is a share of each rig's
    /// mining time, since a token claim pays one address.
    GpuPool {
        fee: crate::donation::bch::BchDonation,
        /// `None` is the payout address.
        address: Option<String>,
    },
}

#[derive(Default)]
pub struct SetupOverrides {
    pub network: Option<MiningNetwork>,
    pub token: Option<String>,
    pub intensity: Option<u8>,
    pub fulcrum: Option<String>,
    pub node_rpc: Option<String>,
    pub source: Option<String>,
    /// GPUs chosen on the command line, as (engine, ordinal) pairs.
    pub gpus: Option<Vec<(BackendKind, u32)>>,
}

impl SetupOverrides {
    fn apply(&self, config: &mut RuntimeConfig) -> Result<(), String> {
        if let Some(network) = self.network {
            config.set_network(network);
        }
        if let Some(token) = &self.token {
            config.set_token(token)?;
        }
        if let Some(intensity) = self.intensity {
            config.set_intensity(intensity)?;
        }
        if let Some(url) = &self.fulcrum {
            config.set_fulcrum_url(url)?;
        }
        if let Some(url) = &self.node_rpc {
            config.set_node_url(url)?;
        }
        if let Some(source) = &self.source {
            config.set_source(source)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    Profiles,
    Hardware,
    Network,
    Token,
    Settings,
    Connections,
    /// The GPU list, opened from the Settings page.
    Gpus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiningMode {
    Gpu,
    Asic,
    /// #### PR #40: run a pool for other miners.
    Pool,
}

/// #### PR #40: an ASIC's work goes to the miner's own node, or to a pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsicMining {
    Solo,
    JoinPool,
}

/// #### PR #40: the kind of pool to join or run; P2Pool v2 is coming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolKind {
    Normal,
    P2PoolV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupAction {
    Continue,
    Complete,
    Cancel,
}

/// One row of the settings page. Rows never change each other's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsRow {
    Gpu,
    AsicTarget,
    /// #### PR #40: solo or join a pool; the pool's kind, address and key;
    /// a public pool's fee, its source and its address.
    Mining,
    PoolKind,
    PoolAddress,
    PoolKey,
    PoolFee,
    FeeFrom,
    FeeAddress,
    /// #### PR #40: the pool's name in its blocks.
    PoolName,
    Address,
    Intensity,
    Fulcrum,
    Node,
    ProfileName,
    Start,
}

/// The text field being typed into, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextField {
    ProfileRename,
    Address,
    ProfileName,
    Connection,
    PoolAddress,
    PoolKey,
    FeeAddress,
    PoolName,
}

/// What an ASIC can mine: BCH today (merge-mined tokens as they appear);
/// ASIC-exclusive tokens come later.
/// #### PR #40: what a pool's miners mine: ASIC pools now, GPU pools later.
const POOL_TARGETS: [&str; 2] = ["ASIC pool", "GPU pool"];

const ASIC_TARGETS: [&str; 2] = [
    "BCH + all merge-mined tokens",
    "ASIC-exclusive token (SAFA, ...)",
];

struct SetupFlow {
    step: SetupStep,
    profiles: MiningProfiles,
    profile_path: Option<PathBuf>,
    profile_selected: usize,
    active_profile: Option<usize>,
    profile_name_input: String,
    profile_delete_pending: bool,
    sources: SharedSources,
    sources_path: Option<PathBuf>,
    base_config: RuntimeConfig,
    overrides: SetupOverrides,
    mode: MiningMode,
    asic_target: usize,
    /// #### PR #40: where an ASIC mines, the pool it joins or runs.
    asic_mining: AsicMining,
    /// #### PR #40: whether GPUs mine alone or join a GPU pool or farm.
    gpu_join: bool,
    pool_kind: PoolKind,
    /// A pool to run is for ASICs (0) or GPUs (1).
    pool_target: usize,
    join_address: String,
    join_key: String,
    pool_fee: crate::donation::bch::BchDonation,
    pool_fee_mode: crate::donation::bch::FeeMode,
    /// Empty: the payout address.
    pool_fee_address: String,
    pool_tag: String,
    token_input: String,
    token_selected: usize,
    settings_row: usize,
    editing: Option<TextField>,
    text_input: String,
    connection_kind: ConnectionKind,
    connection_selected: usize,
    devices: Vec<GpuDevice>,
    /// How `devices` was listed. With `Auto` it holds each physical GPU once
    /// and a saved choice names GPUs by position, as `--device` does; with a
    /// backend it is that backend's list and a choice names its ordinals.
    prefer: BackendKind,
    /// Which of `devices` mine.
    chosen: Vec<bool>,
    /// The GPU under the cursor on the GPU list.
    selected: usize,
    config: RuntimeConfig,
    status_line: String,
    /// #### PR #40
    /// The check for a BCH node on this computer, started when the BCH node
    /// list opens.
    local_node: LocalNodeCheck,
    /// How the check looks; tests replace it.
    probe_local_node: fn(MiningNetwork) -> crate::node::LocalNode,
}

/// #### PR #40
/// The check for a BCH node on this computer runs in the background, so the
/// BCH node list never waits on it (a refused connection takes about two
/// seconds on Windows).
enum LocalNodeCheck {
    Idle,
    Running(MiningNetwork, mpsc::Receiver<crate::node::LocalNode>),
    Done(MiningNetwork, crate::node::LocalNode),
}

/// The GPUs that mine unless chosen otherwise: every discrete GPU, or every
/// GPU when none is discrete.
fn default_choice(devices: &[GpuDevice]) -> Vec<bool> {
    let any_discrete = devices.iter().any(|device| !device.integrated);
    devices
        .iter()
        .map(|device| !device.integrated || !any_discrete)
        .collect()
}

impl SetupFlow {
    /// Creates a SetupFlow for the terminal interface; `default_gpus` start
    /// ticked.
    fn new(
        config: RuntimeConfig,
        devices: Vec<GpuDevice>,
        prefer: BackendKind,
        default_gpus: &[GpuDevice],
    ) -> Result<Self, String> {
        if devices.is_empty() {
            return Err("no validated production GPU device found".into());
        }
        let ticked: Vec<bool> = devices
            .iter()
            .map(|device| {
                default_gpus
                    .iter()
                    .any(|gpu| gpu.backend == device.backend && gpu.index == device.index)
            })
            .collect();
        let chosen = if ticked.contains(&true) {
            ticked
        } else {
            default_choice(&devices)
        };
        let selected = chosen.iter().position(|chosen| *chosen).unwrap_or(0);
        Ok(Self {
            step: SetupStep::Hardware,
            profiles: MiningProfiles::default(),
            profile_path: None,
            profile_selected: 0,
            active_profile: None,
            profile_name_input: String::new(),
            profile_delete_pending: false,
            sources: SharedSources::default(),
            sources_path: None,
            base_config: config.clone(),
            overrides: SetupOverrides::default(),
            mode: MiningMode::Gpu,
            asic_target: 0,
            asic_mining: AsicMining::Solo,
            gpu_join: false,
            pool_kind: PoolKind::Normal,
            pool_target: 0,
            join_address: String::new(),
            join_key: String::new(),
            pool_fee: "1".parse().expect("fee"),
            pool_fee_mode: crate::donation::bch::FeeMode::Coinbase,
            pool_fee_address: String::new(),
            pool_tag: String::new(),
            token_input: String::new(),
            token_selected: 0,
            settings_row: 0,
            editing: None,
            text_input: String::new(),
            connection_kind: ConnectionKind::Fulcrum,
            connection_selected: 0,
            devices,
            prefer,
            chosen,
            selected,
            config,
            status_line: String::new(),
            local_node: LocalNodeCheck::Idle,
            #[cfg(not(test))]
            probe_local_node: crate::node::probe_local_node,
            #[cfg(test)]
            probe_local_node: |_| crate::node::LocalNode::Missing,
        })
    }

    /// #### PR #40
    /// Starts looking for a BCH node on this computer.
    fn check_local_node(&mut self) {
        let (send, receive) = mpsc::channel();
        let network = self.config.network;
        let probe = self.probe_local_node;
        thread::spawn(move || {
            let _ = send.send(probe(network));
        });
        self.local_node = LocalNodeCheck::Running(network, receive);
    }

    fn checking_local_node(&self) -> bool {
        matches!(self.local_node, LocalNodeCheck::Running(..))
    }

    /// Takes the check's result once it is in. The cursor moves to the
    /// offered node if it still rests on "+ add node", so Enter adds it.
    fn poll_local_node(&mut self) {
        let LocalNodeCheck::Running(network, receive) = &self.local_node else {
            return;
        };
        let network = *network;
        let result = match receive.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => crate::node::LocalNode::Missing,
        };
        self.local_node = LocalNodeCheck::Done(network, result);
        let count = self
            .sources
            .list(self.config.network, ConnectionKind::Node)
            .len();
        if self.step == SetupStep::Connections
            && self.editing.is_none()
            && self.connection_selected == count
            && self.local_node_offer().is_some()
        {
            self.connection_selected = count + 1;
        }
    }

    /// The node on this computer when it can be added: found on the
    /// selected network and not saved yet.
    fn local_node_offer(&self) -> Option<&crate::node::NodeInfo> {
        let network = self.config.network;
        match &self.local_node {
            LocalNodeCheck::Done(checked, crate::node::LocalNode::Found(info))
                if *checked == network
                    && self.connection_kind == ConnectionKind::Node
                    && info.network() == Some(network)
                    && !self
                        .sources
                        .list(network, ConnectionKind::Node)
                        .iter()
                        .any(|url| crate::node::is_local_node_url(url, network)) =>
            {
                Some(info)
            }
            _ => None,
        }
    }

    /// Saves the node on this computer for every profile on the network.
    fn add_local_node(&mut self) {
        let network = self.config.network;
        let count = self.sources.list(network, ConnectionKind::Node).len();
        let mut updated = self.sources.clone();
        match updated.put(
            network,
            ConnectionKind::Node,
            None,
            crate::node::local_node_url(network),
        ) {
            Ok(()) => {
                self.sources = updated;
                self.status_line = match self.save_sources() {
                    Ok(()) => format!(
                        "Added the BCH node on this PC for every profile on {}.",
                        network_label(network)
                    ),
                    Err(error) => error,
                };
                self.connection_selected = count;
            }
            Err(error) => self.status_line = error,
        }
    }

    /// The GPUs ticked in the setup wizard, in list order.
    fn chosen_gpus(&self) -> Vec<GpuDevice> {
        self.devices
            .iter()
            .zip(&self.chosen)
            .filter(|(_, chosen)| **chosen)
            .map(|(device, _)| device.clone())
            .collect()
    }

    /// Which listed GPUs a saved or command-line choice names; `None` when
    /// it names none of them.
    fn choice_for(&self, backend: BackendKind, selection: &DeviceSelection) -> Option<Vec<bool>> {
        let chosen = match selection {
            DeviceSelection::Default => default_choice(&self.devices),
            DeviceSelection::WithIntegrated => vec![true; self.devices.len()],
            DeviceSelection::Indices(indices) => self
                .devices
                .iter()
                .enumerate()
                .map(|(position, device)| {
                    if backend == BackendKind::Auto {
                        indices.contains(&(position as u32))
                    } else {
                        device.backend == backend && indices.contains(&device.index)
                    }
                })
                .collect(),
        };
        chosen.contains(&true).then_some(chosen)
    }

    /// Ticks exactly `chosen` and puts the cursor on the first ticked GPU.
    fn set_choice(&mut self, chosen: Vec<bool>) {
        self.selected = chosen.iter().position(|chosen| *chosen).unwrap_or(0);
        self.chosen = chosen;
    }

    /// The ticked GPUs as a saved choice. Every discrete GPU, or every GPU,
    /// stays a general choice, so a GPU added later mines too.
    fn saved_choice(&self) -> DeviceSelection {
        if self.chosen == default_choice(&self.devices) {
            DeviceSelection::Default
        } else if self.chosen.iter().all(|chosen| *chosen) {
            DeviceSelection::WithIntegrated
        } else {
            DeviceSelection::Indices(
                self.devices
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| self.chosen[*position])
                    .map(|(position, device)| {
                        if self.prefer == BackendKind::Auto {
                            position as u32
                        } else {
                            device.index
                        }
                    })
                    .collect(),
            )
        }
    }

    /// The GPUs a saved profile mines on, for the profile list.
    fn profile_gpus(&self, settings: &SavedConfig) -> String {
        let backend = settings
            .backend
            .as_deref()
            .and_then(|value| BackendKind::parse(value).ok())
            .unwrap_or(BackendKind::Auto);
        let Some(chosen) = self.choice_for(backend, &settings.device_selection()) else {
            return "GPU".into();
        };
        let names: Vec<&str> = self
            .devices
            .iter()
            .zip(&chosen)
            .filter(|(_, chosen)| **chosen)
            .map(|(device, _)| device.name.as_str())
            .collect();
        match names.as_slice() {
            [name] => (*name).to_string(),
            names => format!("{} GPUs", names.len()),
        }
    }

    fn matching_tokens(&self) -> Vec<crate::config::MiningToken> {
        let query = self.token_input.trim();
        let mut tokens = crate::config::MiningToken::GPU_SUPPORTED
            .iter()
            .copied()
            .filter(|token| {
                query.is_empty()
                    || token
                        .as_str()
                        .to_ascii_lowercase()
                        .contains(&query.to_ascii_lowercase())
                    || crate::config::MiningToken::parse(query, self.config.network).ok()
                        == Some(*token)
            })
            .collect::<Vec<_>>();
        tokens.sort_unstable_by_key(|token| token.as_str());
        tokens
    }

    /// Rows of the settings page for the chosen hardware.
    fn settings_rows(&self) -> Vec<SettingsRow> {
        match self.mode {
            // #### PR #40: a rig takes its jobs from its coordinator.
            MiningMode::Gpu if self.gpu_join => vec![
                SettingsRow::Gpu,
                SettingsRow::Mining,
                SettingsRow::PoolAddress,
                SettingsRow::PoolKey,
                SettingsRow::Address,
                SettingsRow::Intensity,
                SettingsRow::ProfileName,
                SettingsRow::Start,
            ],
            MiningMode::Gpu => vec![
                SettingsRow::Gpu,
                SettingsRow::Mining,
                SettingsRow::Address,
                SettingsRow::Intensity,
                SettingsRow::Fulcrum,
                SettingsRow::Node,
                SettingsRow::ProfileName,
                SettingsRow::Start,
            ],
            MiningMode::Asic if self.asic_mining == AsicMining::JoinPool => vec![
                SettingsRow::AsicTarget,
                SettingsRow::Mining,
                SettingsRow::PoolKind,
                SettingsRow::PoolAddress,
                SettingsRow::PoolKey,
                SettingsRow::Address,
                SettingsRow::ProfileName,
                SettingsRow::Start,
            ],
            MiningMode::Asic => vec![
                SettingsRow::AsicTarget,
                SettingsRow::Mining,
                SettingsRow::Address,
                SettingsRow::Fulcrum,
                SettingsRow::Node,
                SettingsRow::ProfileName,
                SettingsRow::Start,
            ],
            // #### PR #40: a GPU pool's fee is always mining time.
            MiningMode::Pool if self.pool_target == 1 => vec![
                SettingsRow::PoolKind,
                SettingsRow::Address,
                SettingsRow::Fulcrum,
                SettingsRow::Node,
                SettingsRow::PoolFee,
                SettingsRow::FeeAddress,
                SettingsRow::ProfileName,
                SettingsRow::Start,
            ],
            MiningMode::Pool => vec![
                SettingsRow::PoolKind,
                SettingsRow::Address,
                SettingsRow::Node,
                SettingsRow::PoolFee,
                SettingsRow::FeeFrom,
                SettingsRow::FeeAddress,
                SettingsRow::PoolName,
                SettingsRow::ProfileName,
                SettingsRow::Start,
            ],
        }
    }

    /// #### PR #40: the server this setup starts, if not GPU mining.
    fn server_setup(&self) -> Option<ServerSetup> {
        match self.mode {
            MiningMode::Gpu if self.gpu_join => Some(ServerSetup::JoinGpuPool {
                address: self.join_address.trim().to_owned(),
                key: self.join_key.trim().to_owned(),
            }),
            MiningMode::Gpu => None,
            MiningMode::Asic if self.asic_mining == AsicMining::JoinPool => {
                Some(ServerSetup::JoinPool {
                    address: self.join_address.trim().to_owned(),
                    key: self.join_key.trim().to_owned(),
                })
            }
            MiningMode::Asic => Some(ServerSetup::Solo),
            MiningMode::Pool if self.pool_target == 1 => Some(ServerSetup::GpuPool {
                fee: self.pool_fee,
                address: Some(self.pool_fee_address.trim().to_owned()).filter(|a| !a.is_empty()),
            }),
            MiningMode::Pool => Some(ServerSetup::Public {
                fee: self.pool_fee,
                mode: self.pool_fee_mode,
                address: Some(self.pool_fee_address.trim().to_owned()).filter(|a| !a.is_empty()),
                tag: Some(self.pool_tag.trim().to_owned()).filter(|t| !t.is_empty()),
            }),
        }
    }

    fn current_row(&self) -> SettingsRow {
        let rows = self.settings_rows();
        rows[self.settings_row.min(rows.len() - 1)]
    }

    fn open_settings(&mut self, row: SettingsRow) {
        self.step = SetupStep::Settings;
        self.settings_row = self
            .settings_rows()
            .iter()
            .position(|candidate| *candidate == row)
            .unwrap_or(0);
    }

    /// Puts the saved connections of the selected network on the config, then
    /// any command-line connection overrides on top.
    fn apply_connections(&mut self) -> Result<(), String> {
        self.sources.apply_to_runtime(&mut self.config)?;
        if let Some(url) = &self.overrides.fulcrum {
            self.config.set_fulcrum_url(url)?;
        }
        if let Some(url) = &self.overrides.node_rpc {
            self.config.set_node_url(url)?;
        }
        Ok(())
    }

    fn begin_edit(&mut self, field: TextField, value: String) {
        self.editing = Some(field);
        self.text_input = value;
        self.status_line.clear();
    }

    /// Loads a saved profile and opens its settings with Start selected.
    fn open_profile(&mut self, index: usize) -> Result<(), String> {
        let profile = self
            .profiles
            .profiles
            .get(index)
            .ok_or("profile no longer exists")?
            .clone();
        let mut config = RuntimeConfig::default();
        profile.settings.apply_to_runtime(&mut config)?;
        self.overrides.apply(&mut config)?;
        // Profiles saved before several GPUs could mine name one engine and
        // ordinal, such as cuda and 0; they still find that GPU.
        let backend = profile
            .settings
            .backend
            .as_deref()
            .and_then(|value| BackendKind::parse(value).ok())
            .unwrap_or(BackendKind::Auto);
        if let Some(chosen) = self.choice_for(backend, &profile.settings.device_selection()) {
            self.set_choice(chosen);
        }
        if let Some(gpus) = &self.overrides.gpus {
            let chosen: Vec<bool> = self
                .devices
                .iter()
                .map(|device| gpus.contains(&(device.backend, device.index)))
                .collect();
            if chosen.contains(&true) {
                self.set_choice(chosen);
            }
        }
        self.config = config;
        self.apply_connections()?;
        self.mode = MiningMode::Gpu;
        self.active_profile = Some(index);
        self.profile_name_input = profile.name.clone();
        self.open_settings(SettingsRow::Start);
        self.status_line = format!(
            "Loaded \u{201c}{}\u{201d}. Press Enter to start mining, or change a setting first.",
            profile.name
        );
        Ok(())
    }

    /// Starts a new profile from the launch configuration.
    fn new_profile(&mut self) -> Result<(), String> {
        self.config = self.base_config.clone();
        self.apply_connections()?;
        self.active_profile = None;
        self.profile_name_input.clear();
        self.mode = MiningMode::Gpu;
        self.step = SetupStep::Hardware;
        Ok(())
    }

    fn save_sources(&mut self) -> Result<(), String> {
        if let Some(path) = self.sources_path.as_deref() {
            self.sources.save(path)?;
        }
        self.apply_connections()
    }

    /// Handles keyboard input for the active setup screen.
    fn handle_key(&mut self, key: KeyEvent) -> SetupAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SetupAction::Cancel;
        }
        if let Some(field) = self.editing {
            self.handle_text_key(field, key);
            return SetupAction::Continue;
        }
        match self.step {
            SetupStep::Profiles => self.handle_profiles_key(key),
            SetupStep::Hardware => {
                match key.code {
                    KeyCode::Esc if !self.profiles.profiles.is_empty() => {
                        self.step = SetupStep::Profiles;
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                        return SetupAction::Cancel
                    }
                    KeyCode::Up => {
                        self.mode = match self.mode {
                            MiningMode::Gpu => MiningMode::Pool,
                            MiningMode::Asic => MiningMode::Gpu,
                            MiningMode::Pool => MiningMode::Asic,
                        };
                    }
                    KeyCode::Down => {
                        self.mode = match self.mode {
                            MiningMode::Gpu => MiningMode::Asic,
                            MiningMode::Asic => MiningMode::Pool,
                            MiningMode::Pool => MiningMode::Gpu,
                        };
                    }
                    KeyCode::Enter => self.step = SetupStep::Network,
                    _ => {}
                }
                self.status_line.clear();
                SetupAction::Continue
            }
            SetupStep::Network => {
                match key.code {
                    KeyCode::Esc => self.step = SetupStep::Hardware,
                    KeyCode::Up | KeyCode::Down => {
                        let next = match self.config.network {
                            MiningNetwork::Mainnet => MiningNetwork::Chipnet,
                            MiningNetwork::Chipnet => MiningNetwork::Mainnet,
                        };
                        self.config.set_network(next);
                        self.token_selected = 0;
                        if let Err(error) = self.apply_connections() {
                            self.status_line = error;
                            return SetupAction::Continue;
                        }
                    }
                    KeyCode::Enter => {
                        self.step = SetupStep::Token;
                        self.token_input.clear();
                        self.token_selected = 0;
                    }
                    _ => {}
                }
                self.status_line.clear();
                SetupAction::Continue
            }
            SetupStep::Token => self.handle_token_key(key),
            SetupStep::Settings => self.handle_settings_key(key),
            SetupStep::Connections => self.handle_connections_key(key),
            SetupStep::Gpus => self.handle_gpus_key(key),
        }
    }

    /// The GPU list: Space ticks or unticks a GPU, A ticks every GPU.
    fn handle_gpus_key(&mut self, key: KeyEvent) -> SetupAction {
        let count = self.devices.len();
        self.status_line.clear();
        match key.code {
            KeyCode::Up => self.selected = (self.selected + count - 1) % count,
            KeyCode::Down => self.selected = (self.selected + 1) % count,
            KeyCode::Char(' ') | KeyCode::Char('x') | KeyCode::Char('X') => {
                let tick = !self.chosen[self.selected];
                if !tick && self.chosen.iter().filter(|chosen| **chosen).count() == 1 {
                    self.status_line = "At least one GPU must mine.".into();
                } else {
                    self.chosen[self.selected] = tick;
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A') => self.chosen = vec![true; count],
            KeyCode::Enter | KeyCode::Esc => self.open_settings(SettingsRow::Gpu),
            _ => {}
        }
        SetupAction::Continue
    }

    fn handle_profiles_key(&mut self, key: KeyEvent) -> SetupAction {
        let count = self.profiles.profiles.len();
        if self.profile_delete_pending {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let mut updated = self.profiles.clone();
                    let result = updated.remove(self.profile_selected).and_then(|name| {
                        if let Some(path) = self.profile_path.as_deref() {
                            updated.save(path)?;
                        }
                        Ok(name)
                    });
                    match result {
                        Ok(name) => {
                            self.profiles = updated;
                            self.profile_selected =
                                self.profile_selected.min(self.profiles.profiles.len());
                            self.status_line = format!("Deleted profile \u{201c}{name}\u{201d}");
                        }
                        Err(error) => self.status_line = error,
                    }
                    self.profile_delete_pending = false;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.profile_delete_pending = false;
                    self.status_line.clear();
                }
                _ => {}
            }
            return SetupAction::Continue;
        }
        match key.code {
            KeyCode::Esc => return SetupAction::Cancel,
            KeyCode::Up => self.profile_selected = (self.profile_selected + count) % (count + 1),
            KeyCode::Down => self.profile_selected = (self.profile_selected + 1) % (count + 1),
            KeyCode::Char('r') | KeyCode::Char('R') if self.profile_selected < count => {
                let name = self.profiles.profiles[self.profile_selected].name.clone();
                self.begin_edit(TextField::ProfileRename, name);
            }
            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete
                if self.profile_selected < count =>
            {
                self.profile_delete_pending = true;
                self.status_line = format!(
                    "Delete profile \u{201c}{}\u{201d}? [Y] delete   [N] keep",
                    self.profiles.profiles[self.profile_selected].name
                );
            }
            KeyCode::Enter => {
                let result = if self.profile_selected < count {
                    self.open_profile(self.profile_selected)
                } else {
                    self.status_line.clear();
                    self.new_profile()
                };
                if let Err(error) = result {
                    self.status_line = error;
                }
            }
            _ => {}
        }
        SetupAction::Continue
    }

    fn handle_token_key(&mut self, key: KeyEvent) -> SetupAction {
        if self.mode == MiningMode::Pool {
            match key.code {
                KeyCode::Esc => self.step = SetupStep::Network,
                KeyCode::Up | KeyCode::Down => self.pool_target = 1 - self.pool_target,
                KeyCode::Enter => self.open_settings(SettingsRow::PoolKind),
                _ => {}
            }
            self.status_line.clear();
            return SetupAction::Continue;
        }
        if self.mode == MiningMode::Asic {
            match key.code {
                KeyCode::Esc => self.step = SetupStep::Network,
                KeyCode::Up | KeyCode::Down => self.asic_target = 1 - self.asic_target,
                KeyCode::Enter => self.open_settings(SettingsRow::AsicTarget),
                _ => {}
            }
            self.status_line.clear();
            return SetupAction::Continue;
        }
        match key.code {
            KeyCode::Esc => {
                self.step = SetupStep::Network;
                self.status_line.clear();
            }
            KeyCode::Enter => {
                let matches = self.matching_tokens();
                let query = matches
                    .get(self.token_selected)
                    .map(|token| token.as_str())
                    .unwrap_or(self.token_input.as_str());
                match self
                    .config
                    .set_token(query)
                    .and_then(|()| self.config.token.ensure_supported(self.config.network))
                {
                    Ok(()) => {
                        self.status_line.clear();
                        self.open_settings(SettingsRow::Gpu);
                    }
                    Err(error) => self.status_line = error,
                }
            }
            KeyCode::Up | KeyCode::Down => {
                let count = self.matching_tokens().len();
                if count > 0 {
                    self.token_selected = if key.code == KeyCode::Up {
                        (self.token_selected + count - 1) % count
                    } else {
                        (self.token_selected + 1) % count
                    };
                }
                self.status_line.clear();
            }
            KeyCode::Backspace => {
                self.token_input.pop();
                self.token_selected = 0;
                self.status_line.clear();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.token_input.push(ch);
                self.token_selected = 0;
                self.status_line.clear();
            }
            _ => {}
        }
        SetupAction::Continue
    }

    fn handle_settings_key(&mut self, key: KeyEvent) -> SetupAction {
        let rows = self.settings_rows();
        let row = self.current_row();
        self.status_line.clear();
        match key.code {
            KeyCode::Esc => {
                if let Some(index) = self.active_profile {
                    self.step = SetupStep::Profiles;
                    self.profile_selected = index;
                } else {
                    self.step = SetupStep::Token;
                }
            }
            KeyCode::Up => self.settings_row = (self.settings_row + rows.len() - 1) % rows.len(),
            KeyCode::Down => self.settings_row = (self.settings_row + 1) % rows.len(),
            KeyCode::Left | KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('-') => {
                let forward = matches!(key.code, KeyCode::Right | KeyCode::Char('+'));
                match row {
                    // Left/Right moves a single choice to the next GPU; with
                    // several ticked, the list keeps them.
                    SettingsRow::Gpu
                        if self.chosen.iter().filter(|chosen| **chosen).count() == 1 =>
                    {
                        let count = self.devices.len();
                        let current = self.chosen.iter().position(|chosen| *chosen).unwrap_or(0);
                        let next = if forward {
                            (current + 1) % count
                        } else {
                            (current + count - 1) % count
                        };
                        let mut chosen = vec![false; count];
                        chosen[next] = true;
                        self.set_choice(chosen);
                    }
                    SettingsRow::Gpu => {
                        self.status_line =
                            "Several GPUs are ticked; press Enter to change them.".into();
                    }
                    SettingsRow::Intensity => {
                        let next = if forward {
                            self.config.intensity.saturating_add(10).min(100)
                        } else {
                            self.config.intensity.saturating_sub(10).max(10)
                        };
                        let _ = self.config.set_intensity(next);
                    }
                    SettingsRow::AsicTarget => self.asic_target = 1 - self.asic_target,
                    // #### PR #40
                    SettingsRow::Mining if self.mode == MiningMode::Gpu => {
                        self.gpu_join = !self.gpu_join;
                    }
                    SettingsRow::Mining => {
                        self.asic_mining = match self.asic_mining {
                            AsicMining::Solo => AsicMining::JoinPool,
                            AsicMining::JoinPool => AsicMining::Solo,
                        };
                    }
                    SettingsRow::PoolKind => {
                        self.pool_kind = match self.pool_kind {
                            PoolKind::Normal => PoolKind::P2PoolV2,
                            PoolKind::P2PoolV2 => PoolKind::Normal,
                        };
                    }
                    SettingsRow::PoolFee => self.pool_fee = self.pool_fee.adjusted(forward),
                    SettingsRow::FeeFrom => {
                        use crate::donation::bch::FeeMode;
                        self.pool_fee_mode = match (self.pool_fee_mode, forward) {
                            (FeeMode::Coinbase, true) | (FeeMode::Both, false) => FeeMode::Work,
                            (FeeMode::Work, true) | (FeeMode::Coinbase, false) => FeeMode::Both,
                            (FeeMode::Both, true) | (FeeMode::Work, false) => FeeMode::Coinbase,
                        };
                    }
                    _ => {}
                }
            }
            KeyCode::Enter => match row {
                SettingsRow::Address => {
                    let value = self.config.payout_address.clone();
                    self.begin_edit(TextField::Address, value);
                }
                // #### PR #40
                SettingsRow::PoolAddress => {
                    let value = self.join_address.clone();
                    self.begin_edit(TextField::PoolAddress, value);
                }
                SettingsRow::PoolKey => {
                    let value = self.join_key.clone();
                    self.begin_edit(TextField::PoolKey, value);
                }
                SettingsRow::FeeAddress => {
                    let value = self.pool_fee_address.clone();
                    self.begin_edit(TextField::FeeAddress, value);
                }
                SettingsRow::PoolName => {
                    let value = self.pool_tag.clone();
                    self.begin_edit(TextField::PoolName, value);
                }
                SettingsRow::Mining
                | SettingsRow::PoolKind
                | SettingsRow::PoolFee
                | SettingsRow::FeeFrom => {
                    self.status_line = "Use Left/Right to change this row.".into();
                }
                SettingsRow::ProfileName => {
                    let value = self.profile_name_input.clone();
                    self.begin_edit(TextField::ProfileName, value);
                }
                SettingsRow::Fulcrum | SettingsRow::Node => {
                    self.connection_kind = if row == SettingsRow::Fulcrum {
                        ConnectionKind::Fulcrum
                    } else {
                        ConnectionKind::Node
                    };
                    self.connection_selected = 0;
                    self.step = SetupStep::Connections;
                    if row == SettingsRow::Node {
                        self.check_local_node();
                    }
                }
                // #### PR #40
                // Joining a GPU pool or farm needs its coordinator and key,
                // and the payout a public pool claims this rig's wins to.
                SettingsRow::Start if self.mode == MiningMode::Gpu && self.gpu_join => {
                    if !self.join_address.contains(':') {
                        self.status_line =
                            "Enter the coordinator's address as HOST:PORT (its Connection info shows it)."
                                .into();
                        self.open_settings(SettingsRow::PoolAddress);
                    } else if self.join_key.trim().is_empty() {
                        self.status_line = "Enter the coordinator's key.".into();
                        self.open_settings(SettingsRow::PoolKey);
                    } else if crate::config::validate_payout_address(
                        self.config.network,
                        &self.config.payout_address,
                    )
                    .is_err()
                    {
                        self.status_line =
                            "Enter your payout address (a q address): a public pool claims your wins to it."
                                .into();
                        self.open_settings(SettingsRow::Address);
                    } else if self.chosen_gpus().is_empty() {
                        self.status_line = "Choose at least one GPU.".into();
                        self.open_settings(SettingsRow::Gpu);
                    } else {
                        return SetupAction::Complete;
                    }
                }
                // #### PR #40
                // Joining a pool needs the pool; P2Pool v2 is coming.
                SettingsRow::Start
                    if self.mode == MiningMode::Asic
                        && self.asic_mining == AsicMining::JoinPool =>
                {
                    if self.asic_target != 0 {
                        self.status_line =
                            "ASIC-exclusive tokens are not available yet; choose BCH.".into();
                    } else if self.pool_kind == PoolKind::P2PoolV2 {
                        self.status_line =
                            "P2Pool v2 is coming soon; choose a normal pool for now.".into();
                    } else if !self.join_address.contains(':') {
                        self.status_line = "Enter the pool's address as HOST:PORT.".into();
                        self.open_settings(SettingsRow::PoolAddress);
                    } else if self.join_key.trim().is_empty() {
                        self.status_line =
                            "Enter the pool's authority key, as it publishes it.".into();
                        self.open_settings(SettingsRow::PoolKey);
                    } else if self.config.payout_address.trim().is_empty() {
                        self.status_line =
                            "Enter your payout address: the pool knows you by it.".into();
                        self.open_settings(SettingsRow::Address);
                    } else {
                        return SetupAction::Complete;
                    }
                }
                // Running a pool needs a payout and a valid fee address, and
                // an ASIC pool the miner's own node; P2Pool v2 is coming.
                SettingsRow::Start if self.mode == MiningMode::Pool => {
                    if self.pool_target != 0 && self.pool_kind == PoolKind::Normal {
                        // #### PR #40: a GPU pool pays token claims, which
                        // go to q addresses only.
                        if crate::config::validate_payout_address(
                            self.config.network,
                            &self.config.payout_address,
                        )
                        .is_err()
                        {
                            self.status_line =
                                "Enter your payout address (a q address) for the selected network."
                                    .into();
                            self.open_settings(SettingsRow::Address);
                        } else if !self.pool_fee_address.trim().is_empty()
                            && crate::config::validate_payout_address(
                                self.config.network,
                                &self.pool_fee_address,
                            )
                            .is_err()
                        {
                            self.status_line =
                                "A GPU pool's fee address must be a q address on this network: token claims pay q addresses."
                                    .into();
                            self.open_settings(SettingsRow::FeeAddress);
                        } else {
                            return SetupAction::Complete;
                        }
                    } else if self.pool_kind == PoolKind::P2PoolV2 {
                        self.status_line =
                            "P2Pool v2 is coming soon; run a normal pool for now.".into();
                    } else if crate::config::validate_payout_address(
                        self.config.network,
                        &self.config.payout_address,
                    )
                    .is_err()
                    {
                        self.status_line =
                            "Enter your BCH payout address for the selected network.".into();
                        self.open_settings(SettingsRow::Address);
                    } else if !self.pool_fee_address.trim().is_empty()
                        && crate::config::validate_coinbase_address(
                            self.config.network,
                            &self.pool_fee_address,
                        )
                        .is_err()
                    {
                        self.status_line =
                            "The fee address must be a q or p address on this network.".into();
                        self.open_settings(SettingsRow::FeeAddress);
                    } else if self.config.custom_node_endpoints().is_empty() {
                        self.connection_kind = ConnectionKind::Node;
                        self.connection_selected = self
                            .sources
                            .list(self.config.network, ConnectionKind::Node)
                            .len();
                        self.step = SetupStep::Connections;
                        self.check_local_node();
                        self.status_line =
                            "A pool builds blocks from your own BCH node: add it here.".into();
                    } else {
                        return SetupAction::Complete;
                    }
                }
                // #### PR #40
                // ASIC mode starts the BCH ASIC server: blocks come from the
                // miner's own BCH node and pay the payout address directly.
                SettingsRow::Start if self.mode == MiningMode::Asic => {
                    if self.asic_target != 0 {
                        self.status_line =
                            "ASIC-exclusive tokens are not available yet; choose BCH.".into();
                    } else if self.config.payout_address.trim().is_empty() {
                        self.status_line = "Enter a payout address first.".into();
                        self.open_settings(SettingsRow::Address);
                    } else if crate::config::validate_payout_address(
                        self.config.network,
                        &self.config.payout_address,
                    )
                    .is_err()
                    {
                        self.status_line =
                            "Enter a BCH payout address for the selected network.".into();
                        self.open_settings(SettingsRow::Address);
                    } else if self.config.custom_node_endpoints().is_empty() {
                        // #### PR #40
                        // Straight to the BCH node list, which looks for a
                        // node on this computer.
                        self.connection_kind = ConnectionKind::Node;
                        self.connection_selected = self
                            .sources
                            .list(self.config.network, ConnectionKind::Node)
                            .len();
                        self.step = SetupStep::Connections;
                        self.check_local_node();
                        self.status_line =
                            "BCH ASIC mining builds blocks from your own BCH node: add it here."
                                .into();
                    } else {
                        return SetupAction::Complete;
                    }
                }
                SettingsRow::Start if self.config.payout_address.trim().is_empty() => {
                    self.status_line = "Enter a payout address first.".into();
                    self.open_settings(SettingsRow::Address);
                }
                SettingsRow::Start => match self.config.ensure_mining_supported() {
                    Ok(()) => return SetupAction::Complete,
                    Err(error) => self.status_line = error,
                },
                SettingsRow::Gpu => self.step = SetupStep::Gpus,
                SettingsRow::Intensity | SettingsRow::AsicTarget => {
                    self.status_line = "Use Left/Right to change this row.".into();
                }
            },
            _ => {}
        }
        SetupAction::Continue
    }

    fn handle_connections_key(&mut self, key: KeyEvent) -> SetupAction {
        let network = self.config.network;
        let count = self.sources.list(network, self.connection_kind).len();
        // #### PR #40
        // The node found on this computer is one more row, after "+ add".
        let offer = self.local_node_offer().is_some();
        let rows = count + 1 + usize::from(offer);
        self.connection_selected = self.connection_selected.min(rows - 1);
        self.status_line.clear();
        match key.code {
            KeyCode::Esc => {
                let row = match self.connection_kind {
                    ConnectionKind::Fulcrum => SettingsRow::Fulcrum,
                    ConnectionKind::Node => SettingsRow::Node,
                };
                self.open_settings(row);
            }
            KeyCode::Up => self.connection_selected = (self.connection_selected + rows - 1) % rows,
            KeyCode::Down => self.connection_selected = (self.connection_selected + 1) % rows,
            KeyCode::Enter if offer && self.connection_selected == count + 1 => {
                self.add_local_node()
            }
            KeyCode::Enter => {
                let value = self
                    .sources
                    .list(network, self.connection_kind)
                    .get(self.connection_selected)
                    .cloned()
                    .unwrap_or_default();
                self.begin_edit(TextField::Connection, value);
            }
            KeyCode::Delete | KeyCode::Char('d') | KeyCode::Char('D')
                if self.connection_selected < count =>
            {
                self.sources
                    .remove(network, self.connection_kind, self.connection_selected);
                self.status_line = match self.save_sources() {
                    Ok(()) => format!("Removed from every profile on {}.", network_label(network)),
                    Err(error) => error,
                };
                self.connection_selected = self.connection_selected.min(count - 1);
            }
            _ => {}
        }
        SetupAction::Continue
    }

    fn handle_text_key(&mut self, field: TextField, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.editing = None;
                self.status_line.clear();
            }
            KeyCode::Backspace => {
                self.text_input.pop();
                self.status_line.clear();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text_input.push(ch);
                self.status_line.clear();
            }
            KeyCode::Enter => match self.commit_text(field) {
                Ok(message) => {
                    self.editing = None;
                    self.status_line = message;
                }
                Err(error) => self.status_line = error,
            },
            _ => {}
        }
    }

    /// Saves the typed value; on error the field stays open.
    fn commit_text(&mut self, field: TextField) -> Result<String, String> {
        let value = self.text_input.trim().to_string();
        match field {
            TextField::ProfileRename => {
                let mut updated = self.profiles.clone();
                updated.rename(self.profile_selected, &value)?;
                if let Some(path) = self.profile_path.as_deref() {
                    updated.save(path)?;
                }
                self.profiles = updated;
                Ok("Profile renamed".into())
            }
            TextField::Address => {
                self.config.set_payout(value)?;
                Ok(String::new())
            }
            TextField::ProfileName => {
                self.profile_name_input = value;
                Ok(String::new())
            }
            // #### PR #40
            // The pool's address, or the one line SV2 pools publish with
            // their key (stratum2+tcp://HOST:PORT/KEY), which fills the key.
            TextField::PoolAddress => {
                if value.is_empty() {
                    self.join_address = value;
                    return Ok(String::new());
                }
                let (address, key) = crate::stratum_v2::split_pool_address(&value)?;
                self.join_address = address;
                Ok(match key {
                    Some(key) => {
                        self.join_key = key;
                        "The pool's key came with its address.".into()
                    }
                    None => String::new(),
                })
            }
            TextField::PoolKey => {
                self.join_key = value;
                Ok(String::new())
            }
            TextField::FeeAddress => {
                if !value.is_empty() {
                    crate::config::validate_coinbase_address(self.config.network, &value)
                        .map_err(|_| "enter a q or p address on this network")?;
                }
                self.pool_fee_address = value;
                Ok(String::new())
            }
            TextField::PoolName => {
                if value.len() > 20 || !value.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                    return Err("use up to 20 printable characters".into());
                }
                self.pool_tag = value;
                Ok(String::new())
            }
            TextField::Connection => {
                let network = self.config.network;
                let index = self.connection_selected;
                if value.is_empty() {
                    self.sources.remove(network, self.connection_kind, index);
                } else {
                    let mut updated = self.sources.clone();
                    updated.put(network, self.connection_kind, Some(index), &value)?;
                    self.sources = updated;
                }
                self.save_sources()?;
                Ok(format!(
                    "Saved for every profile on {}.",
                    network_label(network)
                ))
            }
        }
    }
}

fn network_label(network: MiningNetwork) -> &'static str {
    match network {
        MiningNetwork::Mainnet => "Mainnet",
        MiningNetwork::Chipnet => "Chipnet",
    }
}

pub(crate) struct TerminalSession {
    pub(crate) terminal: PickaxeTerminal,
}

impl TerminalSession {
    /// Enters raw terminal mode and switches to the alternate screen.
    pub(crate) fn enter() -> Result<Self, String> {
        enable_raw_mode().map_err(|error| format!("enable terminal raw mode: {error}"))?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(format!("enter alternate terminal screen: {error}"));
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(mut terminal) => {
                if let Err(error) = terminal.clear() {
                    let _ = disable_raw_mode();
                    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                    let _ = terminal.show_cursor();
                    return Err(format!("clear terminal: {error}"));
                }
                Ok(Self { terminal })
            }
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stdout = io::stdout();
                let _ = execute!(stdout, LeaveAlternateScreen);
                Err(format!("initialize terminal: {error}"))
            }
        }
    }
}

impl Drop for TerminalSession {
    /// Releases resources owned by TerminalSession.
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PaletteCommand {
    SetIntensity(u8),
    Pause,
    Resume,
    Reconnect,
    SetPayout(String),
    SetEndpoint(Option<String>),
    Status,
    Config,
    Logs,
    Devices,
    Backend,
    Benchmark,
    Charts(ChartRequest),
    Help,
    Quit,
}

/// A live value the optional history charts can plot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartMetric {
    Hashrate,
    Temperature,
    Power,
    Fan,
    Utilization,
    CoreClock,
}

impl ChartMetric {
    const ALL: [Self; 6] = [
        Self::Hashrate,
        Self::Temperature,
        Self::Power,
        Self::Fan,
        Self::Utilization,
        Self::CoreClock,
    ];

    /// Accepts the command names shown in help, plus a few aliases.
    fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "hash" | "hashrate" | "rate" => Some(Self::Hashrate),
            "temp" | "temperature" => Some(Self::Temperature),
            "power" | "watts" => Some(Self::Power),
            "fan" => Some(Self::Fan),
            "util" | "utilization" | "load" => Some(Self::Utilization),
            "clock" | "clocks" | "core" => Some(Self::CoreClock),
            _ => None,
        }
    }

    /// Short command name.
    fn name(self) -> &'static str {
        match self {
            Self::Hashrate => "hash",
            Self::Temperature => "temp",
            Self::Power => "power",
            Self::Fan => "fan",
            Self::Utilization => "util",
            Self::CoreClock => "clock",
        }
    }

    /// Chart title.
    fn title(self) -> &'static str {
        match self {
            Self::Hashrate => "Hashrate",
            Self::Temperature => "GPU temperature",
            Self::Power => "GPU power",
            Self::Fan => "Fan speed",
            Self::Utilization => "GPU utilization",
            Self::CoreClock => "Core clock",
        }
    }

    /// Reads this metric from one history sample.
    fn value(self, sample: &HistorySample) -> Option<f64> {
        let gpu = &sample.gpu;
        match self {
            Self::Hashrate => Some(sample.rate),
            Self::Temperature => gpu.temperature_c,
            Self::Power => gpu.power_watts,
            Self::Fan => gpu.fan_percent,
            Self::Utilization => gpu.gpu_utilization_percent,
            Self::CoreClock => gpu.graphics_clock_mhz,
        }
        .filter(|value| value.is_finite())
    }

    /// Formats a value of this metric for titles and axis labels.
    fn format(self, value: f64) -> String {
        match self {
            Self::Hashrate => crate::telemetry::format_hash_rate(value),
            Self::Temperature => format!("{value:.0} C"),
            Self::Power => format!("{value:.0} W"),
            Self::Fan | Self::Utilization => format!("{value:.0}%"),
            Self::CoreClock => format!("{value:.0} MHz"),
        }
    }

    /// Axis top: 100 for percentages and temperature unless exceeded,
    /// otherwise a round number above the largest value.
    fn axis_top(self, largest: f64) -> f64 {
        match self {
            Self::Fan | Self::Utilization => 100.0,
            Self::Temperature => nice_ceiling(largest * 1.1).max(100.0),
            _ => nice_ceiling(largest * 1.1),
        }
    }
}

/// One history point for the charts.
#[derive(Debug, Clone, Default)]
struct HistorySample {
    rate: f64,
    gpu: crate::telemetry::GpuTelemetry,
}

/// A `/chart` request or chart key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChartRequest {
    /// `G`: show or hide the chosen charts.
    Toggle,
    On,
    Off,
    /// Show exactly these charts.
    Show(Vec<ChartMetric>),
    Add(Vec<ChartMetric>),
    Remove(Vec<ChartMetric>),
    /// `/chart options`: open the chart options menu.
    Options,
}

/// Charts shown the first time `G` is pressed.
const DEFAULT_CHARTS: [ChartMetric; 2] = [ChartMetric::Hashrate, ChartMetric::Temperature];

/// Puts charts in the fixed on-screen order and drops duplicates.
fn chart_order(metrics: &[ChartMetric]) -> Vec<ChartMetric> {
    ChartMetric::ALL
        .into_iter()
        .filter(|metric| metrics.contains(metric))
        .collect()
}

struct TuiState {
    command_mode: bool,
    command_input: String,
    command_history: VecDeque<String>,
    command_history_cursor: Option<usize>,
    command_draft: String,
    show_help: bool,
    settings_mode: bool,
    /// #### PR #32: Advanced settings (`a`), where the donation is set.
    advanced_mode: bool,
    logs_mode: bool,
    /// #### PR #40: Connection info (`i`): the command each rig runs, one per
    /// address, found when the page opens.
    connect: Option<Vec<(crate::reach::Place, String)>>,
    status_line: String,
    events: VecDeque<String>,
    devices: Vec<GpuDevice>,
    history: VecDeque<HistorySample>,
    last_history_sample: Option<Instant>,
    /// Charts on screen; empty means charts are hidden (the default).
    charts: Vec<ChartMetric>,
    /// The charts `G` shows; hash rate and temperature at first.
    chart_selection: Vec<ChartMetric>,
    /// Cursor row while the chart options menu is open.
    chart_options: Option<usize>,
}

impl TuiState {
    /// Creates a TuiState for the terminal interface.
    fn new(_snapshot: &RuntimeSnapshot) -> Self {
        let mut events = VecDeque::with_capacity(EVENT_HISTORY_CAP);
        events.push_back("started; supervising PHOTON state".into());
        Self {
            command_mode: false,
            command_input: String::new(),
            command_history: VecDeque::with_capacity(COMMAND_HISTORY_CAP),
            command_history_cursor: None,
            command_draft: String::new(),
            show_help: false,
            settings_mode: false,
            advanced_mode: false,
            logs_mode: false,
            connect: None,
            status_line: String::new(),
            events,
            devices: Vec::new(),
            history: VecDeque::with_capacity(HISTORY_CAP),
            last_history_sample: None,
            charts: Vec::new(),
            chart_selection: DEFAULT_CHARTS.to_vec(),
            chart_options: None,
        }
    }

    /// Records hash rate and GPU telemetry for the charts at most once per
    /// sample interval, whether or not charts are shown.
    fn record_sample(&mut self, snapshot: &RuntimeSnapshot, now: Instant) {
        if self
            .last_history_sample
            .is_some_and(|sampled| now.duration_since(sampled) < HISTORY_SAMPLE_INTERVAL)
        {
            return;
        }
        if self.history.len() == HISTORY_CAP {
            self.history.pop_front();
        }
        // A coordinator charts its whole farm: its own rate and its rigs'.
        let rate =
            snapshot.search.current_rate + snapshot.rigs.as_ref().map_or(0.0, |rigs| rigs.rate);
        self.history.push_back(HistorySample {
            rate: if rate.is_finite() { rate.max(0.0) } else { 0.0 },
            gpu: snapshot.gpu_telemetry.clone(),
        });
        self.last_history_sample = Some(now);
    }

    /// Applies a `/chart` request, a chart key, or a menu change. Changes
    /// to the selection show the charts right away.
    fn apply_chart_request(&mut self, request: ChartRequest) {
        match request {
            ChartRequest::Toggle if self.charts.is_empty() => self.show_selected_charts(),
            ChartRequest::Toggle | ChartRequest::Off => self.charts.clear(),
            ChartRequest::On => self.show_selected_charts(),
            ChartRequest::Show(metrics) => {
                self.chart_selection = chart_order(&metrics);
                self.charts = self.chart_selection.clone();
            }
            ChartRequest::Add(metrics) => {
                let mut selection = self.chart_selection.clone();
                selection.extend(metrics);
                self.chart_selection = chart_order(&selection);
                self.charts = self.chart_selection.clone();
            }
            ChartRequest::Remove(metrics) => {
                self.chart_selection
                    .retain(|metric| !metrics.contains(metric));
                self.charts = self.chart_selection.clone();
            }
            ChartRequest::Options => {
                self.chart_options = Some(0);
                self.status_line =
                    "Chart options: arrows move, Space toggles, A all, N none, Enter closes".into();
                return;
            }
        }
        self.status_line = if self.charts.is_empty() {
            "Charts hidden. G shows them, O picks which.".into()
        } else {
            format!(
                "Charts: {}. G hides them, O picks which.",
                self.charts
                    .iter()
                    .map(|metric| metric.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
    }

    /// Shows the chosen charts, falling back to the defaults when none are
    /// chosen.
    fn show_selected_charts(&mut self) {
        if self.chart_selection.is_empty() {
            self.chart_selection = DEFAULT_CHARTS.to_vec();
        }
        self.charts = self.chart_selection.clone();
    }

    /// Handles a key while the chart options menu is open. Returns true
    /// when the key asks to quit.
    fn chart_options_key(&mut self, code: KeyCode) -> bool {
        let Some(cursor) = self.chart_options else {
            return false;
        };
        let count = ChartMetric::ALL.len();
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.chart_options = Some(cursor.checked_sub(1).unwrap_or(count - 1));
            }
            KeyCode::Down | KeyCode::Char('j') => self.chart_options = Some((cursor + 1) % count),
            KeyCode::Char(' ') | KeyCode::Char('x') => {
                let metric = ChartMetric::ALL[cursor];
                let request = if self.chart_selection.contains(&metric) {
                    ChartRequest::Remove(vec![metric])
                } else {
                    ChartRequest::Add(vec![metric])
                };
                self.apply_chart_request(request);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.apply_chart_request(ChartRequest::Show(ChartMetric::ALL.to_vec()));
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.apply_chart_request(ChartRequest::Show(Vec::new()));
            }
            KeyCode::Enter | KeyCode::Esc | KeyCode::Char('o') | KeyCode::Char('O') => {
                self.chart_options = None;
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => return true,
            _ => {}
        }
        false
    }

    /// Adds a runtime event to the bounded terminal log.
    fn push_event(&mut self, message: String) {
        if self.events.len() == EVENT_HISTORY_CAP {
            self.events.pop_front();
        }
        self.events.push_back(message);
    }

    /// Updates GPU devices displayed by the setup wizard.
    fn set_devices(&mut self, devices: Vec<GpuDevice>) {
        self.devices = devices;
    }

    /// Opens the command palette for interactive input.
    fn open_command(&mut self, prefill: &str) {
        self.command_mode = true;
        self.command_input.clear();
        self.command_input.push_str(prefill);
        self.command_history_cursor = None;
        self.command_draft.clear();
        self.show_help = false;
        self.settings_mode = false;
        self.advanced_mode = false;
        self.logs_mode = false;
        self.connect = None;
    }

    /// Closes the command palette without applying its draft.
    fn cancel_command(&mut self) {
        self.command_mode = false;
        self.command_input.clear();
        self.command_history_cursor = None;
        self.command_draft.clear();
    }

    /// Commits a command and updates the bounded command history.
    fn finish_command(&mut self) -> String {
        let input = std::mem::take(&mut self.command_input);
        self.command_mode = false;
        self.command_history_cursor = None;
        self.command_draft.clear();
        let trimmed = input.trim();
        if !trimmed.is_empty() && self.command_history.back().map(String::as_str) != Some(trimmed) {
            if self.command_history.len() == COMMAND_HISTORY_CAP {
                self.command_history.pop_front();
            }
            self.command_history.push_back(trimmed.to_string());
        }
        input
    }

    /// Loads the preceding command from history into the palette.
    fn history_previous(&mut self) {
        if self.command_history.is_empty() {
            return;
        }
        let next = match self.command_history_cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.command_draft = self.command_input.clone();
                self.command_history.len() - 1
            }
        };
        self.command_history_cursor = Some(next);
        self.command_input = self.command_history[next].clone();
    }

    /// Loads the next command from history into the palette.
    fn history_next(&mut self) {
        let Some(index) = self.command_history_cursor else {
            return;
        };
        if index + 1 < self.command_history.len() {
            let next = index + 1;
            self.command_history_cursor = Some(next);
            self.command_input = self.command_history[next].clone();
        } else {
            self.command_history_cursor = None;
            self.command_input = self.command_draft.clone();
        }
    }
}

/// Runs the interactive setup flow before starting mining.
#[allow(clippy::too_many_arguments)]
pub fn run_setup(
    config: RuntimeConfig,
    devices: Vec<GpuDevice>,
    prefer: BackendKind,
    default_gpus: &[GpuDevice],
    profile_path: &Path,
    profiles: MiningProfiles,
    sources_path: &Path,
    sources: SharedSources,
    overrides: SetupOverrides,
) -> Result<Option<SetupResult>, String> {
    let mut state = SetupFlow::new(config, devices, prefer, default_gpus)?;
    if !profiles.profiles.is_empty() {
        state.step = SetupStep::Profiles;
    }
    state.profiles = profiles;
    state.profile_path = Some(profile_path.to_path_buf());
    state.sources = sources;
    state.sources_path = Some(sources_path.to_path_buf());
    state.overrides = overrides;
    state.apply_connections()?;
    run_setup_terminal(state)
}

/// Draws and processes setup screens in the terminal.
fn run_setup_terminal(mut state: SetupFlow) -> Result<Option<SetupResult>, String> {
    let mut terminal = TerminalSession::enter()?;
    loop {
        state.poll_local_node();
        terminal
            .terminal
            .draw(|frame| render_setup(frame, &state))
            .map_err(|error| format!("draw mining setup: {error}"))?;

        // #### PR #40
        // While the check for a node on this computer runs, wake up to show
        // its result as soon as it is in.
        if state.checking_local_node()
            && !event::poll(EVENT_POLL_INTERVAL)
                .map_err(|error| format!("read mining setup input: {error}"))?
        {
            continue;
        }
        let input = event::read().map_err(|error| format!("read mining setup input: {error}"))?;
        let Event::Key(key) = input else {
            continue;
        };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            continue;
        }

        match state.handle_key(key) {
            SetupAction::Continue => {}
            SetupAction::Cancel => return Ok(None),
            SetupAction::Complete => {
                let gpus = state.chosen_gpus();
                let mut settings = SavedConfig::from_effective(
                    state.prefer.as_str(),
                    &state.saved_choice(),
                    &state.config,
                );
                // Servers and nodes live in the shared per-network store.
                settings.fulcrum = None;
                settings.node_rpc = None;
                let mut profiles = state.profiles.clone();
                let saved = profiles
                    .upsert(state.active_profile, &state.profile_name_input, settings)
                    .and_then(|name| {
                        profiles
                            .save(state.profile_path.as_deref().expect("setup profile path"))?;
                        Ok(name)
                    });
                match saved {
                    Ok(profile_name) => {
                        return Ok(Some(SetupResult {
                            config: state.config.clone(),
                            gpus,
                            profile_name,
                            server: state.server_setup(),
                        }));
                    }
                    Err(error) => state.status_line = error,
                }
            }
        }
    }
}

/// Appends one Unix-timestamped line to the optional `PICKAXE_TUI_LOG` file.
fn append_tui_log(line: &str) {
    let Ok(path) = std::env::var("PICKAXE_TUI_LOG") else {
        return;
    };
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = std::io::Write::write_all(&mut file, format!("{seconds} {line}\n").as_bytes());
    }
}

/// Expected seconds between winners at `rate` candidates/s for a
/// little-endian hex PHOTON target, if both are known.
fn expected_winner_seconds(target_le_hex: &str, rate: f64, network: MiningNetwork) -> Option<f64> {
    if rate <= 0.0 {
        return None;
    }
    win_probability(target_le_hex, network).map(|probability| 1.0 / (probability * rate))
}

/// Chance that one candidate wins against a little-endian hex target under
/// the network's PHOTON deployment.
fn win_probability(target_le_hex: &str, network: MiningNetwork) -> Option<f64> {
    win_probability_for_rule(
        target_le_hex,
        MiningToken::Photon.photon_deployment(network).proof_rule,
    )
}

// #### PR #22: win odds follow the deployment's proof rule
// What: the v0 covenant compares ABS(hash), so digest bit 255 never matters
// and a candidate wins with probability target / 2^255. v3.2 requires a
// positive hash: target / 2^256.
// Why: mainnet moved to v3.2 on 2026-10-03, but the odds still treated
// mainnet as v0, so the dashboard and expected_winner_s promised twice the
// real win rate.
// Check: display only; the search and the winner checks already apply the
// rule. Look here if the dashboard's odds disagree with the observed wins.
fn win_probability_for_rule(target_le_hex: &str, rule: ProofRule) -> Option<f64> {
    let bytes = hex::decode(target_le_hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let probability = bytes
        .iter()
        .rev()
        .fold(0.0_f64, |value, byte| value * 256.0 + f64::from(*byte))
        / match rule {
            ProofRule::Absolute => 2f64.powi(255),
            ProofRule::Positive => 2f64.powi(256),
        };
    (probability > 0.0).then_some(probability)
}

/// Formats the periodic status line for the TUI observation log.
fn tui_status_line(snapshot: &RuntimeSnapshot) -> String {
    let expected = expected_winner_seconds(
        &snapshot.photon_target_le,
        snapshot.search.rate,
        snapshot.network,
    )
    .map(|seconds| format!("{seconds:.0}"))
    .unwrap_or_else(|| "n/a".into());
    // A rig also logs each GPU's recent rate and status.
    let gpus = if snapshot.gpus.len() > 1 {
        let each = snapshot
            .gpus
            .iter()
            .zip(&snapshot.search.gpus)
            .map(|(gpu, search)| {
                format!(
                    "{}:{}={:.0}/{:?}",
                    gpu.backend.as_str(),
                    gpu.device,
                    search.active_rate,
                    search.status
                )
                .to_ascii_lowercase()
            })
            .collect::<Vec<_>>();
        format!(" gpus={}", each.join(","))
    } else {
        String::new()
    };
    format!(
        "status state={:?} waiting_for_job={} key_rotations={} intensity={} rate={:.0} avg_rate={:.0} peak_rate={:.0} expected_winner_s={} reconnects={} rotations={} job_changes={} checks={} batches={} candidates={} verified_winners={} stale_winners={} rejected_winners={} pending_winners={}{gpus} height={} target_le={} endpoint={} last_error={}",
        snapshot.state,
        snapshot.search.waiting_for_job,
        snapshot.search.key_rotations,
        snapshot.search.intensity,
        snapshot.search.current_rate,
        snapshot.search.rate,
        snapshot.search.peak_rate,
        expected,
        snapshot.reconnects,
        snapshot.endpoint_rotations,
        snapshot.job_changes,
        snapshot.state_checks,
        snapshot.search.batches,
        snapshot.search.candidates,
        snapshot.verified_winners,
        snapshot.stale_winners,
        snapshot.search.rejected_winners,
        snapshot.pending_winners,
        snapshot.height,
        if snapshot.photon_target_le.is_empty() {
            "n/a"
        } else {
            snapshot.photon_target_le.as_str()
        },
        redact_endpoint(&snapshot.endpoint),
        snapshot.last_error.as_deref().unwrap_or("none"),
    )
}

/// Runs the mining terminal event loop until exit.
pub fn run(
    supervisor: RuntimeSupervisor,
    devices: Vec<GpuDevice>,
) -> Result<RuntimeSnapshot, String> {
    let initial = supervisor.snapshot();
    let mut state = TuiState::new(&initial);
    state.set_devices(devices);
    let mut terminal = TerminalSession::enter()?;
    let mut quit = false;
    let mut last_draw = Instant::now() - DRAW_INTERVAL;

    let mut last_status_log: Option<Instant> = None;
    while !quit {
        let snapshot = supervisor.snapshot();
        state.record_sample(&snapshot, Instant::now());

        for event in supervisor.drain_events() {
            append_tui_log(&format!("event {}", event_log_text(&event)));
            state.push_event(event_log_text(&event));
        }

        if last_draw.elapsed() >= DRAW_INTERVAL {
            terminal
                .terminal
                .draw(|frame| render(frame, &snapshot, &state))
                .map_err(|error| format!("draw terminal UI: {error}"))?;
            last_draw = Instant::now();
        }

        if last_status_log.is_none_or(|logged| logged.elapsed() >= STATUS_LOG_INTERVAL) {
            append_tui_log(&tui_status_line(&snapshot));
            last_status_log = Some(Instant::now());
        }

        if event::poll(EVENT_POLL_INTERVAL)
            .map_err(|error| format!("poll terminal input: {error}"))?
        {
            if let Event::Key(key) =
                event::read().map_err(|error| format!("read terminal input: {error}"))?
            {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    quit = handle_key(key, &supervisor, &snapshot, &mut state)?;
                }
            }
        }
    }

    drop(terminal);
    Ok(supervisor.stop())
}

/// Returns the render interval for a benchmark run.
pub(crate) fn benchmark_draw_interval() -> Duration {
    DRAW_INTERVAL
}

/// Measures terminal redraw overhead during a GPU benchmark.
pub(crate) fn benchmark_render_load(stop: Arc<AtomicBool>) -> Result<u64, String> {
    let mut snapshot = RuntimeSnapshot {
        state: SupervisorState::Mining,
        gpu_backend: "cuda".into(),
        gpu_device: 0,
        gpus: Vec::new(),
        generation_id: 1,
        network: MiningNetwork::Mainnet,
        fee_scheme: crate::config::MiningToken::Photon
            .fee_policy(MiningNetwork::Mainnet)
            .scheme,
        payout_address: "bitcoincash:qbenchmark".into(),
        endpoint: "offline-benchmark".into(),
        height: 1,
        baton_txid: "00".repeat(32),
        baton_vout: 0,
        photon_target_le: String::new(),
        state_checks: 1,
        transient_refresh_failures: 0,
        transport_failures: 0,
        consecutive_refresh_failures: 0,
        source_degraded: false,
        job_changes: 0,
        reconnects: 0,
        endpoint_rotations: 0,
        stale_winners: 0,
        verified_winners: 0,
        pending_winners: 0,
        last_error: None,
        search: Default::default(),
        gpu_telemetry: Default::default(),
        rigs: None,
        token_donation: crate::donation::TokenDonation::from_bps(400),
        donation_minimum: crate::donation::TokenDonation::from_bps(400),
    };
    snapshot.search.intensity = 100;
    snapshot.search.rate = 500_000.0;

    let state = TuiState::new(&snapshot);
    let backend = CrosstermBackend::new(io::sink());
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(
                0,
                0,
                BENCHMARK_TERMINAL_WIDTH,
                BENCHMARK_TERMINAL_HEIGHT,
            )),
        },
    )
    .map_err(|error| format!("create Ratatui benchmark terminal: {error}"))?;
    let started = Instant::now();
    let mut next_draw = Instant::now();
    let mut draws = 0u64;

    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= next_draw {
            draws = draws.saturating_add(1);
            snapshot.search.candidates = draws.saturating_mul(100_000);
            snapshot.search.batches = draws;
            snapshot.search.elapsed_secs = started.elapsed().as_secs();
            terminal
                .draw(|frame| render(frame, &snapshot, &state))
                .map_err(|error| format!("draw Ratatui benchmark frame: {error}"))?;
            next_draw += DRAW_INTERVAL;
            continue;
        }

        let sleep_for = next_draw
            .saturating_duration_since(now)
            .min(Duration::from_millis(10));
        if !sleep_for.is_zero() {
            thread::sleep(sleep_for);
        }
    }

    Ok(draws)
}

/// Handles keyboard input for the active terminal view.
fn handle_key(
    key: KeyEvent,
    supervisor: &RuntimeSupervisor,
    snapshot: &RuntimeSnapshot,
    state: &mut TuiState,
) -> Result<bool, String> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }

    if state.command_mode {
        match key.code {
            KeyCode::Esc => {
                state.cancel_command();
            }
            KeyCode::Enter => {
                let input = state.finish_command();
                match parse_palette_command(&input) {
                    Ok(command) => {
                        return apply_palette_command(command, supervisor, snapshot, state)
                    }
                    Err(error) => state.status_line = error,
                }
            }
            KeyCode::Backspace => {
                state.command_input.pop();
                state.command_history_cursor = None;
            }
            KeyCode::Up => state.history_previous(),
            KeyCode::Down => state.history_next(),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.command_input.push(ch);
                state.command_history_cursor = None;
            }
            _ => {}
        }
        return Ok(false);
    }

    if state.chart_options.is_some() {
        return Ok(state.chart_options_key(key.code));
    }

    if state.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => state.show_help = false,
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            _ => {}
        }
        return Ok(false);
    }

    // #### PR #32
    // Advanced settings: Left/Right move the token donation in 0.5% steps,
    // never below the token's minimum. It applies to every GPU and rig at
    // once and is saved to the profile when mining stops.
    if state.advanced_mode {
        match key.code {
            KeyCode::Esc | KeyCode::Char('a') | KeyCode::Char('A') => state.advanced_mode = false,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('-') | KeyCode::Char('+') => {
                let raise = matches!(key.code, KeyCode::Right | KeyCode::Char('+'));
                let next = snapshot
                    .token_donation
                    .adjusted(raise, snapshot.donation_minimum);
                if next == snapshot.token_donation {
                    state.status_line = if raise {
                        "The donation is at its highest.".into()
                    } else {
                        format!("{} is this token's minimum.", snapshot.donation_minimum)
                    };
                } else {
                    apply_result(
                        supervisor.set_donation(next),
                        &format!("Donation {next}"),
                        state,
                    );
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            _ => {}
        }
        return Ok(false);
    }

    // #### PR #40
    // Connection info: a number copies that address's rig command.
    if let Some(lines) = state.connect.as_ref() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('I') => state.connect = None,
            KeyCode::Char(digit @ '1'..='9') => {
                if let Some((place, command)) = lines.get(digit as usize - '1' as usize) {
                    state.status_line = if crate::reach::copy(command) {
                        format!("Copied the rig command for {}.", place.label())
                    } else {
                        "Could not copy; select the command with the mouse.".into()
                    };
                }
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            _ => {}
        }
        return Ok(false);
    }

    if state.settings_mode {
        match key.code {
            KeyCode::Esc => {
                state.settings_mode = false;
                return Ok(false);
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                state.open_command("address ");
                return Ok(false);
            }
            KeyCode::Char('f') | KeyCode::Char('F') => {
                state.open_command("endpoint ");
                return Ok(false);
            }
            _ => {}
        }
    }

    if state.logs_mode {
        match key.code {
            KeyCode::Esc | KeyCode::Char('l') | KeyCode::Char('L') => state.logs_mode = false,
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => Ok(true),
        KeyCode::Char('?') => {
            state.show_help = true;
            Ok(false)
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            state.settings_mode = !state.settings_mode;
            Ok(false)
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            state.advanced_mode = true;
            state.settings_mode = false;
            Ok(false)
        }
        // #### PR #40: Connection info, on a coordinator.
        KeyCode::Char('i') | KeyCode::Char('I') => {
            match snapshot.rigs.as_ref() {
                Some(rigs) => {
                    state.connect = Some(crate::rigs::join_lines(
                        rigs,
                        snapshot.network,
                        crate::reach::Interfaces::detect(),
                    ));
                    state.settings_mode = false;
                }
                None => {
                    state.status_line =
                        "Connection info is for a coordinator (--rigs-listen).".into()
                }
            }
            Ok(false)
        }
        KeyCode::Char('/') | KeyCode::Char(':') | KeyCode::Char('c') | KeyCode::Char('C') => {
            state.open_command("");
            Ok(false)
        }
        KeyCode::Char('p') | KeyCode::Char('P') | KeyCode::Char(' ') => {
            toggle_pause(supervisor, snapshot, state)?;
            Ok(false)
        }
        KeyCode::Char('r') | KeyCode::Char('R') => {
            apply_result(supervisor.reconnect(), "Reconnect requested", state);
            Ok(false)
        }
        KeyCode::Char('g') | KeyCode::Char('G') => {
            state.apply_chart_request(ChartRequest::Toggle);
            Ok(false)
        }
        KeyCode::Char('o') | KeyCode::Char('O') => {
            state.apply_chart_request(ChartRequest::Options);
            Ok(false)
        }
        KeyCode::Char('+') | KeyCode::Char(']') => {
            adjust_intensity(supervisor, snapshot, 10, state);
            Ok(false)
        }
        KeyCode::Char('-') | KeyCode::Char('[') => {
            adjust_intensity(supervisor, snapshot, -10, state);
            Ok(false)
        }
        KeyCode::Up | KeyCode::Right => {
            adjust_intensity(supervisor, snapshot, 5, state);
            Ok(false)
        }
        KeyCode::Down | KeyCode::Left => {
            adjust_intensity(supervisor, snapshot, -5, state);
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Changes GPU intensity from a keyboard shortcut.
fn adjust_intensity(
    supervisor: &RuntimeSupervisor,
    snapshot: &RuntimeSnapshot,
    delta: i16,
    state: &mut TuiState,
) {
    let current = i16::from(snapshot.search.intensity);
    let next = (current + delta).clamp(10, 100) as u8;
    if next == snapshot.search.intensity {
        state.status_line = format!("Intensity already at {next}%");
        return;
    }
    apply_result(
        supervisor.set_intensity(next),
        &format!("Intensity set to {next}%"),
        state,
    );
}

/// Switches GPU mining between paused and running states.
fn toggle_pause(
    supervisor: &RuntimeSupervisor,
    snapshot: &RuntimeSnapshot,
    state: &mut TuiState,
) -> Result<(), String> {
    let result = if snapshot.state == SupervisorState::Paused {
        supervisor.resume()
    } else {
        supervisor.pause()
    };
    let success = if snapshot.state == SupervisorState::Paused {
        "Mining resumed"
    } else {
        "Mining paused"
    };
    apply_result(result, success, state);
    Ok(())
}

/// Applies a parsed palette command to the running miner.
fn apply_palette_command(
    command: PaletteCommand,
    supervisor: &RuntimeSupervisor,
    snapshot: &RuntimeSnapshot,
    state: &mut TuiState,
) -> Result<bool, String> {
    match command {
        PaletteCommand::SetIntensity(value) => {
            apply_result(
                supervisor.set_intensity(value),
                &format!("Intensity set to {value}%"),
                state,
            );
        }
        PaletteCommand::Pause => apply_result(supervisor.pause(), "Mining paused", state),
        PaletteCommand::Resume => apply_result(supervisor.resume(), "Mining resumed", state),
        PaletteCommand::Reconnect => {
            apply_result(supervisor.reconnect(), "Reconnect requested", state)
        }
        PaletteCommand::SetPayout(address) => apply_result(
            supervisor.set_payout(address),
            "Payout address updated",
            state,
        ),
        PaletteCommand::SetEndpoint(Some(endpoint)) => apply_result(
            supervisor.set_fulcrum_endpoint(endpoint),
            "Fulcrum endpoint updated; reconnecting",
            state,
        ),
        PaletteCommand::SetEndpoint(None) => apply_result(
            supervisor.clear_fulcrum_endpoint(),
            "Custom Fulcrum endpoint cleared; reconnecting",
            state,
        ),
        PaletteCommand::Status => {
            let status = format!(
                "generation={} height={} rate={} avg={} peak={} target={} pending={}",
                snapshot.generation_id,
                snapshot.height,
                crate::telemetry::format_hash_rate(snapshot.search.current_rate),
                crate::telemetry::format_hash_rate(snapshot.search.rate),
                crate::telemetry::format_hash_rate(snapshot.search.peak_rate),
                crate::telemetry::format_photon_target(&snapshot.photon_target_le),
                snapshot.pending_winners
            );
            state.status_line = status.clone();
            state.push_event(format!("status: {status}"));
        }
        PaletteCommand::Config => {
            let config = format!(
                "config: gpu={} intensity={} payout={} endpoint={} generation={}",
                gpu_list(snapshot).to_ascii_lowercase(),
                snapshot.search.intensity,
                shorten(&snapshot.payout_address, 42),
                shorten(&redact_endpoint(&snapshot.endpoint), 42),
                snapshot.generation_id,
            );
            state.status_line = "Effective configuration added to runtime log".into();
            state.push_event(config);
        }
        PaletteCommand::Logs => {
            state.logs_mode = true;
            state.status_line = format!(
                "Runtime log focused ({} bounded events)",
                state.events.len()
            );
        }
        PaletteCommand::Devices => {
            if state.devices.is_empty() {
                state.push_event(format!(
                    "mining on {} (startup device catalog unavailable)",
                    gpu_list(snapshot)
                ));
            } else {
                state.push_event(format!(
                    "devices: {} detected at startup",
                    state.devices.len()
                ));
                let lines = state
                    .devices
                    .iter()
                    .map(|device| {
                        format!(
                            "device {}:{} {} {} | {}",
                            device.backend.as_str(),
                            device.index,
                            device.vendor,
                            device.name,
                            device.detail
                        )
                    })
                    .collect::<Vec<_>>();
                for line in lines {
                    state.push_event(line);
                }
            }
            state.status_line = "GPU device catalog added to runtime log".into();
        }
        PaletteCommand::Backend => {
            let backend = if snapshot.gpus.len() > 1
                || (snapshot.gpus.is_empty() && snapshot.rigs.is_some())
            {
                format!("GPUs: {}", gpu_list(snapshot))
            } else {
                format!(
                    "backend: {} device {}",
                    snapshot.gpu_backend.to_ascii_uppercase(),
                    snapshot.gpu_device
                )
            };
            state.status_line = backend.clone();
            state.push_event(backend);
        }
        PaletteCommand::Benchmark => {
            let message =
                "Benchmark not started: quit mining and run `pickaxe benchmark`; live mining remains active";
            state.status_line = message.into();
            state.push_event(message.into());
        }
        PaletteCommand::Charts(request) => state.apply_chart_request(request),
        PaletteCommand::Help => state.show_help = true,
        PaletteCommand::Quit => return Ok(true),
    }
    Ok(false)
}

/// Parses `/chart` arguments: none shows the ticked charts, `options`
/// opens the chart menu; also `on`, `off`, `all`, `add <names>`,
/// `remove <names>`, or a list of names to show exactly those charts.
fn parse_chart_request(argument: &str) -> Result<PaletteCommand, String> {
    let words = argument
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let names = |names: &[&str]| -> Result<Vec<ChartMetric>, String> {
        if names.is_empty() {
            return Err("name at least one chart: hash temp power fan util clock".into());
        }
        let mut metrics = Vec::new();
        for name in names {
            let metric = ChartMetric::parse(name).ok_or_else(|| {
                format!("unknown chart {name}. Charts: hash temp power fan util clock.")
            })?;
            if !metrics.contains(&metric) {
                metrics.push(metric);
            }
        }
        Ok(metrics)
    };
    let first = words.first().map(|word| word.to_ascii_lowercase());
    let request = match first.as_deref() {
        None | Some("on" | "show") if words.len() <= 1 => ChartRequest::On,
        Some("options" | "menu" | "choose" | "pick") if words.len() == 1 => ChartRequest::Options,
        Some("off" | "hide" | "none") if words.len() == 1 => ChartRequest::Off,
        Some("all") if words.len() == 1 => ChartRequest::Show(ChartMetric::ALL.to_vec()),
        Some("add" | "+") => ChartRequest::Add(names(&words[1..])?),
        Some("remove" | "rm" | "del" | "-") => ChartRequest::Remove(names(&words[1..])?),
        _ => ChartRequest::Show(names(&words)?),
    };
    Ok(PaletteCommand::Charts(request))
}

/// Shows the result of a palette command in the event log.
fn apply_result(result: Result<(), String>, success: &str, state: &mut TuiState) {
    match result {
        Ok(()) => {
            state.status_line = success.into();
            state.push_event(success.into());
        }
        Err(error) => {
            state.status_line = format!("Error: {error}");
            state.push_event(format!("Control rejected: {error}"));
        }
    }
}

/// Parses interactive text into a runtime control command.
fn parse_palette_command(input: &str) -> Result<PaletteCommand, String> {
    let trimmed = input
        .trim()
        .strip_prefix('/')
        .unwrap_or(input.trim())
        .trim();
    if trimmed.is_empty() {
        return Err("Command required. Try help.".into());
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or_default().to_ascii_lowercase();
    let argument = parts.next().unwrap_or_default().trim();

    match command.as_str() {
        "intensity" => {
            let value: u8 = argument
                .parse()
                .map_err(|_| "usage: intensity <10-100>".to_string())?;
            if !(10..=100).contains(&value) {
                return Err("intensity must be 10..=100".into());
            }
            Ok(PaletteCommand::SetIntensity(value))
        }
        "pause" => Ok(PaletteCommand::Pause),
        "resume" => Ok(PaletteCommand::Resume),
        "reconnect" => Ok(PaletteCommand::Reconnect),
        "wallet" | "address" | "payout" => {
            if argument.is_empty() {
                Err("usage: address <cashaddr>".into())
            } else {
                Ok(PaletteCommand::SetPayout(argument.into()))
            }
        }
        "endpoint" | "fulcrum" => {
            if argument.eq_ignore_ascii_case("clear") || argument.eq_ignore_ascii_case("auto") {
                Ok(PaletteCommand::SetEndpoint(None))
            } else if argument.is_empty() {
                Err("usage: endpoint <wss://...> | endpoint auto".into())
            } else {
                Ok(PaletteCommand::SetEndpoint(Some(argument.into())))
            }
        }
        "status" => Ok(PaletteCommand::Status),
        "config" => Ok(PaletteCommand::Config),
        "logs" | "log" => Ok(PaletteCommand::Logs),
        "devices" | "gpus" => Ok(PaletteCommand::Devices),
        "backend" => Ok(PaletteCommand::Backend),
        "benchmark" | "bench" => Ok(PaletteCommand::Benchmark),
        "chart" | "charts" | "graph" | "graphs" => parse_chart_request(argument),
        "help" | "?" => Ok(PaletteCommand::Help),
        "quit" | "exit" => Ok(PaletteCommand::Quit),
        _ => Err(format!("unknown command {command}. Try help.")),
    }
}

/// Renders the current setup screen.
fn render_setup(frame: &mut Frame<'_>, state: &SetupFlow) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);
    let network = network_label(state.config.network);
    let step = match state.step {
        SetupStep::Profiles => "Profiles".to_string(),
        SetupStep::Hardware => "Setup   1/4 Hardware".to_string(),
        SetupStep::Network => "Setup   2/4 Network".to_string(),
        SetupStep::Token => "Setup   3/4 Token".to_string(),
        SetupStep::Settings | SetupStep::Connections | SetupStep::Gpus => {
            let what = match state.mode {
                MiningMode::Gpu => format!("GPU · {network} · {}", state.config.token.as_str()),
                MiningMode::Asic => {
                    format!("ASIC · {network} · {}", ASIC_TARGETS[state.asic_target])
                }
                MiningMode::Pool => {
                    format!("Pool · {network} · {}", POOL_TARGETS[state.pool_target])
                }
            };
            let who = if state.profile_name_input.trim().is_empty() {
                "new profile".to_string()
            } else {
                format!("profile: {}", state.profile_name_input.trim())
            };
            format!("Settings · {what}   {who}")
        }
    };
    frame.render_widget(
        Paragraph::new(format!("PICKAXE MINER   {step}"))
            .block(Block::default().borders(Borders::ALL)),
        rows[0],
    );
    match state.step {
        SetupStep::Profiles => render_setup_profiles(frame, rows[1], state),
        SetupStep::Hardware => render_setup_hardware(frame, rows[1], state),
        SetupStep::Network => render_setup_network(frame, rows[1], state),
        SetupStep::Token => render_setup_token(frame, rows[1], state),
        SetupStep::Settings => render_setup_settings(frame, rows[1], state),
        SetupStep::Connections => render_setup_connections(frame, rows[1], state),
        SetupStep::Gpus => render_setup_gpus(frame, rows[1], state),
    }
    let keys = if state.editing.is_some() {
        "[Type] edit   [Enter] save   [Esc] cancel"
    } else {
        match state.step {
            SetupStep::Profiles if state.profile_delete_pending => "[Y] delete   [N] keep",
            SetupStep::Profiles => {
                "[Up/Down] choose   [Enter] open   [R] rename   [D] delete   [Esc] quit"
            }
            SetupStep::Hardware | SetupStep::Network => {
                "[Up/Down] choose   [Enter] next   [Esc] back"
            }
            SetupStep::Token if state.mode != MiningMode::Gpu => {
                "[Up/Down] choose   [Enter] next   [Esc] back"
            }
            SetupStep::Token => {
                "[Up/Down] choose   [Type] search name, category or covenant   [Enter] select   [Esc] back"
            }
            SetupStep::Settings => {
                "[Up/Down] row   [Left/Right] change   [Enter] edit / start   [Esc] back"
            }
            SetupStep::Connections => {
                "[Up/Down] choose   [Enter] add / edit   [Del] remove   [Esc] done"
            }
            SetupStep::Gpus => {
                "[Up/Down] choose   [Space] mine on it or not   [A] all   [Enter/Esc] done"
            }
        }
    };
    let message = if state.status_line.is_empty() {
        keys.to_string()
    } else {
        format!("{keys}\n{}", state.status_line)
    };
    frame.render_widget(
        Paragraph::new(message)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        rows[2],
    );
}

fn selection_marker(selected: bool) -> &'static str {
    if selected {
        ">"
    } else {
        " "
    }
}

fn dim(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(Color::DarkGray))
}

/// Shows the text being typed, or the stored value, or a hint when empty.
fn edit_value(state: &SetupFlow, field: TextField, value: &str, hint: &str) -> Span<'static> {
    if state.editing == Some(field) {
        Span::raw(format!("{}_", state.text_input))
    } else if value.trim().is_empty() {
        dim(hint.to_string())
    } else {
        Span::raw(value.to_string())
    }
}

fn render_setup_profiles(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let mut lines = state
        .profiles
        .profiles
        .iter()
        .enumerate()
        .map(|(index, profile)| {
            let settings = &profile.settings;
            let network = settings
                .network
                .as_deref()
                .and_then(|value| MiningNetwork::parse(value).ok())
                .unwrap_or(MiningNetwork::Mainnet);
            let device = state.profile_gpus(settings);
            let selected = state.profile_selected == index;
            let name = if selected && state.editing == Some(TextField::ProfileRename) {
                format!("{}_", state.text_input)
            } else {
                profile.name.clone()
            };
            Line::from(vec![
                Span::raw(format!("{} {name:<18} ", selection_marker(selected))),
                dim(format!(
                    "GPU · {} · {} · {device}",
                    network_label(network),
                    settings.token.as_deref().unwrap_or("PHOTON")
                )),
            ])
        })
        .collect::<Vec<_>>();
    lines.push(Line::from(format!(
        "{} + New profile",
        selection_marker(state.profile_selected == state.profiles.profiles.len())
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(dim(
        "A saved profile opens Settings with Start selected; one more Enter mines.",
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Mining profiles ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_setup_hardware(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    // #### PR #40: running a pool for other miners is a choice of its own.
    let choice = |mode: MiningMode, name: &str, what: &str| {
        Line::from(vec![
            Span::raw(format!(
                "{} {name:<14}",
                selection_marker(state.mode == mode)
            )),
            dim(what.to_owned()),
        ])
    };
    frame.render_widget(
        Paragraph::new(vec![
            choice(
                MiningMode::Gpu,
                "GPU mining",
                "your GPUs; one PC or a farm of rigs",
            ),
            choice(
                MiningMode::Asic,
                "ASIC mining",
                "your devices: solo on your node, or join a pool",
            ),
            choice(
                MiningMode::Pool,
                "Run a pool",
                "let other miners mine on your server",
            ),
            Line::from(""),
            Line::from(dim("This choice changes the screens after it.")),
        ])
        .block(
            Block::default()
                .title(" What do you want to do? ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_setup_network(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "{} Mainnet",
                selection_marker(state.config.network == MiningNetwork::Mainnet)
            )),
            Line::from(vec![
                Span::raw(format!(
                    "{} Chipnet ",
                    selection_marker(state.config.network == MiningNetwork::Chipnet)
                )),
                dim("(test network)"),
            ]),
            Line::from(""),
            Line::from(dim(
                "Sets the tokens, the server list, the address prefix and the donation addresses.",
            )),
        ])
        .block(
            Block::default()
                .title(" Bitcoin Cash network ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_setup_token(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let soon = Style::default().fg(Color::Yellow);
    let lines = if state.mode == MiningMode::Pool {
        // #### PR #40
        vec![
            Line::from(format!(
                "{} {}",
                selection_marker(state.pool_target == 0),
                POOL_TARGETS[0]
            )),
            Line::from(dim(
                "    BCH + merge-mined tokens, for SHA-256 ASICs over SV1 and SV2.",
            )),
            Line::from(format!(
                "{} {}",
                selection_marker(state.pool_target == 1),
                POOL_TARGETS[1]
            )),
            Line::from(dim(
                "    PHOTON and other GPU tokens, for GPU rigs, each with its own address.",
            )),
        ]
    } else if state.mode == MiningMode::Asic {
        vec![
            // #### PR #40: BCH mining works today; only P2Pool v2 is "coming
            // soon", and ASIC-exclusive tokens are not supported yet.
            Line::from(Span::raw(format!(
                "{} {}",
                selection_marker(state.asic_target == 0),
                ASIC_TARGETS[0]
            ))),
            Line::from(dim(
                "    Mines BCH and adds every merge-mined token automatically as each is supported.",
            )),
            Line::from(vec![
                Span::raw(format!(
                    "{} {:<34}",
                    selection_marker(state.asic_target == 1),
                    ASIC_TARGETS[1]
                )),
                Span::styled("not supported yet", soon),
            ]),
            Line::from(dim(
                "    Tokens only ASICs mine. Choose one from this list once supported.",
            )),
        ]
    } else {
        let matches = state.matching_tokens();
        let mut lines = matches
            .iter()
            .enumerate()
            .map(|(index, token)| {
                Line::from(format!(
                    "{} {}  GPU  available",
                    selection_marker(index == state.token_selected),
                    token.as_str()
                ))
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(Line::from("No matching token"));
        }
        lines.extend([
            Line::from(""),
            Line::from(format!("Search: {}", state.token_input)),
            Line::from(dim(
                "Use arrows to choose, or type a token name, category ID or covenant.",
            )),
        ]);
        lines
    };
    let title = match state.mode {
        MiningMode::Gpu => " GPU tokens ",
        MiningMode::Asic => " ASIC targets ",
        MiningMode::Pool => " What will miners mine? ",
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_setup_settings(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let network = state.config.network;
    let label = network_label(network);
    let builtin = match network {
        MiningNetwork::Mainnet => crate::protocol::FULCRUM_WSS_BOOTSTRAP.len(),
        MiningNetwork::Chipnet => crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP.len(),
    };
    let fulcrum = state.sources.list(network, ConnectionKind::Fulcrum).len();
    let nodes = state.sources.list(network, ConnectionKind::Node).len();
    let mut lines = Vec::new();
    for (index, row) in state.settings_rows().into_iter().enumerate() {
        let selected = state.settings_row == index;
        let marker = selection_marker(selected);
        if row == SettingsRow::Start {
            lines.push(Line::from(""));
            lines.push(match state.mode {
                MiningMode::Gpu => Line::from(vec![
                    Span::raw(format!("{marker} ")),
                    Span::styled("Start mining", Style::default().fg(Color::Green)),
                ]),
                MiningMode::Asic if state.asic_target == 0 => Line::from(vec![
                    Span::raw(format!("{marker} ")),
                    Span::styled(
                        "Start the BCH ASIC server",
                        Style::default().fg(Color::Green),
                    ),
                ]),
                MiningMode::Asic => Line::from(vec![
                    Span::raw(format!("{marker} ")),
                    dim("Start (ASIC-exclusive tokens come later)"),
                ]),
                MiningMode::Pool => Line::from(vec![
                    Span::raw(format!("{marker} ")),
                    Span::styled("Start the pool", Style::default().fg(Color::Green)),
                ]),
            });
            continue;
        }
        let (name, value, hint) = match row {
            SettingsRow::Gpu => {
                let gpus = state.chosen_gpus();
                let value = match gpus.as_slice() {
                    [gpu] => format!(
                        "{}:{}  {} {}",
                        gpu.backend.as_str().to_ascii_uppercase(),
                        gpu.index,
                        gpu.vendor,
                        gpu.name
                    ),
                    gpus => format!(
                        "{} GPUs: {}",
                        gpus.len(),
                        gpus.iter()
                            .map(|gpu| gpu.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                ("GPU", Span::raw(value), "< >  [Enter] list")
            }
            SettingsRow::AsicTarget => (
                "ASIC target",
                Span::raw(if state.asic_target == 0 {
                    ASIC_TARGETS[0].to_owned()
                } else {
                    format!("{}  (not supported yet)", ASIC_TARGETS[1])
                }),
                "< >",
            ),
            // #### PR #40
            SettingsRow::Mining => (
                "Mining",
                Span::raw(match (state.mode, state.gpu_join, state.asic_mining) {
                    (MiningMode::Gpu, false, _) => "Alone (claims to your address)",
                    (MiningMode::Gpu, true, _) => "Join a GPU pool or farm",
                    (_, _, AsicMining::Solo) => "Solo (your BCH node)",
                    (_, _, AsicMining::JoinPool) => "Join a pool",
                }),
                "< >",
            ),
            SettingsRow::PoolKind => (
                "Pool type",
                Span::raw(match state.pool_kind {
                    PoolKind::Normal => "Normal pool",
                    PoolKind::P2PoolV2 => "P2Pool v2  (coming soon)",
                }),
                "< >",
            ),
            SettingsRow::PoolAddress => (
                "Pool",
                edit_value(
                    state,
                    TextField::PoolAddress,
                    &state.join_address,
                    "HOST:PORT, or paste stratum2+tcp://HOST:PORT/KEY",
                ),
                "[Enter]",
            ),
            SettingsRow::PoolKey => (
                "Pool key",
                edit_value(
                    state,
                    TextField::PoolKey,
                    &state.join_key,
                    "the authority key the pool publishes",
                ),
                "[Enter]",
            ),
            SettingsRow::PoolFee => (
                "Pool fee",
                Span::raw(if state.pool_target == 1 {
                    format!(
                        "{} of each rig's mining time, after the donation",
                        state.pool_fee
                    )
                } else {
                    format!("{} after the donation", state.pool_fee)
                }),
                "< >",
            ),
            SettingsRow::FeeFrom => (
                "Fee from",
                Span::raw(match state.pool_fee_mode {
                    crate::donation::bch::FeeMode::Coinbase => "Coinbase",
                    crate::donation::bch::FeeMode::Work => "Mining work",
                    crate::donation::bch::FeeMode::Both => "Both (1/3 work, 2/3 coinbase)",
                }),
                "< >",
            ),
            SettingsRow::PoolName => (
                "Pool name",
                edit_value(
                    state,
                    TextField::PoolName,
                    &state.pool_tag,
                    "written into your blocks, such as /MyPool/",
                ),
                "[Enter]",
            ),
            SettingsRow::FeeAddress => (
                "Fee address",
                edit_value(
                    state,
                    TextField::FeeAddress,
                    &state.pool_fee_address,
                    if state.pool_target == 1 {
                        "your payout address; a q address"
                    } else {
                        "your payout address; q or p (multisig)"
                    },
                ),
                "[Enter]",
            ),
            SettingsRow::Address => (
                "Address",
                edit_value(
                    state,
                    TextField::Address,
                    &state.config.payout_address,
                    &format!("type your {label} payout address"),
                ),
                "[Enter]",
            ),
            SettingsRow::Intensity => {
                let filled = usize::from(state.config.intensity / 10);
                (
                    "Intensity",
                    Span::raw(format!(
                        "{}{} {:>3}%",
                        "█".repeat(filled),
                        "░".repeat(10 - filled),
                        state.config.intensity
                    )),
                    "< >",
                )
            }
            SettingsRow::Fulcrum => (
                "Fulcrum",
                Span::raw(format!("{builtin} built-in + {fulcrum} yours ({label})")),
                "[Enter]",
            ),
            // #### PR #40: GPU mining takes its PHOTON jobs from a saved
            // node first, and from the Fulcrum servers without one.
            SettingsRow::Node => (
                "BCH node",
                Span::raw(match (nodes, state.mode) {
                    (0, MiningMode::Gpu) => {
                        format!("none saved for {label}; jobs come from the Fulcrum servers")
                    }
                    (0, _) => format!("none saved for {label}"),
                    (_, MiningMode::Gpu) => {
                        format!("{nodes} saved for {label}; jobs come from your node first")
                    }
                    _ => format!("{nodes} saved for {label}"),
                }),
                "[Enter]",
            ),
            SettingsRow::ProfileName => (
                "Profile name",
                edit_value(
                    state,
                    TextField::ProfileName,
                    &state.profile_name_input,
                    "random name if blank",
                ),
                "[Enter]",
            ),
            SettingsRow::Start => unreachable!("handled above"),
        };
        lines.push(Line::from(vec![
            Span::raw(format!("{marker} {name:<14}")),
            value,
            Span::raw("   "),
            dim(hint),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(dim(
        "Servers and nodes you add are saved once per network and shared by every profile.",
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Settings ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The GPU list: one checkbox per GPU that can mine.
fn render_setup_gpus(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let mut lines = state
        .devices
        .iter()
        .zip(&state.chosen)
        .enumerate()
        .map(|(index, (device, chosen))| {
            Line::from(vec![
                Span::raw(format!(
                    "{} {} {:<40} ",
                    selection_marker(index == state.selected),
                    if *chosen { "[x]" } else { "[ ]" },
                    device.name
                )),
                dim(format!(
                    "{}:{} · {}",
                    device.backend.as_str().to_ascii_uppercase(),
                    device.index,
                    if device.integrated {
                        "integrated"
                    } else {
                        "discrete"
                    }
                )),
            ])
        })
        .collect::<Vec<_>>();
    lines.extend([
        Line::from(""),
        Line::from(dim(
            "Every ticked GPU mines the same job for your address, so they never compete.",
        )),
        Line::from(dim(
            "An integrated GPU shares the CPU's power and memory; it adds little on most PCs.",
        )),
    ]);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" GPUs ").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_setup_connections(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let network = state.config.network;
    let saved = state.sources.list(network, state.connection_kind);
    let mut lines = vec![Line::from(vec![
        Span::raw("Yours "),
        dim("(tried first, unless also built in):"),
    ])];
    for (index, entry) in saved.iter().enumerate() {
        let selected = state.connection_selected == index;
        let shown = if selected && state.editing == Some(TextField::Connection) {
            format!("{}_", state.text_input)
        } else {
            crate::node::redact_url(entry)
        };
        lines.push(Line::from(format!(
            "{} {shown}",
            selection_marker(selected)
        )));
    }
    let adding = state.connection_selected == saved.len();
    let add_label = match state.connection_kind {
        ConnectionKind::Fulcrum => "+ add server",
        ConnectionKind::Node => "+ add node",
    };
    lines.push(Line::from(format!(
        "{} {}",
        selection_marker(adding),
        if adding && state.editing == Some(TextField::Connection) {
            format!("{}_", state.text_input)
        } else {
            add_label.to_string()
        }
    )));
    if let Some(info) = state.local_node_offer() {
        lines.push(Line::from(format!(
            "{} Use the node on this PC: {}",
            selection_marker(state.connection_selected == saved.len() + 1),
            info.summary()
        )));
    }
    lines.push(Line::from(""));
    match state.connection_kind {
        ConnectionKind::Fulcrum => {
            lines.push(Line::from(vec![
                Span::raw("Built-in "),
                dim("(health-ranked once mining starts):"),
            ]));
            let builtin = match network {
                MiningNetwork::Mainnet => crate::protocol::FULCRUM_WSS_BOOTSTRAP,
                MiningNetwork::Chipnet => crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP,
            };
            for endpoint in builtin {
                lines.push(Line::from(dim(format!("  {endpoint}"))));
            }
        }
        // #### PR #40
        // What the check for a node on this computer found, then how to make
        // one answer: BCHN answers RPC with server=1, and with no rpcpassword
        // set Pickaxe logs in with its cookie.
        ConnectionKind::Node => {
            let local = crate::node::local_node_url(network).trim_start_matches("http://");
            match &state.local_node {
                LocalNodeCheck::Running(checked, _) if *checked == network => {
                    lines.push(Line::from(dim(format!(
                        "Looking for a BCH node on this PC ({local})..."
                    ))))
                }
                LocalNodeCheck::Done(checked, result) if *checked == network => match result {
                    crate::node::LocalNode::Found(info) if info.network() == Some(network) => {
                        if state.local_node_offer().is_none() {
                            lines.push(Line::from(dim(format!(
                                "On this PC: {} (saved)",
                                info.summary()
                            ))));
                        }
                    }
                    crate::node::LocalNode::Found(_) => lines.push(Line::from(dim(format!(
                        "The node on this PC ({local}) is not on {}.",
                        network_label(network)
                    )))),
                    crate::node::LocalNode::NeedsLogin => {
                        lines.push(Line::from(dim(format!(
                            "A BCH node answers on this PC ({local}) but wants its RPC login:"
                        ))));
                        lines.push(Line::from(dim(format!(
                            "add it as http://USER:PASSWORD@{local}"
                        ))));
                    }
                    crate::node::LocalNode::Missing => lines.push(Line::from(dim(format!(
                        "No BCH node answers on this PC ({local})."
                    )))),
                },
                _ => {}
            }
            lines.push(Line::from(dim(
                "There are no public nodes: node RPC is private.",
            )));
            lines.push(Line::from(dim(match network {
                MiningNetwork::Mainnet => {
                    "Bitcoin Cash Node on this PC answers with server=1 in bitcoin.conf; Pickaxe reads its cookie, so no password is needed."
                }
                MiningNetwork::Chipnet => {
                    "Bitcoin Cash Node on this PC answers with server=1 and chipnet=1 in bitcoin.conf; Pickaxe reads its cookie, so no password is needed."
                }
            })));
            lines.push(Line::from(dim(
                "A node on another computer needs its RPC login: http://USER:PASSWORD@HOST:PORT.",
            )));
        }
    }
    let title = format!(
        " {} · {} · shared by all profiles ",
        match state.connection_kind {
            ConnectionKind::Fulcrum => "Fulcrum servers",
            ConnectionKind::Node => "BCH nodes",
        },
        network_label(network)
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Renders the active mining dashboard.
fn render(frame: &mut Frame<'_>, snapshot: &RuntimeSnapshot, state: &TuiState) {
    let area = frame.area();
    // The runtime and events panes are only as tall as the runtime rows
    // need, so a tall window never shows empty boxes. Charts are off until
    // asked for; then they get the spare rows, or at least enough for every
    // selected chart, with the least important runtime rows giving way.
    let runtime_width = runtime_pane_width(area.width);
    let available = area.height.saturating_sub(3 + 3 + 4);
    let needed = u16::try_from(runtime_fields(snapshot, runtime_width).len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let (body_height, chart_height) = if state.charts.is_empty() {
        (needed.min(available), 0)
    } else {
        let charts = available
            .saturating_sub(needed)
            .max(charts_min_height(state))
            .min(available.saturating_sub(RUNTIME_MIN_ROWS.min(available)));
        (available - charts, charts)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(body_height),
            Constraint::Length(chart_height),
            Constraint::Length(4),
            Constraint::Min(0),
        ])
        .split(area);

    render_header(frame, rows[0], snapshot);
    render_intensity(frame, rows[1], snapshot);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(runtime_width), Constraint::Min(0)])
        .split(rows[2]);
    render_stats(frame, body[0], snapshot);
    render_events(frame, body[1], state);
    if chart_height > 0 {
        render_charts(frame, rows[3], state);
    }
    render_footer(frame, rows[4], state);

    if state.show_help {
        render_help(frame, area);
    }
    if state.settings_mode {
        render_settings(frame, area, snapshot);
    }
    if state.advanced_mode {
        render_advanced(frame, area, snapshot);
    }
    if state.logs_mode {
        render_logs(frame, area, state);
    }
    if let Some(lines) = state.connect.as_ref() {
        render_connect(frame, area, lines, snapshot);
    }
    if let Some(cursor) = state.chart_options {
        render_chart_options(frame, area, state, cursor);
    }
}

/// Renders the chart options menu: one checkbox per chart.
fn render_chart_options(frame: &mut Frame<'_>, area: Rect, state: &TuiState, cursor: usize) {
    let width = 52.min(area.width);
    let height = (ChartMetric::ALL.len() as u16 + 6).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::from(Span::styled(
            "Space toggles · A all · N none · Enter closes",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];
    for (index, metric) in ChartMetric::ALL.iter().enumerate() {
        let mark = if state.chart_selection.contains(metric) {
            "[x]"
        } else {
            "[ ]"
        };
        let note = if metric_reported(state, *metric) {
            ""
        } else {
            "  (not reported)"
        };
        let style = if index == cursor {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!(" {mark} {}{note} ", metric.title()),
            style,
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(if state.charts.is_empty() {
        "Charts hidden · G shows them"
    } else {
        "Charts shown · G hides them"
    }));
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(" Chart options ")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

/// Renders the dashboard header and live connection state.
fn render_header(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let mut state_text = if snapshot.pending_winners > 0 {
        "WINNER PENDING (GPU PAUSED)".to_string()
    } else {
        format!("{:?}", snapshot.state).to_ascii_uppercase()
    };
    if snapshot.search.waiting_for_job {
        state_text.push_str(" (all nonces tried, waiting for next job)");
    }
    let gpus = if snapshot.gpus.len() > 1 {
        format!("{} GPUs", snapshot.gpus.len())
    } else if snapshot.gpus.is_empty() && snapshot.rigs.is_some() {
        // #### PR #32: `--rigs-only`.
        "no GPU here · rigs mine".to_owned()
    } else {
        format!(
            "{} device {}",
            snapshot.gpu_backend.to_ascii_uppercase(),
            snapshot.gpu_device
        )
    };
    let line = Line::from(vec![
        Span::styled(" PICKAXE ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("PHOTON   {state_text}   {gpus}")),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Renders the GPU intensity control and current value.
fn render_intensity(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let ratio = f64::from(snapshot.search.intensity.min(100)) / 100.0;
    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(" GPU intensity ")
                .borders(Borders::ALL),
        )
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(ratio)
        .label(format!("{}%", snapshot.search.intensity));
    frame.render_widget(gauge, area);
}

/// Width of the label column in the runtime pane.
const RUNTIME_LABEL_WIDTH: usize = 11;

/// One labelled group of runtime rows. When the pane is too short, the
/// group with the largest `priority` number is dropped first.
struct RuntimeField {
    priority: u8,
    lines: Vec<Line<'static>>,
}

impl RuntimeField {
    /// Lays out `rows` under a fixed-width label; continuation rows are
    /// indented to the value column.
    fn new(label: &str, rows: Vec<String>, priority: u8) -> Self {
        let lines = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let label = if index == 0 { label } else { "" };
                Line::from(vec![
                    Span::styled(
                        format!("{label:<RUNTIME_LABEL_WIDTH$}"),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(row),
                ])
            })
            .collect();
        Self { priority, lines }
    }
}

/// Keeps fields in priority order while they fit `height` rows, so a
/// short pane still fills its space, then shows them in display order.
fn fit_runtime_fields(fields: Vec<RuntimeField>, height: usize) -> Vec<Line<'static>> {
    let mut order = (0..fields.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| (fields[index].priority, index));
    let mut keep = vec![false; fields.len()];
    let mut used = 0;
    for index in order {
        let rows = fields[index].lines.len();
        if used + rows <= height {
            keep[index] = true;
            used += rows;
        }
    }
    fields
        .into_iter()
        .zip(keep)
        .filter(|(_, keep)| *keep)
        .flat_map(|(field, _)| field.lines)
        .collect()
}

/// Wraps a value to `width` columns without dropping any of it: breaks
/// between " · " parts first, then between words, then splits a single
/// over-long word into even pieces.
fn wrap_value(value: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut rows = Vec::new();
    let mut current = String::new();
    push_wrapped(value, &[" · ", " "], width, &mut rows, &mut current);
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    rows
}

/// Appends `text` to the wrapped rows, splitting on the coarsest separator
/// that makes each piece fit.
fn push_wrapped(
    text: &str,
    separators: &[&str],
    width: usize,
    rows: &mut Vec<String>,
    current: &mut String,
) {
    let Some((separator, finer)) = separators.split_first() else {
        let chars = text.chars().collect::<Vec<_>>();
        let pieces = chars.len().div_ceil(width).max(1);
        let size = chars.len().div_ceil(pieces).max(1);
        let mut chunks = chars.chunks(size).peekable();
        while let Some(chunk) = chunks.next() {
            let piece = chunk.iter().collect::<String>();
            if chunks.peek().is_some() {
                rows.push(piece);
            } else {
                *current = piece;
            }
        }
        return;
    };
    for part in text.split(separator) {
        let part_len = part.chars().count();
        let joined = if current.is_empty() {
            part_len
        } else {
            current.chars().count() + separator.chars().count() + part_len
        };
        if joined <= width {
            if !current.is_empty() {
                current.push_str(separator);
            }
            current.push_str(part);
            continue;
        }
        if !current.is_empty() {
            rows.push(std::mem::take(current));
        }
        if part_len <= width {
            current.push_str(part);
        } else {
            push_wrapped(part, finer, width, rows, current);
        }
    }
}

/// The full PHOTON target as big-endian hex in 8-digit groups, with as many
/// groups per row as the width allows (8, 4, 2 or 1).
fn target_rows(target_le_hex: &str, width: usize) -> Vec<String> {
    let Ok(mut bytes) = hex::decode(target_le_hex.trim()) else {
        return vec!["unavailable".into()];
    };
    if bytes.len() != 32 {
        return vec!["unavailable".into()];
    }
    bytes.reverse();
    let groups = bytes.chunks(4).map(hex::encode).collect::<Vec<_>>();
    let mut per_row = 8;
    while per_row > 1 && per_row * 9 - 1 > width {
        per_row /= 2;
    }
    groups.chunks(per_row).map(|row| row.join(" ")).collect()
}

/// Formats an integer with thousands separators.
fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Formats a count with a 1000-based K/M/G/T/P/E suffix.
fn format_si_count(value: f64) -> String {
    const UNITS: [&str; 7] = ["", " K", " M", " G", " T", " P", " E"];
    let mut value = if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    };
    let mut unit = 0;
    while unit + 1 < UNITS.len() && value >= 1000.0 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// Formats seconds as its two or three most significant units.
fn format_duration(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        seconds.round() as u64
    } else {
        0
    };
    let (days, hours, minutes, secs) = (
        total / 86_400,
        total / 3_600 % 24,
        total / 60 % 60,
        total % 60,
    );
    if days > 0 {
        format!("{days}d {hours:02}h {minutes:02}m")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m {secs:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Renders live hash rates, winners and GPU statistics. Every value is
/// shown in full: long values wrap under their label, and when the pane is
/// too short the least important fields are left out.
fn render_stats(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let lines = fit_runtime_fields(runtime_field_groups(snapshot, area.width), inner_height);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().title(" Runtime ").borders(Borders::ALL)),
        area,
    );
}

/// Width of the runtime pane: enough for the full target on one row when
/// the window allows, at most 58% of a narrow window, and never less than
/// half; the events pane gets the rest.
fn runtime_pane_width(total: u16) -> u16 {
    const PREFERRED: u16 = 2 + RUNTIME_LABEL_WIDTH as u16 + 71;
    PREFERRED.min(total * 58 / 100).max(total / 2)
}

/// Every runtime row at `pane_width`, before any are dropped for height.
fn runtime_fields(snapshot: &RuntimeSnapshot, pane_width: u16) -> Vec<Line<'static>> {
    runtime_field_groups(snapshot, pane_width)
        .into_iter()
        .flat_map(|field| field.lines)
        .collect()
}

/// Builds the labelled runtime fields for a pane `pane_width` columns wide.
fn runtime_field_groups(snapshot: &RuntimeSnapshot, pane_width: u16) -> Vec<RuntimeField> {
    let inner_width = pane_width.saturating_sub(2) as usize;
    let value_width = inner_width.saturating_sub(RUNTIME_LABEL_WIDTH);
    let wrap = |value: String| wrap_value(&value, value_width);
    let search = &snapshot.search;
    let telemetry = &snapshot.gpu_telemetry;
    let efficiency = telemetry.candidates_per_watt(search.current_rate);
    let last_error = snapshot.last_error.as_deref().unwrap_or("none");
    let hash_rate = crate::telemetry::format_hash_rate;

    let odds = match (
        win_probability(&snapshot.photon_target_le, snapshot.network),
        expected_winner_seconds(&snapshot.photon_target_le, search.rate, snapshot.network),
    ) {
        (Some(probability), _) if snapshot.pending_winners > 0 => format!(
            "1 win per {} hashes · waiting for winner resolution",
            format_si_count(1.0 / probability)
        ),
        (Some(probability), Some(seconds)) => format!(
            "1 win per {} hashes · ~{} at avg rate",
            format_si_count(1.0 / probability),
            format_duration(seconds)
        ),
        (Some(probability), None) => {
            format!("1 win per {} hashes", format_si_count(1.0 / probability))
        }
        _ => "waiting for the live target".into(),
    };
    // A rig shows each GPU's own rate and health under the totals.
    let several = snapshot.gpus.len() > 1;
    let per_gpu = snapshot
        .gpus
        .iter()
        .zip(&search.gpus)
        .enumerate()
        .filter(|_| several)
        .map(|(position, (gpu, gpu_search))| {
            let status = match gpu_search.status {
                GpuStatus::Mining => String::new(),
                GpuStatus::Recovering => " · recovering".into(),
                GpuStatus::Stopped => " · stopped".into(),
            };
            // Telemetry shows only when the card reports it.
            let reported = [
                (gpu.telemetry.temperature_c, " C"),
                (gpu.telemetry.power_watts, " W"),
            ]
            .into_iter()
            .filter_map(|(value, unit)| value.map(|value| format!(" · {value:.1}{unit}")))
            .collect::<String>();
            RuntimeField::new(
                &format!("GPU {position}"),
                wrap(format!(
                    "{} · {}:{} · {}{reported}{status}",
                    gpu.name,
                    gpu.backend.as_str(),
                    gpu.device,
                    hash_rate(gpu_search.active_rate),
                )),
                2,
            )
        })
        .collect::<Vec<_>>();

    // #### PR #32
    // A coordinator shows its rigs' rate too; with no GPU of its own, only
    // theirs.
    let rigs_rate = snapshot.rigs.as_ref().map(|rigs| rigs.rate);
    let mut fields = vec![
        RuntimeField::new(
            "Hashrate",
            wrap(match (snapshot.network, rigs_rate) {
                (_, Some(rigs)) if snapshot.gpus.is_empty() => {
                    format!("{} from the rigs", hash_rate(rigs))
                }
                (_, Some(rigs)) => format!(
                    "{} here + {} rigs = {} · avg here {}",
                    hash_rate(search.current_rate),
                    hash_rate(rigs),
                    hash_rate(search.current_rate + rigs),
                    hash_rate(search.rate),
                ),
                (MiningNetwork::Chipnet, None) => format!(
                    "now {} · active GPU {} · wall avg {}",
                    hash_rate(search.current_rate),
                    hash_rate(search.active_rate),
                    hash_rate(search.rate),
                ),
                (MiningNetwork::Mainnet, None) => format!(
                    "{} · avg {} · peak {}",
                    hash_rate(search.current_rate),
                    hash_rate(search.rate),
                    hash_rate(search.peak_rate),
                ),
            }),
            1,
        ),
        RuntimeField::new(
            "Wins",
            wrap(format!(
                "{} found · {} stale · {} rejected · {} pending",
                snapshot.verified_winners,
                snapshot.stale_winners,
                search.rejected_winners,
                snapshot.pending_winners
            )),
            1,
        ),
        RuntimeField::new("Odds", wrap(odds), 3),
        RuntimeField::new(
            "Target",
            target_rows(&snapshot.photon_target_le, value_width),
            6,
        ),
        RuntimeField::new(
            "Work",
            wrap(format!(
                "{} candidates · {} batches · {} key switches",
                group_digits(search.candidates),
                group_digits(search.batches),
                group_digits(search.key_rotations)
            )),
            5,
        ),
        RuntimeField::new(
            "Uptime",
            wrap(format_duration(search.elapsed_secs as f64)),
            4,
        ),
        RuntimeField::new(
            if several { "All GPUs" } else { "GPU" },
            wrap(format!(
                "util {} · {} · {} · VRAM {}",
                format_metric(telemetry.gpu_utilization_percent, "%"),
                format_metric(telemetry.temperature_c, " C"),
                format_metric(telemetry.power_watts, " W"),
                format_metric(telemetry.vram_used_mib, " MiB"),
            )),
            3,
        ),
        RuntimeField::new(
            "Clocks",
            wrap(format!(
                "core {} · memory {} · {}",
                format_whole(telemetry.graphics_clock_mhz, " MHz"),
                format_whole(telemetry.memory_clock_mhz, " MHz"),
                efficiency
                    .map(|value| format!("{} cand/s/W", format_si_count(value)))
                    .unwrap_or_else(|| "N/A cand/s/W".into()),
            )),
            9,
        ),
        RuntimeField::new("Payout", wrap(snapshot.payout_address.clone()), 7),
        RuntimeField::new(
            "Chain",
            wrap(format!(
                "height {} · generation {} · checks {}",
                snapshot.height,
                snapshot.generation_id,
                group_digits(snapshot.state_checks)
            )),
            6,
        ),
        RuntimeField::new(
            "Baton",
            wrap(format!("{}:{}", snapshot.baton_txid, snapshot.baton_vout)),
            8,
        ),
        RuntimeField::new("Endpoint", wrap(redact_endpoint(&snapshot.endpoint)), 8),
        RuntimeField::new(
            "Network",
            wrap(format!(
                "reconnects {} · rotations {} · jobs {}",
                snapshot.reconnects, snapshot.endpoint_rotations, snapshot.job_changes
            )),
            5,
        ),
        RuntimeField::new(
            "Source",
            wrap(format!(
                "{} · errors: transport {} · refresh {} ({} in a row)",
                if snapshot.source_degraded {
                    "degraded"
                } else {
                    "healthy"
                },
                snapshot.transport_failures,
                snapshot.transient_refresh_failures,
                snapshot.consecutive_refresh_failures
            )),
            5,
        ),
        RuntimeField::new(
            "Last error",
            wrap(last_error.to_string()),
            if snapshot.last_error.is_some() { 2 } else { 9 },
        ),
    ];
    fields.splice(1..1, per_gpu);
    if let Some(rigs) = snapshot.rigs.as_ref() {
        for (offset, rig) in rigs.rigs.iter().enumerate() {
            fields.insert(
                1 + offset,
                RuntimeField::new(
                    "  Rig",
                    wrap(format!(
                        "{} · {} GPUs · {} · winners {} · {}m",
                        rig.name,
                        rig.gpus,
                        crate::telemetry::format_hash_rate(rig.rate),
                        rig.winners,
                        rig.connected_secs / 60
                    )),
                    4,
                ),
            );
        }
        fields.insert(
            1,
            RuntimeField::new(
                "Rigs",
                wrap(format!(
                    "{} connected · {} GPUs · {} · winners {} (rejected {}) · {} · [I] how rigs join",
                    rigs.connected,
                    rigs.gpus,
                    crate::telemetry::format_hash_rate(rigs.rate),
                    rigs.winners,
                    rigs.rejected,
                    rigs.listen,
                )),
                3,
            ),
        );
    }
    fields
}

/// The mining GPUs as `CUDA:0 + WGPU:1`.
fn gpu_list(snapshot: &RuntimeSnapshot) -> String {
    if snapshot.gpus.is_empty() && snapshot.rigs.is_some() {
        return "none here; the rigs mine".to_owned();
    }
    if snapshot.gpus.is_empty() {
        return format!(
            "{}:{}",
            snapshot.gpu_backend.to_ascii_uppercase(),
            snapshot.gpu_device
        );
    }
    snapshot
        .gpus
        .iter()
        .map(|gpu| {
            format!(
                "{}:{}",
                gpu.backend.as_str().to_ascii_uppercase(),
                gpu.device
            )
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Rounds a positive value up to 1, 1.5, 2, 2.5, 3, 4, 5, 6 or 8 times a
/// power of ten, so chart labels read as round numbers.
fn nice_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 1.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(value.log10().floor());
    [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|candidate| *candidate >= value)
        .unwrap_or(10.0 * magnitude)
}

/// Whether the GPU has reported `metric` yet. Before the first sample
/// every metric counts as reported, so its chart space is kept.
fn metric_reported(state: &TuiState, metric: ChartMetric) -> bool {
    state.history.is_empty()
        || state
            .history
            .iter()
            .any(|sample| metric.value(sample).is_some())
}

/// Rows the selected charts need at their smallest, including the time
/// axis under the bottom chart.
fn charts_min_height(state: &TuiState) -> u16 {
    let mut any_chart = false;
    let rows = state
        .charts
        .iter()
        .map(|metric| {
            if metric_reported(state, *metric) {
                any_chart = true;
                CHART_MIN_ROWS
            } else {
                CHART_NOTE_ROWS
            }
        })
        .sum::<u16>();
    if any_chart {
        rows + CHART_AXIS_ROWS
    } else {
        rows
    }
}

/// Stacks the selected charts top to bottom in the order chosen. Reported
/// metrics share the height equally; one the GPU does not report is a
/// short note. Only the bottom chart carries the time axis.
fn render_charts(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    // Keep charts in the chosen order while they fit at their smallest.
    let mut shown = Vec::new();
    let mut used = 0u16;
    for metric in &state.charts {
        let reported = metric_reported(state, *metric);
        let rows = if reported {
            CHART_MIN_ROWS
        } else {
            CHART_NOTE_ROWS
        };
        let axis = if reported && !shown.iter().any(|(_, reported)| *reported) {
            CHART_AXIS_ROWS
        } else {
            0
        };
        if used + rows + axis > area.height && !shown.is_empty() {
            break;
        }
        used += rows + axis;
        shown.push((*metric, reported));
    }
    // Charts share the rows left after the notes and the time axis; the
    // bottom chart also gets the axis rows.
    let axis_chart = shown.iter().rposition(|(_, reported)| *reported);
    let charts = shown.iter().filter(|(_, reported)| *reported).count() as u16;
    let notes = shown.len() as u16 - charts;
    let pool = area
        .height
        .saturating_sub(notes * CHART_NOTE_ROWS)
        .saturating_sub(if charts > 0 { CHART_AXIS_ROWS } else { 0 });
    let mut remainder = if charts > 0 { pool % charts } else { 0 };
    let heights = shown
        .iter()
        .enumerate()
        .map(|(index, (_, reported))| {
            if !*reported {
                return CHART_NOTE_ROWS;
            }
            let mut rows = pool / charts.max(1);
            if remainder > 0 {
                rows += 1;
                remainder -= 1;
            }
            if axis_chart == Some(index) {
                rows += CHART_AXIS_ROWS;
            }
            rows
        })
        .collect::<Vec<_>>();
    let cells = Layout::default()
        .direction(Direction::Vertical)
        .constraints(heights.iter().map(|rows| Constraint::Length(*rows)))
        .split(area);
    for (index, ((metric, _), cell)) in shown.iter().zip(cells.iter()).enumerate() {
        render_metric_chart(frame, *cell, state, *metric, axis_chart == Some(index));
    }
}

/// Charts the last hour of one metric; a hash-rate dip to zero shows idle
/// GPU time.
fn render_metric_chart(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &TuiState,
    metric: ChartMetric,
    time_axis: bool,
) {
    let step = HISTORY_SAMPLE_INTERVAL.as_secs_f64();
    let window = HISTORY_CAP as f64 * step;
    let newest = state.history.len().saturating_sub(1) as f64;
    let points = state
        .history
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| {
            metric
                .value(sample)
                .map(|value| ((index as f64 - newest) * step, value))
        })
        .collect::<Vec<_>>();
    let current = state.history.back().and_then(|sample| metric.value(sample));
    let block = Block::default()
        .title(match current {
            Some(value) => format!(
                " {} · {} · last 60 min ",
                metric.title(),
                metric.format(value)
            ),
            None => format!(" {} · last 60 min ", metric.title()),
        })
        .borders(Borders::ALL);
    if points.is_empty() {
        let message = if state.history.is_empty() {
            "collecting samples…".to_string()
        } else {
            format!(
                "{} is not reported by this GPU",
                metric.title().to_ascii_lowercase()
            )
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }
    let top = metric.axis_top(
        points
            .iter()
            .map(|(_, value)| *value)
            .fold(0.0_f64, f64::max),
    );
    let minutes = |seconds: f64| format!("{:.0}m", seconds / 60.0);
    let mut x_axis = Axis::default()
        .style(Style::default().fg(Color::DarkGray))
        .bounds([-window, 0.0]);
    if time_axis {
        x_axis = x_axis.labels([
            Span::raw(format!("-{}", minutes(window))),
            Span::raw(format!("-{}", minutes(window / 2.0))),
            Span::raw("now"),
        ]);
    }
    // A short chart has room for the bottom and top labels only.
    let y_labels = if area.height >= 8 {
        vec![
            Span::raw(metric.format(0.0)),
            Span::raw(metric.format(top / 2.0)),
            Span::raw(metric.format(top)),
        ]
    } else {
        vec![Span::raw(metric.format(0.0)), Span::raw(metric.format(top))]
    };
    let chart = Chart::new(vec![Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&points)])
    .block(block)
    .x_axis(x_axis)
    .y_axis(
        Axis::default()
            .style(Style::default().fg(Color::DarkGray))
            .bounds([0.0, top])
            .labels(y_labels),
    );
    frame.render_widget(chart, area);
}

/// Formats an optional GPU metric as a whole number.
fn format_whole(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.0}{unit}"))
        .unwrap_or_else(|| "N/A".into())
}

/// Formats an optional GPU metric for the dashboard.
fn format_metric(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.1}{unit}"))
        .unwrap_or_else(|| "N/A".into())
}

/// Renders the bounded runtime event list.
fn render_events(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let height = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    // Newest first. Events keep their full text and wrap with a hanging
    // indent, so a wider pane shows more per row and nothing is cut out.
    let mut items: Vec<ListItem<'_>> = Vec::new();
    let mut used = 0;
    for event in state.events.iter().rev() {
        let mut rows = wrap_value(event, width.saturating_sub(2))
            .into_iter()
            .enumerate()
            .map(|(index, row)| if index == 0 { row } else { format!("  {row}") })
            .map(Line::from)
            .collect::<Vec<_>>();
        if used + rows.len() > height {
            if items.is_empty() {
                rows.truncate(height);
                items.push(ListItem::new(rows));
            }
            break;
        }
        used += rows.len();
        items.push(ListItem::new(rows));
    }
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Recent events ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

/// Renders keyboard shortcuts and the command prompt.
fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let text = if state.command_mode {
        vec![Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::raw(state.command_input.as_str()),
        ])]
    } else {
        vec![
            Line::from(
                "[/] command  [P] pause  [+/-] intensity  [R] reconnect  [S] settings  [G] charts  [O] pick charts  [?] help  [Q] quit",
            ),
            Line::from(state.status_line.as_str()),
        ]
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Renders active miner configuration.
fn render_settings(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let popup = centered_rect(82, 62, area);
    frame.render_widget(Clear, popup);
    let row = |name: &str, value: String, keys: &str| {
        Line::from(vec![
            Span::raw(format!("{name:<11}{value}   ")),
            Span::styled(keys.to_string(), Style::default().fg(Color::DarkGray)),
        ])
    };
    let settings = Paragraph::new(vec![
        Line::from(Span::styled(
            "Mining settings",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        row(
            "Address",
            shorten(&snapshot.payout_address, 56),
            "[A] change",
        ),
        row(
            "Intensity",
            format!("{}%", snapshot.search.intensity),
            "[+/-] change",
        ),
        row(
            "Fulcrum",
            shorten(&redact_endpoint(&snapshot.endpoint), 48),
            "[F] use another server   [R] reconnect",
        ),
        row("GPU", gpu_list(snapshot), ""),
        row(
            "Network",
            network_label(snapshot.network).to_string(),
            "",
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Address and intensity are saved to your profile when you stop. [F] lasts this session.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "Hardware, network, token, GPU and saved servers and nodes are set in setup.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from("[S/Esc] close settings"),
    ])
    .block(Block::default().title(" Settings ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(settings, popup);
}

/// #### PR #32
/// Advanced settings: the token donation, a share of the mining work.
fn render_advanced(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let popup = centered_rect(82, 50, area);
    frame.render_widget(Clear, popup);
    let dim = |text: String| Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)));
    let lines = vec![
        Line::from(Span::styled(
            "Advanced settings",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw(format!("Donation   {}   ", snapshot.token_donation)),
            Span::styled("[←/→] change", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        dim(format!(
            "A share of the mining work itself mines for the Pickaxe donation address. This token's minimum is {}; raise it in 0.5% steps.",
            snapshot.donation_minimum
        )),
        dim("It applies to every GPU and rig at once and is saved to your profile when you stop.".into()),
        Line::from(""),
        Line::from("[A/Esc] close"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Advanced settings ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// #### PR #40
/// Connection info: the command each rig runs to join this coordinator, one
/// per address and numbered for copying, with Tailscale for rigs elsewhere.
fn render_connect(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[(crate::reach::Place, String)],
    snapshot: &RuntimeSnapshot,
) {
    use crate::reach::Place;
    let popup = centered_rect(90, 70, area);
    frame.render_widget(Clear, popup);
    let dim = |text: &str| {
        Line::from(Span::styled(
            text.to_owned(),
            Style::default().fg(Color::DarkGray),
        ))
    };
    let mut text = vec![
        Line::from(Span::styled(
            "How rigs join",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        dim("Run this on each rig, a computer with GPUs and Pickaxe:"),
    ];
    let mut setup_heading = false;
    for (number, (place, command)) in lines.iter().enumerate() {
        // #### PR #40: after the commands, the one-line addresses the
        // setup's Join a GPU pool or farm takes.
        if command.starts_with("stratum2+tcp://") && !setup_heading {
            setup_heading = true;
            text.push(Line::from(""));
            text.push(dim(
                "Or in the setup (GPU mining, Mining: Join a GPU pool or farm), paste:",
            ));
        }
        text.push(Line::from(""));
        text.push(Line::from(format!("[{}] {}", number + 1, place.label())));
        text.push(Line::from(format!("    {command}")));
    }
    text.push(Line::from(""));
    if snapshot.rigs.as_ref().is_some_and(|rigs| rigs.public) {
        text.push(dim(
            "Public pool: each rig puts its own payout address in place of YOUR_BCH_ADDRESS, and its wins are claimed to it.",
        ));
    }
    if lines.iter().all(|(place, _)| *place == Place::ThisComputer) {
        text.push(dim(
            "Only rigs on this computer can join. For rigs on other computers, listen on every interface: --rigs-listen 0.0.0.0:3340.",
        ));
    } else if !lines.iter().any(|(place, _)| *place == Place::Tailscale) {
        text.push(dim(crate::reach::TAILSCALE_HINT));
    }
    text.push(Line::from(""));
    text.push(Line::from(format!(
        "[1-{}] copy   [I/Esc] close",
        lines.len().max(1)
    )));
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .title(" Connection info ")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Renders the interactive keyboard help panel.
fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let popup = centered_rect(76, 72, area);
    frame.render_widget(Clear, popup);
    let help = Paragraph::new(vec![
        Line::from(Span::styled(
            "Pickaxe controls",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("+ / ]        increase intensity 10%"),
        Line::from("- / [        decrease intensity 10%"),
        Line::from("arrows       adjust intensity 5%"),
        Line::from("Space / P    pause or resume"),
        Line::from("R            reconnect the current source"),
        Line::from("S            settings"),
        Line::from("A            advanced settings"),
        Line::from("I            connection info: the command rigs run to join"),
        Line::from("G            show or hide charts (hash rate and temperature at first)"),
        Line::from("O            chart options: pick which charts to show"),
        Line::from("/ (: or C)   command bar"),
        Line::from("?            close/open help"),
        Line::from("Q / Ctrl+C   graceful quit"),
        Line::from(""),
        Line::from("Commands:"),
        Line::from("  /intensity <10-100> | /pause | /resume | /reconnect"),
        Line::from("  /address <cashaddr> | /endpoint <wss://...> | /endpoint auto"),
        Line::from("  /status | /config | /devices | /backend | /logs"),
        Line::from("  /chart shows the ticked charts | /chart options picks them"),
        Line::from("  /chart off | /chart all"),
        Line::from("  /chart add <names> | /chart remove <names> | /chart <names>"),
        Line::from("      names: hash temp power fan util clock"),
        Line::from("  /benchmark | /help | /quit"),
    ])
    .block(Block::default().title(" Help ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(help, popup);
}

/// Renders the diagnostic event log.
fn render_logs(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let popup = centered_rect(92, 82, area);
    frame.render_widget(Clear, popup);
    let max_items = popup.height.saturating_sub(2) as usize;
    let items = state
        .events
        .iter()
        .rev()
        .take(max_items)
        .map(|event| ListItem::new(event.as_str()))
        .collect::<Vec<_>>();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Runtime logs - Esc/L close ")
                .borders(Borders::ALL),
        ),
        popup,
    );
}

/// Computes a centered terminal area for a dialog.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Formats a runtime event for the observation log: the same facts as the
/// TUI event line, but with full error text, txids, and hashes instead of
/// width-truncated ones. Endpoints are still redacted.
fn event_log_text(event: &RuntimeEvent) -> String {
    match event {
        RuntimeEvent::JobRefreshed {
            generation_id,
            height,
            baton_txid,
            baton_vout,
        } => format!("job g={generation_id} h={height} {baton_txid}:{baton_vout}"),
        RuntimeEvent::StateRefreshFailed { error, consecutive } => {
            format!("refresh failed x{consecutive}: {error}")
        }
        RuntimeEvent::Reconnecting(error) => format!("reconnecting {error}"),
        RuntimeEvent::Reconnected(endpoint) => {
            format!("reconnected {}", redact_endpoint(endpoint))
        }
        RuntimeEvent::EndpointRotated { from, to } => format!(
            "switch {} -> {}",
            redact_endpoint(from),
            redact_endpoint(to)
        ),
        RuntimeEvent::StaleWinner {
            winner_generation,
            current_generation,
        } => format!(
            "stale winner discarded: generation {winner_generation} -> {current_generation}"
        ),
        RuntimeEvent::VerifiedWinner(winner) => format!(
            "winner verified: gen={} nonce={} hash={}",
            winner.generation_id,
            winner.nonce,
            hex::encode(winner.digest)
        ),
        RuntimeEvent::SubmissionAccepted {
            parent_txid,
            child_txid,
        } => format!("submission accepted: parent={parent_txid} reward={child_txid}"),
        RuntimeEvent::DirectRewardAccepted { txid, .. } => {
            format!("direct reward accepted: tx={txid}")
        }
        RuntimeEvent::Error(error) => format!("error: {error}"),
    }
}

/// Removes embedded credentials from a displayed endpoint.
fn redact_endpoint(input: &str) -> String {
    let Some((scheme, rest)) = input.split_once("://") else {
        return input.into();
    };
    match rest.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}"),
        None => input.into(),
    }
}

/// Truncates display text to fit the available terminal width.
fn shorten(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.into();
    }
    if max <= 3 {
        return input.chars().take(max).collect();
    }
    let keep = max - 3;
    let head = keep * 2 / 3;
    let tail = keep - head;
    let start: String = input.chars().take(head).collect();
    let end: String = input
        .chars()
        .rev()
        .take(tail)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{start}...{end}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns synthetic GPU devices for interactive setup tests.
    fn test_devices() -> Vec<GpuDevice> {
        vec![
            GpuDevice {
                index: 0,
                name: "Primary CUDA".into(),
                vendor: "NVIDIA".into(),
                vram_bytes: None,
                backend: BackendKind::Cuda,
                detail: String::new(),
                integrated: false,
                ready: true,
                pci: None,
            },
            GpuDevice {
                index: 0,
                name: "Secondary HIP".into(),
                vendor: "AMD".into(),
                vram_bytes: None,
                backend: BackendKind::Hip,
                detail: String::new(),
                integrated: false,
                ready: true,
                pci: None,
            },
        ]
    }

    /// Creates a key event without modifier keys for input tests.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Returns synthetic runtime status for dashboard rendering tests.
    fn test_snapshot() -> RuntimeSnapshot {
        RuntimeSnapshot {
            state: SupervisorState::Mining,
            gpu_backend: "cuda".into(),
            gpu_device: 0,
            gpus: Vec::new(),
            generation_id: 1,
            network: MiningNetwork::Mainnet,
            fee_scheme: crate::config::MiningToken::Photon
                .fee_policy(MiningNetwork::Mainnet)
                .scheme,
            payout_address: "bitcoincash:qexample".into(),
            endpoint: "wss://example.test".into(),
            height: 1,
            baton_txid: "00".repeat(32),
            baton_vout: 0,
            photon_target_le: String::new(),
            state_checks: 0,
            transient_refresh_failures: 0,
            transport_failures: 0,
            consecutive_refresh_failures: 0,
            source_degraded: false,
            job_changes: 0,
            reconnects: 0,
            endpoint_rotations: 0,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: Default::default(),
            gpu_telemetry: Default::default(),
            rigs: None,
            token_donation: crate::donation::TokenDonation::from_bps(400),
            donation_minimum: crate::donation::TokenDonation::from_bps(400),
        }
    }

    /// Renders a setup screen into plain text for assertions.
    fn setup_text(setup: &SetupFlow) -> String {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render_setup(frame, setup)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    fn type_text(setup: &mut SetupFlow, text: &str) {
        for ch in text.chars() {
            setup.handle_key(key(KeyCode::Char(ch)));
        }
    }

    // #### PR #40
    fn chipnet_payout(seed: u8) -> String {
        let public_key = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes([seed; 32]).unwrap(),
        )
        .serialize();
        crate::config::reprefix_p2pkh_payout(
            &crate::reward::p2pkh_cashaddr_from_public_key(&public_key).unwrap(),
            MiningNetwork::Chipnet,
        )
        .unwrap()
    }

    fn setup_for(mode: MiningMode) -> SetupFlow {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.config.set_network(MiningNetwork::Chipnet);
        setup.mode = mode;
        setup
    }

    #[test]
    fn the_first_screen_offers_gpu_asic_and_running_a_pool() {
        let mut setup = setup_for(MiningMode::Gpu);
        setup.step = SetupStep::Hardware;
        let screen = setup_text(&setup);
        assert!(screen.contains("What do you want to do?"), "{screen}");
        assert!(screen.contains("let other miners mine on your server"));
        for expected in [MiningMode::Asic, MiningMode::Pool, MiningMode::Gpu] {
            setup.handle_key(key(KeyCode::Down));
            assert_eq!(setup.mode, expected);
        }
        setup.handle_key(key(KeyCode::Up));
        assert_eq!(setup.mode, MiningMode::Pool);
    }

    #[test]
    fn an_asic_can_join_a_pool_and_p2pool_v2_is_coming() {
        let mut setup = setup_for(MiningMode::Asic);
        setup.open_settings(SettingsRow::Mining);
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.asic_mining, AsicMining::JoinPool);
        let rows = setup.settings_rows();
        for row in [
            SettingsRow::PoolKind,
            SettingsRow::PoolAddress,
            SettingsRow::PoolKey,
        ] {
            assert!(rows.contains(&row), "{row:?}");
        }
        assert!(!rows.contains(&SettingsRow::Node));
        assert!(setup_text(&setup).contains("Join a pool"));
        // P2Pool v2 is listed but not yet startable.
        setup.open_settings(SettingsRow::PoolKind);
        setup.handle_key(key(KeyCode::Right));
        assert!(setup_text(&setup).contains("P2Pool v2  (coming soon)"));
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Continue);
        assert!(setup.status_line.contains("P2Pool v2 is coming soon"));
        setup.pool_kind = PoolKind::Normal;
        // The pool's address and key are asked for, then the payout.
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.current_row(), SettingsRow::PoolAddress);
        setup.handle_key(key(KeyCode::Enter));
        type_text(&mut setup, "pool.example:3336");
        setup.handle_key(key(KeyCode::Enter));
        setup.open_settings(SettingsRow::PoolKey);
        setup.handle_key(key(KeyCode::Enter));
        type_text(
            &mut setup,
            "9auqWEzQDVyLAAnYFbEqV2LDhYMyMEcuBJdJzkWW4GEk2Ss4Dnf",
        );
        setup.handle_key(key(KeyCode::Enter));
        setup.config.payout_address = chipnet_payout(2);
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
        assert_eq!(
            setup.server_setup(),
            Some(ServerSetup::JoinPool {
                address: "pool.example:3336".into(),
                key: "9auqWEzQDVyLAAnYFbEqV2LDhYMyMEcuBJdJzkWW4GEk2Ss4Dnf".into(),
            })
        );
        // Back to solo, the node rows return.
        setup.open_settings(SettingsRow::Mining);
        setup.handle_key(key(KeyCode::Left));
        assert!(setup.settings_rows().contains(&SettingsRow::Node));
        assert_eq!(setup.server_setup(), Some(ServerSetup::Solo));
    }

    // #### PR #40
    #[test]
    fn gpus_can_join_a_gpu_pool_or_farm_as_a_rig() {
        let mut setup = setup_for(MiningMode::Gpu);
        assert!(setup.settings_rows().contains(&SettingsRow::Fulcrum));
        setup.open_settings(SettingsRow::Mining);
        setup.handle_key(key(KeyCode::Right));
        assert!(setup.gpu_join);
        // A rig takes its jobs from its coordinator: no sources of its own.
        let rows = setup.settings_rows();
        assert!(rows.contains(&SettingsRow::PoolAddress) && rows.contains(&SettingsRow::PoolKey));
        assert!(!rows.contains(&SettingsRow::Fulcrum) && !rows.contains(&SettingsRow::Node));
        assert!(setup_text(&setup).contains("Join a GPU pool or farm"));
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert!(
            setup.status_line.contains("coordinator's address"),
            "{}",
            setup.status_line
        );
        // The coordinator's one-line address fills its key too.
        setup.open_settings(SettingsRow::PoolAddress);
        setup.handle_key(key(KeyCode::Enter));
        type_text(&mut setup, "stratum2+tcp://192.168.0.160:3340/KEY");
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(
            (setup.join_address.as_str(), setup.join_key.as_str()),
            ("192.168.0.160:3340", "KEY")
        );
        setup.config.payout_address = chipnet_payout(3);
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
        assert_eq!(
            setup.server_setup(),
            Some(ServerSetup::JoinGpuPool {
                address: "192.168.0.160:3340".into(),
                key: "KEY".into(),
            })
        );
    }

    // #### PR #40
    #[test]
    fn an_asic_pool_can_name_its_blocks() {
        let mut setup = setup_for(MiningMode::Pool);
        assert!(setup.settings_rows().contains(&SettingsRow::PoolName));
        setup.open_settings(SettingsRow::PoolName);
        setup.handle_key(key(KeyCode::Enter));
        type_text(&mut setup, &"x".repeat(21));
        setup.handle_key(key(KeyCode::Enter));
        assert!(
            setup.status_line.contains("20 printable"),
            "{}",
            setup.status_line
        );
        setup.handle_key(key(KeyCode::Esc));
        setup.open_settings(SettingsRow::PoolName);
        setup.handle_key(key(KeyCode::Enter));
        type_text(&mut setup, "/MyPool/");
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup_text(&setup).contains("/MyPool/"));
        let Some(ServerSetup::Public { tag, .. }) = setup.server_setup() else {
            panic!("an ASIC pool setup")
        };
        assert_eq!(tag.as_deref(), Some("/MyPool/"));
        // A GPU pool's claims have no coinbase of their own to name.
        setup.pool_target = 1;
        assert!(!setup.settings_rows().contains(&SettingsRow::PoolName));
    }

    // #### PR #40
    #[test]
    fn only_p2pool_v2_is_coming_soon_and_bch_asic_mining_is_offered() {
        let mut setup = setup_for(MiningMode::Asic);
        setup.step = SetupStep::Token;
        let screen = setup_text(&setup);
        assert!(screen.contains("BCH + all merge-mined tokens"), "{screen}");
        assert!(!screen.contains("coming soon"), "{screen}");
        assert!(screen.contains("not supported yet"), "{screen}");
        setup.open_settings(SettingsRow::AsicTarget);
        assert!(!setup_text(&setup).contains("coming soon"));
    }

    #[test]
    fn running_a_pool_sets_its_fee_and_starts_a_public_pool() {
        use crate::donation::bch::FeeMode;
        let mut setup = setup_for(MiningMode::Pool);
        setup.step = SetupStep::Token;
        let screen = setup_text(&setup);
        assert!(screen.contains("ASIC pool"), "{screen}");
        assert!(screen.contains("GPU pool"));
        assert!(!screen.contains("coming soon"));
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.current_row(), SettingsRow::PoolKind);
        // The fee: 0.5% steps, and where it comes from.
        setup.open_settings(SettingsRow::PoolFee);
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.pool_fee.to_string(), "1.50%");
        setup.open_settings(SettingsRow::FeeFrom);
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.pool_fee_mode, FeeMode::Work);
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.pool_fee_mode, FeeMode::Both);
        setup.handle_key(key(KeyCode::Left));
        assert_eq!(setup.pool_fee_mode, FeeMode::Work);
        // The fee address must be a q or p address on this network.
        setup.open_settings(SettingsRow::FeeAddress);
        setup.handle_key(key(KeyCode::Enter));
        type_text(&mut setup, "not-an-address");
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup.status_line.contains("q or p"));
        setup.handle_key(key(KeyCode::Esc));
        let screen = setup_text(&setup);
        assert!(screen.contains("1.50% after the donation"), "{screen}");
        assert!(screen.contains("Mining work"));
        assert!(screen.contains("Start the pool"));
        // #### PR #40: a GPU pool starts with the token's sources (no node
        // of its own), its fee is mining time, and it pays q addresses.
        setup.config.payout_address = chipnet_payout(3);
        setup.pool_target = 1;
        assert!(!setup.settings_rows().contains(&SettingsRow::FeeFrom));
        assert!(setup.settings_rows().contains(&SettingsRow::Fulcrum));
        let screen = setup_text(&setup);
        assert!(screen.contains("of each rig's mining time"), "{screen}");
        setup.pool_fee_address = crate::tx::cashaddr_with_version(
            1 << 3,
            &[0x33; 20],
            crate::config::MiningNetwork::Chipnet,
        );
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert!(
            setup.status_line.contains("must be a q address"),
            "{}",
            setup.status_line
        );
        setup.pool_fee_address.clear();
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
        assert_eq!(
            setup.server_setup(),
            Some(ServerSetup::GpuPool {
                fee: "1.5".parse().unwrap(),
                address: None,
            })
        );
        // P2Pool v2 is the one thing coming.
        setup.pool_kind = PoolKind::P2PoolV2;
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup.status_line.contains("P2Pool v2 is coming soon"));
        setup.pool_target = 0;
        setup.pool_kind = PoolKind::P2PoolV2;
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup.status_line.contains("P2Pool v2 is coming soon"));
        setup.pool_kind = PoolKind::Normal;
        // A pool builds blocks from the operator's own node.
        setup.config.node_url = None;
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Connections);
        setup.config.node_url = Some("http://user:pass@127.0.0.1:48332".into());
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
        assert_eq!(
            setup.server_setup(),
            Some(ServerSetup::Public {
                fee: "1.5".parse().unwrap(),
                mode: FeeMode::Work,
                address: None,
                tag: None,
            })
        );
    }

    #[test]
    /// Checks that a new setup reaches Settings in three choices.
    fn new_setup_walks_hardware_network_token_to_settings() {
        let devices = test_devices();
        let default_device = devices[0].clone();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices,
            BackendKind::Auto,
            std::slice::from_ref(&default_device),
        )
        .unwrap();
        assert_eq!(setup.selected, 0);
        assert_eq!(setup.step, SetupStep::Hardware);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Network);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Token);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Settings);
        assert_eq!(setup.current_row(), SettingsRow::Gpu);
        assert_eq!(setup.config.token, crate::config::MiningToken::Photon);
        setup.handle_key(key(KeyCode::Esc));
        assert_eq!(setup.step, SetupStep::Token);
    }

    #[test]
    /// Checks that Start needs a valid address and then completes setup.
    fn settings_start_requires_a_valid_address() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Continue);
        assert_eq!(setup.current_row(), SettingsRow::Address);
        assert!(setup.status_line.contains("payout address"));

        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.editing, Some(TextField::Address));
        type_text(&mut setup, "x");
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.editing, Some(TextField::Address));
        assert!(!setup.status_line.is_empty());

        setup.text_input = crate::config::DONATION_ADDRESS.into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.editing, None);
        assert_eq!(setup.config.payout_address, crate::config::DONATION_ADDRESS);
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
    }

    #[test]
    /// Checks that Left/Right change the GPU and intensity rows within range.
    fn settings_rows_change_gpu_and_intensity_in_place() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.open_settings(SettingsRow::Gpu);
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.selected, 1);
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.selected, 0);

        setup.open_settings(SettingsRow::Intensity);
        for _ in 0..20 {
            setup.handle_key(key(KeyCode::Left));
        }
        assert_eq!(setup.config.intensity, 10);
        for _ in 0..20 {
            setup.handle_key(key(KeyCode::Right));
        }
        assert_eq!(setup.config.intensity, 100);
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup.status_line.contains("Left/Right"));
    }

    /// Two discrete GPUs and an integrated one, as setup lists them.
    fn rig_devices() -> Vec<GpuDevice> {
        let mut devices = test_devices();
        devices.push(GpuDevice {
            index: 1,
            name: "Radeon iGPU".into(),
            vendor: "AMD".into(),
            vram_bytes: None,
            backend: BackendKind::Wgpu,
            detail: String::new(),
            integrated: true,
            ready: true,
            pci: None,
        });
        devices
    }

    // #### PR #22 test: choosing several GPUs in setup ####
    #[test]
    fn gpu_list_ticks_several_gpus_and_saves_the_choice() {
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            rig_devices(),
            BackendKind::Auto,
            &[],
        )
        .unwrap();
        // Every discrete GPU mines by default, and that saves as the default.
        assert_eq!(setup.chosen, [true, true, false]);
        assert_eq!(setup.saved_choice(), DeviceSelection::Default);
        setup.open_settings(SettingsRow::Gpu);
        assert!(setup_text(&setup).contains("2 GPUs: Primary CUDA, Secondary HIP"));
        setup.handle_key(key(KeyCode::Right));
        assert!(setup.status_line.contains("press Enter"));

        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Gpus);
        assert!(setup_text(&setup).contains("[ ] Radeon iGPU"));
        setup.handle_key(key(KeyCode::Down));
        setup.handle_key(key(KeyCode::Down));
        setup.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(setup.saved_choice(), DeviceSelection::WithIntegrated);
        setup.handle_key(key(KeyCode::Up));
        setup.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(setup.saved_choice(), DeviceSelection::Indices(vec![0, 2]));
        setup.handle_key(key(KeyCode::Up));
        setup.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(setup.chosen, [false, false, true]);
        // The last ticked GPU stays ticked.
        setup.handle_key(key(KeyCode::Up));
        setup.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(setup.chosen, [false, false, true]);
        assert!(setup.status_line.contains("At least one GPU"));

        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Settings);
        assert_eq!(setup.current_row(), SettingsRow::Gpu);
        assert_eq!(setup.chosen_gpus()[0].name, "Radeon iGPU");
        // With one GPU ticked, Left/Right moves the choice.
        setup.handle_key(key(KeyCode::Right));
        assert_eq!(setup.chosen, [true, false, false]);

        let mut named = SetupFlow::new(
            RuntimeConfig::default(),
            rig_devices(),
            BackendKind::Cuda,
            &rig_devices()[..1],
        )
        .unwrap();
        named.chosen = vec![true, true, true];
        assert_eq!(named.saved_choice(), DeviceSelection::WithIntegrated);
        named.chosen = vec![false, true, true];
        // With a named backend, a saved choice keeps the backend's ordinals.
        assert_eq!(named.saved_choice(), DeviceSelection::Indices(vec![0, 1]));
    }

    #[test]
    fn profiles_restore_their_gpu_choice_and_the_command_line_wins() {
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            rig_devices(),
            BackendKind::Auto,
            &[],
        )
        .unwrap();
        let mut payout = RuntimeConfig::default();
        payout
            .set_payout("bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh".into())
            .unwrap();
        let profile = |backend: &str, devices: DeviceSelection| {
            SavedConfig::from_effective(backend, &devices, &payout)
        };
        for (name, settings) in [
            ("Old", profile("cuda", DeviceSelection::Indices(vec![0]))),
            (
                "Pair",
                profile("auto", DeviceSelection::Indices(vec![0, 2])),
            ),
            ("Every", profile("auto", DeviceSelection::WithIntegrated)),
        ] {
            setup.profiles.upsert(None, name, settings).unwrap();
        }
        let label = |setup: &SetupFlow, name: &str| {
            let profile = setup
                .profiles
                .profiles
                .iter()
                .find(|profile| profile.name == name)
                .unwrap();
            setup.profile_gpus(&profile.settings)
        };
        assert_eq!(label(&setup, "Old"), "Primary CUDA");
        assert_eq!(label(&setup, "Pair"), "2 GPUs");
        assert_eq!(label(&setup, "Every"), "3 GPUs");

        let position = |setup: &SetupFlow, name: &str| {
            setup
                .profiles
                .profiles
                .iter()
                .position(|profile| profile.name == name)
                .unwrap()
        };
        let old = position(&setup, "Old");
        setup.open_profile(old).unwrap();
        assert_eq!(setup.chosen, [true, false, false]);
        let pair = position(&setup, "Pair");
        setup.open_profile(pair).unwrap();
        assert_eq!(setup.chosen, [true, false, true]);
        let every = position(&setup, "Every");
        setup.open_profile(every).unwrap();
        assert_eq!(setup.chosen, [true, true, true]);

        setup.overrides.gpus = Some(vec![(BackendKind::Hip, 0)]);
        setup.open_profile(pair).unwrap();
        assert_eq!(setup.chosen, [false, true, false]);
    }
    // #### end PR #22 test ####

    #[test]
    fn setup_rejects_unknown_token_and_accepts_chipnet_photon() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.handle_key(key(KeyCode::Enter));
        setup.handle_key(key(KeyCode::Enter));
        setup.token_input = "wrong-token".into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Token);
        assert!(setup.status_line.contains("unknown token"));
        setup.handle_key(key(KeyCode::Esc));
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.config.network, MiningNetwork::Chipnet);
        setup.handle_key(key(KeyCode::Enter));
        setup.token_input = crate::protocol::MAINNET_CATEGORY_HEX.into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Token);
        assert!(setup.status_line.contains("unknown token"));
        setup.token_input = "PHOTON".into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Settings);
        assert!(setup.status_line.is_empty());
        assert_eq!(setup.config.network, MiningNetwork::Chipnet);
        assert_eq!(setup.config.token, crate::config::MiningToken::Photon);
    }

    #[test]
    fn asic_path_starts_bch_mining_once_payout_and_node_are_set() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.mode, MiningMode::Asic);
        setup.handle_key(key(KeyCode::Enter));
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Token);
        let token_screen = setup_text(&setup);
        assert!(token_screen.contains("BCH + all merge-mined tokens"));
        assert!(token_screen.contains("ASIC-exclusive token"));
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.asic_target, 1);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Settings);
        let rows = setup.settings_rows();
        assert!(!rows.contains(&SettingsRow::Gpu));
        assert!(!rows.contains(&SettingsRow::Intensity));
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Continue);
        assert!(setup.status_line.contains("not available yet"));
        // BCH: a payout address, then the miner's own node, are required.
        setup.asic_target = 0;
        assert!(setup_text(&setup).contains("Start the BCH ASIC server"));
        setup.config.payout_address.clear();
        setup.open_settings(SettingsRow::Start);
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup.status_line.contains("payout address"));
        let public_key = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes([2; 32]).unwrap(),
        )
        .serialize();
        setup.config.payout_address = crate::config::reprefix_p2pkh_payout(
            &crate::reward::p2pkh_cashaddr_from_public_key(&public_key).unwrap(),
            setup.config.network,
        )
        .unwrap();
        setup.config.node_url = None;
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Continue);
        assert!(setup.status_line.contains("own BCH node"));
        // #### PR #40
        // Straight to the BCH node list, which looks for a node on this PC.
        assert_eq!(setup.step, SetupStep::Connections);
        assert_eq!(setup.connection_kind, ConnectionKind::Node);
        assert!(!matches!(setup.local_node, LocalNodeCheck::Idle));
        setup.config.node_url = Some("http://user:pass@127.0.0.1:8332".into());
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
    }

    // #### PR #40
    /// Waits for the background check for a node on this computer.
    fn finish_local_node_check(setup: &mut SetupFlow) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while setup.checking_local_node() {
            assert!(Instant::now() < deadline, "local node check never finished");
            thread::sleep(Duration::from_millis(2));
            setup.poll_local_node();
        }
    }

    #[test]
    fn the_node_list_offers_a_node_found_on_this_computer() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.config.set_network(MiningNetwork::Chipnet);
        setup.probe_local_node = |network| {
            assert_eq!(network, MiningNetwork::Chipnet);
            crate::node::LocalNode::Found(crate::node::NodeInfo {
                client: "Bitcoin Cash Node 29.1.0".into(),
                chain: "chip".into(),
                blocks: 326900,
                headers: 326900,
                syncing: false,
                progress: 1.0,
            })
        };
        setup.open_settings(SettingsRow::Node);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Connections);
        finish_local_node_check(&mut setup);

        // The cursor moved from "+ add node" to the node found, so Enter
        // saves it for every profile on Chipnet.
        let screen = setup_text(&setup);
        assert!(
            screen.contains(
                "> Use the node on this PC: Bitcoin Cash Node 29.1.0 · synced at height 326900"
            ),
            "{screen}"
        );
        setup.handle_key(key(KeyCode::Enter));
        assert!(setup.status_line.contains("Added the BCH node on this PC"));
        assert_eq!(
            setup
                .sources
                .list(MiningNetwork::Chipnet, ConnectionKind::Node),
            ["http://127.0.0.1:48332"]
        );
        assert_eq!(
            setup.config.custom_node_endpoints(),
            ["http://127.0.0.1:48332"]
        );
        let screen = setup_text(&setup);
        assert!(!screen.contains("Use the node on this PC"));
        assert!(screen
            .contains("On this PC: Bitcoin Cash Node 29.1.0 · synced at height 326900 (saved)"));

        // A node on the other network is not offered.
        setup
            .sources
            .remove(MiningNetwork::Chipnet, ConnectionKind::Node, 0);
        setup.probe_local_node = |_| {
            crate::node::LocalNode::Found(crate::node::NodeInfo {
                client: "Bitcoin Cash Node 29.1.0".into(),
                chain: "main".into(),
                blocks: 900000,
                headers: 900000,
                syncing: false,
                progress: 1.0,
            })
        };
        setup.open_settings(SettingsRow::Node);
        setup.handle_key(key(KeyCode::Enter));
        finish_local_node_check(&mut setup);
        assert!(setup.local_node_offer().is_none());
        assert!(setup_text(&setup).contains("is not on Chipnet"));

        // A node that wants its login, and no node at all, are explained.
        setup.probe_local_node = |_| crate::node::LocalNode::NeedsLogin;
        setup.open_settings(SettingsRow::Node);
        setup.handle_key(key(KeyCode::Enter));
        finish_local_node_check(&mut setup);
        let screen = setup_text(&setup);
        assert!(screen.contains("wants its RPC login"), "{screen}");
        assert!(screen.contains("http://USER:PASSWORD@127.0.0.1:48332"));
        setup.probe_local_node = |_| crate::node::LocalNode::Missing;
        setup.open_settings(SettingsRow::Node);
        setup.handle_key(key(KeyCode::Enter));
        finish_local_node_check(&mut setup);
        let screen = setup_text(&setup);
        assert!(screen.contains("No BCH node answers on this PC (127.0.0.1:48332)."));
        assert!(screen.contains("server=1 and chipnet=1"));
        assert_eq!(setup.handle_key(key(KeyCode::Down)), SetupAction::Continue);
        assert_eq!(setup.connection_selected, 0);
    }

    #[test]
    fn setup_screens_render_their_choices() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        for (step, expected) in [
            (SetupStep::Hardware, "ASIC mining"),
            (SetupStep::Network, "Chipnet"),
            (SetupStep::Token, "> PHOTON  GPU  available"),
            (SetupStep::Settings, "Start mining"),
            (SetupStep::Connections, "shared by all profiles"),
        ] {
            setup.step = step;
            let rendered = setup_text(&setup);
            assert!(rendered.contains(expected), "{step:?} missing {expected}");
        }
        setup.config.set_network(MiningNetwork::Chipnet);
        setup.step = SetupStep::Settings;
        assert!(setup_text(&setup).contains(&format!(
            "{} built-in + 0 yours (Chipnet)",
            crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP.len()
        )));
        setup.step = SetupStep::Connections;
        assert!(setup_text(&setup).contains("chipnet.bch.ninja"));
    }

    fn setup_with_saved_profile() -> (SetupFlow, RuntimeConfig) {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        let mut saved = RuntimeConfig::default();
        saved
            .set_payout("bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh".into())
            .unwrap();
        saved.set_intensity(70).unwrap();
        setup
            .profiles
            .upsert(
                None,
                "Rig A",
                SavedConfig::from_effective(
                    "cuda",
                    &crate::backend::DeviceSelection::Indices(vec![0]),
                    &saved,
                ),
            )
            .unwrap();
        setup.step = SetupStep::Profiles;
        (setup, saved)
    }

    #[test]
    fn saved_profile_opens_settings_ready_to_start() {
        let (mut setup, saved) = setup_with_saved_profile();
        assert!(setup_text(&setup).contains("GPU · Mainnet · PHOTON"));
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Settings);
        assert_eq!(setup.current_row(), SettingsRow::Start);
        assert_eq!(setup.profile_name_input, "Rig A");
        assert_eq!(setup.config.intensity, 70);
        assert_eq!(setup.config.payout_address, saved.payout_address);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);

        setup.step = SetupStep::Profiles;
        setup.overrides.intensity = Some(90);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.config.intensity, 90);
        setup.handle_key(key(KeyCode::Esc));
        assert_eq!(setup.step, SetupStep::Profiles);
    }

    #[test]
    fn setup_network_change_requires_a_matching_payout_before_start() {
        let (mut setup, saved) = setup_with_saved_profile();
        setup.handle_key(key(KeyCode::Enter));
        setup.step = SetupStep::Network;
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.config.network, MiningNetwork::Chipnet);
        assert_eq!(setup.config.payout_address, saved.payout_address);
        setup.open_settings(SettingsRow::Start);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Continue);
        assert!(setup.status_line.contains("bchtest:"));
        let chipnet =
            crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x42; 20], MiningNetwork::Chipnet)
                .unwrap();
        setup.config.set_payout(chipnet).unwrap();
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
    }

    #[test]
    fn profiles_can_be_renamed_and_deleted_after_confirmation() {
        let (mut setup, _) = setup_with_saved_profile();
        setup.handle_key(key(KeyCode::Char('r')));
        assert_eq!(setup.editing, Some(TextField::ProfileRename));
        setup.text_input = "Rig B".into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.profiles.profiles[0].name, "Rig B");

        setup.handle_key(key(KeyCode::Char('d')));
        assert!(setup.profile_delete_pending);
        assert!(setup_text(&setup).contains("[Y] delete"));
        setup.handle_key(key(KeyCode::Char('n')));
        assert!(!setup.profile_delete_pending);
        assert_eq!(setup.profiles.profiles.len(), 1);

        setup.handle_key(key(KeyCode::Char('d')));
        setup.handle_key(key(KeyCode::Char('y')));
        assert!(setup.profiles.profiles.is_empty());
        assert!(setup.status_line.contains("Deleted profile"));
        assert_eq!(setup.profile_selected, 0);
        // Only "+ New profile" remains, and it cannot be deleted.
        setup.handle_key(key(KeyCode::Char('d')));
        assert!(!setup.profile_delete_pending);
    }

    #[test]
    fn connections_are_validated_and_kept_per_network() {
        let devices = test_devices();
        let mut setup = SetupFlow::new(
            RuntimeConfig::default(),
            devices.clone(),
            BackendKind::Auto,
            &devices[..1],
        )
        .unwrap();
        setup.open_settings(SettingsRow::Fulcrum);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Connections);
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.editing, Some(TextField::Connection));
        type_text(&mut setup, "https://wrong");
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.editing, Some(TextField::Connection));
        assert!(setup.status_line.contains("fulcrum URL"));
        setup.text_input = "wss://example.test:50004".into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.editing, None);
        assert_eq!(
            setup.config.fulcrum_url.as_deref(),
            Some("wss://example.test:50004")
        );

        // The entry belongs to Mainnet only and comes back with it.
        setup.config.set_network(MiningNetwork::Chipnet);
        setup.apply_connections().unwrap();
        assert!(setup.config.fulcrum_url.is_none());
        setup.config.set_network(MiningNetwork::Mainnet);
        setup.apply_connections().unwrap();
        assert_eq!(
            setup.config.fulcrum_url.as_deref(),
            Some("wss://example.test:50004")
        );

        setup.handle_key(key(KeyCode::Up));
        setup.handle_key(key(KeyCode::Up));
        assert_eq!(setup.connection_selected, 0);
        setup.handle_key(key(KeyCode::Delete));
        assert!(setup.config.fulcrum_url.is_none());
        assert!(setup.status_line.contains("Removed from every profile"));

        setup.handle_key(key(KeyCode::Esc));
        assert_eq!(setup.current_row(), SettingsRow::Fulcrum);
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.current_row(), SettingsRow::Node);
        setup.handle_key(key(KeyCode::Enter));
        setup.handle_key(key(KeyCode::Enter));
        setup.text_input = "http://127.0.0.1:8332".into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(
            setup.config.node_url.as_deref(),
            Some("http://127.0.0.1:8332")
        );
        assert!(setup_text(&setup).contains("BCH nodes · Mainnet"));
    }

    #[test]
    /// Checks that palette parses shared runtime controls.
    fn palette_parses_shared_runtime_controls() {
        assert_eq!(
            parse_palette_command("intensity 75").unwrap(),
            PaletteCommand::SetIntensity(75)
        );
        assert_eq!(
            parse_palette_command("address bitcoincash:qexample").unwrap(),
            PaletteCommand::SetPayout("bitcoincash:qexample".into())
        );
        assert_eq!(
            parse_palette_command("endpoint wss://example.test:50004").unwrap(),
            PaletteCommand::SetEndpoint(Some("wss://example.test:50004".into()))
        );
        assert_eq!(
            parse_palette_command("endpoint clear").unwrap(),
            PaletteCommand::SetEndpoint(None)
        );
        assert_eq!(
            parse_palette_command("reconnect").unwrap(),
            PaletteCommand::Reconnect
        );
        assert_eq!(parse_palette_command("quit").unwrap(), PaletteCommand::Quit);
    }

    #[test]
    /// Checks that slash palette parses extended runtime commands.
    fn slash_palette_parses_extended_runtime_commands() {
        assert_eq!(
            parse_palette_command("/endpoint auto").unwrap(),
            PaletteCommand::SetEndpoint(None)
        );
        assert_eq!(
            parse_palette_command("/status").unwrap(),
            PaletteCommand::Status
        );
        assert_eq!(
            parse_palette_command("/config").unwrap(),
            PaletteCommand::Config
        );
        assert_eq!(
            parse_palette_command("/logs").unwrap(),
            PaletteCommand::Logs
        );
        assert_eq!(
            parse_palette_command("/devices").unwrap(),
            PaletteCommand::Devices
        );
        assert_eq!(
            parse_palette_command("/backend").unwrap(),
            PaletteCommand::Backend
        );
        assert_eq!(
            parse_palette_command("/benchmark").unwrap(),
            PaletteCommand::Benchmark
        );
    }

    #[test]
    /// Checks that command history is bounded and restores draft.
    fn command_history_is_bounded_and_restores_draft() {
        let snapshot = test_snapshot();
        let mut state = TuiState::new(&snapshot);
        for index in 0..(COMMAND_HISTORY_CAP + 8) {
            state.open_command("");
            state.command_input = format!("status {index}");
            let _ = state.finish_command();
        }
        assert_eq!(state.command_history.len(), COMMAND_HISTORY_CAP);
        assert_eq!(state.command_history.front().unwrap(), "status 8");
        assert_eq!(
            state.command_history.back().unwrap(),
            &format!("status {}", COMMAND_HISTORY_CAP + 7)
        );

        state.open_command("sta");
        state.history_previous();
        assert_eq!(
            state.command_input,
            format!("status {}", COMMAND_HISTORY_CAP + 7)
        );
        state.history_next();
        assert_eq!(state.command_input, "sta");
    }

    #[test]
    /// Checks that eighty column footer keeps slash command hint visible.
    fn eighty_column_footer_keeps_slash_command_hint_visible() {
        let snapshot = test_snapshot();
        let state = TuiState::new(&snapshot);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("[/] command"));
    }

    #[test]
    /// Checks that the observation log line carries rates, churn, and winners.
    fn tui_status_line_reports_rates_churn_and_winners() {
        let mut snapshot = test_snapshot();
        snapshot.search.current_rate = 16_900_000.4;
        snapshot.search.rate = 16_800_000.0;
        snapshot.search.peak_rate = 17_000_000.0;
        snapshot.reconnects = 3;
        snapshot.endpoint_rotations = 1;
        snapshot.verified_winners = 5;
        snapshot.stale_winners = 2;
        snapshot.search.rejected_winners = 1;
        let line = tui_status_line(&snapshot);
        for field in [
            "rate=16900000 ",
            "avg_rate=16800000 ",
            "peak_rate=17000000 ",
            "reconnects=3 ",
            "rotations=1 ",
            "verified_winners=5 ",
            "stale_winners=2 ",
            "rejected_winners=1 ",
        ] {
            assert!(line.contains(field), "{field} missing from {line}");
        }
        assert!(line.starts_with("status "));
        assert!(!line.contains('\n'));
    }

    #[test]
    /// Checks the expected time between winners from the live target.
    fn expected_winner_seconds_follows_target_and_rate() {
        // Target 2^224 (LE byte 28 = 1): v3.2 needs a positive digest, so one
        // winner per 2^32 candidates.
        let mut target = [0u8; 32];
        target[28] = 1;
        let target = hex::encode(target);
        let seconds =
            expected_winner_seconds(&target, 2f64.powi(32) / 100.0, MiningNetwork::Mainnet)
                .unwrap();
        assert!((seconds - 100.0).abs() < 1e-6, "{seconds}");
        assert!(expected_winner_seconds(&target, 0.0, MiningNetwork::Mainnet).is_none());
        assert!(expected_winner_seconds("", 1.0, MiningNetwork::Mainnet).is_none());
        assert!(expected_winner_seconds(&"00".repeat(32), 1.0, MiningNetwork::Mainnet).is_none());
    }

    #[test]
    fn chipnet_live_target_odds_use_the_full_digest_range() {
        // The chipnet dashboard showed 1/5.2K for this live target. The
        // positive-digest covenant actually gives about 1/10.4K.
        let mut target =
            hex::decode("000653ac2450dad20d93e4c456b89bb5f915e110e5c3fec49f74cfb564310cfa")
                .unwrap();
        target.reverse();
        let target = hex::encode(target);
        let probability = win_probability(&target, MiningNetwork::Chipnet).unwrap();
        let candidates_per_win = 1.0 / probability;
        assert!(
            (10_300.0..10_400.0).contains(&candidates_per_win),
            "{candidates_per_win}"
        );
        // #### PR #22: win odds follow the deployment's proof rule
        // Mainnet runs the same v3.2 contract; only the retired v0 rule, which
        // ignores digest bit 255, wins twice as often.
        assert_eq!(
            win_probability(&target, MiningNetwork::Mainnet),
            Some(probability)
        );
        let v0_candidates_per_win = 1.0
            / win_probability_for_rule(&target, crate::protocol::MAINNET_V0_PHOTON.proof_rule)
                .unwrap();
        assert!((5_100.0..5_300.0).contains(&v0_candidates_per_win));

        let mut snapshot = test_snapshot();
        snapshot.network = MiningNetwork::Chipnet;
        snapshot.photon_target_le = target;
        snapshot.search.rate = 1.0e9;
        let odds = runtime_field_groups(&snapshot, 140)
            .into_iter()
            .find(|field| field.lines[0].spans[0].content.starts_with("Odds"))
            .unwrap();
        assert!(odds
            .lines
            .iter()
            .any(|line| line.spans[1].content.contains("10.4 K")));
    }

    #[test]
    fn paused_runtime_shows_last_active_gpu_rate() {
        let mut snapshot = test_snapshot();
        snapshot.network = MiningNetwork::Chipnet;
        snapshot.state = SupervisorState::Paused;
        snapshot.search.current_rate = 0.0;
        snapshot.search.active_rate = 1.23e9;
        snapshot.search.rate = 928_400.0;
        let hash_rate = runtime_field_groups(&snapshot, 140)
            .into_iter()
            .find(|field| field.lines[0].spans[0].content.starts_with("Hashrate"))
            .unwrap();
        let displayed = hash_rate
            .lines
            .iter()
            .map(|line| line.spans[1].content.as_ref())
            .collect::<String>();
        assert!(displayed.contains("now 0.00 H/s"), "{displayed}");
        assert!(displayed.contains("active GPU 1.23 GH/s"), "{displayed}");
        assert!(displayed.contains("wall avg 928.4 KH/s"), "{displayed}");
        assert!(!displayed.contains("peak"), "{displayed}");

        snapshot.network = MiningNetwork::Mainnet;
        let mainnet = runtime_field_groups(&snapshot, 140)
            .into_iter()
            .find(|field| field.lines[0].spans[0].content.starts_with("Hashrate"))
            .unwrap();
        let displayed = mainnet
            .lines
            .iter()
            .map(|line| line.spans[1].content.as_ref())
            .collect::<String>();
        assert!(displayed.contains("avg 928.4 KH/s · peak 0.00 H/s"));
        assert!(!displayed.contains("active GPU"));

        snapshot.network = MiningNetwork::Chipnet;
        snapshot.pending_winners = 1;
        snapshot.photon_target_le = "01".repeat(32);
        let rows = rendered_rows(&snapshot, 120, 40);
        assert!(rows
            .iter()
            .any(|row| row.contains("WINNER PENDING (GPU PAUSED)")));
        assert!(rows
            .iter()
            .any(|row| row.contains("waiting for winner resolution")));
    }

    #[test]
    fn runtime_view_lists_each_gpu_of_a_rig() {
        let mut snapshot = test_snapshot();
        let gpu = |backend, device, name: &str, temperature| crate::runtime::RuntimeGpu {
            backend,
            device,
            name: name.into(),
            telemetry: crate::telemetry::GpuTelemetry {
                temperature_c: Some(temperature),
                ..Default::default()
            },
        };
        snapshot.gpus = vec![
            gpu(BackendKind::Cuda, 0, "RTX 5070 Ti", 71.0),
            gpu(BackendKind::Wgpu, 1, "Radeon 610M", 55.0),
        ];
        let search = |backend, device, active_rate, status| crate::search::GpuSearchStats {
            backend,
            device,
            candidates: 1,
            rate: active_rate,
            active_rate,
            winners: 0,
            status,
            last_error: None,
        };
        snapshot.search.gpus = vec![
            search(BackendKind::Cuda, 0, 1.45e9, GpuStatus::Mining),
            search(BackendKind::Wgpu, 1, 2.9e7, GpuStatus::Recovering),
        ];
        let state = TuiState::new(&snapshot);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(140, 45)).unwrap();
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("2 GPUs"));
        assert!(rendered.contains("RTX 5070 Ti · cuda:0 · 1.45 GH/s · 71.0 C"));
        assert!(rendered.contains("Radeon 610M · wgpu:1 · 29.00 MH/s · 55.0 C"));
        assert!(rendered.contains("recovering"));
        assert!(rendered.contains("All GPUs"));
        let line = tui_status_line(&snapshot);
        assert!(line.contains(
            "pending_winners=0 gpus=cuda:0=1450000000/mining,wgpu:1=29000000/recovering height="
        ));
        assert_eq!(gpu_list(&snapshot), "CUDA:0 + WGPU:1");
    }

    #[test]
    /// Checks that runtime view renders shared gpu telemetry and efficiency.
    fn runtime_view_renders_shared_gpu_telemetry_and_efficiency() {
        let mut snapshot = test_snapshot();
        snapshot.search.current_rate = 500_000.0;
        snapshot.search.rate = 1.2e9;
        snapshot.search.peak_rate = 2.5e15;
        snapshot.search.rejected_winners = 2;
        snapshot.photon_target_le = "ab".repeat(32);
        snapshot.gpu_telemetry = crate::telemetry::GpuTelemetry {
            samples: 2,
            gpu_utilization_percent: Some(88.0),
            power_watts: Some(100.0),
            temperature_c: Some(72.0),
            vram_used_mib: Some(640.0),
            graphics_clock_mhz: Some(2_500.0),
            memory_clock_mhz: Some(8_100.0),
            fan_percent: Some(45.0),
        };
        let state = TuiState::new(&snapshot);
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("util 88.0%"));
        assert!(rendered.contains("72.0 C"));
        assert!(rendered.contains("100.0 W"));
        assert!(rendered.contains("5.0 K cand/s/W"));
        assert!(rendered.contains("core 2500 MHz"));
        assert!(rendered.contains("500.0 KH/s"));
        assert!(rendered.contains("1.20 GH/s"));
        assert!(rendered.contains("2.50 PH/s"));
        assert_eq!(rendered.matches("abababab").count(), 8, "full target");
        assert!(!rendered.contains("..."), "nothing is cut short");
        assert!(rendered.contains("reconnects"));
        assert!(
            rendered.contains("2 rejected"),
            "host-rejected GPU winners must be visible on the runtime dashboard"
        );
    }

    #[test]
    /// Checks that runtime view shows live intensity and reconnect counts.
    fn runtime_view_shows_live_intensity_and_reconnect_counts() {
        let mut snapshot = test_snapshot();
        snapshot.search.intensity = 30;
        snapshot.reconnects = 4;
        snapshot.endpoint_rotations = 2;
        snapshot.job_changes = 1;
        let mut state = TuiState::new(&snapshot);
        state.push_event(event_log_text(&RuntimeEvent::EndpointRotated {
            from: "wss://blackie.c3-soft.com:50004".into(),
            to: "wss://bch.soul-dev.com:50004".into(),
        }));
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let width = 120usize;
        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|row| row.contains("30%")));
        assert!(
            rows.iter()
                .any(|row| row.contains("reconnects 4") && row.contains("rotations 2")),
            "reconnect and rotation counts must share one 120-column row: {rows:?}"
        );
        assert!(rows.iter().any(|row| row.contains("jobs 1")));
        let row_of = |needle: &str| rows.iter().position(|row| row.contains(needle)).unwrap();
        assert!(
            row_of("Hashrate ") < row_of("Wins ") && row_of("Wins ") < row_of("reconnects "),
            "hash rate, then winners, then connection counters: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("supervising PHOTON state")),
            "startup event must fit on one events row"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("switch ") && row.contains("->")),
            "peer switch must be visible on one events row: {rows:?}"
        );
    }

    // #### PR #32
    #[test]
    fn advanced_settings_show_the_donation_and_its_minimum() {
        let mut snapshot = test_snapshot();
        snapshot.token_donation = crate::donation::TokenDonation::from_bps(550);
        let mut state = TuiState::new(&snapshot);
        state.advanced_mode = true;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Advanced settings"), "{text}");
        assert!(text.contains("Donation   5.50%"), "{text}");
        assert!(text.contains("minimum is 4.00%"), "{text}");
        // The main dashboard does not show the slider.
        state.advanced_mode = false;
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!text.contains("Donation   5.50%"), "{text}");
    }

    #[test]
    fn a_coordinator_without_gpus_says_its_rigs_mine() {
        let mut snapshot = test_snapshot();
        snapshot.gpus.clear();
        snapshot.gpu_backend = crate::runtime::RIGS_ONLY.into();
        snapshot.rigs = Some(crate::rigs::RigSummary {
            listen: "0.0.0.0:3340".into(),
            key: "key".into(),
            connected: 1,
            gpus: 2,
            rate: 2.0e9,
            winners: 3,
            rejected: 0,
            rigs: vec![crate::rigs::RigLine {
                name: "rack-1".into(),
                gpus: 2,
                rate: 2.0e9,
                winners: 3,
                connected_secs: 120,
            }],
            public: false,
        });
        let rows = rendered_rows(&snapshot, 140, 40).join("\n");
        assert!(rows.contains("no GPU here · rigs mine"), "{rows}");
        assert!(rows.contains("2.00 GH/s from the rigs"), "{rows}");
        assert!(rows.contains("rack-1 · 2 GPUs"), "{rows}");
        assert!(!rows.contains("RIGS ONLY device"), "{rows}");
        assert_eq!(gpu_list(&snapshot), "none here; the rigs mine");
    }

    // #### PR #40
    #[test]
    fn connection_info_shows_each_rig_command_and_suggests_tailscale() {
        use crate::reach::Interfaces;
        let mut snapshot = test_snapshot();
        snapshot.rigs = Some(crate::rigs::RigSummary {
            listen: "0.0.0.0:3340".into(),
            key: "KEY".into(),
            public: true,
            ..Default::default()
        });
        let page = |interfaces: Interfaces| {
            let mut state = TuiState::new(&snapshot);
            state.connect = Some(crate::rigs::join_lines(
                snapshot.rigs.as_ref().unwrap(),
                snapshot.network,
                interfaces,
            ));
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
            terminal
                .draw(|frame| render(frame, &snapshot, &state))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .chunks(160)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        };
        let local = Some("192.168.0.160".parse().unwrap());
        let lan = page(Interfaces {
            local,
            tailscale: None,
        });
        assert!(lan.contains("How rigs join"), "{lan}");
        assert!(lan.contains("[1] your network"), "{lan}");
        assert!(
            lan.contains(
                "pickaxe mine --coordinator 192.168.0.160:3340 --coordinator-key KEY --address YOUR_BCH_ADDRESS"
            ),
            "{lan}"
        );
        assert!(lan.contains("in place of YOUR_BCH_ADDRESS"), "{lan}");
        assert!(lan.contains("Install Tailscale"), "{lan}");
        let tailnet = page(Interfaces {
            local,
            tailscale: Some("100.101.102.103".parse().unwrap()),
        });
        assert!(tailnet.contains("[2] Tailscale"), "{tailnet}");
        assert!(
            tailnet.contains("--coordinator 100.101.102.103:3340"),
            "{tailnet}"
        );
        assert!(!tailnet.contains("Install Tailscale"), "{tailnet}");
    }

    /// Renders the dashboard at `width` x `height` and returns its rows.
    fn rendered_rows(snapshot: &RuntimeSnapshot, width: u16, height: u16) -> Vec<String> {
        let state = TuiState::new(snapshot);
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, snapshot, &state))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    #[test]
    /// Checks that the runtime pane shows full values when maximized, wraps
    /// them under their label when narrower, and drops the least important
    /// rows when short.
    fn runtime_pane_adapts_to_window_size() {
        let mut snapshot = test_snapshot();
        // Little-endian bytes 00..1f read most-significant first as 1f..00.
        snapshot.photon_target_le = (0u8..32).map(|byte| format!("{byte:02x}")).collect();
        snapshot.baton_txid = "0123456789abcdef".repeat(4);
        snapshot.search.rate = 30e6;
        snapshot.last_error = Some("settlement preflight retry".into());
        let target = "1f1e1d1c 1b1a1918 17161514 13121110 0f0e0d0c 0b0a0908 07060504 03020100";

        let wide = rendered_rows(&snapshot, 220, 50);
        assert!(wide.iter().any(|row| row.contains(target)), "{wide:?}");
        let baton = format!("{}:0", snapshot.baton_txid);
        assert!(wide.iter().any(|row| row.contains(&baton)), "{wide:?}");

        let normal = rendered_rows(&snapshot, 120, 40);
        let first = normal
            .iter()
            .position(|row| row.contains("Target     1f1e1d1c 1b1a1918 17161514 13121110"))
            .unwrap_or_else(|| panic!("{normal:?}"));
        assert!(normal[first + 1].contains("           0f0e0d0c 0b0a0908 07060504 03020100"));
        assert!(normal.iter().any(|row| row.contains("1 win per ")));

        let short = rendered_rows(&snapshot, 120, 16);
        for kept in ["Hashrate", "Wins", "Last error"] {
            assert!(
                short.iter().any(|row| row.contains(kept)),
                "{kept}: {short:?}"
            );
        }
        for dropped in ["Clocks", "Baton", "Endpoint"] {
            assert!(
                !short.iter().any(|row| row.contains(dropped)),
                "{dropped}: {short:?}"
            );
        }
    }

    #[test]
    fn chart_history_samples_on_its_interval_and_stays_bounded() {
        let mut state = TuiState::new(&test_snapshot());
        let mut snapshot = test_snapshot();
        let start = Instant::now();
        state.record_sample(&snapshot, start);
        state.record_sample(&snapshot, start + Duration::from_secs(1));
        assert_eq!(state.history.len(), 1);
        for step in 1..=(HISTORY_CAP as u32 + 5) {
            snapshot.search.current_rate = f64::from(step);
            state.record_sample(&snapshot, start + HISTORY_SAMPLE_INTERVAL * step);
        }
        assert_eq!(state.history.len(), HISTORY_CAP);
        assert_eq!(
            state.history.back().map(|sample| sample.rate),
            Some((HISTORY_CAP + 5) as f64)
        );
    }

    #[test]
    fn chart_commands_parse() {
        let charts = |argument: &str| match parse_palette_command(argument) {
            Ok(PaletteCommand::Charts(request)) => Ok(request),
            Ok(other) => panic!("{argument}: {other:?}"),
            Err(error) => Err(error),
        };
        assert_eq!(charts("/chart"), Ok(ChartRequest::On));
        assert_eq!(charts("chart options"), Ok(ChartRequest::Options));
        assert_eq!(charts("chart on"), Ok(ChartRequest::On));
        assert_eq!(charts("graph off"), Ok(ChartRequest::Off));
        assert_eq!(
            charts("charts all"),
            Ok(ChartRequest::Show(ChartMetric::ALL.to_vec()))
        );
        assert_eq!(
            charts("chart add power, fan power"),
            Ok(ChartRequest::Add(vec![
                ChartMetric::Power,
                ChartMetric::Fan
            ]))
        );
        assert_eq!(
            charts("chart remove temp"),
            Ok(ChartRequest::Remove(vec![ChartMetric::Temperature]))
        );
        assert_eq!(
            charts("chart temp hash"),
            Ok(ChartRequest::Show(vec![
                ChartMetric::Temperature,
                ChartMetric::Hashrate
            ]))
        );
        assert!(charts("chart add").is_err());
        assert!(charts("chart bogus").is_err());
    }

    #[test]
    fn g_shows_hash_and_temperature_and_options_change_the_choice() {
        let mut state = TuiState::new(&test_snapshot());
        assert!(state.charts.is_empty(), "charts are off by default");
        state.apply_chart_request(ChartRequest::Toggle);
        assert_eq!(state.charts, DEFAULT_CHARTS.to_vec());

        state.apply_chart_request(ChartRequest::Add(vec![ChartMetric::Power]));
        assert_eq!(
            state.charts,
            vec![
                ChartMetric::Hashrate,
                ChartMetric::Temperature,
                ChartMetric::Power
            ]
        );
        state.apply_chart_request(ChartRequest::Remove(vec![ChartMetric::Temperature]));
        assert_eq!(
            state.charts,
            vec![ChartMetric::Hashrate, ChartMetric::Power]
        );

        state.apply_chart_request(ChartRequest::Toggle);
        assert!(state.charts.is_empty());
        state.apply_chart_request(ChartRequest::Toggle);
        assert_eq!(
            state.charts,
            vec![ChartMetric::Hashrate, ChartMetric::Power],
            "G brings back the last choice"
        );

        state.apply_chart_request(ChartRequest::Show(vec![
            ChartMetric::Temperature,
            ChartMetric::Hashrate,
        ]));
        assert_eq!(state.charts, DEFAULT_CHARTS.to_vec(), "fixed order");

        // The options menu: move to power, tick it, untick hash rate.
        state.apply_chart_request(ChartRequest::Options);
        assert_eq!(state.chart_options, Some(0));
        assert!(!state.chart_options_key(KeyCode::Char(' ')));
        assert_eq!(state.charts, vec![ChartMetric::Temperature]);
        state.chart_options_key(KeyCode::Down);
        state.chart_options_key(KeyCode::Down);
        state.chart_options_key(KeyCode::Char(' '));
        assert_eq!(
            state.charts,
            vec![ChartMetric::Temperature, ChartMetric::Power]
        );
        state.chart_options_key(KeyCode::Char('n'));
        assert!(state.charts.is_empty());
        state.chart_options_key(KeyCode::Char('a'));
        assert_eq!(state.charts, ChartMetric::ALL.to_vec());
        state.chart_options_key(KeyCode::Enter);
        assert_eq!(state.chart_options, None);
        assert!(
            !state.chart_options_key(KeyCode::Char('q')),
            "closed menu ignores keys"
        );
    }

    #[test]
    fn chart_options_menu_lists_every_chart_with_its_state() {
        let mut snapshot = test_snapshot();
        snapshot.gpu_telemetry.temperature_c = Some(70.0);
        let mut state = TuiState::new(&snapshot);
        state.record_sample(&snapshot, Instant::now());
        state.apply_chart_request(ChartRequest::Toggle);
        state.apply_chart_request(ChartRequest::Options);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| render(frame, &snapshot, &state))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains(" Chart options "));
        assert!(rendered.contains("[x] Hashrate"));
        assert!(rendered.contains("[x] GPU temperature"));
        assert!(rendered.contains("[ ] GPU power  (not reported)"));
        assert!(rendered.contains("[ ] Core clock"));
    }

    #[test]
    fn charts_are_off_by_default_and_panes_never_pad_with_empty_rows() {
        let snapshot = test_snapshot();
        let rows = rendered_rows(&snapshot, 190, 50);
        assert!(
            !rows.iter().any(|row| row.contains("last 60 min")),
            "{rows:?}"
        );
        let runtime_bottom = rows
            .iter()
            .position(|row| row.contains("Last error"))
            .unwrap();
        assert!(rows[runtime_bottom + 1].starts_with("└"), "{rows:?}");
        assert!(
            rows[runtime_bottom + 2].contains("┌"),
            "footer follows the panes"
        );
    }

    /// Renders with the given charts selected.
    fn rendered_rows_with_charts(
        snapshot: &RuntimeSnapshot,
        charts: &[ChartMetric],
        width: u16,
        height: u16,
    ) -> Vec<String> {
        let mut state = TuiState::new(snapshot);
        state.record_sample(snapshot, Instant::now());
        state.apply_chart_request(ChartRequest::Show(charts.to_vec()));
        let mut terminal =
            Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, snapshot, &state))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    #[test]
    fn charts_stack_top_to_bottom_in_the_chosen_order() {
        let mut snapshot = test_snapshot();
        snapshot.search.current_rate = 30e6;
        snapshot.gpu_telemetry.temperature_c = Some(71.0);
        snapshot.gpu_telemetry.power_watts = Some(95.0);
        snapshot.gpu_telemetry.gpu_utilization_percent = Some(99.0);
        snapshot.gpu_telemetry.graphics_clock_mhz = Some(2550.0);

        let rows = rendered_rows_with_charts(&snapshot, &ChartMetric::ALL, 190, 50);
        let row_of = |needle: &str| {
            rows.iter()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("{needle}: {rows:?}"))
        };
        let order = [
            row_of("Hashrate · 30.00 MH/s"),
            row_of("GPU temperature · 71 C"),
            row_of("GPU power · 95 W"),
            row_of("fan speed is not reported"),
            row_of("GPU utilization · 99%"),
            row_of("Core clock · 2550 MHz"),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{rows:?}");
        assert_eq!(
            rows.iter().filter(|row| row.contains("now")).count(),
            1,
            "only the bottom chart has the time axis: {rows:?}"
        );
        assert!(
            rows.iter().any(|row| row.contains("Hashrate   ")),
            "runtime stays"
        );

        let chosen = rendered_rows_with_charts(
            &snapshot,
            &[ChartMetric::Temperature, ChartMetric::Hashrate],
            120,
            30,
        );
        let temp = chosen
            .iter()
            .position(|row| row.contains("GPU temperature · "))
            .unwrap();
        let hash = chosen
            .iter()
            .position(|row| row.contains("Hashrate · "))
            .unwrap();
        assert!(hash < temp, "charts keep the fixed order: {chosen:?}");
        assert!(!chosen.iter().any(|row| row.contains("GPU power")));
    }

    #[test]
    fn value_wrapping_never_drops_text() {
        assert_eq!(wrap_value("a · b · c", 20), vec!["a · b · c"]);
        assert_eq!(
            wrap_value("alpha · beta · gamma", 12),
            vec!["alpha · beta", "gamma"]
        );
        let txid = "ab".repeat(33);
        let rows = wrap_value(&txid, 40);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.concat(), txid);
        assert_eq!(rows[0].len(), rows[1].len());
        assert_eq!(group_digits(45_460_291_584), "45,460,291,584");
        assert_eq!(group_digits(999), "999");
        assert_eq!(format_duration(59_018.0), "16h 23m 38s");
        assert_eq!(format_duration(4_195.0), "1h 09m 55s");
        assert_eq!(format_duration(90_061.0), "1d 01h 01m");
        assert_eq!(format_duration(42.0), "42s");
        assert_eq!(format_si_count(126_400_000_000.0), "126.4 G");
        assert_eq!(nice_ceiling(34.5e6), 40e6);
        assert_eq!(nice_ceiling(31.42e6 * 1.1), 40e6);
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(2.0e3), 2.0e3);
    }

    #[test]
    /// Checks that palette rejects invalid intensity without touching runtime.
    fn palette_rejects_invalid_intensity_without_touching_runtime() {
        assert!(parse_palette_command("intensity 9").is_err());
        assert!(parse_palette_command("intensity 101").is_err());
        assert!(parse_palette_command("intensity nope").is_err());
    }

    #[test]
    /// Checks that endpoint redaction removes embedded credentials.
    fn endpoint_redaction_removes_embedded_credentials() {
        assert_eq!(
            redact_endpoint("wss://user:secret@example.test:50004"),
            "wss://example.test:50004"
        );
        assert_eq!(
            redact_endpoint("wss://example.test:50004"),
            "wss://example.test:50004"
        );
    }

    #[test]
    /// Checks that event history is bounded.
    fn event_history_is_bounded() {
        let snapshot = RuntimeSnapshot {
            state: SupervisorState::Mining,
            gpu_backend: "cuda".into(),
            gpu_device: 2,
            gpus: Vec::new(),
            generation_id: 1,
            network: MiningNetwork::Mainnet,
            fee_scheme: crate::config::MiningToken::Photon
                .fee_policy(MiningNetwork::Mainnet)
                .scheme,
            payout_address: "bitcoincash:qexample".into(),
            endpoint: "wss://example.test".into(),
            height: 1,
            baton_txid: "00".repeat(32),
            baton_vout: 0,
            photon_target_le: String::new(),
            state_checks: 0,
            transient_refresh_failures: 0,
            transport_failures: 0,
            consecutive_refresh_failures: 0,
            source_degraded: false,
            job_changes: 0,
            reconnects: 0,
            endpoint_rotations: 0,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: Default::default(),
            gpu_telemetry: Default::default(),
            rigs: None,
            token_donation: crate::donation::TokenDonation::from_bps(400),
            donation_minimum: crate::donation::TokenDonation::from_bps(400),
        };
        let mut state = TuiState::new(&snapshot);
        for i in 0..(EVENT_HISTORY_CAP + 20) {
            state.push_event(format!("event {i}"));
        }
        assert_eq!(state.events.len(), EVENT_HISTORY_CAP);
        assert_eq!(state.events.back().unwrap(), "event 115");
    }
}
