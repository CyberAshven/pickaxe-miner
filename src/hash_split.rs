//! #### PR #60: the token hash split. One session mines several GPU tokens
//! on the same GPUs, each for its share of the time in 5% steps, the shares
//! together at most 100% (the rest idles). Only one token's kernels run at a
//! time: on one GPU two tokens' kernels queue behind each other anyway, and
//! measured together without a split PHOTON kept 64% and ELECTRIC 35% of
//! their solo rates (docs/photon-electric-auxpow.md). A token that cannot
//! mine for now (ELECTRIC with an empty fee pot, PHOTON claiming a winner)
//! gives its time to the others until it can. See docs/hash-split.md.
use crate::backend::GpuDevice;
use crate::config::{MiningNetwork, MiningToken, RuntimeConfig};
use crate::electric::ui::Monitor;
use crate::runtime::{RuntimeEvent, RuntimeSupervisor, SupervisorState};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Shares move in steps of this many percent.
pub const STEP: u8 = 5;
/// One round of turns: every token mining its share once. A 5% share is
/// 100 ms of it, several PHOTON batches and hundreds of ELECTRIC ones.
pub const PERIOD: Duration = Duration::from_secs(2);

/// A token the split can give GPU time to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitToken {
    /// A covenant token the PHOTON runtime mines (`MiningToken`).
    Gpu(MiningToken),
    /// ELECTRIC, mined by its own session and paid from its fee pot.
    Electric,
}

