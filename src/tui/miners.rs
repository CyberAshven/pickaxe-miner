//! #### PR #65: carry the dashboard's charts, logs and controls into token sessions.
//! Rendering and history use the same widgets as PHOTON; mining and claims stay
//! in their existing supervisors. Closing a panel never stops those workers.
use super::*;
use crate::electric::ui::{Monitor, Summary};
use crate::hash_split::{Slot, SplitToken};
use crate::telemetry::{GpuTelemetry, LiveTelemetrySampler};

pub struct Miners<'a> {
    pub network: MiningNetwork,
    pub runtimes: Vec<(SplitToken, &'a RuntimeSupervisor)>,
    pub electric: Option<&'a Monitor>,
    pub split: Option<(&'a [Slot], &'a std::sync::Mutex<String>)>,
    pub stop: Option<&'a AtomicBool>,
}

pub(super) struct TokenRow {
    token: SplitToken,
    pub(super) name: String,
    share: Option<u8>,
    pub(super) rate: f64,
    rewards: u64,
    state: String,
    runtime: Option<RuntimeSnapshot>,
}

pub(super) struct Tokens {
    network: MiningNetwork,
    pub(super) split: Vec<TokenRow>,
    pub(super) electric: Option<Summary>,
    pub(super) telemetry: GpuTelemetry,
    layout: String,
    selected: usize,
    editing: bool,
    show_pot: bool,
    intensity: u8,
    paused: bool,
    photon: Option<RuntimeSnapshot>,
}

/// The event source may already name its token; label it only once.
fn label_event(token: &str, message: &str) -> String {
    if message.strip_prefix(token).is_some_and(|tail| {
        tail.is_empty() || tail.starts_with(char::is_whitespace) || tail.starts_with(':')
    }) {
        message.to_owned()
    } else {
        format!("{token} {message}")
    }
}

pub fn run_electric(
    monitor: &Monitor,
    gpus: &[GpuDevice],
    network: MiningNetwork,
    finished: &dyn Fn() -> bool,
) -> Result<(), String> {
    run_miners(
        &Miners {
            network,
            runtimes: Vec::new(),
            electric: Some(monitor),
            split: None,
            stop: None,
        },
        gpus,
        finished,
    )
}

impl Miners<'_> {
    fn stop(&self) {
        if let Some(stop) = self.stop {
            stop.store(true, Ordering::SeqCst);
        }
        if let Some(electric) = self.electric {
            electric.stop.store(true, Ordering::SeqCst);
        }
        // Ownership stays with the caller, which joins and stops supervisors.
    }

    fn pause(&self, paused: bool) -> Result<(), String> {
        for (_, runtime) in &self.runtimes {
            if paused {
                runtime.pause()?;
            } else {
                runtime.resume()?;
            }
        }
        if let Some(electric) = self.electric {
            electric.paused.store(paused, Ordering::Relaxed);
        }
        Ok(())
    }

    fn intensity(&self, value: u8) -> Result<(), String> {
        for (_, runtime) in &self.runtimes {
            runtime.set_intensity(value)?;
        }
        if let Some(electric) = self.electric {
            electric.intensity.store(value, Ordering::Relaxed);
        }
        Ok(())
    }
}

