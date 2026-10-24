//! #### PR #53: the token hash split. One session mines several GPU tokens
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

/// A token mining in a split: whether it can use the GPUs now, the hold
/// that keeps it off a GPU while another token has that GPU, and (#### PR
/// #61) each GPU's hashes a second while it mines, to weigh the GPUs.
pub trait Member: Send + Sync {
    fn wants_gpu(&self) -> bool;
    /// Holds or releases the GPU at `gpu`, in the order the GPUs were chosen.
    fn hold(&self, gpu: usize, held: bool);
    /// The first `gpus` GPUs' hashes a second while this token mines them;
    /// 0 for a GPU not measured yet.
    fn gpu_rates(&self, gpus: usize) -> Vec<f64>;
}

/// A token the PHOTON runtime mines: its search's hold and its GPUs' rates.
pub struct RuntimeMember {
    pub hold: crate::search::SearchHold,
    pub rates: crate::runtime::GpuRates,
}

impl Member for RuntimeMember {
    fn wants_gpu(&self) -> bool {
        self.hold.wants_gpu()
    }
    fn hold(&self, gpu: usize, held: bool) {
        self.hold.set_gpu(gpu, held);
    }
    fn gpu_rates(&self, gpus: usize) -> Vec<f64> {
        let mut rates = self.rates.rates();
        rates.resize(gpus, 0.0);
        rates
    }
}

/// ELECTRIC wants the GPUs while its search runs: the pot can pay a claim
/// and the baton is minable.
pub struct ElectricMember(pub Arc<Monitor>);

impl Member for ElectricMember {
    fn wants_gpu(&self) -> bool {
        self.0.searching.load(Ordering::Acquire) && !self.0.paused.load(Ordering::Relaxed)
    }
    fn hold(&self, gpu: usize, held: bool) {
        self.0.hold_gpu(gpu, held);
    }
    fn gpu_rates(&self, gpus: usize) -> Vec<f64> {
        self.0.gpu_rates(gpus)
    }
}

/// One token of a running split; its share changes live from the screen.
pub struct Slot {
    pub token: SplitToken,
    pub share: AtomicU8,
    pub member: Box<dyn Member>,
}

/// Parts of a whole closer than this count as equal.
const EPSILON: f64 = 1e-9;

/// #### PR #61
/// What: one round of the split on the GPUs. Whole GPUs go to the tokens
/// first: each GPU, the fastest first, to whoever needs the most of the
/// rig's speed without the GPU being more than that (the idle rest of 100%
/// counts too). Only the GPUs left over take turns, each token for what it
/// still needs. A token that cannot mine now gives its share to the others
/// in proportion to theirs; with nobody able to mine, the round idles.
/// `weights` are the GPUs' speeds (any scale; equal for one GPU model).
/// Why: on five GPUs at 80/20 every GPU took turns between the tokens; now
/// four mine PHOTON and one ELECTRIC all round, and one GPU still takes
/// turns as before.
/// Look here if: a rig's GPUs keep switching tokens although the shares
/// divide into whole GPUs, or a token mines more than its share.
#[derive(Debug, Clone, PartialEq)]
pub struct Round {
    /// Each GPU's token for the whole round (`Some(None)`: it idles), or
    /// `None` for a GPU that takes turns.
    pub gpus: Vec<Option<Option<usize>>>,
    /// The turns on the GPUs that take turns, in round order.
    pub turns: Vec<(Option<usize>, Duration)>,
}

/// What each slot with a share that wants the GPUs, and the idle rest
/// (`None`), needs of the rig's speed, in parts of 1.
fn needs(shares: &[u8], wants: &[bool]) -> Vec<(Option<usize>, f64)> {
    let total = total(shares.iter().copied());
    let wanted: u32 = shares
        .iter()
        .zip(wants)
        .filter(|(_, wants)| **wants)
        .map(|(share, _)| u32::from(*share))
        .sum();
    let mut needs = Vec::new();
    if wanted > 0 {
        for (index, (&share, &wants)) in shares.iter().zip(wants).enumerate() {
            if share > 0 && wants {
                let percent = f64::from(share) * f64::from(total) / f64::from(wanted);
                needs.push((Some(index), percent / 100.0));
            }
        }
    }
    let idle = if wanted == 0 { 100 } else { 100 - total };
    if idle > 0 {
        needs.push((None, f64::from(idle) / 100.0));
    }
    needs
}

