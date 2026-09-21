use crate::runtime::{RuntimeEvent, RuntimeSnapshot, RuntimeSupervisor, SupervisorState};
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
    Frame, Terminal,
};
use std::{
    collections::VecDeque,
    io::{self, Stdout},
    time::{Duration, Instant},
};

const DRAW_INTERVAL: Duration = Duration::from_millis(200);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const EVENT_HISTORY_CAP: usize = 96;

type PickaxeTerminal = Terminal<CrosstermBackend<Stdout>>;

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
    Help,
    Quit,
}

struct TuiState {
    command_mode: bool,
    command_input: String,
    show_help: bool,
    status_line: String,
    events: VecDeque<String>,
    last_sample: Instant,
    last_candidates: u64,
    current_rate: f64,
    peak_rate: f64,
    dry_run: bool,
}

impl TuiState {
    fn new(snapshot: &RuntimeSnapshot, dry_run: bool) -> Self {
        let mut events = VecDeque::with_capacity(EVENT_HISTORY_CAP);
        events.push_back("Runtime started; authoritative PHOTON state is supervised.".into());
        Self {
            command_mode: false,
            command_input: String::new(),
            show_help: false,
            status_line: if dry_run {
                "DRY RUN: verified winners are never broadcast.".into()
            } else {
                "Donation: 2%".into()
            },
            events,
            last_sample: Instant::now(),
            last_candidates: snapshot.search.candidates,
            current_rate: 0.0,
            peak_rate: snapshot.search.rate,
            dry_run,
        }
    }

    fn push_event(&mut self, message: String) {
        if self.events.len() == EVENT_HISTORY_CAP {
            self.events.pop_front();
        }
        self.events.push_back(message);
    }

    fn update_rates(&mut self, snapshot: &RuntimeSnapshot) {
        let elapsed = self.last_sample.elapsed().as_secs_f64();
        if elapsed >= 0.15 {
            let completed = snapshot
                .search
                .candidates
                .saturating_sub(self.last_candidates);
            self.current_rate = completed as f64 / elapsed;
            self.peak_rate = self
                .peak_rate
                .max(self.current_rate)
                .max(snapshot.search.rate);
            self.last_candidates = snapshot.search.candidates;
            self.last_sample = Instant::now();
        }
    }
}