impl SplitToken {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gpu(token) => token.as_str(),
            Self::Electric => "ELECTRIC",
        }
    }

    /// Every GPU token this build mines on `network`, in the setup's order:
    /// the runtime's GPU tokens, then ELECTRIC if it is deployed there. A
    /// new GPU token joins the split by being one of these.
    pub fn available(network: MiningNetwork) -> Vec<Self> {
        let mut tokens: Vec<Self> = MiningToken::GPU_SUPPORTED
            .iter()
            .copied()
            .filter(|token| token.ensure_supported(network).is_ok())
            .map(Self::Gpu)
            .collect();
        tokens.sort_unstable_by_key(|token| token.name());
        if crate::electric::Deployment::select(network, None, None, None, None).is_ok() {
            tokens.push(Self::Electric);
        }
        tokens
    }

    /// The token named `name` (case-insensitive) on `network`.
    pub fn parse(name: &str, network: MiningNetwork) -> Result<Self, String> {
        Self::available(network)
            .into_iter()
            .find(|token| token.name().eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| {
                format!(
                    "{name} is not a GPU token on {}; the split mines {}",
                    network.as_str(),
                    Self::available(network)
                        .iter()
                        .map(|token| token.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// Each token's share of GPU time, in percent: multiples of 5 that add up
/// to at most 100.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shares(Vec<(SplitToken, u8)>);

impl Shares {
    /// `tokens` with the GPUs split evenly in 5% steps, the first tokens
    /// taking what does not divide (two: 50/50; three: 35/35/30).
    pub fn even(tokens: &[SplitToken]) -> Self {
        let count = tokens.len().max(1) as u8;
        let each = 100 / count / STEP * STEP;
        let mut left = 100 - each * count;
        Self(
            tokens
                .iter()
                .map(|&token| {
                    let extra = left.min(STEP);
                    left -= extra;
                    (token, each + extra)
                })
                .collect(),
        )
    }

    /// Shares as `NAME=PERCENT` pairs, such as `photon=80` and `electric=20`;
    /// tokens not named get 0%.
    pub fn parse(pairs: &[String], network: MiningNetwork) -> Result<Self, String> {
        let mut shares = Self(
            SplitToken::available(network)
                .into_iter()
                .map(|token| (token, 0))
                .collect(),
        );
        for pair in pairs {
            let (name, percent) = pair
                .split_once('=')
                .ok_or_else(|| format!("--share {pair}: write it as TOKEN=PERCENT"))?;
            let token = SplitToken::parse(name, network)?;
            let percent: u8 = percent
                .trim()
                .trim_end_matches('%')
                .parse()
                .map_err(|_| format!("--share {pair}: the share is a whole percent"))?;
            let slot = shares
                .0
                .iter_mut()
                .find(|(candidate, _)| *candidate == token)
                .expect("parsed tokens are available");
            slot.1 = percent;
        }
        shares.validate()?;
        Ok(shares)
    }

    pub fn iter(&self) -> impl Iterator<Item = (SplitToken, u8)> + '_ {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, token: SplitToken) -> u8 {
        self.iter()
            .find(|(candidate, _)| *candidate == token)
            .map_or(0, |(_, share)| share)
    }

    /// The token and share at `index`.
    pub fn at(&self, index: usize) -> Option<(SplitToken, u8)> {
        self.0.get(index).copied()
    }

    pub fn total(&self) -> u8 {
        total(self.iter().map(|(_, share)| share))
    }

    /// Moves the share at `index` one step up or down. Up is refused when
    /// the shares already add up to 100%.
    pub fn step(&mut self, index: usize, up: bool) -> Result<(), String> {
        let total = self.total();
        let Some((token, share)) = self.0.get_mut(index) else {
            return Ok(());
        };
        *share = step(*share, total, up).map_err(|error| format!("{}: {error}", token.name()))?;
        Ok(())
    }

    /// Multiples of 5, at most 100% together, and at least one token mining.
    pub fn validate(&self) -> Result<(), String> {
        if let Some((token, share)) = self.iter().find(|(_, share)| share % STEP != 0) {
            return Err(format!(
                "{}: {share}% is not a multiple of {STEP}%",
                token.name()
            ));
        }
        let sum: u32 = self.iter().map(|(_, share)| u32::from(share)).sum();
        if sum > 100 {
            return Err(format!("the shares add up to {sum}%; at most 100%"));
        }
        if sum == 0 {
            return Err("give at least one token a share of the GPUs".into());
        }
        Ok(())
    }
}

fn total(shares: impl Iterator<Item = u8>) -> u8 {
    shares.map(u32::from).sum::<u32>().min(100) as u8
}

/// One share moved a step up or down, with the shares adding up to `total`.
fn step(share: u8, total: u8, up: bool) -> Result<u8, String> {
    if !up {
        return Ok(share.saturating_sub(STEP));
    }
    if total.saturating_add(STEP) > 100 {
        return Err("the shares already add up to 100%; lower another token first".into());
    }
    Ok(share + STEP)
}

/// A token mining in a split: whether it can use the GPUs now, and the
/// hold that keeps it off them during other tokens' turns.
pub trait Member: Send + Sync {
    fn wants_gpu(&self) -> bool;
    fn hold(&self, held: bool);
}

impl Member for crate::search::SearchHold {
    fn wants_gpu(&self) -> bool {
        crate::search::SearchHold::wants_gpu(self)
    }
    fn hold(&self, held: bool) {
        self.set(held);
    }
}

/// ELECTRIC wants the GPUs while its search runs: the pot can pay a claim
/// and the baton is minable.
pub struct ElectricMember(pub Arc<Monitor>);

impl Member for ElectricMember {
    fn wants_gpu(&self) -> bool {
        self.0.searching.load(Ordering::Acquire) && !self.0.paused.load(Ordering::Relaxed)
    }
    fn hold(&self, held: bool) {
        self.0.held.store(held, Ordering::Release);
    }
}

/// One token of a running split; its share changes live from the screen.
pub struct Slot {
    pub token: SplitToken,
    pub share: AtomicU8,
    pub member: Box<dyn Member>,
}

/// One round's turns: which slot has the GPUs (`None`: they idle) and for
/// how long. Each slot that wants the GPUs mines for its share, plus a
/// part of the shares of slots that cannot mine now, in proportion to its
/// own; the shares' remainder to 100% idles. With no slot able to mine,
/// the whole round idles.
pub fn plan(shares: &[u8], wants: &[bool], period: Duration) -> Vec<(Option<usize>, Duration)> {
    let total = total(shares.iter().copied());
    let wanted: u32 = shares
        .iter()
        .zip(wants)
        .filter(|(_, wants)| **wants)
        .map(|(share, _)| u32::from(*share))
        .sum();
    let mut turns = Vec::new();
    if wanted > 0 {
        for (index, (&share, &wants)) in shares.iter().zip(wants).enumerate() {
            if share > 0 && wants {
                let percent = f64::from(share) * f64::from(total) / f64::from(wanted);
                turns.push((Some(index), period.mul_f64(percent / 100.0)));
            }
        }
    }
    let idle = if wanted == 0 { 100 } else { 100 - total };
    if idle > 0 {
        turns.push((None, period.mul_f64(f64::from(idle) / 100.0)));
    }
    turns
}

/// Hands the GPUs to one slot at a time by `plan`, every `period`, until
/// `stop`; then releases every hold so each token can stop cleanly.
pub fn schedule(slots: &[Slot], stop: &AtomicBool, period: Duration) {
    let mut current: Option<Option<usize>> = None;
    let give = |turn: Option<usize>| {
        for (index, slot) in slots.iter().enumerate() {
            slot.member.hold(Some(index) != turn);
        }
    };
    while !stop.load(Ordering::Relaxed) {
        let shares: Vec<u8> = slots
            .iter()
            .map(|slot| slot.share.load(Ordering::Relaxed))
            .collect();
        let wants: Vec<bool> = slots.iter().map(|slot| slot.member.wants_gpu()).collect();
        let nobody = !shares
            .iter()
            .zip(&wants)
            .any(|(share, wants)| *share > 0 && *wants);
        for (turn, length) in plan(&shares, &wants, period) {
            if current != Some(turn) {
                give(turn);
                current = Some(turn);
            }
            let end = Instant::now() + length;
            while !stop.load(Ordering::Relaxed) {
                let now = Instant::now();
                if now >= end {
                    break;
                }
                // A token that can no longer mine (a winner to claim, an
                // empty pot) ends its turn early; an idle round with nobody
                // able to mine ends as soon as one can.
                let ended = match turn {
                    Some(index) => !slots[index].member.wants_gpu(),
                    None => {
                        nobody
                            && slots.iter().any(|slot| {
                                slot.share.load(Ordering::Relaxed) > 0 && slot.member.wants_gpu()
                            })
                    }
                };
                if ended {
                    break;
                }
                std::thread::sleep((end - now).min(Duration::from_millis(5)));
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
    }
    for slot in slots {
        slot.member.hold(false);
    }
}

/// A running PHOTON-runtime token of the split.
struct Runtime {
    token: SplitToken,
    supervisor: RuntimeSupervisor,
    rewards: u64,
}

/// Mines `shares` on `gpus` until Ctrl+C or `q`: each runtime token with a
/// share on its own supervisor, ELECTRIC in its own session, one screen
/// (or status lines with `plain`), and one GPU lock for all of them.
pub fn run(
    cfg: &RuntimeConfig,
    gpus: &[GpuDevice],
    shares: &Shares,
    config_path: &Path,
    plain: bool,
) -> Result<(), String> {
    shares.validate()?;
    if gpus.is_empty() {
        return Err("no GPU selected for mining".into());
    }
    let _gpu_lock = crate::mining_lock::acquire_gpu_lock()?;
    let electric_devices = crate::backend::electric_engines_for(gpus.to_vec())?;
    let monitor = Arc::new(Monitor::new(
        100,
        plain,
        crate::electric::miner::device_names(&electric_devices),
    ));
    // ELECTRIC's monitor carries the session's stop signal for every token.
    let stop = Arc::clone(&monitor.stop);
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|error| error.to_string())?;
    }
    let mut runtimes = Vec::new();
    let mut slots = Vec::new();
    for (token, share) in shares.iter().filter(|(_, share)| *share > 0) {
        match token {
            SplitToken::Gpu(mining_token) => {
                let mut token_cfg = cfg.clone();
                token_cfg.set_token(mining_token.as_str())?;
                // The split decides how much of the time each token mines.
                token_cfg.set_intensity(100)?;
                let supervisor = RuntimeSupervisor::start_on_gpus(token_cfg, gpus)?;
                let hold = supervisor.hold_handle();
                hold.set(true);
                slots.push(Slot {
                    token,
                    share: AtomicU8::new(share),
                    member: Box::new(hold),
                });
                runtimes.push(Runtime {
                    token,
                    supervisor,
                    rewards: 0,
                });
            }
            SplitToken::Electric => {
                monitor.held.store(true, Ordering::Release);
                slots.push(Slot {
                    token,
                    share: AtomicU8::new(share),
                    member: Box::new(ElectricMember(Arc::clone(&monitor))),
                });
            }
        }
    }
    let electric = shares.get(SplitToken::Electric) > 0;
    let outcome = std::thread::scope(|scope| {
        let electric = electric.then(|| {
            scope.spawn(|| {
                let mined = crate::electric::miner::run_in_split(cfg, gpus, config_path, &monitor);
                if let Err(error) = &mined {
                    // The other tokens keep the GPUs; ELECTRIC no longer asks.
                    monitor.state(format!("Stopped: {error}"));
                    monitor.event(format!("ELECTRIC stopped: {error}"));
                }
                mined
            })
        });
        let scheduler = scope.spawn(|| schedule(&slots, &stop, PERIOD));
        let shown = if plain {
            report(&stop, &mut runtimes, &monitor, &slots)
        } else {
            screen(&stop, &mut runtimes, &monitor, &slots, cfg.network)
        };
        stop.store(true, Ordering::SeqCst);
        let _ = scheduler.join();
        let electric = match electric.map(|worker| worker.join()) {
            Some(Ok(mined)) => mined,
            Some(Err(_)) => Err("ELECTRIC mining thread panicked".into()),
            None => Ok(()),
        };
        shown.and(electric)
    });
    for runtime in runtimes {
        runtime.supervisor.stop();
    }
    outcome
}

/// What one row of the split screen shows.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    token: &'static str,
    share: u8,
    rate: f64,
    rewards: u64,
    state: String,
}

fn runtime_state(supervisor: &RuntimeSupervisor) -> (f64, String) {
    let snapshot = supervisor.snapshot();
    let state = match snapshot.state {
        SupervisorState::Mining => format!("Mining · height {}", snapshot.height),
        SupervisorState::Paused if snapshot.pending_winners > 0 => "Claiming a winner".into(),
        SupervisorState::Paused => "Paused".into(),
        SupervisorState::Reconnecting => "Reconnecting".into(),
        SupervisorState::Error => snapshot
            .last_error
            .map_or_else(|| "Error".into(), |error| format!("Error: {error}")),
        SupervisorState::Stopped => "Stopped".into(),
    };
    (snapshot.search.current_rate, state)
}

/// Counts accepted rewards and keeps the newest events, newest last.
fn take_events(runtimes: &mut [Runtime], events: &mut VecDeque<String>) {
    for runtime in runtimes {
        for event in runtime.supervisor.drain_events() {
            if matches!(
                event,
                RuntimeEvent::SubmissionAccepted { .. } | RuntimeEvent::DirectRewardAccepted { .. }
            ) {
                runtime.rewards += 1;
            }
            events.push_back(format!(
                "{} {}",
                runtime.token.name(),
                crate::tui::event_log_text(&event)
            ));
        }
    }
    while events.len() > 40 {
        events.pop_front();
    }
}

fn rows(runtimes: &[Runtime], electric: &crate::electric::ui::Summary, slots: &[Slot]) -> Vec<Row> {
    slots
        .iter()
        .map(|slot| {
            let share = slot.share.load(Ordering::Relaxed);
            match slot.token {
                SplitToken::Electric => Row {
                    token: slot.token.name(),
                    share,
                    rate: electric.rate,
                    rewards: electric.claims,
                    state: electric.state.clone(),
                },
                token => {
                    let runtime = runtimes.iter().find(|runtime| runtime.token == token);
                    let (rate, state) = runtime
                        .map(|runtime| runtime_state(&runtime.supervisor))
                        .unwrap_or_default();
                    Row {
                        token: token.name(),
                        share,
                        rate,
                        rewards: runtime.map_or(0, |runtime| runtime.rewards),
                        state,
                    }
                }
            }
        })
        .collect()
}

/// Hashes a second with its unit.
fn rate_text(rate: f64) -> String {
    if rate >= 1e9 {
        format!("{:.2} GH/s", rate / 1e9)
    } else {
        format!("{:.0} MH/s", rate / 1e6)
    }
}

/// Without a screen: each token's line every 30 seconds, and PHOTON's
/// events as they come (ELECTRIC prints its own).
fn report(
    stop: &AtomicBool,
    runtimes: &mut [Runtime],
    monitor: &Monitor,
    slots: &[Slot],
) -> Result<(), String> {
    println!(
        "Hash split: {}. Ctrl+C stops.",
        slots
            .iter()
            .map(|slot| format!(
                "{} {}%",
                slot.token.name(),
                slot.share.load(Ordering::Relaxed)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut events = VecDeque::new();
    let mut printed = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        take_events(runtimes, &mut events);
        for event in events.drain(..) {
            println!("{event}");
        }
        if printed.elapsed() >= Duration::from_secs(30) {
            printed = Instant::now();
            let electric = monitor.summary();
            for row in rows(runtimes, &electric, slots) {
                println!(
                    "{} {}% · {} · {} reward(s) · {}",
                    row.token,
                    row.share,
                    rate_text(row.rate),
                    row.rewards,
                    row.state
                );
            }
        }
    }
    Ok(())
}

/// The split's screen until `q`, Esc or Ctrl+C. Up/Down picks a token and
/// Left/Right (or +/-) moves its share by 5%, live.
fn screen(
    stop: &AtomicBool,
    runtimes: &mut [Runtime],
    monitor: &Monitor,
    slots: &[Slot],
    network: MiningNetwork,
) -> Result<(), String> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::{backend::CrosstermBackend, Terminal};
    enable_raw_mode().map_err(|e| e.to_string())?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        }
    }
    let _restore = Restore;
    execute!(std::io::stdout(), EnterAlternateScreen).map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(std::io::stdout())).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut events = VecDeque::new();
    let mut view = View {
        selected: 0,
        show_qr: None,
        status: String::new(),
    };
    while !stop.load(Ordering::Relaxed) {
        take_events(runtimes, &mut events);
        let electric = monitor.summary();
        let rows = rows(runtimes, &electric, slots);
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    &Shown {
                        network,
                        uptime: started.elapsed(),
                        devices: &monitor.devices(),
                        rows: &rows,
                        electric: slots
                            .iter()
                            .any(|slot| slot.token == SplitToken::Electric)
                            .then_some(&electric),
                        events: &events,
                        view: &view,
                    },
                )
            })
            .map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|e| e.to_string())? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        view.status.clear();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => stop.store(true, Ordering::SeqCst),
            KeyCode::Up => view.selected = (view.selected + slots.len() - 1) % slots.len(),
            KeyCode::Down => view.selected = (view.selected + 1) % slots.len(),
            KeyCode::Left | KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('-') => {
                let up = matches!(key.code, KeyCode::Right | KeyCode::Char('+'));
                let total = total(slots.iter().map(|slot| slot.share.load(Ordering::Relaxed)));
                let slot = &slots[view.selected];
                match step(slot.share.load(Ordering::Relaxed), total, up) {
                    Ok(next) => slot.share.store(next, Ordering::Relaxed),
                    Err(error) => view.status = error,
                }
            }
            KeyCode::Char('f') => {
                let funded = electric.pot.is_some_and(|(_, claims)| claims > 0);
                view.show_qr = Some(!view.show_qr.unwrap_or(!funded));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The screen's own state: the picked token, the QR choice (`None`: shown
/// while the pot cannot pay a claim) and a status line.
struct View {
    selected: usize,
    show_qr: Option<bool>,
    status: String,
}

/// Everything one frame draws.
struct Shown<'a> {
    network: MiningNetwork,
    uptime: Duration,
    devices: &'a str,
    rows: &'a [Row],
    electric: Option<&'a crate::electric::ui::Summary>,
    events: &'a VecDeque<String>,
    view: &'a View,
}

