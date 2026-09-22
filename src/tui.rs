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
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap},
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
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const EVENT_HISTORY_CAP: usize = 96;
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

    fn selected_device(&self) -> &GpuDevice {
        &self.devices[self.selected]
    }

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
    Help,
    Quit,
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
}

impl TuiState {
    fn new(_snapshot: &RuntimeSnapshot) -> Self {
        let mut events = VecDeque::with_capacity(EVENT_HISTORY_CAP);
        events.push_back("Runtime started; authoritative PHOTON state is supervised.".into());
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
        }
    }

    fn push_event(&mut self, message: String) {
        if self.events.len() == EVENT_HISTORY_CAP {
            self.events.pop_front();
        }
        self.events.push_back(message);
    }

    fn set_devices(&mut self, devices: Vec<GpuDevice>) {
        self.devices = devices;
    }

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

    fn cancel_command(&mut self) {
        self.command_mode = false;
        self.command_input.clear();
        self.command_history_cursor = None;
        self.command_draft.clear();
    }

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

pub(crate) fn run_setup(
    config: RuntimeConfig,
    devices: Vec<GpuDevice>,
    default_device: &GpuDevice,
) -> Result<Option<SetupResult>, String> {
    run_setup_terminal(SetupFlow::new(config, devices, default_device)?)
}

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

    while !quit {
        let snapshot = supervisor.snapshot();

        for event in supervisor.drain_events() {
            state.push_event(format_event(event));
        }

        if last_draw.elapsed() >= DRAW_INTERVAL {
            terminal
                .terminal
                .draw(|frame| render(frame, &snapshot, &state))
                .map_err(|error| format!("draw terminal UI: {error}"))?;
            last_draw = Instant::now();
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

pub(crate) fn benchmark_draw_interval() -> Duration {
    DRAW_INTERVAL
}

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
                "generation={} height={} rate={:.0}/s pending={}",
                snapshot.generation_id,
                snapshot.height,
                snapshot.search.rate,
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
        PaletteCommand::Help => state.show_help = true,
        PaletteCommand::Quit => return Ok(true),
    }
    Ok(false)
}

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
        "help" | "?" => Ok(PaletteCommand::Help),
        "quit" | "exit" => Ok(PaletteCommand::Quit),
        _ => Err(format!("unknown command {command}. Try help.")),
    }
}

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

fn render_setup_payout(frame: &mut Frame<'_>, area: Rect, state: &SetupFlow) {
    frame.render_widget(
        Paragraph::new(state.payout_input.as_str())
            .block(Block::default().title(" Address ").borders(Borders::ALL)),
        area,
    );
}

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