pub fn run(supervisor: RuntimeSupervisor, dry_run: bool) -> Result<RuntimeSnapshot, String> {
    let initial = supervisor.snapshot();
    let mut state = TuiState::new(&initial, dry_run);
    let mut terminal = TerminalSession::enter()?;
    let mut quit = false;
    let mut last_draw = Instant::now() - DRAW_INTERVAL;

    while !quit {
        let snapshot = supervisor.snapshot();
        state.update_rates(&snapshot);

        for event in supervisor.drain_events() {
            state.push_event(format_event(event));
        }

        if dry_run && snapshot.pending_winners > 0 {
            state.status_line =
                "Dry-run winner verified; mining is paused. Press Q to exit.".into();
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
                state.command_mode = false;
                state.command_input.clear();
            }
            KeyCode::Enter => {
                let input = std::mem::take(&mut state.command_input);
                state.command_mode = false;
                match parse_palette_command(&input) {
                    Ok(command) => {
                        return apply_palette_command(command, supervisor, snapshot, state)
                    }
                    Err(error) => state.status_line = error,
                }
            }
            KeyCode::Backspace => {
                state.command_input.pop();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.command_input.push(ch);
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

    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => Ok(true),
        KeyCode::Char('?') => {
            state.show_help = true;
            Ok(false)
        }
        KeyCode::Char(':') | KeyCode::Char('c') | KeyCode::Char('C') => {
            state.command_mode = true;
            state.command_input.clear();
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
            state.status_line = format!(
                "generation={} height={} rate={:.0}/s pending={}",
                snapshot.generation_id,
                snapshot.height,
                snapshot.search.rate,
                snapshot.pending_winners
            );
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
    let trimmed = input.trim();
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
            if argument.eq_ignore_ascii_case("clear") {
                Ok(PaletteCommand::SetEndpoint(None))
            } else if argument.is_empty() {
                Err("usage: endpoint <wss://...> | endpoint clear".into())
            } else {
                Ok(PaletteCommand::SetEndpoint(Some(argument.into())))
            }
        }
        "status" => Ok(PaletteCommand::Status),
        "help" | "?" => Ok(PaletteCommand::Help),
        "quit" | "exit" => Ok(PaletteCommand::Quit),
        _ => Err(format!("unknown command {command}. Try help.")),
    }
}

fn render(frame: &mut Frame<'_>, snapshot: &RuntimeSnapshot, state: &TuiState) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(area);

    render_header(frame, rows[0], snapshot, state);
    render_intensity(frame, rows[1], snapshot);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(rows[2]);
    render_stats(frame, body[0], snapshot, state);
    render_events(frame, body[1], state);
    render_footer(frame, rows[3], state);

    if state.show_help {
        render_help(frame, area);
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot, state: &TuiState) {
    let state_text = format!("{:?}", snapshot.state).to_ascii_uppercase();
    let dry = if state.dry_run { "  DRY RUN" } else { "" };
    let line = Line::from(vec![
        Span::styled(" PICKAXE ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "PHOTON   {state_text}{dry}   {} device {}   Donation: 2%",
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

fn render_stats(frame: &mut Frame<'_>, area: Rect, snapshot: &RuntimeSnapshot, state: &TuiState) {
    let endpoint = redact_endpoint(&snapshot.endpoint);
    let last_error = snapshot.last_error.as_deref().unwrap_or("none");
    let lines = vec![
        Line::from(format!(
            "rate: current {:>10.0}/s   average {:>10.0}/s   peak {:>10.0}/s",
            state.current_rate, snapshot.search.rate, state.peak_rate
        )),
        Line::from(format!(
            "candidates: {}   batches: {}   uptime: {}s",
            snapshot.search.candidates, snapshot.search.batches, snapshot.search.elapsed_secs
        )),
        Line::from(format!(
            "generation: {}   height: {}   refreshes: {}",
            snapshot.generation_id, snapshot.height, snapshot.refreshes
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
            "reconnects: {}   stale rebuilds: {}",
            snapshot.reconnects, snapshot.stale_rebuilds
        )),
        Line::from(format!("payout: {}", shorten(&snapshot.payout_address, 66))),
        Line::from("GPU telemetry: runtime provider unavailable"),
        Line::from(format!("last error: {}", shorten(last_error, 70))),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title(" Runtime ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
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
        Line::from(vec![
            Span::styled(":", Style::default().fg(Color::Cyan)),
            Span::raw(state.command_input.as_str()),
        ])
    } else {
        Line::from(format!(
            "{}   [+/-] intensity  [Space/P] pause  [R] reconnect  [:] command  [?] help  [Q] quit",
            state.status_line
        ))
    };
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
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
        Line::from(": / C        command palette"),
        Line::from("?            close/open help"),
        Line::from("Q / Ctrl+C   graceful quit"),
        Line::from(""),
        Line::from("Commands:"),
        Line::from("  intensity <10-100>"),
        Line::from("  pause | resume | reconnect | status"),
        Line::from("  address <cashaddr>"),
        Line::from("  endpoint <wss://...> | endpoint clear"),
        Line::from("  help | quit"),
    ])
    .block(Block::default().title(" Help ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(help, popup);
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
            changed,
        } => {
            if changed {
                format!(
                    "job updated: gen={generation_id} height={height} baton={}:{}",
                    shorten(&baton_txid, 18),
                    baton_vout
                )
            } else {
                format!("job refreshed: gen={generation_id} height={height}")
            }
        }
        RuntimeEvent::Reconnecting(error) => format!("reconnecting: {error}"),
        RuntimeEvent::Reconnected(endpoint) => {
            format!("reconnected: {}", redact_endpoint(&endpoint))
        }
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
            refreshes: 0,
            stale_rebuilds: 0,
            reconnects: 0,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: Default::default(),
        };
        let mut state = TuiState::new(&snapshot, true);
        for i in 0..(EVENT_HISTORY_CAP + 20) {
            state.push_event(format!("event {i}"));
        }
        assert_eq!(state.events.len(), EVENT_HISTORY_CAP);
        assert_eq!(state.events.back().unwrap(), "event 115");
    }
}
