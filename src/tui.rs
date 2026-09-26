use crate::{
    backend::{BackendKind, GpuDevice},
    config::RuntimeConfig,
    runtime::{RuntimeEvent, RuntimeSnapshot, RuntimeSupervisor, SupervisorState},
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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
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
/// Height the chart area gets when the window has no spare rows.
const CHART_MIN_ROWS: u16 = 8;
/// Narrowest chart placed beside another instead of stacked.
const CHART_MIN_WIDTH: u16 = 50;
const COMMAND_HISTORY_CAP: usize = 32;
const BENCHMARK_TERMINAL_WIDTH: u16 = 120;
const BENCHMARK_TERMINAL_HEIGHT: u16 = 40;

type PickaxeTerminal = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug, Clone)]
pub(crate) struct SetupResult {
    pub config: RuntimeConfig,
    pub backend: BackendKind,
    pub device: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupStep {
    Gpu,
    Payout,
    Intensity,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupAction {
    Continue,
    Complete,
    Cancel,
}

struct SetupFlow {
    step: SetupStep,
    devices: Vec<GpuDevice>,
    selected: usize,
    config: RuntimeConfig,
    payout_input: String,
    status_line: String,
}

impl SetupFlow {
    /// Creates a SetupFlow for the terminal interface.
    fn new(
        config: RuntimeConfig,
        devices: Vec<GpuDevice>,
        default_device: &GpuDevice,
    ) -> Result<Self, String> {
        if devices.is_empty() {
            return Err("no validated production GPU device found".into());
        }
        let selected = devices
            .iter()
            .position(|device| {
                device.backend == default_device.backend && device.index == default_device.index
            })
            .unwrap_or(0);
        let payout_input = config.payout_address.clone();
        Ok(Self {
            step: SetupStep::Gpu,
            devices,
            selected,
            config,
            payout_input,
            status_line: String::new(),
        })
    }

    /// Returns the GPU device selected in the setup wizard.
    fn selected_device(&self) -> &GpuDevice {
        &self.devices[self.selected]
    }

    /// Handles keyboard input for the active terminal view.
    fn handle_key(&mut self, key: KeyEvent) -> SetupAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return SetupAction::Cancel;
        }

        match self.step {
            SetupStep::Gpu => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => SetupAction::Cancel,
                KeyCode::Up => {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or(self.devices.len().saturating_sub(1));
                    SetupAction::Continue
                }
                KeyCode::Down => {
                    self.selected = (self.selected + 1) % self.devices.len();
                    SetupAction::Continue
                }
                KeyCode::Enter => {
                    self.step = SetupStep::Payout;
                    self.status_line.clear();
                    SetupAction::Continue
                }
                _ => SetupAction::Continue,
            },
            SetupStep::Payout => match key.code {
                KeyCode::Esc => {
                    self.step = SetupStep::Gpu;
                    self.status_line.clear();
                    SetupAction::Continue
                }
                KeyCode::Enter => match self.config.set_payout(self.payout_input.clone()) {
                    Ok(()) => {
                        self.step = SetupStep::Intensity;
                        self.status_line.clear();
                        SetupAction::Continue
                    }
                    Err(error) => {
                        self.status_line = error;
                        SetupAction::Continue
                    }
                },
                KeyCode::Backspace => {
                    self.payout_input.pop();
                    self.status_line.clear();
                    SetupAction::Continue
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.payout_input.push(ch);
                    self.status_line.clear();
                    SetupAction::Continue
                }
                _ => SetupAction::Continue,
            },
            SetupStep::Intensity => match key.code {
                KeyCode::Esc => {
                    self.step = SetupStep::Payout;
                    self.status_line.clear();
                    SetupAction::Continue
                }
                KeyCode::Enter => {
                    self.step = SetupStep::Review;
                    self.status_line.clear();
                    SetupAction::Continue
                }
                KeyCode::Up | KeyCode::Right | KeyCode::Char('+') | KeyCode::Char(']') => {
                    let next = self.config.intensity.saturating_add(10).min(100);
                    let _ = self.config.set_intensity(next);
                    SetupAction::Continue
                }
                KeyCode::Down | KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('[') => {
                    let next = self.config.intensity.saturating_sub(10).max(10);
                    let _ = self.config.set_intensity(next);
                    SetupAction::Continue
                }
                _ => SetupAction::Continue,
            },
            SetupStep::Review => match key.code {
                KeyCode::Esc => {
                    self.step = SetupStep::Intensity;
                    self.status_line.clear();
                    SetupAction::Continue
                }
                KeyCode::Enter => SetupAction::Complete,
                _ => SetupAction::Continue,
            },
        }
    }
}