pub fn run_miners(
    miners: &Miners<'_>,
    gpus: &[GpuDevice],
    finished: &dyn Fn() -> bool,
) -> Result<(), String> {
    struct StopOnExit<'a, 'b>(&'a Miners<'b>);
    impl Drop for StopOnExit<'_, '_> {
        fn drop(&mut self) {
            self.0.stop();
        }
    }
    let _stop = StopOnExit(miners);
    let mut terminal = TerminalSession::enter()?;
    crossterm::execute!(io::stdout(), crossterm::event::EnableBracketedPaste)
        .map_err(|e| format!("enable address paste: {e}"))?;
    struct PasteMode;
    impl Drop for PasteMode {
        fn drop(&mut self) {
            let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableBracketedPaste);
        }
    }
    let _paste = PasteMode;
    let mut state = TuiState::starting("Mining session started".into());
    state.set_devices(gpus.to_vec());
    // PHOTON already samples the same devices. ELECTRIC alone needs a sampler.
    let sampler = miners
        .runtimes
        .is_empty()
        .then(|| LiveTelemetrySampler::start(crate::telemetry::telemetry_sources(gpus)));
    let mut rewards = vec![0u64; miners.runtimes.len()];
    let mut selected = 0;
    let mut editing = false;
    let mut show_pot = false;
    let mut paused = false;
    let mut last_draw = Instant::now() - DRAW_INTERVAL;
    while !finished() {
        let snapshots: Vec<_> = miners.runtimes.iter().map(|(_, r)| r.snapshot()).collect();
        for (i, (token, runtime)) in miners.runtimes.iter().enumerate() {
            for event in runtime.drain_events() {
                if matches!(
                    event,
                    RuntimeEvent::SubmissionAccepted { .. }
                        | RuntimeEvent::DirectRewardAccepted { .. }
                ) {
                    rewards[i] += 1;
                }
                state.push_event(label_event(token.name(), &event_log_text(&event)));
            }
        }
        if let Some(electric) = miners.electric {
            for event in electric.drain_events() {
                state.push_event(label_event("ELECTRIC", &event));
            }
        }
        let electric = miners.electric.map(Monitor::summary);
        let pending = snapshots.iter().any(|s| s.pending_winners > 0)
            || electric.as_ref().is_some_and(|e| {
                e.pending || e.withdrawal.phase == crate::electric::withdrawal::Phase::Sending
            });
        if state.profiles_ready(pending, Instant::now()) {
            break;
        }
        let tokens: Vec<_> = match miners.split {
            Some((slots, _)) => slots.iter().map(|s| s.token).collect(),
            None => vec![SplitToken::Electric],
        };
        let rows = tokens
            .iter()
            .map(|token| {
                let share = miners
                    .split
                    .and_then(|(slots, _)| slots.iter().find(|s| s.token == *token))
                    .map(|s| s.share.load(Ordering::Relaxed));
                let (rate, rewards, status) = match token {
                    SplitToken::Electric => {
                        electric.as_ref().map_or((0.0, 0, "Stopped".into()), |e| {
                            (e.rate, e.claims, e.state.clone())
                        })
                    }
                    _ => {
                        let i = miners
                            .runtimes
                            .iter()
                            .position(|(t, _)| t == token)
                            .expect("running token");
                        (
                            snapshots[i].search.current_rate,
                            rewards[i],
                            crate::hash_split::runtime_state(&snapshots[i]),
                        )
                    }
                };
                TokenRow {
                    token: *token,
                    name: token.name().into(),
                    share,
                    rate,
                    rewards,
                    state: status,
                    runtime: miners
                        .runtimes
                        .iter()
                        .position(|(t, _)| t == token)
                        .map(|i| snapshots[i].clone()),
                }
            })
            .collect();
        let telemetry = snapshots.first().map_or_else(
            || {
                sampler
                    .as_ref()
                    .map_or_else(GpuTelemetry::default, LiveTelemetrySampler::snapshot)
            },
            |s| s.gpu_telemetry.clone(),
        );
        let intensity = snapshots.first().map_or_else(
            || electric.as_ref().map_or(100, |e| e.intensity),
            |s| s.search.intensity,
        );
        state.tokens = Some(Tokens {
            network: miners.network,
            split: rows,
            electric,
            telemetry,
            layout: miners.split.map_or_else(String::new, |(_, layout)| {
                layout.lock().unwrap_or_else(|e| e.into_inner()).clone()
            }),
            selected,
            editing,
            show_pot,
            intensity,
            paused,
            photon: snapshots.first().cloned(),
        });
        state.record_frame(snapshots.first(), Instant::now());
        if last_draw.elapsed() >= DRAW_INTERVAL {
            terminal
                .terminal
                .draw(|frame| render_miners(frame, &state))
                .map_err(|error| format!("draw token dashboard: {error}"))?;
            if let (Some(monitor), Some(tokens)) = (miners.electric, state.tokens.as_ref()) {
                if let Some(electric) = tokens.electric.as_ref().filter(|_| tokens.show_pot) {
                    monitor
                        .withdrawal
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .presented(&electric.withdrawal);
                }
            }
            last_draw = Instant::now();
        }
        if event::poll(EVENT_POLL_INTERVAL).map_err(|e| e.to_string())? {
            match event::read().map_err(|e| e.to_string())? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if token_key(key, miners, &mut state)? {
                        break;
                    }
                }
                Event::Paste(text) if show_pot => {
                    if let Some(monitor) = miners.electric {
                        let mut send = monitor.withdrawal.lock().unwrap_or_else(|e| e.into_inner());
                        send.paste(&text);
                    }
                }
                _ => {}
            }
        }
        let tokens = state.tokens.as_ref().expect("session view");
        selected = tokens.selected;
        editing = tokens.editing;
        show_pot = tokens.show_pot;
        paused = tokens.paused;
    }
    if state.profiles_requested {
        crate::session_control::request_profiles();
    }
    Ok(())
}

fn token_command(
    command: PaletteCommand,
    miners: &Miners<'_>,
    state: &mut TuiState,
) -> Result<bool, String> {
    match command {
        PaletteCommand::Quit => return Ok(true),
        PaletteCommand::Charts(request) => state.apply_chart_request(request),
        PaletteCommand::Logs => state.logs_mode = true,
        PaletteCommand::Help => state.show_help = true,
        PaletteCommand::Pot | PaletteCommand::Electric => {
            state.tokens.as_mut().unwrap().show_pot = true
        }
        PaletteCommand::Split | PaletteCommand::Config => state.settings_mode = true,
        PaletteCommand::Pause | PaletteCommand::Resume => {
            let paused = matches!(command, PaletteCommand::Pause);
            miners.pause(paused)?;
            state.tokens.as_mut().unwrap().paused = paused;
        }
        PaletteCommand::SetIntensity(value) => miners.intensity(value)?,
        PaletteCommand::Devices | PaletteCommand::Backend => {
            for device in state.devices.clone() {
                state.push_event(format!(
                    "{}:{} {}",
                    device.backend.as_str(),
                    device.index,
                    device.name
                ));
            }
            state.logs_mode = true;
        }
        PaletteCommand::Status => {
            state.status_line = "Current state and rates are shown for each token.".into()
        }
        PaletteCommand::Reconnect => {
            for (_, runtime) in &miners.runtimes {
                runtime.reconnect()?;
            }
            state.status_line = "PHOTON reconnect requested; ELECTRIC reconnects automatically on transport errors.".into();
        }
        PaletteCommand::SetPayout(_) | PaletteCommand::SetEndpoint(_) => {
            state.status_line = "Stop and use setup to change this session's address or servers; its fee pot and pending claims remain bound to their recipients.".into();
        }
        PaletteCommand::Benchmark => {
            state.status_line = "Stop mining before running a benchmark.".into()
        }
    }
    Ok(false)
}