/// Turns of one `period` for what `needs` still want, in proportion.
fn turns(needs: &[(Option<usize>, f64)], period: Duration) -> Vec<(Option<usize>, Duration)> {
    let left: f64 = needs.iter().map(|(_, need)| need.max(0.0)).sum();
    if left <= EPSILON {
        return Vec::new();
    }
    needs
        .iter()
        .filter(|(_, need)| *need > EPSILON)
        .map(|(owner, need)| (*owner, period.mul_f64(need / left)))
        .collect()
}

pub fn plan(shares: &[u8], wants: &[bool], weights: &[f64], period: Duration) -> Round {
    let mut needs = needs(shares, wants);
    let sum: f64 = weights.iter().sum();
    let weights: Vec<f64> = if sum > 0.0 && weights.iter().all(|w| w.is_finite() && *w >= 0.0) {
        weights.iter().map(|weight| weight / sum).collect()
    } else {
        vec![1.0 / weights.len().max(1) as f64; weights.len()]
    };
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|a, b| weights[*b].total_cmp(&weights[*a]));
    let mut gpus = vec![None; weights.len()];
    for gpu in order {
        let mut most: Option<usize> = None;
        for (index, (_, need)) in needs.iter().enumerate() {
            // On a near tie the earlier token keeps the GPU.
            if *need + EPSILON >= weights[gpu]
                && most.is_none_or(|best| *need > needs[best].1 + EPSILON)
            {
                most = Some(index);
            }
        }
        if let Some(index) = most {
            gpus[gpu] = Some(needs[index].0);
            needs[index].1 -= weights[gpu];
        }
    }
    let turns = if gpus.iter().any(Option::is_none) {
        turns(&needs, period)
    } else {
        Vec::new()
    };
    Round { gpus, turns }
}

/// #### PR #61: how long every GPU takes turns at the start, so each token
/// mines every GPU and the GPUs' speeds can be measured; and how long the
/// scheduler keeps trying before it weighs the GPUs as equal.
const MEASURE_ROUNDS: u32 = 3;
const MEASURE_LIMIT: Duration = Duration::from_secs(60);

/// #### PR #61: each GPU's speed from the rates of the token with the
/// largest share that measured every GPU, the same for GPUs of the same
/// model (`names`); `None` while no token has measured them all.
fn weigh(slots: &[Slot], names: &[String]) -> Option<Vec<f64>> {
    let gpus = names.len();
    let mut by_share: Vec<&Slot> = slots.iter().collect();
    by_share.sort_by_key(|slot| std::cmp::Reverse(slot.share.load(Ordering::Relaxed)));
    let rates = by_share
        .iter()
        .map(|slot| slot.member.gpu_rates(gpus))
        .find(|rates| {
            rates.len() == gpus && rates.iter().all(|rate| rate.is_finite() && *rate > 0.0)
        })?;
    Some(
        names
            .iter()
            .map(|name| {
                let same: Vec<f64> = names
                    .iter()
                    .zip(&rates)
                    .filter(|(other, _)| *other == name)
                    .map(|(_, rate)| *rate)
                    .collect();
                same.iter().sum::<f64>() / same.len() as f64
            })
            .collect(),
    )
}

/// What the screen says of a round: which GPUs mine which token, and the
/// turns on the rest. GPUs are numbered from 1 in the order chosen.
fn layout_text(round: &Round, slots: &[Slot]) -> String {
    let name = |owner: Option<usize>| owner.map_or("idle", |index| slots[index].token.name());
    let mut parts: Vec<String> = Vec::new();
    let mut owners: Vec<Option<usize>> = Vec::new();
    for owner in round.gpus.iter().flatten() {
        if !owners.contains(owner) {
            owners.push(*owner);
        }
    }
    for owner in owners {
        let numbers: Vec<String> = round
            .gpus
            .iter()
            .enumerate()
            .filter(|(_, gpu)| **gpu == Some(owner))
            .map(|(index, _)| (index + 1).to_string())
            .collect();
        parts.push(format!("GPU {} {}", numbers.join(", "), name(owner)));
    }
    let shared: Vec<String> = round
        .gpus
        .iter()
        .enumerate()
        .filter(|(_, gpu)| gpu.is_none())
        .map(|(index, _)| (index + 1).to_string())
        .collect();
    if !shared.is_empty() {
        let period: Duration = round.turns.iter().map(|(_, length)| *length).sum();
        let turns: Vec<String> = round
            .turns
            .iter()
            .map(|(owner, length)| {
                format!(
                    "{} {:.0}%",
                    name(*owner),
                    100.0 * length.as_secs_f64() / period.as_secs_f64().max(1e-9)
                )
            })
            .collect();
        let gpus = if round.gpus.len() == 1 {
            "The GPU".to_owned()
        } else {
            format!("GPU {}", shared.join(", "))
        };
        parts.push(if turns.is_empty() {
            format!("{gpus} idle")
        } else {
            format!("{gpus} takes turns: {}", turns.join(" · "))
        });
    }
    parts.join(" · ")
}

