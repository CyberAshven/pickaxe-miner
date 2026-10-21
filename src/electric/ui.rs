//! #### PR #58: terminal controls for the ELECTRIC GPU session.
//! #### PR #65: its state, rates, fee pot and events, which the mining
//! dashboard shows and steers (`tui::run_electric`).
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// #### PR #61: how many GPUs the hash split can hold one by one.
pub const MAX_HELD_GPUS: usize = 64;

pub struct Monitor {
    pub(crate) withdrawal: Mutex<super::withdrawal::Control>,
    pub stop: Arc<AtomicBool>,
    pub paused: AtomicBool,
    /// #### PR #53: the hash split holds the GPUs for another token.
    pub held: AtomicBool,
    /// #### PR #61: or one GPU alone, by its place in the session's GPUs
    /// (the first `MAX_HELD_GPUS`).
    held_gpus: Box<[AtomicBool]>,
    /// #### PR #53: a search is running: the pot can pay and the baton is
    /// minable, so ELECTRIC wants its share of the GPUs.
    pub searching: AtomicBool,
    pub intensity: AtomicU8,
    pub hashes: AtomicU64,
    view: Mutex<View>,
    plain: bool,
    started: Instant,
}

/// #### PR #53: what the hash split's screen shows of the ELECTRIC session.
/// #### PR #65: and the mining dashboard, for a split or ELECTRIC alone.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub(crate) withdrawal: super::withdrawal::View,
    pub state: String,
    /// Hashes a second over the last two seconds.
    pub rate: f64,
    /// #### PR #65: the GPUs' hashes a second while they mine (not while
    /// paused or held for another token).
    pub active_rate: f64,
    /// #### PR #65: every hash since the session started, and how long ago
    /// that was.
    pub hashes: u64,
    pub uptime: Duration,
    pub claims: u64,
    pub pot_address: Option<String>,
    pub pot_qr: Vec<String>,
    /// Plain-BCH sats in the pot and the claims they pay, once read.
    pub pot: Option<(u64, u64)>,
    /// #### PR #65: where the pot's recovery key is saved.
    pub pot_key: Option<String>,
    pub events: Vec<String>,
    /// #### PR #65: the GPUs, their intensity and whether mining is paused.
    pub devices: String,
    pub intensity: u8,
    pub paused: bool,
    /// Effective donation of the schedule created by the mining loop.
    pub donation: Option<crate::donation::TokenDonation>,
}

/// #### PR #65: events kept for the dashboard to take, at most.
const FRESH_EVENTS: usize = 96;

struct View {
    state: String,
    events: VecDeque<String>,
    /// #### PR #65: events the dashboard has not taken yet.
    fresh: VecDeque<String>,
    samples: VecDeque<(Instant, usize, u32, Duration)>,
    devices: String,
    claims: u64,
    pot: Option<PotView>,
    donation: Option<crate::donation::TokenDonation>,
}

/// The fee pot the miner funds, as the screen shows it.
struct PotView {
    address: String,
    key: Option<String>,
    qr: Vec<String>,
    /// Plain-BCH sats in the pot, once read.
    sats: Option<u64>,
    /// Claims the pot can pay at the current relay fee.
    claims: u64,
}