struct TerminalSession {
    terminal: PickaxeTerminal,
}

impl TerminalSession {
    /// Enters raw terminal mode and switches to the alternate screen.
    fn enter() -> Result<Self, String> {
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
        let fitted = nice_ceiling(largest * 1.1);
        match self {
            Self::Temperature | Self::Fan | Self::Utilization => fitted.max(100.0),
            _ => fitted,
        }
    }
}

/// One history point for the charts.
#[derive(Debug, Clone, Default)]
struct HistorySample {
    rate: f64,
    gpu: crate::telemetry::GpuTelemetry,
}

/// A `/chart` request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChartRequest {
    Toggle,
    Off,
    Show(Vec<ChartMetric>),
}

struct TuiState {
    command_mode: bool,
    command_input: String,
    command_history: VecDeque<String>,
    command_history_cursor: Option<usize>,
    command_draft: String,
    show_help: bool,
    settings_mode: bool,
    logs_mode: bool,
    status_line: String,
    events: VecDeque<String>,
    devices: Vec<GpuDevice>,
    history: VecDeque<HistorySample>,
    last_history_sample: Option<Instant>,
    /// Charts on screen; empty means charts are hidden (the default).
    charts: Vec<ChartMetric>,
    /// Charts that `G` or a bare `/chart` brings back.
    last_charts: Vec<ChartMetric>,
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
            logs_mode: false,
            status_line: "Donation: 2%".into(),
            events,
            devices: Vec::new(),
            history: VecDeque::with_capacity(HISTORY_CAP),
            last_history_sample: None,
            charts: Vec::new(),
            last_charts: vec![ChartMetric::Hashrate],
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
        let rate = snapshot.search.current_rate;
        self.history.push_back(HistorySample {
            rate: if rate.is_finite() { rate.max(0.0) } else { 0.0 },
            gpu: snapshot.gpu_telemetry.clone(),
        });
        self.last_history_sample = Some(now);
    }

    /// Applies a `/chart` request or the `G` toggle.
    fn apply_chart_request(&mut self, request: ChartRequest) {
        match request {
            ChartRequest::Toggle if self.charts.is_empty() => {
                self.charts = self.last_charts.clone();
            }
            ChartRequest::Toggle | ChartRequest::Off => {
                if !self.charts.is_empty() {
                    self.last_charts = std::mem::take(&mut self.charts);
                }
            }
            ChartRequest::Show(metrics) => {
                self.charts = metrics.clone();
                self.last_charts = metrics;
            }
        }
        self.status_line = if self.charts.is_empty() {
            "Charts hidden. Press G or type /chart to show them.".into()
        } else {
            format!(
                "Charts: {}. Press G to hide.",
                self.charts
                    .iter()
                    .map(|metric| metric.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
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
        self.logs_mode = false;
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
pub(crate) fn run_setup(
    config: RuntimeConfig,
    devices: Vec<GpuDevice>,
    default_device: &GpuDevice,
) -> Result<Option<SetupResult>, String> {
    run_setup_terminal(SetupFlow::new(config, devices, default_device)?)
}

/// Draws and processes setup screens in the terminal.
fn run_setup_terminal(mut state: SetupFlow) -> Result<Option<SetupResult>, String> {
    let mut terminal = TerminalSession::enter()?;
    loop {
        terminal
            .terminal
            .draw(|frame| render_setup(frame, &state))
            .map_err(|error| format!("draw mining setup: {error}"))?;

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
                let selected = state.selected_device();
                return Ok(Some(SetupResult {
                    config: state.config.clone(),
                    backend: selected.backend,
                    device: selected.index,
                }));
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
fn expected_winner_seconds(target_le_hex: &str, rate: f64) -> Option<f64> {
    if rate <= 0.0 {
        return None;
    }
    win_probability(target_le_hex).map(|probability| 1.0 / (probability * rate))
}

/// Chance that one candidate wins against a little-endian hex target. The
/// covenant ignores digest bit 255, so P(win) = target / 2^255.
fn win_probability(target_le_hex: &str) -> Option<f64> {
    let bytes = hex::decode(target_le_hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let probability = bytes
        .iter()
        .rev()
        .fold(0.0_f64, |value, byte| value * 256.0 + f64::from(*byte))
        / 2f64.powi(255);
    (probability > 0.0).then_some(probability)
}

/// Formats the periodic status line for the TUI observation log.
fn tui_status_line(snapshot: &RuntimeSnapshot) -> String {
    let expected = expected_winner_seconds(&snapshot.photon_target_le, snapshot.search.rate)
        .map(|seconds| format!("{seconds:.0}"))
        .unwrap_or_else(|| "n/a".into());
    format!(
        "status state={:?} waiting_for_job={} key_rotations={} intensity={} rate={:.0} avg_rate={:.0} peak_rate={:.0} expected_winner_s={} reconnects={} rotations={} job_changes={} checks={} batches={} candidates={} verified_winners={} stale_winners={} rejected_winners={} pending_winners={} height={} target_le={} endpoint={} last_error={}",
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
        generation_id: 1,
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

    if state.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => state.show_help = false,
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
                "config: backend={}:{} intensity={} payout={} endpoint={} donation=2%-fixed generation={}",
                snapshot.gpu_backend,
                snapshot.gpu_device,
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
                    "device: {}:{} (startup device catalog unavailable)",
                    snapshot.gpu_backend, snapshot.gpu_device
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
            let backend = format!(
                "backend: {} device {}",
                snapshot.gpu_backend.to_ascii_uppercase(),
                snapshot.gpu_device
            );
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

/// Parses `/chart` arguments: none toggles, `off` hides, `all` shows every
/// chart, otherwise a list of chart names.
fn parse_chart_request(argument: &str) -> Result<PaletteCommand, String> {
    let names = argument
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    let request = match names.as_slice() {
        [] => ChartRequest::Toggle,
        [one] if ["off", "hide", "none"].contains(&one.to_ascii_lowercase().as_str()) => {
            ChartRequest::Off
        }
        [one] if one.eq_ignore_ascii_case("all") => ChartRequest::Show(ChartMetric::ALL.to_vec()),
        names => {
            let mut metrics = Vec::new();
            for name in names {
                let metric = ChartMetric::parse(name).ok_or_else(|| {
                    format!(
                        "unknown chart {name}. Choose hash temp power fan util clock, all or off."
                    )
                })?;
                if !metrics.contains(&metric) {
                    metrics.push(metric);
                }
            }
            ChartRequest::Show(metrics)
        }
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

/// Renders the current setup wizard step.
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
    let step = match state.step {
        SetupStep::Gpu => "1/4 GPU",
        SetupStep::Payout => "2/4 Payout",
        SetupStep::Intensity => "3/4 Intensity",
        SetupStep::Review => "4/4 Review",
    };
    frame.render_widget(
        Paragraph::new(format!("PICKAXE MINER   Setup   {step}"))
            .block(Block::default().borders(Borders::ALL)),
        rows[0],
    );
    match state.step {
        SetupStep::Gpu => render_setup_gpu(frame, rows[1], state),
        SetupStep::Payout => render_setup_payout(frame, rows[1], state),
        SetupStep::Intensity => render_setup_intensity(frame, rows[1], state),
        SetupStep::Review => render_setup_review(frame, rows[1], state),
    }
    let keys = match state.step {
        SetupStep::Gpu => "[Up/Down] choose   [Enter] accept default   [Esc] cancel",
        SetupStep::Payout => "[Type] payout address   [Enter] continue   [Esc] Back",
        SetupStep::Intensity => "[+/- or arrows] adjust   [Enter] continue   [Esc] Back",
        SetupStep::Review => "[Enter] start mining   [Esc] Back",
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

/// Renders GPU backend and device selection.
fn render_setup_gpu(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let items = state.devices.iter().enumerate().map(|(index, device)| {
        let marker = if index == state.selected { "> " } else { "  " };
        ListItem::new(format!(
            "{marker}{}:{}  {}  {}",
            device.backend.as_str().to_ascii_uppercase(),
            device.index,
            device.vendor,
            device.name
        ))
    });
    frame.render_widget(
        List::new(items).block(Block::default().title(" GPUs ").borders(Borders::ALL)),
        area,
    );
}

/// Renders miner payout address configuration.
fn render_setup_payout(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    frame.render_widget(
        Paragraph::new(state.payout_input.as_str())
            .block(Block::default().title(" Address ").borders(Borders::ALL)),
        area,
    );
}

/// Renders the GPU intensity configuration step.
fn render_setup_intensity(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let ratio = f64::from(state.config.intensity) / 100.0;
    frame.render_widget(
        Gauge::default()
            .block(
                Block::default()
                    .title(" GPU intensity ")
                    .borders(Borders::ALL),
            )
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio)
            .label(format!("{}%", state.config.intensity)),
        area,
    );
}

/// Renders a review of the configured mining settings.
fn render_setup_review(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    let device = state.selected_device();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                "GPU: {}:{}  {}",
                device.backend.as_str().to_ascii_uppercase(),
                device.index,
                device.name
            )),
            Line::from(format!("Address: {}", state.config.payout_address)),
            Line::from(format!("Intensity: {}%", state.config.intensity)),
            Line::from("Donation: 2%"),
            Line::from(""),
            Line::from("Press Enter to start mining."),
        ])
        .block(Block::default().title(" Review ").borders(Borders::ALL))
        .wrap(Wrap { trim: false }),
        area,
    );
}

/// Renders the active mining dashboard.
fn render(frame: &mut Frame<'_>, snapshot: &RuntimeSnapshot, state: &TuiState) {
    let area = frame.area();
    // The runtime and events panes are only as tall as the runtime rows
    // need, so a tall window never shows empty boxes. Charts are off until
    // asked for; then they get the spare rows, or at least CHART_MIN_ROWS
    // with the least important runtime rows giving way.
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
            .max(CHART_MIN_ROWS.min(available / 2));
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
    if state.logs_mode {
        render_logs(frame, area, state);
    }
}

/// Renders the dashboard header and live connection state.
fn render_header(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let mut state_text = format!("{:?}", snapshot.state).to_ascii_uppercase();
    if snapshot.search.waiting_for_job {
        state_text.push_str(" (all nonces tried, waiting for next job)");
    }
    let line = Line::from(vec![
        Span::styled(" PICKAXE ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "PHOTON   {state_text}   {} device {}   Donation: 2%",
            snapshot.gpu_backend.to_ascii_uppercase(),
            snapshot.gpu_device
        )),
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
        win_probability(&snapshot.photon_target_le),
        expected_winner_seconds(&snapshot.photon_target_le, search.rate),
    ) {
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

    vec![
        RuntimeField::new(
            "Hashrate",
            wrap(format!(
                "{} · avg {} · peak {}",
                hash_rate(search.current_rate),
                hash_rate(search.rate),
                hash_rate(search.peak_rate)
            )),
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
            "GPU",
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
    ]
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

/// Lays out the selected charts side by side when each can be at least
/// CHART_MIN_WIDTH wide, otherwise in stacked rows.
fn render_charts(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let count = state.charts.len();
    if count == 0 {
        return;
    }
    let mut columns = usize::from(area.width / CHART_MIN_WIDTH).clamp(1, count);
    let stacked_rows = u16::try_from(count.div_ceil(columns)).unwrap_or(u16::MAX);
    if area.height / stacked_rows < 6 {
        columns = count;
    }
    let row_count = count.div_ceil(columns);
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Ratio(1, row_count as u32); row_count])
        .split(area);
    for (metrics, row_area) in state.charts.chunks(columns).zip(row_areas.iter()) {
        let cells = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![
                Constraint::Ratio(1, metrics.len() as u32);
                metrics.len()
            ])
            .split(*row_area);
        for (metric, cell) in metrics.iter().zip(cells.iter()) {
            render_metric_chart(frame, *cell, state, *metric);
        }
    }
}

/// Charts the last hour of one metric; a hash-rate dip to zero shows idle
/// GPU time.
fn render_metric_chart(frame: &mut Frame<'_>, area: Rect, state: &TuiState, metric: ChartMetric) {
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
    let chart = Chart::new(vec![Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Cyan))
        .data(&points)])
    .block(block)
    .x_axis(
        Axis::default()
            .style(Style::default().fg(Color::DarkGray))
            .bounds([-window, 0.0])
            .labels([
                Span::raw(format!("-{}", minutes(window))),
                Span::raw(format!("-{}", minutes(window / 2.0))),
                Span::raw("now"),
            ]),
    )
    .y_axis(
        Axis::default()
            .style(Style::default().fg(Color::DarkGray))
            .bounds([0.0, top])
            .labels([
                Span::raw(metric.format(0.0)),
                Span::raw(metric.format(top / 2.0)),
                Span::raw(metric.format(top)),
            ]),
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
                "[/] command  [P] pause  [+/-] intensity  [R] reconnect  [S] settings  [G] charts  [?] help  [Q] quit",
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
    let popup = centered_rect(82, 58, area);
    frame.render_widget(Clear, popup);
    let settings = Paragraph::new(vec![
        Line::from("Mining settings"),
        Line::from(""),
        Line::from(format!(
            "Address: {}",
            shorten(&snapshot.payout_address, 66)
        )),
        Line::from(format!("Intensity: {}%", snapshot.search.intensity)),
        Line::from(format!(
            "GPU/device: {}:{}",
            snapshot.gpu_backend.to_ascii_uppercase(),
            snapshot.gpu_device
        )),
        Line::from(format!(
            "Connection: {}",
            shorten(&redact_endpoint(&snapshot.endpoint), 60)
        )),
        Line::from(""),
        Line::from("[A] edit address   [+/-] intensity   [R] reconnect"),
        Line::from("GPU/device can be selected in startup setup or with --device."),
        Line::from("[S/Esc] close settings"),
    ])
    .block(Block::default().title(" Settings ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(settings, popup);
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
        Line::from("G            show or hide charts"),
        Line::from("/ (: or C)   command bar"),
        Line::from("?            close/open help"),
        Line::from("Q / Ctrl+C   graceful quit"),
        Line::from(""),
        Line::from("Commands:"),
        Line::from("  /intensity <10-100> | /pause | /resume | /reconnect"),
        Line::from("  /address <cashaddr> | /endpoint <wss://...> | /endpoint auto"),
        Line::from("  /status | /config | /devices | /backend | /logs"),
        Line::from("  /chart [hash temp power fan util clock | all | off]"),
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
            },
            GpuDevice {
                index: 0,
                name: "Secondary HIP".into(),
                vendor: "AMD".into(),
                vram_bytes: None,
                backend: BackendKind::Hip,
                detail: String::new(),
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
            generation_id: 1,
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
        }
    }

    #[test]
    /// Checks that setup default selection.
    fn setup_default_selection() {
        let devices = test_devices();
        let default_device = devices[0].clone();
        let mut setup = SetupFlow::new(RuntimeConfig::default(), devices, &default_device).unwrap();
        assert_eq!(setup.selected, 0);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Continue);
        assert_eq!(setup.step, SetupStep::Payout);
        setup.handle_key(key(KeyCode::Esc));
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.selected, 1);
    }

    #[test]
    /// Checks that setup validation stays on input.
    fn setup_validation_stays_on_input() {
        let devices = test_devices();
        let default_device = devices[0].clone();
        let mut setup = SetupFlow::new(RuntimeConfig::default(), devices, &default_device).unwrap();
        setup.handle_key(key(KeyCode::Enter));
        setup.payout_input = "x".into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Payout);
    }

    #[test]
    /// Checks that setup valid input advances.
    fn setup_valid_input_advances() {
        let devices = test_devices();
        let default_device = devices[0].clone();
        let mut setup = SetupFlow::new(RuntimeConfig::default(), devices, &default_device).unwrap();
        setup.handle_key(key(KeyCode::Enter));
        setup.payout_input = crate::config::DONATION_ADDRESS.into();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Intensity);
        assert_eq!(setup.config.payout_address, crate::config::DONATION_ADDRESS);
    }

    #[test]
    /// Checks that setup intensity stays in range.
    fn setup_intensity_stays_in_range() {
        let devices = test_devices();
        let default_device = devices[0].clone();
        let mut config = RuntimeConfig::default();
        config
            .set_payout(crate::config::DONATION_ADDRESS.into())
            .unwrap();
        let mut setup = SetupFlow::new(config, devices, &default_device).unwrap();
        setup.handle_key(key(KeyCode::Enter));
        setup.handle_key(key(KeyCode::Enter));
        for _ in 0..20 {
            setup.handle_key(key(KeyCode::Down));
        }
        assert_eq!(setup.config.intensity, 10);
        for _ in 0..20 {
            setup.handle_key(key(KeyCode::Up));
        }
        assert_eq!(setup.config.intensity, 100);
    }

    #[test]
    /// Checks that setup review displays the selected GPU and payout values.
    fn setup_review_values() {
        let devices = test_devices();
        let default_device = devices[0].clone();
        let mut setup = SetupFlow::new(RuntimeConfig::default(), devices, &default_device).unwrap();
        setup.handle_key(key(KeyCode::Down));
        assert_eq!(setup.selected, 1);
        setup.handle_key(key(KeyCode::Enter));
        setup.payout_input = crate::config::DONATION_ADDRESS.into();
        setup.handle_key(key(KeyCode::Enter));
        setup.config.set_intensity(70).unwrap();
        setup.handle_key(key(KeyCode::Enter));
        assert_eq!(setup.step, SetupStep::Review);
        assert_eq!(setup.config.intensity, 70);
        assert_eq!(setup.handle_key(key(KeyCode::Enter)), SetupAction::Complete);
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
        // Target 2^224 (LE byte 28 = 1): with digest bit 255 ignored, one
        // winner per 2^31 candidates.
        let mut target = [0u8; 32];
        target[28] = 1;
        let target = hex::encode(target);
        let seconds = expected_winner_seconds(&target, 2f64.powi(31) / 100.0).unwrap();
        assert!((seconds - 100.0).abs() < 1e-6, "{seconds}");
        assert!(expected_winner_seconds(&target, 0.0).is_none());
        assert!(expected_winner_seconds("", 1.0).is_none());
        assert!(expected_winner_seconds(&"00".repeat(32), 1.0).is_none());
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
    fn chart_command_selects_toggles_and_hides_charts() {
        assert_eq!(
            parse_palette_command("/chart temp, fan hash temp").unwrap(),
            PaletteCommand::Charts(ChartRequest::Show(vec![
                ChartMetric::Temperature,
                ChartMetric::Fan,
                ChartMetric::Hashrate,
            ]))
        );
        assert_eq!(
            parse_palette_command("chart").unwrap(),
            PaletteCommand::Charts(ChartRequest::Toggle)
        );
        assert_eq!(
            parse_palette_command("graph off").unwrap(),
            PaletteCommand::Charts(ChartRequest::Off)
        );
        assert_eq!(
            parse_palette_command("charts all").unwrap(),
            PaletteCommand::Charts(ChartRequest::Show(ChartMetric::ALL.to_vec()))
        );
        assert!(parse_palette_command("chart bogus").is_err());

        let mut state = TuiState::new(&test_snapshot());
        assert!(state.charts.is_empty(), "charts are off by default");
        state.apply_chart_request(ChartRequest::Toggle);
        assert_eq!(state.charts, vec![ChartMetric::Hashrate]);
        state.apply_chart_request(ChartRequest::Show(vec![ChartMetric::Power]));
        state.apply_chart_request(ChartRequest::Off);
        assert!(state.charts.is_empty());
        state.apply_chart_request(ChartRequest::Toggle);
        assert_eq!(
            state.charts,
            vec![ChartMetric::Power],
            "G brings back the last choice"
        );
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
    fn selected_charts_sit_side_by_side_when_wide_and_stack_when_narrow() {
        let mut snapshot = test_snapshot();
        snapshot.search.current_rate = 30e6;
        snapshot.gpu_telemetry.temperature_c = Some(71.0);
        let charts = [
            ChartMetric::Hashrate,
            ChartMetric::Temperature,
            ChartMetric::Fan,
        ];

        let wide = rendered_rows_with_charts(&snapshot, &charts, 190, 50);
        assert!(
            wide.iter().any(|row| row.contains("Hashrate · 30.00 MH/s")
                && row.contains("GPU temperature · 71 C")
                && row.contains("Fan speed")),
            "{wide:?}"
        );
        assert!(wide
            .iter()
            .any(|row| row.contains("fan speed is not reported")));

        let narrow = rendered_rows_with_charts(&snapshot, &charts[..2], 90, 60);
        let hash = narrow
            .iter()
            .position(|row| row.contains("Hashrate · "))
            .unwrap();
        let temp = narrow
            .iter()
            .position(|row| row.contains("GPU temperature · "))
            .unwrap();
        assert!(temp > hash, "stacked: {narrow:?}");

        let small = rendered_rows_with_charts(&snapshot, &charts[..1], 120, 30);
        assert!(
            small.iter().any(|row| row.contains("Hashrate · ")),
            "{small:?}"
        );
        assert!(
            small.iter().any(|row| row.contains("Hashrate   ")),
            "runtime stays"
        );
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
            generation_id: 1,
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
        };
        let mut state = TuiState::new(&snapshot);
        for i in 0..(EVENT_HISTORY_CAP + 20) {
            state.push_event(format!("event {i}"));
        }
        assert_eq!(state.events.len(), EVENT_HISTORY_CAP);
        assert_eq!(state.events.back().unwrap(), "event 115");
    }
}