/// Hands each GPU to one slot at a time by `plan`, every `period`, until
/// `stop`; then releases every hold so each token can stop cleanly. The
/// GPUs (`names`, in the order chosen) all take turns until their speeds
/// are measured; `layout` says how the GPUs are shared now.
/// #### PR #61: a round starts again as soon as a token starts or stops
/// wanting the GPUs, so its GPUs and time go to the others at once.
pub fn schedule(
    slots: &[Slot],
    names: &[String],
    stop: &AtomicBool,
    period: Duration,
    layout: &std::sync::Mutex<String>,
) {
    let gpus = names.len().max(1);
    let started = Instant::now();
    let mut weights: Option<Vec<f64>> = (gpus == 1).then(|| vec![1.0]);
    let give = |owners: &[Option<usize>]| {
        for (gpu, owner) in owners.iter().enumerate() {
            for (index, slot) in slots.iter().enumerate() {
                slot.member.hold(gpu, *owner != Some(index));
            }
        }
    };
    let wants_now = || -> Vec<bool> {
        slots
            .iter()
            .map(|slot| slot.share.load(Ordering::Relaxed) > 0 && slot.member.wants_gpu())
            .collect()
    };
    while !stop.load(Ordering::Relaxed) {
        if weights.is_none() && started.elapsed() >= period * MEASURE_ROUNDS {
            weights = weigh(slots, names)
                .or_else(|| (started.elapsed() >= MEASURE_LIMIT).then(|| vec![1.0; gpus]));
        }
        let shares: Vec<u8> = slots
            .iter()
            .map(|slot| slot.share.load(Ordering::Relaxed))
            .collect();
        let wants = wants_now();
        let round = match &weights {
            Some(weights) => plan(&shares, &wants, weights, period),
            None => Round {
                gpus: vec![None; gpus],
                turns: turns(&needs(&shares, &wants), period),
            },
        };
        let text = match &weights {
            Some(_) => layout_text(&round, slots),
            None => format!(
                "measuring the GPUs; meanwhile {}",
                layout_text(&round, slots)
            ),
        };
        *layout.lock().unwrap_or_else(|e| e.into_inner()) = text;
        let mut owners: Vec<Option<usize>> = round.gpus.iter().map(|gpu| gpu.flatten()).collect();
        // A round with no turns still lasts a period.
        let turns = if round.turns.is_empty() {
            vec![(None, period)]
        } else {
            round.turns.clone()
        };
        'round: for (turn, length) in turns {
            for (gpu, whole) in round.gpus.iter().enumerate() {
                if whole.is_none() {
                    owners[gpu] = turn;
                }
            }
            give(&owners);
            let end = Instant::now() + length;
            while !stop.load(Ordering::Relaxed) {
                let now = Instant::now();
                if now >= end {
                    break;
                }
                // A token that can no longer mine (a winner to claim, an
                // empty pot) or can again starts a new round.
                if wants_now() != wants {
                    break 'round;
                }
                std::thread::sleep((end - now).min(Duration::from_millis(5)));
            }
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
    }
    for gpu in 0..gpus {
        for slot in slots {
            slot.member.hold(gpu, false);
        }
    }
}

/// A running PHOTON-runtime token of the split.
struct Runtime {
    token: SplitToken,
    supervisor: RuntimeSupervisor,
    rewards: u64,
}

/// Mines `shares` on `gpus` until Ctrl+C or `q`: each runtime token with a
/// share on its own supervisor, ELECTRIC in its own session, the mining
/// dashboard (or status lines with `plain`), and one GPU lock for all of
/// them.
pub fn run(
    cfg: &RuntimeConfig,
    gpus: &[GpuDevice],
    shares: &Shares,
    config_path: &Path,
    plain: bool,
) -> Result<(), String> {
    run_with_pot_mode(cfg, gpus, shares, config_path, plain, false, false)
}