fn token_key(key: KeyEvent, miners: &Miners<'_>, state: &mut TuiState) -> Result<bool, String> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Ok(true);
    }
    if state.profiles_requested && key.code == KeyCode::Esc {
        state.profiles_requested = false;
        state.profile_idle_since = None;
        state.status_line.clear();
        miners.pause(state.profile_was_paused)?;
        state.tokens.as_mut().unwrap().paused = state.profile_was_paused;
        return Ok(false);
    }
    if state.tokens.as_ref().is_some_and(|t| t.show_pot) {
        if let Some(monitor) = miners.electric {
            use crate::electric::withdrawal::Phase;
            let mut send = monitor.withdrawal.lock().unwrap_or_else(|e| e.into_inner());
            let editing = send.view().phase == Phase::Editing;
            match key.code {
                KeyCode::Esc => {
                    send.cancel();
                    state.tokens.as_mut().unwrap().show_pot = false;
                }
                KeyCode::Enter => send.enter(),
                KeyCode::Backspace => send.backspace(),
                KeyCode::Char(c) if editing => send.character(c),
                KeyCode::Char('y' | 'Y')
                    if key.kind == KeyEventKind::Press
                        && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
                {
                    send.confirm()
                }
                KeyCode::Char('m' | 'M') => send.edit(),
                KeyCode::Char('f' | 'F') => {
                    send.cancel();
                    state.tokens.as_mut().unwrap().show_pot = false;
                }
                KeyCode::Char('q' | 'Q') => return Ok(true),
                _ => {}
            }
        } else if matches!(key.code, KeyCode::Esc | KeyCode::Char('f' | 'F')) {
            state.tokens.as_mut().unwrap().show_pot = false;
        }
        return Ok(false);
    }
    if state.command_mode {
        match key.code {
            KeyCode::Esc => state.cancel_command(),
            KeyCode::Enter => {
                let command = state.finish_command();
                match parse_palette_command(&command) {
                    Ok(command) => return token_command(command, miners, state),
                    Err(error) => state.status_line = error,
                }
            }
            KeyCode::Backspace => {
                state.command_input.pop();
            }
            KeyCode::Up => state.history_previous(),
            KeyCode::Down => state.history_next(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.command_input.push(c)
            }
            _ => {}
        }
        return Ok(false);
    }
    if state.chart_options.is_some() {
        return Ok(state.chart_options_key(key.code));
    }
    if matches!(key.code, KeyCode::Char('q' | 'Q')) {
        return Ok(true);
    }
    if state.advanced_mode {
        let tokens = state.tokens.as_mut().unwrap();
        if matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Tab) {
            let count = tokens.split.len();
            tokens.selected = if key.code == KeyCode::Up {
                (tokens.selected + count - 1) % count
            } else {
                (tokens.selected + 1) % count
            };
            return Ok(false);
        }
        if matches!(
            key.code,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('+' | '-')
        ) {
            let token = tokens.split[tokens.selected].token;
            let up = matches!(key.code, KeyCode::Right | KeyCode::Char('+'));
            if let Some((_, runtime)) = miners.runtimes.iter().find(|(t, _)| *t == token) {
                let snapshot = runtime.snapshot();
                let next = snapshot
                    .token_donation
                    .adjusted(up, snapshot.donation_minimum);
                apply_result(
                    runtime.set_donation(next),
                    &format!("Donation {next}"),
                    state,
                );
            } else if token == SplitToken::Electric {
                if let Some(monitor) = miners.electric {
                    let current = monitor
                        .summary()
                        .donation
                        .unwrap_or(crate::electric::DONATION_MINIMUM);
                    let next = current.adjusted(up, crate::electric::DONATION_MINIMUM);
                    apply_result(
                        monitor.request_donation(miners.network, next),
                        &format!("ELECTRIC donation {next}"),
                        state,
                    );
                }
            }
            return Ok(false);
        }
    }
    if state.show_help || state.logs_mode || state.advanced_mode {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('?' | 'l' | 'L' | 'a' | 'A')
        ) {
            state.show_help = false;
            state.logs_mode = false;
            state.advanced_mode = false;
        }
        return Ok(false);
    }
    let tokens = state.tokens.as_mut().expect("session view");
    if state.settings_mode {
        match key.code {
            KeyCode::Esc if tokens.editing => tokens.editing = false,
            KeyCode::Esc | KeyCode::Char('s' | 'S') => {
                state.settings_mode = false;
                tokens.editing = false;
            }
            KeyCode::Up => {
                tokens.selected = tokens
                    .selected
                    .checked_sub(1)
                    .unwrap_or(tokens.split.len() - 1);
                tokens.editing = false;
            }
            KeyCode::Down => {
                tokens.selected = (tokens.selected + 1) % tokens.split.len();
                tokens.editing = false;
            }
            KeyCode::Enter if miners.split.is_some() => tokens.editing = !tokens.editing,
            KeyCode::Left | KeyCode::Right | KeyCode::Char('+' | '-') if tokens.editing => {
                if let Some((slots, _)) = miners.split {
                    let up = matches!(key.code, KeyCode::Right | KeyCode::Char('+'));
                    state.status_line =
                        match crate::hash_split::step_share(slots, tokens.selected, up) {
                            Ok(share) => {
                                format!("{} share {share}%", tokens.split[tokens.selected].name)
                            }
                            Err(error) => error,
                        };
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('+' | '-') => {
                state.status_line = "Press Enter to unlock the selected share.".into()
            }
            _ => {}
        }
        return Ok(false);
    }
    match key.code {
        KeyCode::Char('b' | 'B') => {
            if tokens.split.iter().all(|row| row.share == Some(0)) {
                state.status_line =
                    "Give a token a nonzero share before saving its profile; P pauses mining."
                        .into();
                return Ok(false);
            }
            let paused = tokens.paused;
            miners.pause(true)?;
            tokens.paused = true;
            state.request_profiles(paused);
            return Ok(false);
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
            tokens.selected = (tokens.selected + 1) % tokens.split.len()
        }
        KeyCode::Up | KeyCode::Left => {
            tokens.selected = (tokens.selected + tokens.split.len() - 1) % tokens.split.len()
        }
        KeyCode::Char('g' | 'G') => state.apply_chart_request(ChartRequest::Toggle),
        KeyCode::Char('o' | 'O') => state.apply_chart_request(ChartRequest::Options),
        KeyCode::Char('s' | 'S') => state.settings_mode = true,
        KeyCode::Char('a' | 'A') => state.advanced_mode = true,
        KeyCode::Char('l' | 'L') => state.logs_mode = true,
        KeyCode::Char('?') => state.show_help = true,
        KeyCode::Char('f' | 'F') => tokens.show_pot = true,
        KeyCode::Char('/' | ':' | 'c' | 'C') => state.open_command(""),
        KeyCode::Char('p' | 'P' | ' ') => {
            miners.pause(!tokens.paused)?;
            tokens.paused = !tokens.paused;
        }
        KeyCode::Char('+' | '=' | ']' | '-' | '[') => {
            let up = matches!(key.code, KeyCode::Char('+' | '=' | ']'));
            miners.intensity(
                (i16::from(tokens.intensity) + if up { 5 } else { -5 }).clamp(10, 100) as u8,
            )?;
        }
        KeyCode::Char('r' | 'R') => return token_command(PaletteCommand::Reconnect, miners, state),
        _ => {}
    }
    Ok(false)
}

fn render_miners(frame: &mut Frame<'_>, state: &TuiState) {
    let tokens = state.tokens.as_ref().expect("session view");
    let area = frame.area();
    let available = area.height.saturating_sub(11);
    // Charts must not consume a participating token's hashrate row. Reserve
    // every token row and the selected token's three compact statistics.
    let minimum_session = tokens.split.len() as u16 + 2;
    let minimum_body = minimum_session.saturating_add(5);
    let charts = if state.charts.is_empty() {
        0
    } else {
        charts_min_height(state)
            .max(available / 2)
            .min(available.saturating_sub(minimum_body))
    };
    let parts = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(available - charts),
        Constraint::Length(charts),
        Constraint::Length(5),
    ])
    .split(area);
    let title = tokens
        .split
        .iter()
        .map(|row| row.name.as_str())
        .collect::<Vec<_>>()
        .join(" + ");
    frame.render_widget(
        Paragraph::new(format!(
            " PICKAXE   {title} · {}{} · Uptime {}",
            network_label(tokens.network),
            if tokens.paused { " · PAUSED" } else { "" },
            format_duration(tokens.electric.as_ref().map_or_else(
                || tokens.photon.as_ref().map_or(0, |s| s.search.elapsed_secs) as f64,
                |e| e.uptime.as_secs_f64()
            ))
        ))
        .block(Block::bordered()),
        parts[0],
    );
    frame.render_widget(
        Gauge::default()
            .block(Block::bordered().title(" GPU intensity "))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(f64::from(tokens.intensity.min(100)) / 100.0)
            .label(format!("{}%", tokens.intensity)),
        parts[1],
    );
    let body = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(parts[2]);
    let mut lines = Vec::new();
    for (index, row) in tokens.split.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!(
                "{} Hashrate {:<8} {}{}",
                if index == tokens.selected { ">" } else { " " },
                row.name,
                crate::telemetry::format_hash_rate(row.rate),
                row.share
                    .map_or_else(String::new, |share| format!("   {share}% work"))
            ),
            Style::default().fg(Color::Cyan),
        )));
    }
    lines.push(Line::from(format!(
        "GPUs  {} selected · /devices lists them",
        state.devices.len()
    )));
    lines.push(Line::from(format!(
        "GPU utilization {} · {} · {}",
        format_metric(tokens.telemetry.gpu_utilization_percent, "%"),
        format_metric(tokens.telemetry.temperature_c, " C"),
        format_metric(tokens.telemetry.power_watts, " W")
    )));
    if let Some(electric) = &tokens.electric {
        lines.push(Line::from(match electric.pot {
            Some((sats, claims)) => {
                format!("Fee pot  {sats} sats · {claims} claims · [F] fund / QR")
            }
            None => "Fee pot  reading balance · [F] fund / QR".into(),
        }));
    }
    let runtime_areas = Layout::vertical([
        Constraint::Length(
            (lines.len() as u16 + 2)
                .min(body[0].height.saturating_sub(5))
                .max(minimum_session.min(body[0].height)),
        ),
        Constraint::Min(0),
    ])
    .split(body[0]);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Mining session ")),
        runtime_areas[0],
    );
    if let Some(row) = tokens.split.get(tokens.selected) {
        if runtime_areas[1].height >= 4 {
            let details = if let Some(snapshot) = &row.runtime {
                let mut details = vec![
                    Line::from(format!(
                        "Wins       {} accepted · {} pending",
                        row.rewards, snapshot.pending_winners
                    )),
                    Line::from(
                        match win_probability(&snapshot.photon_target_le, snapshot.network) {
                            Some(probability) => format!(
                                "Odds       1 win per {} hashes",
                                format_si_count(1.0 / probability)
                            ),
                            None => "Odds       Reading current target".into(),
                        },
                    ),
                    Line::from(format!("Work       {} hashes", snapshot.search.candidates)),
                ];
                let extra = runtime_areas[1].height.saturating_sub(5) as usize;
                let fields = runtime_field_groups(snapshot, runtime_areas[1].width)
                    .into_iter()
                    .filter(|field| {
                        field
                            .lines
                            .first()
                            .and_then(|line| line.spans.first())
                            .is_some_and(|span| {
                                matches!(
                                    span.content.trim(),
                                    "Target"
                                        | "Chain"
                                        | "Baton"
                                        | "Endpoint"
                                        | "Network"
                                        | "Source"
                                        | "Last error"
                                )
                            })
                    })
                    .collect();
                details.extend(fit_runtime_fields(fields, extra));
                details
            } else if let Some(e) = &tokens.electric {
                let expected = e.difficulty.map(|d| 256f64.powi(i32::from(d)));
                vec![
                    Line::from(format!(
                        "Wins       {} accepted · {} pending",
                        row.rewards,
                        u8::from(e.pending)
                    )),
                    Line::from(match expected {
                        Some(hashes) => {
                            format!("Odds       1 win per {} hashes", format_si_count(hashes))
                        }
                        None => "Odds       Reading current target".into(),
                    }),
                    Line::from(format!("Work       {} hashes", e.hashes)),
                ]
            } else {
                Vec::new()
            };
            frame.render_widget(
                Paragraph::new(details).block(
                    Block::bordered().title(format!(" {} details · {} ", row.name, row.state)),
                ),
                runtime_areas[1],
            );
        }
    }
    render_events(frame, body[1], state);
    if charts > 0 {
        render_charts(frame, parts[3], state);
    }
    render_footer(frame, parts[4], state);
    if state.settings_mode {
        let mut lines = vec![
            Line::from("↑/↓ select · Enter unlocks a share · ←/→ ±5% · Esc closes"),
            Line::from(""),
        ];
        for (i, row) in tokens.split.iter().enumerate() {
            lines.push(Line::from(format!(
                "{} {}   {}{}",
                if i == tokens.selected { ">" } else { " " },
                row.name,
                row.share
                    .map_or_else(|| "whole GPU session".into(), |s| format!("{s}%")),
                if i == tokens.selected && tokens.editing {
                    "  [editing]"
                } else {
                    ""
                }
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(
            "Shares below 100% leave idle time. Unavailable tokens lend their time.",
        ));
        popup(frame, area, " Settings · hash split ", lines);
    }
    if state.advanced_mode {
        let mut lines = vec![
            Line::from("↑/↓ or Tab selects a token · ←/→ changes its donation"),
            Line::from(""),
        ];
        for (i, row) in tokens.split.iter().enumerate() {
            let donation = row
                .runtime
                .as_ref()
                .map(|s| s.token_donation)
                .or_else(|| tokens.electric.as_ref().and_then(|e| e.donation));
            lines.push(Line::from(format!(
                "{} {} donation {}",
                if i == tokens.selected { ">" } else { " " },
                row.name,
                donation.map_or_else(|| "loading".into(), |d| d.to_string())
            )));
        }
        lines.extend([
            Line::from(""),
            Line::from("Changes apply to future mining work for this session."),
            Line::from("B on the dashboard saves this session and returns to profiles."),
            Line::from("Existing proofs, payouts and the fee pot keep their recipients."),
            Line::from("A / Esc closes; mining continues."),
        ]);
        if !tokens.layout.is_empty() {
            lines.push(Line::from(format!("GPU allocation: {}", tokens.layout)));
        }
        popup(frame, area, " Advanced settings ", lines);
    }
    if state.logs_mode {
        render_logs(frame, area, state);
    }
    if state.show_help {
        popup(
            frame,
            area,
            " Help ",
            vec![
                Line::from("G charts · O chart options · L full logs · / commands"),
                Line::from("S split shares (Enter unlocks; arrows adjust) · A advanced"),
                Line::from("F fee pot / QR · P pause · +/- intensity · Q quit"),
                Line::from("/split · /electric · /pot · /chart options · /status"),
                Line::from("Esc closes a panel. Only Q or Ctrl+C stops mining."),
            ],
        );
    }
    if let Some(cursor) = state.chart_options {
        render_chart_options(frame, area, state, cursor);
    }
    if tokens.show_pot {
        render_pot(frame, area, tokens.electric.as_ref(), tokens.network);
    }
}

fn popup(frame: &mut Frame<'_>, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let popup = centered_rect(90, 70, area);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(title)),
        popup,
    );
}

fn render_pot(
    frame: &mut Frame<'_>,
    area: Rect,
    electric: Option<&Summary>,
    network: MiningNetwork,
) {
    let Some(e) = electric else {
        popup(
            frame,
            area,
            " Fee pot ",
            vec![Line::from(
                "This session has no ELECTRIC miner. Esc closes.",
            )],
        );
        return;
    };
    let panel = centered_rect(96, 92, area);
    frame.render_widget(Clear, panel);
    let block = Block::bordered().title(" ELECTRIC · fund the fee pot · F/Esc closes ");
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    use crate::electric::withdrawal::Phase;
    let send = &e.withdrawal;
    if send.enabled && send.phase != Phase::Idle {
        let mut lines = vec![
            Line::from(format!(
                "Max/send · {} · plain BCH from this fee pot",
                network.as_str()
            )),
            Line::from(""),
            Line::from("Destination:"),
            Line::from(format!(
                "{}{}",
                send.destination,
                if send.phase == Phase::Editing {
                    "_"
                } else {
                    ""
                }
            )),
            Line::from(""),
        ];
        if send.count > 0 {
            lines.push(Line::from(format!("Receive: {} sats", send.amount)));
            lines.push(Line::from(format!(
                "Total fee: {} sats · {} transaction(s)",
                send.fee, send.count
            )));
            lines.push(Line::from(""));
        }
        lines.push(Line::from(match send.phase {
            Phase::Editing => "Type/paste a BCH address; Enter previews, Esc cancels.".into(),
            Phase::Preparing => "Finishing any pending claim; preparing Max/send…".into(),
            _ => send.message.clone(),
        }));
        if send.phase == Phase::Error || send.phase == Phase::Complete {
            lines.push(Line::from("M prepares a new Max/send. Esc closes."));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        return;
    }
    let qr_width = e
        .pot_qr
        .first()
        .map_or(0, |line| line.chars().count() as u16);
    let columns =
        Layout::horizontal([Constraint::Min(25), Constraint::Length(qr_width)]).split(inner);
    let mut lines = vec![
        Line::from(
            e.pot_address
                .clone()
                .unwrap_or_else(|| "Preparing pot address…".into()),
        ),
        Line::from(""),
        Line::from("Send plain BCH on the same network. The pot pays claim fees."),
        Line::from(match e.pot {
            Some((s, c)) => format!("{s} sats · enough for {c} claims"),
            None => "Reading balance…".into(),
        }),
        Line::from("Tokens go directly to each winning recipient."),
    ];
    lines.push(Line::from(""));
    if send.enabled {
        lines.push(Line::from(format!(
            "Owner-controlled pot · {} · M Max/send",
            network.as_str()
        )));
    } else {
        lines.push(Line::from(
            "This funded pot refunds only to its original payout wallet.",
        ));
        lines.push(Line::from(
            "New owner pots support Max/send; old funds are not moved.",
        ));
    }
    if let Some(key) = &e.pot_key {
        lines.push(Line::from(format!("Recovery key file: {key}")));
        lines.push(Line::from("Back up this file; it authorizes refunds."));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), columns[0]);
    if columns[1].width >= qr_width && columns[1].height >= e.pot_qr.len() as u16 {
        frame.render_widget(
            Paragraph::new(e.pot_qr.join("\n"))
                .style(Style::default().fg(Color::White).bg(Color::Black)),
            columns[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new("Enlarge the terminal to show the full QR code."),
            columns[1],
        );
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::atomic::AtomicU8;

    #[test]
    fn electric_donation_is_editable_and_profile_navigation_is_explicit() {
        let monitor = Monitor::new(80, false, "test".into());
        monitor.set_donation_display(crate::donation::Scheme::Work([400, 0]));
        let miners = Miners {
            network: MiningNetwork::Chipnet,
            runtimes: vec![],
            electric: Some(&monitor),
            split: None,
            stop: None,
        };
        let mut state = fixture();
        state.tokens.as_mut().unwrap().split.remove(0);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        token_key(key(KeyCode::Char('a')), &miners, &mut state).unwrap();
        token_key(key(KeyCode::Right), &miners, &mut state).unwrap();
        assert_eq!(monitor.summary().donation.unwrap().bps(), 450);
        token_key(key(KeyCode::Left), &miners, &mut state).unwrap();
        token_key(key(KeyCode::Left), &miners, &mut state).unwrap();
        assert_eq!(monitor.summary().donation.unwrap().bps(), 400);
        assert!(!monitor.stopped());
        token_key(key(KeyCode::Esc), &miners, &mut state).unwrap();
        assert!(!token_key(key(KeyCode::Char('b')), &miners, &mut state).unwrap());
        assert!(state.profiles_requested);
        let now = Instant::now();
        assert!(!state.profiles_ready(true, now));
        assert!(!state.profiles_ready(false, now + Duration::from_secs(1)));
        assert!(!state.profiles_ready(true, now + Duration::from_secs(2)));
        assert!(!state.profiles_ready(false, now + Duration::from_secs(3)));
        assert!(state.profiles_ready(false, now + Duration::from_secs(4)));
        token_key(key(KeyCode::Esc), &miners, &mut state).unwrap();
        assert!(!state.profiles_requested);
        assert!(!monitor.paused.load(Ordering::Relaxed));
    }

    #[test]
    fn small_dashboard_selects_each_tokens_statistics_without_duplicate_rates() {
        let mut state = fixture();
        let tokens = state.tokens.as_mut().unwrap();
        tokens.selected = 1;
        tokens.electric.as_mut().unwrap().difficulty = Some(4);
        tokens.electric.as_mut().unwrap().hashes = 123456;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render_miners(frame, &state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for expected in [
            "ELECTRIC details",
            "Wins",
            "Odds",
            "Work",
            "123456",
            "GPU utilization",
            "[B] profiles",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert_eq!(text.matches("Hashrate").count(), 2);
        assert!(state.charts.is_empty());
    }

    #[test]
    fn charts_never_hide_a_participating_tokens_hashrate() {
        for (width, height) in [(80, 24), (120, 32), (140, 48)] {
            let mut state = fixture();
            state.apply_chart_request(ChartRequest::Toggle);
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render_miners(frame, &state)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            for expected in ["Hashrate PHOTON", "Hashrate ELECTRIC"] {
                assert!(
                    text.contains(expected),
                    "{width}x{height}: missing {expected}"
                );
            }
        }
    }

    fn fixture() -> TuiState {
        let monitor = Monitor::new(80, false, "Preview GPU".into());
        // Deliberately not a usable payment destination.
        monitor.set_pot(
            "PREVIEW ONLY - DO NOT PAY",
            Some(Path::new("electric.pot-key")),
        );
        monitor.pot_balance(8000, 3);
        monitor.set_donation_display(crate::donation::Scheme::Work([400, 0]));
        let mut state = TuiState::starting("UI preview: sample data, no mining or payments".into());
        state.tokens = Some(Tokens {
            network: MiningNetwork::Chipnet,
            split: vec![
                TokenRow {
                    token: SplitToken::Gpu(MiningToken::Photon),
                    name: "PHOTON".into(),
                    share: Some(70),
                    rate: 700e6,
                    rewards: 0,
                    state: "Sample PHOTON data".into(),
                    runtime: Some(super::super::tests::test_snapshot()),
                },
                TokenRow {
                    token: SplitToken::Electric,
                    name: "ELECTRIC".into(),
                    share: Some(30),
                    rate: 200e6,
                    rewards: 0,
                    state: "Sample ELECTRIC data".into(),
                    runtime: None,
                },
            ],
            electric: Some(monitor.summary()),
            telemetry: GpuTelemetry::default(),
            layout: "GPU 0 takes turns · PHOTON 70% / ELECTRIC 30%".into(),
            selected: 0,
            editing: false,
            show_pot: false,
            intensity: 80,
            paused: false,
            photon: None,
        });
        for i in 0..HISTORY_CAP {
            let variation = (i as f64 / 19.0).sin() * 25e6;
            state.history.push_back(HistorySample {
                rate: 900e6 + variation,
                tokens: vec![700e6 + variation, 200e6],
                gpu: GpuTelemetry {
                    temperature_c: Some(62.0 + (i as f64 / 80.0).sin()),
                    power_watts: Some(80.0),
                    ..Default::default()
                },
            });
        }
        state.push_event(label_event("ELECTRIC", "ELECTRIC sample connection"));
        state.push_event(label_event("ELECTRIC", "Sample reward event"));
        state
    }

    #[test]
    fn panels_do_not_quit_or_change_locked_shares_and_live_controls_reach_monitor() {
        let monitor = Arc::new(Monitor::new(80, false, "test".into()));
        let slots = vec![Slot {
            token: SplitToken::Electric,
            share: AtomicU8::new(30),
            member: Box::new(crate::hash_split::ElectricMember(Arc::clone(&monitor))),
        }];
        let layout = std::sync::Mutex::new(String::new());
        let miners = Miners {
            network: MiningNetwork::Chipnet,
            runtimes: vec![],
            electric: Some(&monitor),
            split: Some((&slots, &layout)),
            stop: None,
        };
        let mut state = fixture();
        state.tokens.as_mut().unwrap().split.remove(0);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(state.charts.is_empty(), "all charts start hidden");
        token_key(key(KeyCode::Char('G')), &miners, &mut state).unwrap();
        assert_eq!(state.charts, DEFAULT_CHARTS);
        token_key(key(KeyCode::Char('g')), &miners, &mut state).unwrap();
        assert!(state.charts.is_empty(), "G hides them again");
        assert!(!token_key(key(KeyCode::Char('s')), &miners, &mut state).unwrap());
        token_key(key(KeyCode::Right), &miners, &mut state).unwrap();
        assert_eq!(slots[0].share.load(Ordering::Relaxed), 30);
        token_key(key(KeyCode::Enter), &miners, &mut state).unwrap();
        token_key(key(KeyCode::Right), &miners, &mut state).unwrap();
        assert_eq!(slots[0].share.load(Ordering::Relaxed), 35);
        assert!(!token_key(key(KeyCode::Esc), &miners, &mut state).unwrap());
        assert!(!state.tokens.as_ref().unwrap().editing);
        assert!(!token_key(key(KeyCode::Esc), &miners, &mut state).unwrap());
        token_key(key(KeyCode::Char('+')), &miners, &mut state).unwrap();
        assert_eq!(monitor.intensity.load(Ordering::Relaxed), 85);
        token_key(key(KeyCode::Char('p')), &miners, &mut state).unwrap();
        assert!(monitor.paused.load(Ordering::Relaxed));
        token_key(key(KeyCode::Char('p')), &miners, &mut state).unwrap();
        assert!(!monitor.paused.load(Ordering::Relaxed));
        token_key(key(KeyCode::Char('f')), &miners, &mut state).unwrap();
        assert!(state.tokens.as_ref().unwrap().show_pot);
        assert!(!token_key(key(KeyCode::Esc), &miners, &mut state).unwrap());
        assert!(!monitor.stopped());
        assert!(token_key(key(KeyCode::Char('q')), &miners, &mut state).unwrap());
    }

    #[test]
    fn pot_send_address_editing_does_not_trigger_mining_shortcuts() {
        let monitor = Monitor::new(100, false, "Preview GPU".into());
        monitor.withdrawal.lock().unwrap().enabled = true;
        let miners = Miners {
            network: MiningNetwork::Mainnet,
            runtimes: vec![],
            electric: Some(&monitor),
            split: None,
            stop: None,
        };
        let mut state = fixture();
        state.tokens.as_mut().unwrap().show_pot = true;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        token_key(key(KeyCode::Char('m')), &miners, &mut state).unwrap();
        for ch in "bitcoincash:qfrq".chars() {
            assert!(!token_key(key(KeyCode::Char(ch)), &miners, &mut state).unwrap());
        }
        assert_eq!(
            monitor.withdrawal.lock().unwrap().view().destination,
            "bitcoincash:qfrq"
        );
        assert!(state.tokens.as_ref().unwrap().show_pot);
        assert!(!monitor.stopped());
        token_key(key(KeyCode::Esc), &miners, &mut state).unwrap();
        assert!(!monitor.withdrawal.lock().unwrap().holding());
        assert!(!state.tokens.as_ref().unwrap().show_pot);
    }

    #[test]
    fn max_send_confirmation_shows_network_amount_and_fee_in_a_small_terminal() {
        let mut state = fixture();
        state.tokens.as_mut().unwrap().network = MiningNetwork::Mainnet;
        state.tokens.as_mut().unwrap().show_pot = true;
        state
            .tokens
            .as_mut()
            .unwrap()
            .electric
            .as_mut()
            .unwrap()
            .withdrawal = crate::electric::withdrawal::View {
            enabled: true,
            phase: crate::electric::withdrawal::Phase::Preview,
            destination: "PREVIEW ONLY - DO NOT PAY".into(),
            amount: 12345,
            fee: 456,
            count: 2,
            message: "Y sends Max; Esc cancels. Quote expires in 60 seconds.".into(),
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render_miners(frame, &state)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for expected in [
            "mainnet",
            "Receive: 12345 sats",
            "Total fee: 456 sats",
            "2 transaction(s)",
            "Y sends Max",
            "Esc cancels",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn shared_dashboard_carries_charts_options_advanced_and_complete_qr() {
        let mut state = fixture();
        for (name, expected) in [
            ("02-split-dashboard", "Mining session"),
            ("08-charts-after-g", "Hashrate"),
            ("03-chart-options", "Chart options"),
            ("04-advanced", "Advanced settings"),
            ("05-pot-funding", "PREVIEW ONLY - DO NOT PAY"),
        ] {
            state.apply_chart_request(ChartRequest::Off);
            if name == "08-charts-after-g" {
                state.apply_chart_request(ChartRequest::Toggle);
            }
            // Exercise the existing PHOTON advanced controls in a split, not
            // just the ELECTRIC-only explanation.
            state.tokens.as_mut().unwrap().photon =
                (name == "04-advanced").then(super::super::tests::test_snapshot);
            state.chart_options = (name == "03-chart-options").then_some(0);
            state.advanced_mode = name == "04-advanced";
            state.tokens.as_mut().unwrap().show_pot = name == "05-pot-funding";
            for (width, height) in [(80, 24), (140, 48)] {
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render_miners(frame, &state)).unwrap();
                let buffer = terminal.backend().buffer();
                let text = buffer
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(
                    text.contains(expected),
                    "{name} at {width}x{height}: {text}"
                );
                if name == "02-split-dashboard" && width == 140 {
                    assert_eq!(text.matches("Hashrate").count(), 2);
                    assert!(text.contains("ELECTRIC sample connection"));
                    assert!(text.contains("ELECTRIC Sample reward event"));
                    assert!(!text.contains("ELECTRIC ELECTRIC"));
                }
                if name == "04-advanced" && width == 140 {
                    assert_eq!(text.matches("Advanced settings").count(), 1);
                    assert!(text.contains("for this session"));
                    assert!(text.contains("ELECTRIC donation 4.00%"));
                    assert!(!text.contains("saved to your profile when you stop"));
                }
                if name == "05-pot-funding" && width == 140 {
                    for row in &state
                        .tokens
                        .as_ref()
                        .unwrap()
                        .electric
                        .as_ref()
                        .unwrap()
                        .pot_qr
                    {
                        assert!(text.contains(row), "the QR must not be clipped");
                    }
                }
                if width == 140 {
                    export_preview(name, buffer);
                }
            }
        }
    }

    /// Optional captures of actual Ratatui cells, using only labelled sample data.
    pub(in crate::tui) fn export_preview(name: &str, buffer: &ratatui::buffer::Buffer) {
        let Ok(directory) = std::env::var("PICKAXE_UI_PREVIEW_DIR") else {
            return;
        };
        let cells: Vec<_> = buffer
            .content()
            .iter()
            .map(|cell| {
                serde_json::json!({
                    "text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg),
                })
            })
            .collect();
        let path = Path::new(&directory).join(format!("{name}.json"));
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "width":buffer.area.width,"height":buffer.area.height,"cells":cells,
                "caption":"ACTUAL UI RENDER - SAMPLE DATA - NO MINING OR PAYMENTS"
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