/// `text` as QR code rows of half blocks, two modules per character, with
/// a quiet zone of two modules. Light modules are drawn, so it scans with
/// light characters on a dark background.
pub fn qr_lines(text: &str) -> Result<Vec<String>, String> {
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
        .map_err(|error| format!("QR code: {error}"))?;
    let width = code.width();
    let colors = code.to_colors();
    const QUIET: usize = 2;
    let size = width + 2 * QUIET;
    let light = |x: usize, y: usize| {
        if y >= size {
            return false;
        }
        let inside = (QUIET..QUIET + width).contains(&x) && (QUIET..QUIET + width).contains(&y);
        !inside || colors[(y - QUIET) * width + (x - QUIET)] == qrcode::Color::Light
    };
    Ok((0..size)
        .step_by(2)
        .map(|y| {
            (0..size)
                .map(|x| match (light(x, y), light(x, y + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect()
        })
        .collect())
}

impl Monitor {
    pub fn new(intensity: u8, plain: bool, devices: String) -> Self {
        Self {
            withdrawal: Mutex::new(super::withdrawal::Control::default()),
            stop: Arc::new(AtomicBool::new(false)),
            paused: AtomicBool::new(false),
            held: AtomicBool::new(false),
            held_gpus: (0..MAX_HELD_GPUS).map(|_| AtomicBool::new(false)).collect(),
            searching: AtomicBool::new(false),
            intensity: AtomicU8::new(intensity),
            hashes: AtomicU64::new(0),
            view: Mutex::new(View {
                state: "Connecting".into(),
                events: VecDeque::new(),
                fresh: VecDeque::new(),
                samples: VecDeque::new(),
                devices,
                claims: 0,
                pot: None,
                donation: None,
            }),
            plain,
            started: Instant::now(),
        }
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Relaxed)
    }

    pub fn set_donation_display(&self, scheme: crate::donation::Scheme) {
        let total: u16 = scheme.work().into_iter().sum();
        self.view.lock().unwrap_or_else(|e| e.into_inner()).donation =
            Some(crate::donation::TokenDonation::from_bps(total));
    }

    /// #### PR #61: holds or releases the GPU at `device` alone.
    pub fn hold_gpu(&self, device: usize, held: bool) {
        if let Some(gpu) = self.held_gpus.get(device) {
            gpu.store(held, std::sync::atomic::Ordering::Release);
        }
    }

    /// #### PR #61: whether the GPU at `device` waits for its turn.
    pub fn gpu_held(&self, device: usize) -> bool {
        self.held.load(std::sync::atomic::Ordering::Acquire)
            || self
                .held_gpus
                .get(device)
                .is_some_and(|gpu| gpu.load(std::sync::atomic::Ordering::Acquire))
    }

    /// #### PR #61: each of the first `count` GPUs' hashes a second while
    /// it mines (not while held), over the last five seconds; 0 before it
    /// measured.
    pub fn gpu_rates(&self, count: usize) -> Vec<f64> {
        let view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        (0..count)
            .map(|device| {
                let (hashes, busy) = view
                    .samples
                    .iter()
                    .filter(|(_, sample, _, _)| *sample == device)
                    .fold(
                        (0u64, Duration::ZERO),
                        |(hashes, busy), &(_, _, count, took)| {
                            (hashes + u64::from(count), busy + took)
                        },
                    );
                if busy.is_zero() {
                    0.0
                } else {
                    hashes as f64 / busy.as_secs_f64()
                }
            })
            .collect()
    }
    pub fn wait(&self, duration: Duration) {
        let start = Instant::now();
        while !self.stopped() && start.elapsed() < duration {
            std::thread::sleep(
                duration
                    .saturating_sub(start.elapsed())
                    .min(Duration::from_millis(50)),
            );
        }
    }
    pub fn state(&self, state: impl Into<String>) {
        let state = state.into();
        let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        // Without the screen, each new state is printed once (`--no-tui`).
        if self.plain && view.state != state {
            println!("{state}");
        }
        view.state = state;
    }
    pub fn event(&self, text: impl Into<String>) {
        let text = text.into();
        if self.plain {
            println!("{text}");
        }
        let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if !self.plain {
            view.fresh.push_back(text.clone());
            while view.fresh.len() > FRESH_EVENTS {
                view.fresh.pop_front();
            }
        }
        view.events.push_back(text);
        while view.events.len() > 12 {
            view.events.pop_front();
        }
    }
    /// #### PR #65: the events since the dashboard last took them, oldest
    /// first.
    pub fn drain_events(&self) -> Vec<String> {
        self.view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fresh
            .drain(..)
            .collect()
    }
    pub fn accepted(&self) {
        self.view.lock().unwrap_or_else(|e| e.into_inner()).claims += 1;
    }
    /// The pot to fund, and where its recovery key is saved.
    pub fn set_pot(&self, address: &str, key: Option<&std::path::Path>) {
        let qr = qr_lines(address).unwrap_or_default();
        if self.plain {
            println!("Fee pot: {address} (send plain BCH here; it pays each claim's fee)");
            for line in &qr {
                println!("{line}");
            }
            if let Some(path) = key {
                println!("Refund key: {} (back it up)", path.display());
            }
        }
        self.view.lock().unwrap_or_else(|e| e.into_inner()).pot = Some(PotView {
            address: address.to_owned(),
            key: key.map(|path| path.display().to_string()),
            qr,
            sats: None,
            claims: 0,
        });
    }
    /// What the pot holds now, and how many claims that pays.
    pub fn pot_balance(&self, sats: u64, claims: u64) {
        if let Some(pot) = &mut self.view.lock().unwrap_or_else(|e| e.into_inner()).pot {
            pot.sats = Some(sats);
            pot.claims = claims;
        }
    }
    /// #### PR #53: the GPUs the session mines on, as its screen names them.
    pub fn devices(&self) -> String {
        self.view
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .devices
            .clone()
    }
    /// #### PR #53: the session as the hash split's screen shows it.
    pub fn summary(&self) -> Summary {
        let view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // Wall-clock rate: work done in the last two seconds, so a token
        // that held the GPUs for part of them shows what it really got.
        let window = Duration::from_secs(2);
        let recent: u64 = view
            .samples
            .iter()
            .filter(|(time, ..)| now.duration_since(*time) <= window)
            .map(|&(_, _, count, _)| u64::from(count))
            .sum();
        let elapsed = now.duration_since(self.started).min(window);
        let paused = self.paused.load(Relaxed);
        Summary {
            withdrawal: self
                .withdrawal
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .view(),
            state: if self.stopped() {
                "Stopping; preserving pending claim".into()
            } else if paused {
                "Paused".into()
            } else {
                view.state.clone()
            },
            rate: recent as f64 / elapsed.as_secs_f64().max(1e-3),
            active_rate: rates(&view.samples, now).1 * 1e6,
            hashes: self.hashes.load(Relaxed),
            uptime: now.duration_since(self.started),
            claims: view.claims,
            pot_address: view.pot.as_ref().map(|pot| pot.address.clone()),
            pot_qr: view
                .pot
                .as_ref()
                .map(|pot| pot.qr.clone())
                .unwrap_or_default(),
            pot: view
                .pot
                .as_ref()
                .and_then(|pot| pot.sats.map(|sats| (sats, pot.claims))),
            pot_key: view.pot.as_ref().and_then(|pot| pot.key.clone()),
            events: view.events.iter().cloned().collect(),
            devices: view.devices.clone(),
            intensity: self.intensity.load(Relaxed),
            paused,
            donation: view.donation,
        }
    }
    pub fn record(&self, device: usize, count: u32, duration: Duration) {
        self.hashes.fetch_add(u64::from(count), Relaxed);
        let mut view = self.view.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        view.samples.push_back((now, device, count, duration));
        while view
            .samples
            .front()
            .is_some_and(|(time, _, _, _)| now.duration_since(*time) > Duration::from_secs(5))
        {
            view.samples.pop_front();
        }
    }
}

/// Mines with `mine` until it ends: on the mining dashboard (#### PR #65),
/// or printing each state and event with `plain`. `gpus` are the GPUs it
/// mines, for their telemetry.
pub fn run(
    monitor: &Monitor,
    gpus: &[crate::backend::GpuDevice],
    network: crate::config::MiningNetwork,
    mine: impl FnOnce() -> Result<(), String> + Send,
) -> Result<(), String> {
    let stop = Arc::clone(&monitor.stop);
    ctrlc::set_handler(move || {
        stop.store(true, Relaxed);
    })
    .map_err(|e| e.to_string())?;
    if monitor.plain {
        return mine();
    }
    std::thread::scope(|scope| {
        let worker = scope.spawn(mine);
        let displayed = crate::tui::run_electric(monitor, gpus, network, &|| worker.is_finished());
        monitor.stop.store(true, Relaxed);
        let mined = worker
            .join()
            .map_err(|_| "ELECTRIC mining thread panicked".to_string())?;
        displayed.and(mined)
    })
}

/// Each GPU's hashes a second over its recent batches, in MH/s: those that
/// mined in the last two seconds, and all of them while busy.
fn rates(samples: &VecDeque<(Instant, usize, u32, Duration)>, now: Instant) -> (f64, f64) {
    let mut devices = std::collections::BTreeMap::new();
    for &(time, device, count, duration) in samples {
        let entry = devices
            .entry(device)
            .or_insert((0u64, Duration::ZERO, time));
        entry.0 += u64::from(count);
        entry.1 += duration;
        entry.2 = time;
    }
    devices
        .values()
        .fold((0.0, 0.0), |(current, active), &(work, busy, last)| {
            let rate = work as f64 / busy.as_secs_f64().max(1e-6) / 1e6;
            (
                current
                    + if now.duration_since(last) < Duration::from_secs(2) {
                        rate
                    } else {
                        0.0
                    },
                active + rate,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    // #### PR #65: what the dashboard reads of the session.
    #[test]
    fn the_summary_carries_the_pot_its_key_and_events_for_the_dashboard() {
        let monitor = Monitor::new(80, false, "RTX (cuda)".into());
        let address = "bchtest:p0ssibledemoaddressforthescreenonlyqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        monitor.set_pot(address, Some(std::path::Path::new("C:/keys/e.pot-key")));
        monitor.event("Connected: wss://example");
        monitor.event("Verified proof for you · 120.00 MH/s");
        monitor.record(0, 1_000_000, Duration::from_millis(10));
        monitor.set_donation_display(crate::donation::Scheme::Work([350, 200]));
        let summary = monitor.summary();
        assert_eq!(summary.pot_address.as_deref(), Some(address));
        assert_eq!(summary.pot_key.as_deref(), Some("C:/keys/e.pot-key"));
        assert_eq!(summary.pot, None, "not read yet");
        assert!(!summary.pot_qr.is_empty());
        assert_eq!((summary.intensity, summary.paused), (80, false));
        assert_eq!(summary.hashes, 1_000_000);
        assert_eq!(summary.active_rate, 100e6);
        assert_eq!(summary.devices, "RTX (cuda)");
        assert_eq!(summary.donation.map(|d| d.bps()), Some(550));
        monitor.pot_balance(717_383, 380);
        assert_eq!(monitor.summary().pot, Some((717_383, 380)));
        // The dashboard takes each event once, oldest first.
        assert_eq!(
            monitor.drain_events(),
            [
                "Connected: wss://example",
                "Verified proof for you · 120.00 MH/s"
            ]
        );
        assert!(monitor.drain_events().is_empty());
        monitor.paused.store(true, Relaxed);
        assert_eq!(monitor.summary().state, "Paused");
        monitor.stop.store(true, Relaxed);
        assert_eq!(
            monitor.summary().state,
            "Stopping; preserving pending claim"
        );
        // Printed lines (`--no-tui`) are not kept for a dashboard.
        let plain = Monitor::new(80, true, "RTX (cuda)".into());
        plain.event("Connected: wss://example");
        assert!(plain.drain_events().is_empty());
    }

    #[test]
    fn qr_rows_draw_every_module_and_a_light_quiet_zone() {
        let text = "bchtest:pvnzasq4yyzrxl6c6ygsmg2cvkzrr2t5a6r5s2ejtq7l6qhphtu86m0mjlh8y";
        let rows = qr_lines(text).unwrap();
        let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
            .unwrap();
        let (width, colors) = (code.width(), code.to_colors());
        let size = width + 4;
        assert_eq!(rows.len(), size.div_ceil(2));
        let light = |x: usize, y: usize| {
            let inside = (2..2 + width).contains(&x) && (2..2 + width).contains(&y);
            !inside || colors[(y - 2) * width + x - 2] == qrcode::Color::Light
        };
        for (row, line) in rows.iter().enumerate() {
            let cells: Vec<char> = line.chars().collect();
            assert_eq!(cells.len(), size);
            for (x, cell) in cells.into_iter().enumerate() {
                let top = light(x, 2 * row);
                let bottom = 2 * row + 1 < size && light(x, 2 * row + 1);
                let expected = match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                };
                assert_eq!(cell, expected, "row {row} column {x}");
            }
        }
        // The top-left finder: a dark 7×7 ring right inside the quiet zone.
        assert!(!light(2, 2) && !light(8, 2) && !light(2, 8) && light(3, 3) && !light(4, 4));
    }

    #[test]
    fn rates_sum_devices_and_idle_becomes_zero_without_erasing_active_rate() {
        let now = Instant::now();
        let samples = VecDeque::from([
            (now, 0, 1000000, Duration::from_millis(10)),
            (now, 1, 1000000, Duration::from_millis(20)),
        ]);
        assert_eq!(rates(&samples, now), (150.0, 150.0));
        assert_eq!(rates(&samples, now + Duration::from_secs(3)), (0.0, 150.0));
        assert_eq!(rates(&VecDeque::new(), now), (0.0, 0.0));
    }
}
