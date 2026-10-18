//! #### PR #58: terminal controls for the ELECTRIC GPU session.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub struct Monitor {
    pub stop: Arc<AtomicBool>,
    pub paused: AtomicBool,
    pub intensity: AtomicU8,
    pub hashes: AtomicU64,
    view: Mutex<View>,
    plain: bool,
    started: Instant,
}

struct View {
    state: String,
    events: VecDeque<String>,
    samples: VecDeque<(Instant, usize, u32, Duration)>,
    devices: String,
    claims: u64,
    pot: Option<PotView>,
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
            stop: Arc::new(AtomicBool::new(false)),
            paused: AtomicBool::new(false),
            intensity: AtomicU8::new(intensity),
            hashes: AtomicU64::new(0),
            view: Mutex::new(View {
                state: "Connecting".into(),
                events: VecDeque::new(),
                samples: VecDeque::new(),
                devices,
                claims: 0,
                pot: None,
            }),
            plain,
            started: Instant::now(),
        }
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Relaxed)
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
        view.events.push_back(text);
        while view.events.len() > 12 {
            view.events.pop_front();
        }
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

pub fn run(
    monitor: &Monitor,
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
        let displayed = display(monitor, || worker.is_finished());
        monitor.stop.store(true, Relaxed);
        let mined = worker
            .join()
            .map_err(|_| "ELECTRIC mining thread panicked".to_string())?;
        displayed.and(mined)
    })
}

fn display(monitor: &Monitor, finished: impl Fn() -> bool) -> Result<(), String> {
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
    // None: show the QR code while the pot cannot pay a claim; `f` toggles.
    let mut show_qr: Option<bool> = None;
    while !finished() {
        terminal
            .draw(|frame| draw(frame, monitor, show_qr))
            .map_err(|e| e.to_string())?;
        if event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
            if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => monitor.stop.store(true, Relaxed),
                    KeyCode::Char('f') => {
                        let funded = monitor
                            .view
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .pot
                            .as_ref()
                            .is_some_and(|pot| pot.claims > 0);
                        show_qr = Some(!show_qr.unwrap_or(!funded));
                    }
                    KeyCode::Char('p') => {
                        monitor.paused.fetch_xor(true, Relaxed);
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        monitor
                            .intensity
                            .fetch_update(Relaxed, Relaxed, |n| Some((n + 5).min(100)))
                            .ok();
                    }
                    KeyCode::Char('-') => {
                        monitor
                            .intensity
                            .fetch_update(Relaxed, Relaxed, |n| Some(n.saturating_sub(5).max(10)))
                            .ok();
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// One frame of the ELECTRIC screen: the session's state and rates, the
/// fee pot with its QR code while it cannot pay a claim (or as `f` set
/// it), recent events and the keys.
fn draw(frame: &mut ratatui::Frame<'_>, monitor: &Monitor, show_qr: Option<bool>) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Style},
        widgets::{Block, Paragraph, Wrap},
    };
    let view = monitor.view.lock().unwrap_or_else(|e| e.into_inner());
    let (current, active) = rates(&view.samples, Instant::now());
    let total = monitor.hashes.load(Relaxed);
    let elapsed = monitor.started.elapsed().as_secs_f64();
    let state = if monitor.stopped() {
        "Stopping; preserving pending claim"
    } else if monitor.paused.load(Relaxed) {
        "Paused"
    } else {
        &view.state
    };
    let mut stats = format!(
        "State      {state}\nGPU        {}\nHashrate   now {:.2} MH/s · active {:.2} MH/s · wall avg {:.2} MH/s\nWork       {total} hashes\nRewards    {} accepted\nIntensity  {}%\nUptime     {:.0}s",
        view.devices,
        current,
        active,
        total as f64 / elapsed.max(1e-6) / 1e6,
        view.claims,
        monitor.intensity.load(Relaxed),
        elapsed
    );
    let mut qr: &[String] = &[];
    if let Some(pot) = &view.pot {
        stats.push_str(&format!("\nFee pot    {}", pot.address));
        stats.push_str(&match pot.sats {
            Some(sats) => format!(
                "\n           {sats} sats · pays {} claim{}{}",
                pot.claims,
                if pot.claims == 1 { "" } else { "s" },
                if pot.claims == 0 {
                    " · send BCH to this address"
                } else {
                    ""
                }
            ),
            None => "\n           reading the pot…".to_string(),
        });
        if let Some(key) = &pot.key {
            stats.push_str(&format!("\nRefund key {key} (back it up)"));
        }
        if show_qr.unwrap_or(pot.claims == 0) {
            qr = &pot.qr;
        }
    }
    let lines = stats.lines().count() as u16 + 2;
    let top = lines.max(if qr.is_empty() {
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
    let stats = Paragraph::new(stats)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(" Pickaxe · ELECTRIC "));
    if qr.is_empty() {
        frame.render_widget(stats, areas[0]);
    } else {
        let width = qr[0].chars().count() as u16 + 2;
        let columns =
            Layout::horizontal([Constraint::Min(30), Constraint::Length(width)]).split(areas[0]);
        frame.render_widget(stats, columns[0]);
        frame.render_widget(
            Paragraph::new(qr.join("\n"))
                .style(Style::default().fg(Color::White).bg(Color::Black))
                .block(Block::bordered().title(" Fund the pot ")),
            columns[1],
        );
    }
    frame.render_widget(
        Paragraph::new(view.events.iter().cloned().collect::<Vec<_>>().join("\n"))
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(" Recent events ")),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(
            "p pause/resume · +/- intensity · f fee pot QR · q quit (pending claims saved)",
        ),
        areas[2],
    );
}

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
    #[test]
    fn the_screen_shows_an_unfunded_pot_with_its_qr_code_until_it_pays_a_claim() {
        let monitor = Monitor::new(80, false, "RTX (cuda)".into());
        let address = "bchtest:p0ssibledemoaddressforthescreenonlyqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        monitor.set_pot(address, Some(std::path::Path::new("C:/keys/e.pot-key")));
        let screen = |show_qr| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 45)).unwrap();
            terminal
                .draw(|frame| draw(frame, &monitor, show_qr))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let unread = screen(None);
        for expected in [
            address,
            " Fund the pot ",
            "reading the pot",
            "C:/keys/e.pot-key (back it up)",
            "f fee pot QR",
        ] {
            assert!(unread.contains(expected), "{expected}: {unread}");
        }
        monitor.pot_balance(500, 0);
        let empty = screen(None);
        assert!(empty.contains("500 sats · pays 0 claims · send BCH to this address"));
        assert!(empty.contains(" Fund the pot "));
        monitor.pot_balance(8_000, 3);
        let funded = screen(None);
        assert!(funded.contains("8000 sats · pays 3 claims"), "{funded}");
        assert!(!funded.contains(" Fund the pot "));
        assert!(screen(Some(true)).contains(" Fund the pot "));
        assert!(!screen(Some(false)).contains(" Fund the pot "));
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