fn draw(frame: &mut ratatui::Frame<'_>, state: &Shown<'_>) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Paragraph, Wrap},
    };
    let total: u32 = state.rows.iter().map(|row| u32::from(row.share)).sum();
    let mut lines = vec![
        Line::from(format!("GPU        {}", state.devices)),
        Line::from(format!(
            "Uptime     {}m {:02}s",
            state.uptime.as_secs() / 60,
            state.uptime.as_secs() % 60
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  {:<10} {:>5}  {:>11}  {:>7}  State",
                "Token", "Share", "Rate", "Rewards"
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    for (index, row) in state.rows.iter().enumerate() {
        let marker = if index == state.view.selected {
            '▶'
        } else {
            ' '
        };
        lines.push(Line::from(format!(
            "{marker} {:<10} {:>4}%  {:>11}  {:>7}  {}",
            row.token,
            row.share,
            rate_text(row.rate),
            row.rewards,
            row.state
        )));
    }
    lines.push(Line::from(Span::styled(
        format!(
            "  {:<10} {:>4}%",
            "Idle",
            100u32.saturating_sub(total.min(100))
        ),
        Style::default().fg(Color::DarkGray),
    )));
    let mut qr: &[String] = &[];
    if let Some(electric) = state.electric {
        lines.push(Line::from(""));
        if let Some(address) = &electric.pot_address {
            lines.push(Line::from(format!("Fee pot    {address}")));
        }
        lines.push(Line::from(match electric.pot {
            Some((sats, 0)) => format!(
                "           {sats} sats · pays no claim · the others mine its time until funded"
            ),
            Some((sats, claims)) => format!(
                "           {sats} sats · pays {claims} claim{}",
                if claims == 1 { "" } else { "s" }
            ),
            None => "           reading the pot…".into(),
        }));
        let unfunded = electric.pot.is_none_or(|(_, claims)| claims == 0);
        if state.view.show_qr.unwrap_or(unfunded) {
            qr = &electric.pot_qr;
        }
    }
    if !state.view.status.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            state.view.status.clone(),
            Style::default().fg(Color::Yellow),
        )));
    }
    let top = (lines.len() as u16 + 2).max(if qr.is_empty() {
        0
    } else {
        qr.len() as u16 + 2
    });
    let areas = Layout::vertical([
        Constraint::Length(top),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let title = format!(" Pickaxe · hash split · {} ", state.network.as_str());
    let stats = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(title));
    if qr.is_empty() {
        frame.render_widget(stats, areas[0]);
    } else {
        let width = qr[0].chars().count() as u16 + 2;
        let columns =
            Layout::horizontal([Constraint::Min(40), Constraint::Length(width)]).split(areas[0]);
        frame.render_widget(stats, columns[0]);
        frame.render_widget(
            Paragraph::new(qr.join("\n"))
                .style(Style::default().fg(Color::White).bg(Color::Black))
                .block(Block::bordered().title(" Fund the pot ")),
            columns[1],
        );
    }
    let mut recent: Vec<String> = state.events.iter().cloned().collect();
    if let Some(electric) = state.electric {
        recent.extend(
            electric
                .events
                .iter()
                .map(|event| format!("ELECTRIC {event}")),
        );
    }
    let shown = recent
        .len()
        .saturating_sub(usize::from(areas[1].height.saturating_sub(2)));
    frame.render_widget(
        Paragraph::new(recent[shown..].join("\n"))
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Recent events ")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new("↑/↓ token · ←/→ share ±5% · f fee pot QR · q quit (pending claims saved)"),
        areas[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    #[test]
    fn shares_split_evenly_step_by_five_and_never_exceed_100() {
        let photon = SplitToken::Gpu(MiningToken::Photon);
        let two = Shares::even(&[photon, SplitToken::Electric]);
        assert_eq!(two.0, vec![(photon, 50), (SplitToken::Electric, 50)]);
        let three = Shares::even(&[photon, SplitToken::Electric, photon]);
        assert_eq!(
            three.iter().map(|(_, s)| s).collect::<Vec<_>>(),
            [35, 35, 30]
        );
        let mut shares = two.clone();
        assert!(shares.step(0, true).unwrap_err().contains("100%"));
        shares.step(1, false).unwrap();
        assert_eq!(shares.total(), 95);
        shares.step(0, true).unwrap();
        assert_eq!(
            (shares.get(photon), shares.get(SplitToken::Electric)),
            (55, 45)
        );
        for _ in 0..20 {
            shares.step(1, false).unwrap();
        }
        assert_eq!(shares.get(SplitToken::Electric), 0);
        shares.validate().unwrap();
        for _ in 0..20 {
            shares.step(0, false).unwrap();
        }
        assert!(shares
            .validate()
            .unwrap_err()
            .contains("at least one token"));
        assert!(Shares(vec![(photon, 33)])
            .validate()
            .unwrap_err()
            .contains("multiple of 5"));
        assert!(Shares(vec![(photon, 60), (SplitToken::Electric, 45)])
            .validate()
            .unwrap_err()
            .contains("105%"));
    }

    #[test]
    fn every_gpu_token_is_available_and_shares_parse_by_name() {
        let photon = SplitToken::Gpu(MiningToken::Photon);
        for network in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
            assert_eq!(
                SplitToken::available(network),
                vec![photon, SplitToken::Electric]
            );
        }
        let shares = Shares::parse(
            &["photon=80".into(), "Electric=20%".into()],
            MiningNetwork::Mainnet,
        )
        .unwrap();
        assert_eq!(
            (shares.get(photon), shares.get(SplitToken::Electric)),
            (80, 20)
        );
        let photon_only = Shares::parse(&["PHOTON=100".into()], MiningNetwork::Chipnet).unwrap();
        assert_eq!(photon_only.get(SplitToken::Electric), 0);
        assert!(Shares::parse(
            &["photon=90".into(), "electric=20".into()],
            MiningNetwork::Mainnet
        )
        .unwrap_err()
        .contains("110%"));
        assert!(Shares::parse(&["bch=50".into()], MiningNetwork::Mainnet)
            .unwrap_err()
            .contains("not a GPU token"));
        assert!(Shares::parse(&["photon".into()], MiningNetwork::Mainnet)
            .unwrap_err()
            .contains("TOKEN=PERCENT"));
    }

    fn turn_ms(turns: &[(Option<usize>, Duration)]) -> Vec<(Option<usize>, u128)> {
        turns
            .iter()
            .map(|(turn, length)| (*turn, length.as_millis()))
            .collect()
    }

    #[test]
    fn a_round_follows_the_shares_and_gives_a_waiting_tokens_time_to_the_others() {
        let period = Duration::from_millis(2000);
        assert_eq!(
            turn_ms(&plan(&[80, 20], &[true, true], period)),
            [(Some(0), 1600), (Some(1), 400)]
        );
        // ELECTRIC's pot is empty: PHOTON mines the whole round.
        assert_eq!(
            turn_ms(&plan(&[80, 20], &[true, false], period)),
            [(Some(0), 2000)]
        );
        // The idle part stays idle, whoever waits.
        assert_eq!(
            turn_ms(&plan(&[40, 40], &[true, true], period)),
            [(Some(0), 800), (Some(1), 800), (None, 400)]
        );
        assert_eq!(
            turn_ms(&plan(&[40, 40], &[false, true], period)),
            [(Some(1), 1600), (None, 400)]
        );
        // Nobody can mine: the round idles; a token at 0% never mines.
        assert_eq!(
            turn_ms(&plan(&[80, 20], &[false, false], period)),
            [(None, 2000)]
        );
        assert_eq!(
            turn_ms(&plan(&[100, 0], &[true, true], period)),
            [(Some(0), 2000)]
        );
    }

    /// A member that counts how long it held the GPUs.
    struct Fake {
        wants: AtomicBool,
        held: AtomicBool,
        mined: Mutex<(Duration, Option<Instant>)>,
    }

    impl Fake {
        fn new(wants: bool) -> Arc<Self> {
            Arc::new(Self {
                wants: AtomicBool::new(wants),
                held: AtomicBool::new(true),
                mined: Mutex::new((Duration::ZERO, None)),
            })
        }
        fn mined(&self) -> Duration {
            let (total, since) = *self.mined.lock().unwrap();
            total + since.map_or(Duration::ZERO, |since| since.elapsed())
        }
    }

    impl Member for Arc<Fake> {
        fn wants_gpu(&self) -> bool {
            self.wants.load(Ordering::Relaxed)
        }
        fn hold(&self, held: bool) {
            let mut mined = self.mined.lock().unwrap();
            match (held, mined.1) {
                (true, Some(since)) => *mined = (mined.0 + since.elapsed(), None),
                (false, None) => mined.1 = Some(Instant::now()),
                _ => {}
            }
            self.held.store(held, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_scheduler_gives_each_token_its_share_and_releases_every_hold_at_stop() {
        let (photon, electric) = (Fake::new(true), Fake::new(true));
        let slots = [
            Slot {
                token: SplitToken::Gpu(MiningToken::Photon),
                share: AtomicU8::new(80),
                member: Box::new(Arc::clone(&photon)),
            },
            Slot {
                token: SplitToken::Electric,
                share: AtomicU8::new(20),
                member: Box::new(Arc::clone(&electric)),
            },
        ];
        let stop = AtomicBool::new(false);
        let period = Duration::from_millis(200);
        std::thread::scope(|scope| {
            scope.spawn(|| schedule(&slots, &stop, period));
            std::thread::sleep(period * 5);
            let (p, e) = (photon.mined(), electric.mined());
            let share = p.as_secs_f64() / (p + e).as_secs_f64();
            assert!((0.75..=0.85).contains(&share), "PHOTON had {share:.3}");
            // ELECTRIC's pot runs dry: PHOTON gets its time.
            electric.wants.store(false, Ordering::Relaxed);
            let before = photon.mined();
            std::thread::sleep(period * 3);
            let gained = (photon.mined() - before).as_secs_f64() / (period * 3).as_secs_f64();
            assert!(gained > 0.9, "PHOTON had {gained:.3} with ELECTRIC waiting");
            stop.store(true, Ordering::Relaxed);
        });
        assert!(!photon.held.load(Ordering::Relaxed) && !electric.held.load(Ordering::Relaxed));
    }

    fn screen_text(state: &Shown<'_>) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn the_screen_shows_each_tokens_share_rate_and_an_unfunded_pot_with_its_qr() {
        let rows = [
            Row {
                token: "PHOTON",
                share: 80,
                rate: 1.35e9,
                rewards: 2,
                state: "Mining · height 973600".into(),
            },
            Row {
                token: "ELECTRIC",
                share: 20,
                rate: 0.0,
                rewards: 0,
                state: "Waiting for BCH in the fee pot".into(),
            },
        ];
        let address = "bitcoincash:pdemoaddressforthesplitscreenonlyqqqqqqqqqqqqqqqqqqqqqqqq";
        let mut electric = crate::electric::ui::Summary {
            pot_address: Some(address.into()),
            pot_qr: crate::electric::ui::qr_lines(address).unwrap(),
            pot: Some((500, 0)),
            events: vec!["Connected: wss://example".into()],
            ..Default::default()
        };
        let mut events = VecDeque::new();
        events.push_back("PHOTON job g=1 h=973600 ab:0".to_string());
        let view = View {
            selected: 1,
            show_qr: None,
            status: String::new(),
        };
        let text = |electric: &crate::electric::ui::Summary| {
            screen_text(&Shown {
                network: MiningNetwork::Mainnet,
                uptime: Duration::from_secs(75),
                devices: "RTX 5070 Ti (cuda)",
                rows: &rows,
                electric: Some(electric),
                events: &events,
                view: &view,
            })
        };
        let unfunded = text(&electric);
        for expected in [
            " Pickaxe · hash split · mainnet ".to_string(),
            format!(
                "  {:<10} {:>4}%  {:>11}  {:>7}  Mining · height 973600",
                "PHOTON", 80, "1.35 GH/s", 2
            ),
            format!("▶ {:<10} {:>4}%  {:>11}", "ELECTRIC", 20, "0 MH/s"),
            format!("  {:<10} {:>4}%", "Idle", 0),
            "500 sats · pays no claim · the others mine its time until funded".into(),
            " Fund the pot ".into(),
            "PHOTON job g=1".into(),
            "ELECTRIC Connected: wss://example".into(),
            "←/→ share ±5%".into(),
        ] {
            assert!(unfunded.contains(&expected), "{expected}: {unfunded}");
        }
        electric.pot = Some((717_383, 380));
        let funded = text(&electric);
        assert!(funded.contains("717383 sats · pays 380 claims"), "{funded}");
        assert!(!funded.contains(" Fund the pot "));
    }
}
