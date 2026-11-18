//! A mining session as the window sees it: the worker that runs it and the
//! view that reads and steers it in the terminal dashboard's place.
use super::setup::{Launch, Plan};
use crate::backend::GpuDevice;
use crate::config::MiningNetwork;
use crate::electric::ui::Summary;
use crate::hash_split::SplitToken;
use crate::runtime::{RuntimeEvent, RuntimeSnapshot, RuntimeSupervisor};
use crate::telemetry::{GpuTelemetry, LiveTelemetrySampler};
use crate::tui::Miners;
use eframe::egui;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often the view reads the session, as the terminal dashboard does.
const READ_INTERVAL: Duration = Duration::from_millis(200);
/// Spacing of the hash-rate chart's samples.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// Ten minutes of chart history.
pub const HISTORY_CAP: usize = 300;
const EVENT_CAP: usize = 200;
/// A stop waits this long with nothing pending, as leaving for profiles does.
const SETTLED: Duration = Duration::from_millis(500);
/// Status lines in the opt-in log, as often as the terminal dashboard's.
const LOG_INTERVAL: Duration = Duration::from_secs(10);

/// What the window asks of a running session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause(bool),
    Intensity(u8),
    /// Moves the split's share at this place one 5% step.
    Share {
        index: usize,
        up: bool,
    },
    /// Stops once pending rewards are settled, or `now`.
    Stop {
        now: bool,
    },
}

/// One token of the session, as its row shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenLine {
    pub name: String,
    pub share: Option<u8>,
    pub rate: f64,
    pub rewards: u64,
    pub state: String,
}

/// One mining GPU and what it reports.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuLine {
    pub name: String,
    pub rate: Option<f64>,
    pub telemetry: GpuTelemetry,
}

/// Everything the dashboard draws, read from the session.
#[derive(Debug, Clone, Default)]
pub struct Board {
    /// A session is showing.
    pub live: bool,
    pub network: Option<MiningNetwork>,
    pub tokens: Vec<TokenLine>,
    pub gpus: Vec<GpuLine>,
    /// The GPUs together (ELECTRIC alone reports only this).
    pub telemetry: GpuTelemetry,
    /// The first runtime token's state: height, baton, source.
    pub runtime: Option<RuntimeSnapshot>,
    pub electric: Option<Summary>,
    pub intensity: u8,
    pub paused: bool,
    pub pending: bool,
    pub stopping: bool,
    /// The hash split's schedule, as the scheduler describes it.
    pub layout: String,
    /// Each token's rate, oldest first, one sample every `SAMPLE_INTERVAL`.
    pub history: Vec<(String, VecDeque<f64>)>,
    pub events: VecDeque<String>,
    pub started: Option<Instant>,
    /// Why the last session stopped, if it failed.
    pub error: Option<String>,
}

