//! A mining session as the window sees it: the worker that runs it and the
//! view that reads and steers it in the terminal dashboard's place.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Pause(bool),
    Intensity(u8),
    /// Moves the split's share at this place one 5% step.
    Share {
        index: usize,
        up: bool,
    },
    /// Moves the donation of the token at this place one step.
    Donation {
        index: usize,
        up: bool,
    },
    /// Reconnects the token at this place (runtime tokens).
    Reconnect {
        index: usize,
    },
    /// PHOTON alone, as its terminal dashboard allows: a new payout, or a
    /// Fulcrum server (`None`: the automatic servers again).
    SetPayout(String),
    SetFulcrum(Option<String>),
    /// Stops once pending rewards are settled (pausing the GPUs, as the
    /// terminal's B does), or `now` (as its q does: nothing is saved).
    Stop {
        now: bool,
    },
    /// Takes back a Stop that waits for pending rewards (the terminal's Esc).
    KeepMining,
    /// ELECTRIC's Max/send from an owner-controlled fee pot.
    Withdraw(Withdraw),
}

/// A step of ELECTRIC's Max/send, as the terminal's pot view keys do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Withdraw {
    /// M: start entering a destination.
    Edit,
    /// The destination as typed or pasted, then Enter: prepare a quote.
    Prepare(String),
    /// The quote as it was drawn; only a drawn quote can be sent.
    Presented(crate::electric::withdrawal::View),
    /// Y: send the drawn quote.
    Confirm,
    /// Esc: cancel (not while sending).
    Cancel,
}

/// The terminal token dashboard's answer when a session with a split or
/// ELECTRIC is asked to change its payout or servers.
pub const SETUP_ONLY: &str = "Stop and use setup to change this session's address or servers; \
                              its fee pot and pending claims remain bound to their recipients.";