fn render(frame: &mut Frame<'_>, snapshot: &RuntimeSnapshot, state: &TuiState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);

    render_header(frame, rows[0], snapshot);
    render_intensity(frame, rows[1], snapshot);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(rows[2]);
    render_stats(frame, body[0], snapshot);
    render_events(frame, body[1], state);
    render_footer(frame, rows[3], state);

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

fn render_header(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let state_text = format!("{:?}", snapshot.state).to_ascii_uppercase();
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

fn render_intensity(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let ratio = f64::from(snapshot.search.intensity) / 100.0;
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

fn render_stats(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot) {
    let endpoint = redact_endpoint(&snapshot.endpoint);
    let last_error = snapshot.last_error.as_deref().unwrap_or("none");
    let telemetry = &snapshot.gpu_telemetry;
    let efficiency = telemetry.candidates_per_watt(snapshot.search.current_rate);
    let lines = vec![
        Line::from(format!(
            "rate: current {:>10.0}/s   average {:>10.0}/s   peak {:>10.0}/s",
            snapshot.search.current_rate, snapshot.search.rate, snapshot.search.peak_rate
        )),
        Line::from(format!(
            "candidates: {}   batches: {}   uptime: {}s",
            snapshot.search.candidates, snapshot.search.batches, snapshot.search.elapsed_secs
        )),
        Line::from(format!(
            "generation: {}   height: {}   state checks: {}   refresh failures: {}   consecutive: {}",
            snapshot.generation_id,
            snapshot.height,
            snapshot.state_checks,
            snapshot.transient_refresh_failures,
            snapshot.consecutive_refresh_failures
        )),
        Line::from(format!(
            "baton: {}:{}",
            shorten(&snapshot.baton_txid, 22),
            snapshot.baton_vout
        )),
        Line::from(format!("endpoint: {}", shorten(&endpoint, 64))),
        Line::from(format!(
            "winners: verified {}   stale {}   pending {}",
            snapshot.verified_winners, snapshot.stale_winners, snapshot.pending_winners
        )),
        Line::from(format!(
            "source: {}   transport failures: {}   reconnects: {}   rotations: {}   job changes: {}",
            if snapshot.source_degraded { "degraded" } else { "healthy" },
            snapshot.transport_failures,
            snapshot.reconnects,
            snapshot.endpoint_rotations,
            snapshot.job_changes
        )),
        Line::from(format!("payout: {}", shorten(&snapshot.payout_address, 66))),
        Line::from(format!(
            "GPU: util {}   temp {}   power {}   VRAM {}",
            format_metric(telemetry.gpu_utilization_percent, "%"),
            format_metric(telemetry.temperature_c, "C"),
            format_metric(telemetry.power_watts, "W"),
            format_metric(telemetry.vram_used_mib, "MiB"),
        )),
        Line::from(format!(
            "clocks: {}/{}   efficiency {}",
            format_metric(telemetry.graphics_clock_mhz, "MHz"),
            format_metric(telemetry.memory_clock_mhz, "MHz"),
            format_metric(efficiency, " cand/s/W"),
        )),
        Line::from(format!("last error: {}", shorten(last_error, 70))),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Runtime ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn format_metric(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.1}{unit}"))
        .unwrap_or_else(|| "N/A".into())
}

fn render_events(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let max_items = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem<'_>> = state
        .events
        .iter()
        .rev()
        .take(max_items)
        .map(|event| ListItem::new(event.as_str()))
        .collect();
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title(" Recent events ")
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &TuiState) {
    let text = if state.command_mode {
        vec![Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::raw(state.command_input.as_str()),
        ])]
    } else {
        vec![
            Line::from(
                "[/] command  [P] pause  [+/-] intensity  [R] reconnect  [S] settings  [?] help  [Q] quit",
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
        Line::from("R            reconnect authoritative Fulcrum source"),
        Line::from("S            settings"),
        Line::from("/ (: or C)   command bar"),
        Line::from("?            close/open help"),
        Line::from("Q / Ctrl+C   graceful quit"),
        Line::from(""),
        Line::from("Commands:"),
        Line::from("  /intensity <10-100> | /pause | /resume | /reconnect"),
        Line::from("  /address <cashaddr> | /endpoint <wss://...> | /endpoint auto"),
        Line::from("  /status | /config | /devices | /backend | /logs"),
        Line::from("  /benchmark | /help | /quit"),
    ])
    .block(Block::default().title(" Help ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(help, popup);
}

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

fn format_event(event: RuntimeEvent) -> String {
    match event {
        RuntimeEvent::JobRefreshed {
            generation_id,
            height,
            baton_txid,
            baton_vout,
        } => format!(
            "job updated: gen={generation_id} height={height} baton={}:{}",
            shorten(&baton_txid, 18),
            baton_vout
        ),
        RuntimeEvent::StateRefreshFailed { error, consecutive } => {
            format!(
                "PHOTON state check failed; retaining generation (consecutive={consecutive}): {error}"
            )
        }
        RuntimeEvent::Reconnecting(error) => format!("reconnecting: {error}"),
        RuntimeEvent::Reconnected(endpoint) => {
            format!("reconnected: {}", redact_endpoint(&endpoint))
        }
        RuntimeEvent::EndpointRotated { from, to } => format!(
            "source changed: {} -> {}",
            redact_endpoint(&from),
            redact_endpoint(&to)
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
            shorten(&hex::encode(winner.digest), 20)
        ),
        RuntimeEvent::SubmissionAccepted {
            parent_txid,
            child_txid,
        } => format!(
            "submission accepted: parent={} reward={}",
            shorten(&parent_txid, 18),
            shorten(&child_txid, 18)
        ),
        RuntimeEvent::Error(error) => format!("error: {error}"),
    }
}

fn redact_endpoint(input: &str) -> String {
    let Some((scheme, rest)) = input.split_once("://") else {
        return input.into();
    };
    match rest.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}"),
        None => input.into(),
    }
}

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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

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
    fn runtime_view_renders_shared_gpu_telemetry_and_efficiency() {
        let mut snapshot = test_snapshot();
        snapshot.search.current_rate = 500_000.0;
        snapshot.gpu_telemetry = crate::telemetry::GpuTelemetry {
            samples: 2,
            gpu_utilization_percent: Some(88.0),
            power_watts: Some(100.0),
            temperature_c: Some(72.0),
            vram_used_mib: Some(640.0),
            graphics_clock_mhz: Some(2_500.0),
            memory_clock_mhz: Some(8_100.0),
        };
        let state = TuiState::new(&snapshot);
        let backend = ratatui::backend::TestBackend::new(120, 30);
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
        assert!(rendered.contains("temp 72.0C"));
        assert!(rendered.contains("power 100.0W"));
        assert!(rendered.contains("efficiency 5000.0 cand/s/W"));
    }

    #[test]
    fn palette_rejects_invalid_intensity_without_touching_runtime() {
        assert!(parse_palette_command("intensity 9").is_err());
        assert!(parse_palette_command("intensity 101").is_err());
        assert!(parse_palette_command("intensity nope").is_err());
    }

    #[test]
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