impl Board {
    fn push_event(&mut self, event: String) {
        if self.events.len() == EVENT_CAP {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn sample(&mut self) {
        for line in &self.tokens {
            let at = match self.history.iter().position(|(name, _)| *name == line.name) {
                Some(at) => at,
                None => {
                    self.history.push((line.name.clone(), VecDeque::new()));
                    self.history.len() - 1
                }
            };
            let samples = &mut self.history[at].1;
            if samples.len() == HISTORY_CAP {
                samples.pop_front();
            }
            samples.push_back(line.rate);
        }
    }
}

/// What the window and the session's view share.
#[derive(Default)]
pub struct Link {
    board: Mutex<Board>,
    controls: Mutex<Vec<Control>>,
    context: OnceLock<egui::Context>,
}

impl Link {
    pub fn set_context(&self, context: egui::Context) {
        let _ = self.context.set(context);
    }

    pub fn board(&self) -> Board {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn send(&self, control: Control) {
        self.controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(control);
    }

    /// A new session starts with an empty board and no old requests.
    pub fn reset(&self) {
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = Board::default();
        self.controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    pub fn finished(&self, error: Option<String>) {
        self.update(|board| {
            board.live = false;
            board.error = error;
        });
    }

    fn take(&self) -> Vec<Control> {
        std::mem::take(&mut *self.controls.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn update(&self, change: impl FnOnce(&mut Board)) {
        change(&mut self.board.lock().unwrap_or_else(|e| e.into_inner()));
        if let Some(context) = self.context.get() {
            context.request_repaint();
        }
    }

    /// Reads and steers `miners` until `finished`, or the window stops it.
    fn follow(
        &self,
        miners: &Miners<'_>,
        gpus: &[GpuDevice],
        finished: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        // Runtime tokens sample the same GPUs; ELECTRIC alone needs a sampler.
        let sampler = miners
            .runtimes
            .is_empty()
            .then(|| LiveTelemetrySampler::start(crate::telemetry::telemetry_sources(gpus)));
        let mut rewards = vec![0u64; miners.runtimes.len()];
        let mut paused = false;
        let mut stopping = false;
        let mut settled_since: Option<Instant> = None;
        let mut last_sample = Instant::now();
        let mut last_log: Option<Instant> = None;
        self.update(|board| {
            board.live = true;
            board.network = Some(miners.network);
            board.started = Some(Instant::now());
            board.push_event("Mining session started".into());
        });
        while !finished() {
            let mut events = Vec::new();
            for control in self.take() {
                let done = match control {
                    Control::Pause(pause) => miners.pause(pause).map(|()| paused = pause),
                    Control::Intensity(value) => miners.intensity(value),
                    Control::Share { index, up } => miners.split.map_or(Ok(()), |(slots, _)| {
                        crate::hash_split::step_share(slots, index, up).map(|_| ())
                    }),
                    Control::Stop { now: true } => return Ok(()),
                    Control::Stop { now: false } => {
                        // The session saves what changed, as on leaving
                        // the terminal dashboard for profiles.
                        crate::session_control::request_profiles();
                        stopping = true;
                        Ok(())
                    }
                };
                if let Err(error) = done {
                    events.push(error);
                }
            }
            let reading = read(miners, &mut rewards, sampler.as_ref(), &mut events);
            if stopping && !reading.pending {
                let since = *settled_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= SETTLED {
                    break;
                }
            } else {
                settled_since = None;
            }
            let sample = last_sample.elapsed() >= SAMPLE_INTERVAL;
            if sample {
                last_sample = Instant::now();
            }
            // The same opt-in log as the terminal dashboard (PICKAXE_TUI_LOG).
            for event in &events {
                crate::tui::append_tui_log(&format!("event {event}"));
            }
            if last_log.is_none_or(|logged| logged.elapsed() >= LOG_INTERVAL) {
                if let Some(snapshot) = &reading.runtime {
                    crate::tui::append_tui_log(&crate::tui::tui_status_line(snapshot));
                }
                for token in &reading.tokens {
                    crate::tui::append_tui_log(&format!(
                        "token {} rate {:.0} rewards {} state {}",
                        token.name, token.rate, token.rewards, token.state
                    ));
                }
                last_log = Some(Instant::now());
            }
            self.update(|board| {
                reading.apply(board, paused, stopping);
                for event in events {
                    board.push_event(event);
                }
                if sample {
                    board.sample();
                }
            });
            std::thread::sleep(READ_INTERVAL);
        }
        Ok(())
    }
}

/// One read of the session, as the terminal dashboard reads it.
struct Reading {
    tokens: Vec<TokenLine>,
    gpus: Vec<GpuLine>,
    telemetry: GpuTelemetry,
    runtime: Option<RuntimeSnapshot>,
    electric: Option<Summary>,
    intensity: u8,
    pending: bool,
    layout: String,
}

impl Reading {
    fn apply(self, board: &mut Board, paused: bool, stopping: bool) {
        board.paused = self
            .electric
            .as_ref()
            .filter(|_| self.runtime.is_none())
            .map_or(paused, |electric| electric.paused);
        board.tokens = self.tokens;
        board.gpus = self.gpus;
        board.telemetry = self.telemetry;
        board.runtime = self.runtime;
        board.electric = self.electric;
        board.intensity = self.intensity;
        board.pending = self.pending;
        board.stopping = stopping;
        board.layout = self.layout;
    }
}

fn read(
    miners: &Miners<'_>,
    rewards: &mut [u64],
    sampler: Option<&LiveTelemetrySampler>,
    events: &mut Vec<String>,
) -> Reading {
    let snapshots: Vec<_> = miners.runtimes.iter().map(|(_, r)| r.snapshot()).collect();
    for (i, (token, runtime)) in miners.runtimes.iter().enumerate() {
        for event in runtime.drain_events() {
            if matches!(
                event,
                RuntimeEvent::SubmissionAccepted { .. } | RuntimeEvent::DirectRewardAccepted { .. }
            ) {
                rewards[i] += 1;
            }
            events.push(crate::tui::label_event(
                token.name(),
                &crate::tui::event_log_text(&event),
            ));
        }
    }
    if let Some(electric) = miners.electric {
        for event in electric.drain_events() {
            events.push(crate::tui::label_event("ELECTRIC", &event));
        }
    }
    let electric = miners.electric.map(crate::electric::ui::Monitor::summary);
    let pending = snapshots.iter().any(|s| s.pending_winners > 0)
        || electric.as_ref().is_some_and(|e| {
            e.pending || e.withdrawal.phase == crate::electric::withdrawal::Phase::Sending
        });
    let order: Vec<SplitToken> = match miners.split {
        Some((slots, _)) => slots.iter().map(|slot| slot.token).collect(),
        None => miners
            .runtimes
            .iter()
            .map(|(token, _)| *token)
            .chain(miners.electric.map(|_| SplitToken::Electric))
            .collect(),
    };
    let tokens = order
        .iter()
        .map(|token| {
            let share = miners
                .split
                .and_then(|(slots, _)| slots.iter().find(|slot| slot.token == *token))
                .map(|slot| slot.share.load(Ordering::Relaxed));
            let (rate, rewards, state) = match miners.runtimes.iter().position(|(t, _)| t == token)
            {
                Some(i) => (
                    snapshots[i].search.current_rate,
                    rewards[i],
                    crate::hash_split::runtime_state(&snapshots[i]),
                ),
                None => electric.as_ref().map_or((0.0, 0, "Stopped".into()), |e| {
                    (e.rate, e.claims, e.state.clone())
                }),
            };
            TokenLine {
                name: token.name().into(),
                share,
                rate,
                rewards,
                state,
            }
        })
        .collect();
    let runtime = snapshots.first().cloned();
    let gpus = runtime.as_ref().map_or_else(Vec::new, |snapshot| {
        snapshot
            .gpus
            .iter()
            .enumerate()
            .map(|(i, gpu)| GpuLine {
                name: gpu.name.clone(),
                rate: snapshot.search.gpus.get(i).map(|stats| stats.active_rate),
                telemetry: gpu.telemetry.clone(),
            })
            .collect()
    });
    let telemetry = runtime.as_ref().map_or_else(
        || sampler.map_or_else(GpuTelemetry::default, LiveTelemetrySampler::snapshot),
        |snapshot| snapshot.gpu_telemetry.clone(),
    );
    let intensity = runtime.as_ref().map_or_else(
        || electric.as_ref().map_or(100, |e| e.intensity),
        |snapshot| snapshot.search.intensity,
    );
    Reading {
        tokens,
        gpus,
        telemetry,
        runtime,
        electric,
        intensity,
        pending,
        layout: miners.split.map_or_else(String::new, |(_, layout)| {
            layout.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }),
    }
}

/// Shows token sessions in the window (see `tui::SessionView`).
pub struct WindowView(pub Arc<Link>);

impl crate::tui::SessionView for WindowView {
    fn show(
        &self,
        miners: &Miners<'_>,
        gpus: &[GpuDevice],
        finished: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        self.0.follow(miners, gpus, finished)
    }
}

/// Runs the session the setup chose until it stops, on this thread: the
/// same session functions the terminal setup starts.
pub fn mine(launch: &Launch, config_path: &Path) -> Result<(), String> {
    let mined = match &launch.plan {
        Plan::Split(shares) => {
            crate::hash_split::run(&launch.cfg, &launch.gpus, shares, config_path, false)
        }
        Plan::Electric => {
            crate::electric::miner::run_from_setup(&launch.cfg, &launch.gpus, config_path)
        }
        Plan::Runtime(token) => mine_runtime_token(launch, *token, config_path),
    };
    // The stop asked the session to save its settings; nothing else
    // should read that request later.
    crate::session_control::take_profiles_request();
    mined
}

/// A runtime token (PHOTON) alone, as the terminal's mining dashboard runs it.
fn mine_runtime_token(
    launch: &Launch,
    token: crate::config::MiningToken,
    config_path: &Path,
) -> Result<(), String> {
    let cfg = &launch.cfg;
    cfg.ensure_mining_supported()?;
    let _gpu_lock = crate::mining_lock::acquire_gpu_lock()?;
    let supervisor = RuntimeSupervisor::start_on_gpus(cfg.clone(), &launch.gpus)?;
    let stop = Arc::new(AtomicBool::new(false));
    crate::session_control::install_stop(&stop)?;
    let shown = crate::tui::run_miners(
        &Miners {
            network: cfg.network,
            runtimes: vec![(SplitToken::Gpu(token), &supervisor)],
            electric: None,
            split: None,
            stop: Some(&stop),
        },
        &launch.gpus,
        &|| stop.load(Ordering::Relaxed),
    );
    let last = supervisor.stop();
    if crate::session_control::profiles_requested() {
        let mut saved = cfg.clone();
        saved.payout_address = last.payout_address.clone();
        saved.intensity = last.search.intensity;
        saved
            .token_donations
            .insert(saved.token.as_str().into(), last.token_donation);
        crate::tui::remember_token_session(&saved, &launch.gpus, config_path, Vec::new(), false);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(name: &str, rate: f64) -> TokenLine {
        TokenLine {
            name: name.into(),
            share: None,
            rate,
            rewards: 0,
            state: String::new(),
        }
    }

    #[test]
    fn the_chart_keeps_each_tokens_newest_samples() {
        let mut board = Board {
            tokens: vec![line("PHOTON", 1.0), line("ELECTRIC", 2.0)],
            ..Board::default()
        };
        for _ in 0..HISTORY_CAP + 5 {
            board.sample();
        }
        board.tokens[0].rate = 7.0;
        board.sample();
        assert_eq!(board.history.len(), 2);
        let photon = &board.history[0].1;
        assert_eq!(photon.len(), HISTORY_CAP);
        assert_eq!(photon.back(), Some(&7.0));
        assert_eq!(board.history[1].1.back(), Some(&2.0));
    }

    #[test]
    fn the_event_log_keeps_the_newest_events() {
        let mut board = Board::default();
        for i in 0..EVENT_CAP + 3 {
            board.push_event(i.to_string());
        }
        assert_eq!(board.events.len(), EVENT_CAP);
        assert_eq!(board.events.front().map(String::as_str), Some("3"));
    }

    #[test]
    fn a_new_session_starts_from_an_empty_board() {
        let link = Link::default();
        link.update(|board| board.push_event("old".into()));
        link.send(Control::Pause(true));
        link.reset();
        assert!(link.board().events.is_empty());
        assert!(link.take().is_empty());
        link.finished(Some("no GPU".into()));
        let board = link.board();
        assert!(!board.live);
        assert_eq!(board.error.as_deref(), Some("no GPU"));
    }
}