pub fn run_with_pot_mode(
    cfg: &RuntimeConfig,
    gpus: &[GpuDevice],
    shares: &Shares,
    config_path: &Path,
    plain: bool,
    restricted_electric_pot: bool,
    owner_electric_pot: bool,
) -> Result<(), String> {
    shares.validate()?;
    if gpus.is_empty() {
        return Err("no GPU selected for mining".into());
    }
    let _gpu_lock = crate::mining_lock::acquire_gpu_lock()?;
    let names: Vec<String> = gpus.iter().map(|gpu| gpu.name.clone()).collect();
    let electric_devices = crate::backend::electric_engines_for(gpus.to_vec())?;
    let monitor = Arc::new(Monitor::new(
        cfg.intensity,
        plain,
        crate::electric::miner::device_names(&electric_devices),
    ));
    // ELECTRIC's monitor carries the session's stop signal for every token.
    let stop = Arc::clone(&monitor.stop);
    crate::session_control::install_stop(&stop)?;
    let mut runtimes = Vec::new();
    let mut slots = Vec::new();
    for (token, share) in shares.iter().filter(|(_, share)| *share > 0) {
        match token {
            SplitToken::Gpu(mining_token) => {
                let mut token_cfg = cfg.clone();
                token_cfg.set_token(mining_token.as_str())?;
                token_cfg.token_donation = cfg
                    .token_donations
                    .get(mining_token.as_str())
                    .copied()
                    .or(cfg.token_donation);
                // Shares choose the token; intensity controls work while it
                // has the GPU, including a saved live-session setting.
                token_cfg.set_intensity(cfg.intensity)?;
                let supervisor = RuntimeSupervisor::start_on_gpus(token_cfg, gpus)?;
                // Every GPU waits for the scheduler's first turn.
                let hold = supervisor.hold_handle();
                for gpu in 0..gpus.len() {
                    hold.set_gpu(gpu, true);
                }
                slots.push(Slot {
                    token,
                    share: AtomicU8::new(share),
                    member: Box::new(RuntimeMember {
                        hold,
                        rates: supervisor.gpu_rates(),
                    }),
                });
                runtimes.push(Runtime {
                    token,
                    supervisor,
                    rewards: 0,
                });
            }
            SplitToken::Electric => {
                for gpu in 0..gpus.len() {
                    monitor.hold_gpu(gpu, true);
                }
                slots.push(Slot {
                    token,
                    share: AtomicU8::new(share),
                    member: Box::new(ElectricMember(Arc::clone(&monitor))),
                });
            }
        }
    }
    let electric = shares.get(SplitToken::Electric) > 0;
    let layout = std::sync::Mutex::new(String::new());
    let outcome = std::thread::scope(|scope| {
        let electric = electric.then(|| {
            scope.spawn(|| {
                let mined = crate::electric::miner::run_in_split_with_pot_mode(
                    cfg,
                    gpus,
                    config_path,
                    &monitor,
                    restricted_electric_pot,
                    owner_electric_pot,
                );
                if let Err(error) = &mined {
                    // The other tokens keep the GPUs; ELECTRIC no longer asks.
                    monitor.state(format!("Stopped: {error}"));
                    monitor.event(format!("ELECTRIC stopped: {error}"));
                }
                mined
            })
        });
        let scheduler = scope.spawn(|| schedule(&slots, &names, &stop, PERIOD, &layout));
        let shown = if plain {
            report(&stop, &mut runtimes, &monitor, &slots, &layout)
        } else {
            // #### PR #65: the split uses the main dashboard. On exit the
            // caller below joins ELECTRIC and stops the owned supervisors.
            let miners = crate::tui::Miners {
                network: cfg.network,
                runtimes: runtimes
                    .iter()
                    .map(|runtime| (runtime.token, &runtime.supervisor))
                    .collect(),
                electric: electric.is_some().then_some(&*monitor),
                split: Some((&slots, &layout)),
                stop: Some(&stop),
            };
            crate::tui::run_miners(&miners, gpus, &|| {
                stop.load(Ordering::Relaxed)
                    && electric.as_ref().is_none_or(|worker| worker.is_finished())
            })
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
    let mut final_cfg = cfg.clone();
    final_cfg.intensity = monitor.summary().intensity;
    for runtime in runtimes {
        let snapshot = runtime.supervisor.stop();
        final_cfg.intensity = snapshot.search.intensity;
        final_cfg
            .token_donations
            .insert(runtime.token.name().into(), snapshot.token_donation);
    }
    if let Some(donation) = monitor.summary().donation {
        final_cfg
            .token_donations
            .insert("ELECTRIC".into(), donation);
    }
    if crate::session_control::profiles_requested() {
        // #### PR #65: a pot flag is an explicit choice; otherwise the
        // session's own choice (or the default) is kept.
        if owner_electric_pot {
            final_cfg.choose_electric_pot(Some(crate::config::ElectricPotMode::Owner));
        } else if restricted_electric_pot {
            final_cfg.choose_electric_pot(Some(crate::config::ElectricPotMode::Restricted));
        }
        let pairs = slots
            .iter()
            .map(|slot| {
                format!(
                    "{}={}",
                    slot.token.name(),
                    slot.share.load(Ordering::Relaxed)
                )
            })
            .collect();
        crate::tui::remember_token_session(&final_cfg, gpus, config_path, pairs, false);
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

/// What a PHOTON-runtime token is doing, as the split shows it.
pub fn runtime_state(snapshot: &crate::runtime::RuntimeSnapshot) -> String {
    match snapshot.state {
        SupervisorState::Mining => format!("Mining · height {}", snapshot.height),
        SupervisorState::Paused if snapshot.pending_winners > 0 => "Claiming a winner".into(),
        SupervisorState::Paused => "Paused".into(),
        SupervisorState::Reconnecting => "Reconnecting".into(),
        SupervisorState::Error => snapshot
            .last_error
            .as_ref()
            .map_or_else(|| "Error".into(), |error| format!("Error: {error}")),
        SupervisorState::Stopped => "Stopped".into(),
    }
}

/// #### PR #65: moves the share of `slots[index]` one step up or down while
/// mining, as `Shares::step` does; up is refused when the shares already add
/// up to 100%. Returns the new share.
pub fn step_share(slots: &[Slot], index: usize, up: bool) -> Result<u8, String> {
    let total = total(slots.iter().map(|slot| slot.share.load(Ordering::Relaxed)));
    let slot = slots
        .get(index)
        .ok_or("no token at that place in the split")?;
    let next = step(slot.share.load(Ordering::Relaxed), total, up)?;
    slot.share.store(next, Ordering::Relaxed);
    Ok(next)
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
                        .map(|runtime| {
                            let snapshot = runtime.supervisor.snapshot();
                            (snapshot.search.current_rate, runtime_state(&snapshot))
                        })
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
    layout: &std::sync::Mutex<String>,
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
    let mut shown_layout = String::new();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        // #### PR #61: how the GPUs are shared, when it changes.
        let now = layout.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if now != shown_layout && !now.is_empty() {
            println!("GPUs: {now}");
            shown_layout = now;
        }
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

    /// A round on `gpus` equal GPUs: each GPU's whole owner, and the turns.
    #[allow(clippy::type_complexity)]
    fn round(
        shares: &[u8],
        wants: &[bool],
        gpus: usize,
    ) -> (Vec<Option<Option<usize>>>, Vec<(Option<usize>, u128)>) {
        let round = plan(shares, wants, &vec![1.0; gpus], Duration::from_millis(2000));
        (round.gpus, turn_ms(&round.turns))
    }

    #[test]
    fn a_round_follows_the_shares_and_gives_a_waiting_tokens_time_to_the_others() {
        // One GPU: the tokens take turns as their shares say.
        assert_eq!(
            round(&[80, 20], &[true, true], 1),
            (vec![None], vec![(Some(0), 1600), (Some(1), 400)])
        );
        // ELECTRIC's pot is empty: PHOTON mines the whole round.
        assert_eq!(
            round(&[80, 20], &[true, false], 1),
            (vec![Some(Some(0))], vec![])
        );
        // The idle part stays idle, whoever waits.
        assert_eq!(
            round(&[40, 40], &[true, true], 1),
            (
                vec![None],
                vec![(Some(0), 800), (Some(1), 800), (None, 400)]
            )
        );
        assert_eq!(
            round(&[40, 40], &[false, true], 1),
            (vec![None], vec![(Some(1), 1600), (None, 400)])
        );
        // Nobody can mine: the round idles; a token at 0% never mines.
        assert_eq!(
            round(&[80, 20], &[false, false], 1),
            (vec![Some(None)], vec![])
        );
        assert_eq!(
            round(&[100, 0], &[true, true], 1),
            (vec![Some(Some(0))], vec![])
        );
    }

    // #### PR #61: whole GPUs first; turns only on the GPUs left over.
    #[test]
    fn a_rig_gives_whole_gpus_first_and_turns_only_on_the_rest() {
        // Five GPUs at 80/20: four mine PHOTON, one ELECTRIC; no turns.
        assert_eq!(
            round(&[80, 20], &[true, true], 5),
            (
                vec![
                    Some(Some(0)),
                    Some(Some(0)),
                    Some(Some(0)),
                    Some(Some(0)),
                    Some(Some(1))
                ],
                vec![]
            )
        );
        // ELECTRIC waits for its pot: all five mine PHOTON.
        assert_eq!(
            round(&[80, 20], &[true, false], 5).0,
            vec![Some(Some(0)); 5]
        );
        // Two GPUs at 80/20: PHOTON has one; the other takes turns 60/40.
        assert_eq!(
            round(&[80, 20], &[true, true], 2),
            (
                vec![Some(Some(0)), None],
                vec![(Some(0), 1200), (Some(1), 800)]
            )
        );
        // Three GPUs at 50/50: one each, the third takes turns.
        assert_eq!(
            round(&[50, 50], &[true, true], 3),
            (
                vec![Some(Some(0)), Some(Some(1)), None],
                vec![(Some(0), 1000), (Some(1), 1000)]
            )
        );
        // The idle rest of 100% is a GPU too.
        assert_eq!(
            round(&[60, 20], &[true, true], 5).0,
            vec![
                Some(Some(0)),
                Some(Some(0)),
                Some(Some(0)),
                Some(Some(1)),
                Some(None)
            ]
        );
        // A fast GPU and a slow one (3:1) at 80/20: the fast one mines
        // PHOTON; the slow one takes turns for the rest of both.
        let mixed = plan(
            &[80, 20],
            &[true, true],
            &[3.0, 1.0],
            Duration::from_millis(2000),
        );
        assert_eq!(mixed.gpus, vec![Some(Some(0)), None]);
        assert_eq!(turn_ms(&mixed.turns), [(Some(0), 400), (Some(1), 1600)]);
    }

    /// A member that counts how long it held each GPU, at a fixed rate.
    struct Fake {
        wants: AtomicBool,
        held: Vec<AtomicBool>,
        mined: Vec<Mutex<(Duration, Option<Instant>)>>,
        rate: f64,
    }

    impl Fake {
        fn new(wants: bool, gpus: usize) -> Arc<Self> {
            Arc::new(Self {
                wants: AtomicBool::new(wants),
                held: (0..gpus).map(|_| AtomicBool::new(true)).collect(),
                mined: (0..gpus)
                    .map(|_| Mutex::new((Duration::ZERO, None)))
                    .collect(),
                rate: 1e9,
            })
        }
        fn mined_on(&self, gpu: usize) -> Duration {
            let (total, since) = *self.mined[gpu].lock().unwrap();
            total + since.map_or(Duration::ZERO, |since| since.elapsed())
        }
        fn mined(&self) -> Duration {
            (0..self.mined.len()).map(|gpu| self.mined_on(gpu)).sum()
        }
        fn released(&self) -> bool {
            self.held.iter().all(|held| !held.load(Ordering::Relaxed))
        }
    }

    impl Member for Arc<Fake> {
        fn wants_gpu(&self) -> bool {
            self.wants.load(Ordering::Relaxed)
        }
        fn hold(&self, gpu: usize, held: bool) {
            let mut mined = self.mined[gpu].lock().unwrap();
            match (held, mined.1) {
                (true, Some(since)) => *mined = (mined.0 + since.elapsed(), None),
                (false, None) => mined.1 = Some(Instant::now()),
                _ => {}
            }
            self.held[gpu].store(held, Ordering::Relaxed);
        }
        fn gpu_rates(&self, gpus: usize) -> Vec<f64> {
            vec![self.rate; gpus]
        }
    }

    fn slots_of(photon: &Arc<Fake>, electric: &Arc<Fake>) -> [Slot; 2] {
        [
            Slot {
                token: SplitToken::Gpu(MiningToken::Photon),
                share: AtomicU8::new(80),
                member: Box::new(Arc::clone(photon)),
            },
            Slot {
                token: SplitToken::Electric,
                share: AtomicU8::new(20),
                member: Box::new(Arc::clone(electric)),
            },
        ]
    }

    // #### PR #65: the dashboard's settings move a running split's share.
    #[test]
    fn a_running_share_steps_by_five_and_never_past_100() {
        let (photon, electric) = (Fake::new(true, 1), Fake::new(true, 1));
        let slots = slots_of(&photon, &electric);
        let shares = |slots: &[Slot]| {
            (
                slots[0].share.load(Ordering::Relaxed),
                slots[1].share.load(Ordering::Relaxed),
            )
        };
        assert!(step_share(&slots, 0, true).unwrap_err().contains("100%"));
        assert_eq!(step_share(&slots, 0, false), Ok(75));
        assert_eq!(step_share(&slots, 1, true), Ok(25));
        assert_eq!(shares(&slots), (75, 25));
        assert!(step_share(&slots, 1, true).is_err());
        assert!(step_share(&slots, 2, true).is_err());
        assert_eq!(shares(&slots), (75, 25));
    }

    /// #### PR #72
    /// What: stops the scheduler when dropped, a failed assertion included.
    /// Why: the scope waits for the scheduler, which only stops on `stop`;
    /// a failed assertion on a slow runner hung macOS CI for 27 minutes
    /// instead of failing.
    /// Look here if: a scheduler test hangs rather than failing.
    struct StopOnDrop<'a>(&'a AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    /// Waits up to 10 seconds for `ready`, for slow runners.
    fn wait_until(what: &str, ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_scheduler_gives_each_token_its_share_and_releases_every_hold_at_stop() {
        let (photon, electric) = (Fake::new(true, 1), Fake::new(true, 1));
        let slots = slots_of(&photon, &electric);
        let stop = AtomicBool::new(false);
        let period = Duration::from_millis(200);
        let layout = Mutex::new(String::new());
        std::thread::scope(|scope| {
            scope.spawn(|| schedule(&slots, &["GPU".into()], &stop, period, &layout));
            let _stop = StopOnDrop(&stop);
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
        assert!(photon.released() && electric.released());
    }

    // #### PR #61: once the GPUs are measured, a rig mines whole GPUs.
    #[test]
    fn the_scheduler_measures_a_rig_then_gives_it_whole_gpus() {
        let (photon, electric) = (Fake::new(true, 5), Fake::new(true, 5));
        let slots = slots_of(&photon, &electric);
        let stop = AtomicBool::new(false);
        let period = Duration::from_millis(100);
        let layout = Mutex::new(String::new());
        let names = vec!["RTX 3070".to_string(); 5];
        let whole = "GPU 1, 2, 3, 4 PHOTON · GPU 5 ELECTRIC";
        std::thread::scope(|scope| {
            scope.spawn(|| schedule(&slots, &names, &stop, period, &layout));
            let _stop = StopOnDrop(&stop);
            // #### PR #72: each stage is waited for, not slept through.
            wait_until("the first round", || !layout.lock().unwrap().is_empty());
            assert!(layout.lock().unwrap().starts_with("measuring the GPUs"));
            wait_until("whole GPUs", || *layout.lock().unwrap() == whole);
            // The layout is written before the GPUs are given: wait for
            // ELECTRIC to hold GPUs 1-4 and mine GPU 5 before measuring.
            wait_until("whole GPUs given", || {
                (0..4).all(|gpu| electric.held[gpu].load(Ordering::Relaxed))
                    && !electric.held[4].load(Ordering::Relaxed)
            });
            let before: Vec<Duration> = (0..5).map(|gpu| electric.mined_on(gpu)).collect();
            std::thread::sleep(period * 5);
            let gained: Vec<Duration> = (0..5)
                .map(|gpu| electric.mined_on(gpu) - before[gpu])
                .collect();
            assert!(gained[..4].iter().all(Duration::is_zero), "{gained:?}");
            assert!(gained[4] > period * 4, "{gained:?}");
            assert_eq!(*layout.lock().unwrap(), whole);
        });
        assert!(photon.released() && electric.released());
    }
}