/// One token of the session, as its row shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenLine {
    pub name: String,
    pub share: Option<u8>,
    pub rate: f64,
    pub rewards: u64,
    pub state: String,
    /// The donation in effect, once the token reports it.
    pub donation: Option<crate::donation::TokenDonation>,
    /// A runtime token, which reconnects on request.
    pub runtime: bool,
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
    /// PHOTON alone: its payout and servers can change while it mines.
    pub single_runtime: bool,
    /// The command each rig runs to join this coordinator, by place.
    pub rig_join: Vec<(String, String)>,
    /// The answer to the last control, as the terminal's status line, and
    /// when it was given.
    pub notice: Option<String>,
    pub notice_at: Option<Instant>,
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
        // Whether mining was paused before a Stop paused it.
        let mut paused_before_stop = false;
        let mut settled_since: Option<Instant> = None;
        let mut last_sample = Instant::now();
        let mut last_log: Option<Instant> = None;
        self.update(|board| {
            board.live = true;
            board.network = Some(miners.network);
            board.started = Some(Instant::now());
            board.push_event("Mining session started".into());
        });
        let mut rig_join: Option<Vec<(String, String)>> = None;
        while !finished() {
            let mut events = Vec::new();
            let mut notice = None;
            for control in self.take() {
                let done = match control {
                    Control::Pause(pause) => miners.pause(pause).map(|()| {
                        paused = pause;
                        None
                    }),
                    Control::Intensity(value) => miners
                        .intensity(value)
                        .map(|()| Some(format!("Intensity {value}%"))),
                    Control::Share { index, up } => miners.split.map_or(Ok(None), |(slots, _)| {
                        crate::hash_split::step_share(slots, index, up).map(|share| {
                            slots
                                .get(index)
                                .map(|slot| format!("{} share {share}%", slot.token.name()))
                        })
                    }),
                    Control::Donation { index, up } => donate(miners, index, up),
                    Control::Reconnect { index } => reconnect(miners, index),
                    Control::SetPayout(address) => lone_runtime(miners).and_then(|runtime| {
                        runtime
                            .set_payout(address)
                            .map(|()| Some("Payout address updated".into()))
                    }),
                    Control::SetFulcrum(Some(url)) => lone_runtime(miners).and_then(|runtime| {
                        runtime
                            .set_fulcrum_endpoint(url)
                            .map(|()| Some("Fulcrum endpoint updated; reconnecting".into()))
                    }),
                    Control::SetFulcrum(None) => lone_runtime(miners).and_then(|runtime| {
                        runtime
                            .clear_fulcrum_endpoint()
                            .map(|()| Some("Custom Fulcrum endpoint cleared; reconnecting".into()))
                    }),
                    Control::Stop { now: true } => return Ok(()),
                    Control::Stop { now: false } if stopping => Ok(None),
                    Control::Stop { now: false } => {
                        let all_zero = miners.split.is_some_and(|(slots, _)| {
                            slots
                                .iter()
                                .all(|slot| slot.share.load(Ordering::Relaxed) == 0)
                        });
                        if all_zero {
                            Err("Give a token a nonzero share before saving its profile; \
                                 Pause pauses mining."
                                .into())
                        } else {
                            // As the terminal's B: pause, settle, then save.
                            paused_before_stop = paused;
                            miners.pause(true).map(|()| {
                                paused = true;
                                stopping = true;
                                Some("Finishing pending rewards before stopping.".into())
                            })
                        }
                    }
                    Control::KeepMining if stopping => {
                        stopping = false;
                        settled_since = None;
                        miners.pause(paused_before_stop).map(|()| {
                            paused = paused_before_stop;
                            Some("Mining continues.".into())
                        })
                    }
                    Control::KeepMining => Ok(None),
                    Control::Withdraw(step) => withdraw(miners, step),
                };
                match done {
                    Ok(Some(message)) => {
                        events.push(message.clone());
                        notice = Some(message);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        events.push(error.clone());
                        notice = Some(error);
                    }
                }
            }
            let reading = read(miners, &mut rewards, sampler.as_ref(), &mut events);
            if stopping && !reading.pending {
                let since = *settled_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= SETTLED {
                    // The session saves what changed, as on leaving the
                    // terminal dashboard for profiles.
                    crate::session_control::request_profiles();
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
            // Where rigs join this coordinator: its listen address and key
            // never change while it runs.
            if rig_join.is_none() {
                if let Some(rigs) = reading.runtime.as_ref().and_then(|s| s.rigs.as_ref()) {
                    rig_join = Some(
                        crate::rigs::join_lines(
                            rigs,
                            miners.network,
                            crate::reach::Interfaces::detect(),
                        )
                        .into_iter()
                        .map(|(place, command)| (place.label().to_owned(), command))
                        .collect(),
                    );
                }
            }
            self.update(|board| {
                reading.apply(board, paused, stopping);
                board.single_runtime = lone_runtime(miners).is_ok();
                if let Some(lines) = &rig_join {
                    board.rig_join.clone_from(lines);
                }
                if notice.is_some() {
                    board.notice = notice;
                    board.notice_at = Some(Instant::now());
                }
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
    let order = token_order(miners);
    let tokens = order
        .iter()
        .map(|token| {
            let share = miners
                .split
                .and_then(|(slots, _)| slots.iter().find(|slot| slot.token == *token))
                .map(|slot| slot.share.load(Ordering::Relaxed));
            let runtime = miners.runtimes.iter().position(|(t, _)| t == token);
            let (rate, rewards, state, donation) = match runtime {
                Some(i) => (
                    snapshots[i].search.current_rate,
                    rewards[i],
                    crate::hash_split::runtime_state(&snapshots[i]),
                    Some(snapshots[i].token_donation),
                ),
                None => electric
                    .as_ref()
                    .map_or((0.0, 0, "Stopped".into(), None), |e| {
                        (e.rate, e.claims, e.state.clone(), e.donation)
                    }),
            };
            TokenLine {
                name: token.name().into(),
                share,
                rate,
                rewards,
                state,
                donation,
                runtime: runtime.is_some(),
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

/// The session's tokens in the order its rows show them: the split's, or
/// the runtime tokens and then ELECTRIC.
fn token_order(miners: &Miners<'_>) -> Vec<SplitToken> {
    match miners.split {
        Some((slots, _)) => slots.iter().map(|slot| slot.token).collect(),
        None => miners
            .runtimes
            .iter()
            .map(|(token, _)| *token)
            .chain(miners.electric.map(|_| SplitToken::Electric))
            .collect(),
    }
}

/// PHOTON alone, whose payout and servers can change while it mines (as on
/// its terminal dashboard); with a split or ELECTRIC they are set up again.
fn lone_runtime<'a>(miners: &Miners<'a>) -> Result<&'a RuntimeSupervisor, String> {
    match (miners.runtimes.as_slice(), miners.split, miners.electric) {
        ([(_, runtime)], None, None) => Ok(*runtime),
        _ => Err(SETUP_ONLY.into()),
    }
}

/// Moves a token's donation one step, as the terminal token dashboard's
/// Left/Right do.
fn donate(miners: &Miners<'_>, index: usize, up: bool) -> Result<Option<String>, String> {
    let Some(token) = token_order(miners).get(index).copied() else {
        return Ok(None);
    };
    if let Some((_, runtime)) = miners.runtimes.iter().find(|(t, _)| *t == token) {
        let next = runtime.snapshot().token_donation.adjusted(up);
        return runtime
            .set_donation(next)
            .map(|()| Some(format!("Donation {next}")));
    }
    let Some(monitor) = miners.electric.filter(|_| token == SplitToken::Electric) else {
        return Ok(None);
    };
    let next = monitor
        .summary()
        .donation
        .unwrap_or(crate::electric::DONATION_DEFAULT)
        .adjusted(up);
    monitor
        .request_donation(miners.network, next)
        .map(|()| Some(format!("ELECTRIC donation {next}")))
}

/// One Max/send step on ELECTRIC's withdrawal control, as the terminal's
/// pot view keys drive it; the control keeps its own rules (an owner pot
/// only, a quote sent only after it was drawn, no cancel while sending).
fn withdraw(miners: &Miners<'_>, step: Withdraw) -> Result<Option<String>, String> {
    let Some(monitor) = miners.electric else {
        return Ok(Some("This session has no ELECTRIC fee pot.".into()));
    };
    let mut send = monitor.withdrawal.lock().unwrap_or_else(|e| e.into_inner());
    match step {
        Withdraw::Edit => send.edit(),
        Withdraw::Prepare(destination) => {
            if send.view().phase == crate::electric::withdrawal::Phase::Editing {
                while !send.view().destination.is_empty() {
                    send.backspace();
                }
                send.paste(&destination);
                send.enter();
            }
        }
        Withdraw::Presented(view) => send.presented(&view),
        Withdraw::Confirm => send.confirm(),
        Withdraw::Cancel => send.cancel(),
    }
    Ok(None)
}

/// Reconnects a runtime token, as the terminal token dashboard's `r` does.
fn reconnect(miners: &Miners<'_>, index: usize) -> Result<Option<String>, String> {
    let Some(token) = token_order(miners).get(index).copied() else {
        return Ok(None);
    };
    match miners.runtimes.iter().find(|(t, _)| *t == token) {
        Some((_, runtime)) => runtime
            .reconnect()
            .map(|()| Some(format!("{} reconnect requested.", token.name()))),
        None => Ok(Some(
            "ELECTRIC reconnects automatically on transport errors.".into(),
        )),
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

/// A runtime token (PHOTON) alone, as `pickaxe` runs it after its setup
/// (`run_headless_mining` in src/main.rs): with this miner's rigs when it
/// coordinates them, and the session saved to `profile` when it stops.
pub(super) fn mine_runtime_token(
    cfg: &crate::config::RuntimeConfig,
    token: crate::config::MiningToken,
    gpus: &[GpuDevice],
    rigs: Option<crate::rigs::RigHub>,
    config_path: &Path,
    profile: Option<&str>,
) -> Result<(), String> {
    cfg.ensure_mining_supported()?;
    // A coordinator with no GPU of its own uses none, so takes no lock.
    let _gpu_lock = if gpus.is_empty() && rigs.is_some() {
        None
    } else {
        Some(crate::mining_lock::acquire_gpu_lock()?)
    };
    let supervisor = RuntimeSupervisor::start_on_gpus_with_rigs(cfg.clone(), gpus, rigs)?;
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
        gpus,
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
        crate::tui::remember_token_session(&saved, gpus, config_path, Vec::new(), false);
    }
    if let Some(name) = profile {
        // As every stop of a profile's PHOTON session saves it; the
        // token's minimum is not saved, so a later minimum applies.
        crate::config::MiningProfiles::save_session(
            &crate::config::profiles_path(config_path),
            name,
            &crate::config::SessionSettings {
                intensity: last.search.intensity,
                address: last.payout_address.clone(),
                token_donation: (last.token_donation != last.donation_default)
                    .then_some(last.token_donation),
            },
        )
        .map_err(|error| format!("save mining profile: {error}"))?;
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
            donation: None,
            runtime: true,
        }
    }

    #[test]
    fn max_send_steps_drive_the_withdrawal_as_the_pot_keys_do() {
        use crate::electric::withdrawal::Phase;
        let monitor = crate::electric::ui::Monitor::new(100, false, "Preview GPU".into());
        let miners = Miners {
            network: MiningNetwork::Mainnet,
            runtimes: vec![],
            electric: Some(&monitor),
            split: None,
            stop: None,
        };
        let phase = || monitor.withdrawal.lock().unwrap().view().phase;
        // Only an owner-controlled pot sends.
        withdraw(&miners, Withdraw::Edit).unwrap();
        assert_eq!(phase(), Phase::Idle);
        monitor.withdrawal.lock().unwrap().enabled = true;
        withdraw(&miners, Withdraw::Edit).unwrap();
        assert_eq!(phase(), Phase::Editing);
        // A destination with extra text is refused, as a paste is.
        withdraw(&miners, Withdraw::Prepare("pay me bitcoincash:q".into())).unwrap();
        assert_eq!(phase(), Phase::Error);
        withdraw(&miners, Withdraw::Edit).unwrap();
        withdraw(
            &miners,
            Withdraw::Prepare(crate::config::DONATION_ADDRESS.into()),
        )
        .unwrap();
        let view = monitor.withdrawal.lock().unwrap().view();
        assert_eq!(view.phase, Phase::Preparing);
        assert_eq!(view.destination, crate::config::DONATION_ADDRESS);
        // No quote was drawn, so nothing is sent.
        withdraw(&miners, Withdraw::Confirm).unwrap();
        assert_eq!(phase(), Phase::Preparing);
        withdraw(&miners, Withdraw::Cancel).unwrap();
        assert_eq!(phase(), Phase::Idle);
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
