//! #### PR #38
//! Native node preflight and mining-server lifecycle. Reuse the terminal guard,
//! keep authority secrets private, and never print node credentials or payouts.

use super::{
    device_api::DeviceAction,
    fleet::Fleet,
    provider::{NativeNodeRpc, TemplateProvider},
    server::{self, ServerConfig, ServerStats},
    telemetry::DeviceSnapshot,
    template::{compact_target, BchTemplate},
};
use crate::{
    cli::StratumV2Command,
    config::{self, RuntimeConfig},
    donation::bch::BchDonation,
    tui::TerminalSession,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    widgets::{Block, Paragraph, Row, Table, Wrap},
    Frame,
};
use serde::Deserialize;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn run(
    action: StratumV2Command,
    config: &RuntimeConfig,
    config_path: &Path,
    no_tui: bool,
    json: bool,
) -> Result<(), String> {
    if let StratumV2Command::Status = action {
        print!("{}", super::status_report());
        return Ok(());
    }
    if let StratumV2Command::Watch = action {
        return watch(&status_path(config_path));
    }
    if matches!(action, StratumV2Command::Serve { .. }) {
        config::validate_payout_address(config.network, &config.payout_address)
            .map_err(|_| "a valid payout for the selected network is required")?;
    }
    let (rpc, template) = preflight(config)?;
    let StratumV2Command::Serve {
        listen,
        sv1_listen,
        donation,
    } = action
    else {
        println!(
            "{}",
            serde_json::json!({
                "network":config.network.as_str(),"template_height":template.height,
                "transactions":template.transaction_count(),"bits":format!("{:08x}", template.bits),
                "size_limit":template.size_limit,"ready":true,"mining":false,
            })
        );
        return Ok(());
    };
    let donation = Arc::new(RwLock::new(donation.unwrap_or(config.bch_donation)));
    let listener = TcpListener::bind(listen).map_err(|_| "cannot bind mining listener")?;
    let bound = listener
        .local_addr()
        .map_err(|_| "cannot read mining listener")?;
    let sv1_listener = sv1_listen
        .map(TcpListener::bind)
        .transpose()
        .map_err(|_| "cannot bind SV1 listener")?;
    let sv1_bound = sv1_listener
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()
        .map_err(|_| "cannot read SV1 listener")?;
    let authority_secret = load_authority(&config_path.with_extension("sv2-key"))?;
    let public = server::authority_public(&authority_secret)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_signal = stop.clone();
    ctrlc::set_handler(move || stop_signal.store(true, Ordering::Relaxed))
        .map_err(|_| "cannot install clean shutdown handler")?;
    let mut terminal = if no_tui || json {
        None
    } else {
        Some(TerminalSession::enter()?)
    };
    let stats = Arc::new(Mutex::new(ServerStats::default()));
    // Difficulty 4096 is each device's starting target; vardiff then moves it
    // toward 20 shares a minute. No nominal device rate is shown as measured.
    let settings = ServerConfig {
        network: config.network,
        payout: config.payout_address.clone(),
        authority_secret,
        share_target: compact_target(0x1b0ffff0)?,
        journal_path: config_path.with_extension("sv2-blocks.json"),
        source_identity: rpc.source_identity()?,
        donation: donation.clone(),
        #[cfg(test)]
        allocation_phase: None,
    };
    let worker = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || server::run(listener, rpc, settings, stop, stats))
    };
    let firmware = sv1_listener.map(|listener| {
        let stop = stop.clone();
        let stats = stats.clone();
        // Wildcard listeners are dialed through local loopback, never via an
        // arbitrary network route. The SV2 authority remains pinned.
        let mut upstream = bound;
        if upstream.ip().is_unspecified() {
            upstream.set_ip(if upstream.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        thread::spawn(move || super::sv1::run(listener, upstream, public, stop, stats))
    });
    // #### PR #40
    // Device reports every 15 seconds, outside the stats lock, from asic-rs
    // mixed with Pickaxe's own reader; all devices are asked in parallel.
    let fleet = Arc::new(Fleet::new());
    let reports = {
        let stop = stop.clone();
        let stats = stats.clone();
        let fleet = fleet.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let addresses = stats
                    .lock()
                    .map(|stats| stats.device_stats.addresses())
                    .unwrap_or_default();
                for (id, report) in fleet.poll(&addresses, &stop) {
                    if let Ok(mut stats) = stats.lock() {
                        stats.device_stats.set_report(id, report);
                    }
                }
                for _ in 0..150 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })
    };
    // #### PR #40
    // Tell the user where to point devices: a wildcard listener shows this
    // computer's address on the local network instead.
    let devices_hint = device_hint(bound, sv1_bound, lan_address());
    // SV2 reference authority public-key encoding: version 1 (little endian),
    // 32-byte x-only key, Base58Check. Only the public key is displayed.
    let mut encoded = vec![1, 0];
    encoded.extend(public);
    let authority = stratum_core::bitcoin::base58::encode_check(&encoded);
    let result = (|| {
        if terminal.is_none() {
            println!(
                "{}",
                serde_json::json!({"listen":bound.to_string(),"sv1_listen":sv1_bound.map(|address| address.to_string()),"authority":authority,"network":config.network.as_str()})
            );
        }
        let mut device_offset = 0usize;
        let mut setting_error = None;
        let mut advanced = false;
        // The workers table opens first; Tab switches to the overview.
        let mut overview = false;
        let status_file = status_path(config_path);
        let mut status_saved: Option<Instant> = None;
        let mut controls: Option<Controls> = None;
        while !stop.load(Ordering::Relaxed) && !worker.is_finished() {
            if firmware.as_ref().is_some_and(|worker| worker.is_finished()) {
                break;
            }
            let snapshot = stats
                .lock()
                .map_err(|_| "mining statistics unavailable")?
                .clone();
            let devices = snapshot.device_stats.snapshots(Instant::now());
            device_offset = device_offset.min(devices.len().saturating_sub(1));
            let donation_value = *donation
                .read()
                .map_err(|_| "donation setting unavailable")?;
            // #### PR #40
            // The status the JSON mode prints is also saved once a second
            // beside the config, so `stratum-v2 watch` can show the workers
            // table of a server running as a service. It never holds payouts
            // or credentials, and a failed save never stops mining.
            let status = status_json(config.network.as_str(), &snapshot, donation_value, &devices);
            if status_saved.is_none_or(|saved| saved.elapsed() >= Duration::from_secs(1)) {
                let _ = write_status(&status_file, &status);
                status_saved = Some(Instant::now());
            }
            if let Some(terminal) = terminal.as_mut() {
                let overview_text = format!(
                    "{} · Node {} · Height {} · Donation {}\nDevices {} · Sessions {} · Shares {} accepted / {} rejected ({} at SV1 adapter)\nBlocks {} accepted / {} pending / {} rejected · Retries {} · Last {}\nConnection errors: SV2 {} / SV1 {}\nTemplate errors {} · Last {}\nSV2 {} · SV1 {}\n{}",
                    config.network.as_str(), if snapshot.template_ready { "Ready" } else { "Waiting" },
                    snapshot.height.map(|height| height.to_string()).unwrap_or_else(|| "Waiting".into()), donation_summary(donation_value), snapshot.connections, snapshot.sessions_started,
                    snapshot.shares_accepted, snapshot.shares_rejected, snapshot.sv1_local_rejected, snapshot.blocks_accepted, snapshot.blocks_pending,
                    snapshot.blocks_rejected, snapshot.block_retries, snapshot.last_block_result.unwrap_or("Waiting"),
                    snapshot.connection_errors, snapshot.sv1_connection_errors,
                    snapshot.template_failures, snapshot.last_template_error.unwrap_or("None"), bound,
                    sv1_bound.map(|address| address.to_string()).unwrap_or_else(|| "Off".into()),
                    setting_error.map(str::to_owned).unwrap_or_else(|| format!("Authority {authority}")),
                );
                let online = devices.iter().filter(|device| device.connected).count();
                let total_rate: f64 = devices
                    .iter()
                    .filter(|device| device.connected)
                    .filter_map(|device| device.hashrate_estimate)
                    .sum();
                let header = format!(
                    "{} · Node {} · Height {} · Donation {}\n{online} of {} workers online · {} · Shares {} accepted / {} rejected · Blocks {} accepted / {} pending\n{devices_hint}",
                    config.network.as_str(),
                    if snapshot.template_ready { "Ready" } else { "Waiting" },
                    snapshot.height.map(|height| height.to_string()).unwrap_or_else(|| "Waiting".into()),
                    donation_summary(donation_value),
                    devices.len(),
                    crate::telemetry::format_hash_rate(total_rate),
                    snapshot.shares_accepted,
                    snapshot.shares_rejected,
                    snapshot.blocks_accepted,
                    snapshot.blocks_pending,
                );
                let lines: Vec<WorkerLine> = devices.iter().map(WorkerLine::from).collect();
                terminal
                    .terminal
                    .draw(|frame| {
                        if let Some(view) = controls.as_ref() {
                            render_controls(frame, view)
                        } else if advanced {
                            render_advanced(frame, donation_value, setting_error)
                        } else if overview {
                            render_dashboard(frame, &overview_text, &devices, device_offset)
                        } else {
                            render_workers(
                                frame,
                                &header,
                                &lines,
                                device_offset,
                                SERVE_FOOTER,
                                true,
                            )
                        }
                    })
                    .map_err(|_| "cannot draw mining dashboard")?;
                if event::poll(Duration::from_millis(500)).map_err(|_| "cannot read terminal")? {
                    if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                        if key.kind == KeyEventKind::Press {
                            if let Some(view) = controls.as_mut() {
                                if handle_controls_key(view, key.code, &fleet) {
                                    controls = None;
                                }
                                continue;
                            }
                            match key.code {
                                KeyCode::Char('c') | KeyCode::Char('C')
                                    if !advanced && !overview =>
                                {
                                    if let Some(device) =
                                        devices.get(device_offset).filter(|device| device.connected)
                                    {
                                        let address = stats.lock().ok().and_then(|stats| {
                                            stats.device_stats.address_for_label(&device.label)
                                        });
                                        controls = Some(Controls::new(device, address, &fleet));
                                    }
                                }
                                KeyCode::Char('a') | KeyCode::Char('A') => advanced = !advanced,
                                KeyCode::Esc => advanced = false,
                                KeyCode::Tab if !advanced => overview = !overview,
                                // The donation changes only in Advanced settings.
                                KeyCode::Char('+')
                                | KeyCode::Char('=')
                                | KeyCode::Char('-')
                                | KeyCode::Left
                                | KeyCode::Right
                                    if advanced =>
                                {
                                    let next = donation_value.adjusted(matches!(
                                        key.code,
                                        KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Right
                                    ));
                                    if next != donation_value {
                                        match save_donation(config_path, next) {
                                            Ok(()) => {
                                                *donation.write().map_err(|_| "donation setting unavailable")? = next;
                                                setting_error = None;
                                            }
                                            Err(()) => setting_error = Some("Could not save donation; previous setting retained"),
                                        }
                                    }
                                }
                                KeyCode::Char('q') => stop.store(true, Ordering::Relaxed),
                                KeyCode::Char('c')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    stop.store(true, Ordering::Relaxed)
                                }
                                KeyCode::Up => device_offset = device_offset.saturating_sub(1),
                                KeyCode::Down => device_offset = device_offset.saturating_add(1),
                                KeyCode::PageUp => device_offset = device_offset.saturating_sub(10),
                                KeyCode::PageDown => {
                                    device_offset = device_offset.saturating_add(10)
                                }
                                _ => (),
                            }
                        }
                    }
                }
            } else {
                println!("{status}");
                thread::sleep(Duration::from_secs(1));
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    let _ = reports.join();
    let firmware_result = firmware
        .map(|worker| {
            worker
                .join()
                .map_err(|_| "SV1 listener stopped unexpectedly".to_owned())
                .and_then(|r| r)
        })
        .unwrap_or(Ok(()));
    let server_result = worker
        .join()
        .map_err(|_| "mining server stopped unexpectedly")?;
    result.and(server_result).and(firmware_result)
}

fn save_donation(path: &Path, value: BchDonation) -> Result<(), ()> {
    // Preserve unrelated saved settings. Saving must succeed before new jobs
    // use the changed percentage; in-flight jobs retain their original policy.
    let mut saved = config::SavedConfig::load_optional(path)
        .map_err(|_| ())?
        .unwrap_or_default();
    saved.bch_donation_bps = Some(value);
    saved.save(path).map_err(|_| ())
}

/// The donation as the dashboard shows it, with its two parts.
fn donation_summary(donation: BchDonation) -> String {
    let (work, reward) = donation.shares();
    format!("{donation} ({work} of work · {reward} of block rewards)")
}

/// Advanced settings: the donation, adjustable from 0% to 100%.
fn render_advanced(frame: &mut Frame<'_>, donation: BchDonation, error: Option<&str>) {
    let (work, reward) = donation.shares();
    let mut text = format!(
        "Donation  {donation}\n\n{work} of mining work and {reward} of each block reward go to the \
         Pickaxe donation address.\nThe default is 1.50%; any setting from 0% to 100% works, in \
         0.5% steps. Changes apply to new jobs and are saved.\n\n←/→ or +/-  Change donation · \
         a or Esc  Back"
    );
    if let Some(error) = error {
        text.push_str(&format!("\n\n{error}"));
    }
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::bordered().title("Pickaxe · Advanced settings"))
            .wrap(Wrap { trim: false }),
        frame.area(),
    );
}

fn render_dashboard(
    frame: &mut Frame<'_>,
    status: &str,
    devices: &[DeviceSnapshot],
    offset: usize,
) {
    let areas = Layout::vertical([
        Constraint::Length(9),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(status)
            .block(Block::bordered().title("Pickaxe · BCH ASIC mining"))
            .wrap(Wrap { trim: false }),
        areas[0],
    );
    let rows = devices.iter().skip(offset).map(|device| {
        Row::new(vec![
            device.label.clone(),
            device.protocol.to_owned(),
            if device.connected {
                "Online"
            } else {
                "Offline"
            }
            .to_owned(),
            device.model.clone().unwrap_or_else(|| "—".into()),
            device
                .hashrate_estimate
                .map(crate::telemetry::format_hash_rate)
                .unwrap_or_else(|| "Measuring".into()),
            power(device.power_w),
            device.accepted.to_string(),
            device.rejected.to_string(),
            device
                .adapter_error
                .or(device.connection_error)
                .or(device.last_rejection)
                .unwrap_or("—")
                .to_owned(),
        ])
    });
    let title = format!(
        "Devices · {} sessions · {} onward",
        devices.len(),
        if devices.is_empty() { 0 } else { offset + 1 }
    );
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(3),
                Constraint::Length(7),
                Constraint::Length(24),
                Constraint::Length(12),
                Constraint::Length(7),
                Constraint::Length(8),
                Constraint::Length(8),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new([
                "Device",
                "Via",
                "State",
                "Model",
                "Est. 5m",
                "Power",
                "Accepted",
                "Rejected",
                "Last issue",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::bordered().title(title)),
        areas[1],
    );
    frame.render_widget(Paragraph::new("Tab  Workers · ↑/↓ PgUp/PgDn  Devices · a  Advanced settings · q  Stop server\nRate uses validated shares; 30s warm-up, up to 5m window."), areas[2]);
}

/// A device's reported power draw: "140 W".
fn power(watts: Option<f64>) -> String {
    watts
        .filter(|watts| watts.is_finite() && *watts > 0.0)
        .map(|watts| format!("{watts:.0} W"))
        .unwrap_or_else(|| "—".into())
}

/// Seconds since the last share, as "12s ago", "4m ago" or "2h ago".
fn ago(seconds: Option<u64>) -> String {
    match seconds {
        None => "—".into(),
        Some(s) if s < 60 => format!("{s}s ago"),
        Some(s) if s < 3600 => format!("{}m ago", s / 60),
        Some(s) => format!("{}h ago", s / 3600),
    }
}

/// A share difficulty in thousands, millions and so on: "4.10K".
fn format_difficulty(difficulty: Option<f64>) -> String {
    let Some(mut value) = difficulty.filter(|value| value.is_finite() && *value > 0.0) else {
        return "—".into();
    };
    for unit in ["", "K", "M", "G", "T"] {
        if value < 1000.0 {
            return if unit.is_empty() {
                format!("{value:.0}")
            } else {
                format!("{value:.2}{unit}")
            };
        }
        value /= 1000.0;
    }
    format!("{value:.2}P")
}

/// The workers table, laid out like a pool's worker list; the default page.
fn render_workers(
    frame: &mut Frame<'_>,
    header: &str,
    devices: &[WorkerLine],
    offset: usize,
    footer: &str,
    highlight: bool,
) {
    let areas = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(header)
            .block(Block::bordered().title("Pickaxe · BCH ASIC mining · Workers"))
            .wrap(Wrap { trim: false }),
        areas[0],
    );
    let rate = |value: Option<f64>| {
        value
            .map(crate::telemetry::format_hash_rate)
            .unwrap_or_else(|| "Measuring".into())
    };
    let rows = devices
        .iter()
        .skip(offset)
        .enumerate()
        .map(|(index, device)| {
            let total = device.accepted + device.rejected;
            let style = if highlight && index == 0 {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                device.label.clone(),
                if device.connected {
                    "Online"
                } else {
                    "Offline"
                }
                .to_owned(),
                rate(device.hashrate_estimate),
                rate(device.hashrate_hour),
                device
                    .reported_hashrate
                    .map(crate::telemetry::format_hash_rate)
                    .unwrap_or_else(|| "—".into()),
                device
                    .temperature_c
                    .map(|temperature| format!("{temperature:.0} °C"))
                    .unwrap_or_else(|| "—".into()),
                device.fan.clone().unwrap_or_else(|| "—".into()),
                device.accepted.to_string(),
                device.rejected.to_string(),
                if total == 0 {
                    "—".to_owned()
                } else {
                    format!("{:.2}%", device.rejected as f64 * 100.0 / total as f64)
                },
                ago(device.last_share_seconds),
                format_difficulty(device.difficulty),
                device.protocol.clone(),
                device
                    .adapter_error
                    .as_deref()
                    .or(device.connection_error.as_deref())
                    .or(device.last_rejection.as_deref())
                    .unwrap_or("—")
                    .to_owned(),
            ])
            .style(style)
        });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(4),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new([
                "Worker",
                "Status",
                "Now (5m)",
                "1 hour",
                "Device says",
                "Temp",
                "Fan",
                "Accepted",
                "Rejected",
                "Reject",
                "Last share",
                "Difficulty",
                "Via",
                "Last issue",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::bordered().title(format!(
            "Workers · {} · {} onward",
            devices.len(),
            if devices.is_empty() { 0 } else { offset + 1 }
        ))),
        areas[1],
    );
    frame.render_widget(Paragraph::new(footer), areas[2]);
}

const SERVE_FOOTER: &str = "Tab  Overview · ↑/↓ PgUp/PgDn  Scroll · c  Controls (top row) · a  Advanced settings · q  Stop server\nNow and 1 hour come from validated shares (30s warm-up); Device says, Temp and Fan are the device's own report.";
const WATCH_FOOTER: &str = "↑/↓ PgUp/PgDn  Scroll · q  Quit (the server keeps running)\nRead-only view of the server's saved status; Now and 1 hour come from validated shares.";

/// One row of the workers table, from the live server or its saved status.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
struct WorkerLine {
    label: String,
    protocol: String,
    connected: bool,
    accepted: u64,
    rejected: u64,
    last_rejection: Option<String>,
    hashrate_estimate: Option<f64>,
    hashrate_hour: Option<f64>,
    difficulty: Option<f64>,
    last_share_seconds: Option<u64>,
    connection_error: Option<String>,
    adapter_error: Option<String>,
    reported_hashrate: Option<f64>,
    temperature_c: Option<f64>,
    fan: Option<String>,
    model: Option<String>,
    firmware: Option<String>,
    power_w: Option<f64>,
}

impl From<&DeviceSnapshot> for WorkerLine {
    fn from(device: &DeviceSnapshot) -> Self {
        Self {
            label: device.label.clone(),
            protocol: device.protocol.to_owned(),
            connected: device.connected,
            accepted: device.accepted,
            rejected: device.rejected,
            last_rejection: device.last_rejection.map(str::to_owned),
            hashrate_estimate: device.hashrate_estimate,
            hashrate_hour: device.hashrate_hour,
            difficulty: device.difficulty,
            last_share_seconds: device.last_share_seconds,
            connection_error: device.connection_error.map(str::to_owned),
            adapter_error: device.adapter_error.map(str::to_owned),
            reported_hashrate: device.reported_hashrate,
            temperature_c: device.temperature_c,
            fan: device.fan.clone(),
            model: device.model.clone(),
            firmware: device.firmware.clone(),
            power_w: device.power_w,
        }
    }
}

/// This computer's address on the local network. No packet is sent:
/// connecting a UDP socket only chooses the outgoing interface.
fn lan_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// Where devices connect, with wildcard listeners shown as this computer's
/// local network address.
fn device_hint(
    sv2: std::net::SocketAddr,
    sv1: Option<std::net::SocketAddr>,
    lan: Option<std::net::IpAddr>,
) -> String {
    let shown = |address: std::net::SocketAddr| match lan {
        Some(ip) if address.ip().is_unspecified() => std::net::SocketAddr::new(ip, address.port()),
        _ => address,
    };
    match sv1 {
        Some(sv1) => format!(
            "Point devices at: SV1 stratum+tcp://{} · SV2 {}",
            shown(sv1),
            shown(sv2)
        ),
        None => format!("Point devices at: SV2 {}", shown(sv2)),
    }
}

/// #### PR #40
/// The controls page for one worker: choose an action, confirm it, and read
/// the device's reply. The actions are the ones this device offers (see
/// `Fleet::actions`); they run in the background so the page stays live.
struct Controls {
    label: String,
    /// Model, firmware and power, as the device reports them.
    details: String,
    address: Option<std::net::IpAddr>,
    actions: Vec<DeviceAction>,
    confirming: Option<DeviceAction>,
    reply: Arc<Mutex<Option<String>>>,
}

impl Controls {
    fn new(device: &DeviceSnapshot, address: Option<std::net::IpAddr>, fleet: &Fleet) -> Self {
        let details = [
            device.model.clone(),
            device
                .firmware
                .as_ref()
                .map(|firmware| format!("firmware {firmware}")),
            device.power_w.map(|watts| power(Some(watts))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        Self {
            label: device.label.clone(),
            details,
            address,
            actions: address.map_or_else(|| DeviceAction::OWN.to_vec(), |ip| fleet.actions(ip)),
            confirming: None,
            reply: Arc::new(Mutex::new(None)),
        }
    }

    fn set_reply(&self, text: String) {
        if let Ok(mut reply) = self.reply.lock() {
            *reply = Some(text);
        }
    }
}

/// Handles one key on the controls page; true closes it.
fn handle_controls_key(view: &mut Controls, code: KeyCode, fleet: &Arc<Fleet>) -> bool {
    match (view.confirming, code) {
        (_, KeyCode::Esc) => return true,
        (None, KeyCode::Char(choice @ '1'..='9')) => {
            view.confirming = view.actions.get(usize::from(choice as u8 - b'1')).copied();
        }
        (Some(action), KeyCode::Char('y') | KeyCode::Char('Y')) => {
            view.confirming = None;
            match view.address {
                None => view.set_reply(
                    "This worker's network address is not known, so it cannot be controlled."
                        .into(),
                ),
                Some(ip) => {
                    view.set_reply(format!("Sending: {}…", action.label()));
                    let reply = Arc::clone(&view.reply);
                    let fleet = Arc::clone(fleet);
                    thread::spawn(move || {
                        let text = match fleet.control(ip, action) {
                            Ok(message) => format!("{}: {message}", action.label()),
                            Err(error) => format!("{}: not done; {error}", action.label()),
                        };
                        if let Ok(mut reply) = reply.lock() {
                            *reply = Some(text);
                        }
                    });
                }
            }
        }
        (Some(_), _) => view.confirming = None,
        _ => {}
    }
    false
}

fn render_controls(frame: &mut Frame<'_>, view: &Controls) {
    let mut text = format!("Worker  {}\n", view.label);
    if !view.details.is_empty() {
        text.push_str(&format!("{}\n", view.details));
    }
    text.push('\n');
    match view.confirming {
        Some(action) => text.push_str(&format!(
            "{} {}?\n\ny  Yes · any other key  No\n",
            action.label(),
            view.label
        )),
        None => {
            for (index, action) in view.actions.iter().enumerate() {
                text.push_str(&format!("{}  {}\n", index + 1, action.label()));
            }
            text.push_str("\nEsc  Back to workers\n");
        }
    }
    if let Some(reply) = view.reply.lock().ok().and_then(|reply| reply.clone()) {
        text.push_str(&format!("\n{reply}\n"));
    }
    text.push_str(
        "\nActions go to the device's own API on your local network and each needs your confirmation. The list is what this device's make and firmware support through asic-rs, plus Avalon work levels; a device not yet identified offers Restart and work levels.",
    );
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::bordered().title("Pickaxe · Device controls"))
            .wrap(Wrap { trim: false }),
        frame.area(),
    );
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Where the server saves its status for `stratum-v2 watch`: beside the
/// config, like the authority key and the block journal.
fn status_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("sv2-status.json")
}

/// The status published each second: printed in JSON mode and saved for
/// `stratum-v2 watch`. It never contains payouts or credentials.
fn status_json(
    network: &str,
    snapshot: &ServerStats,
    donation: BchDonation,
    devices: &[DeviceSnapshot],
) -> serde_json::Value {
    serde_json::json!({
        "network": network,
        "updated": unix_now(),
        "ready": snapshot.template_ready,
        "height": snapshot.height,
        "donation": donation.to_string(),
        "donation_summary": donation_summary(donation),
        "devices": snapshot.connections,
        "shares_accepted": snapshot.shares_accepted,
        "shares_rejected": snapshot.shares_rejected,
        "blocks_accepted": snapshot.blocks_accepted,
        "blocks_unconfirmed": snapshot.blocks_pending,
        "blocks_pending": snapshot.blocks_pending,
        "blocks_rejected": snapshot.blocks_rejected,
        "block_retries": snapshot.block_retries,
        "last_block_result": snapshot.last_block_result,
        "connection_errors": snapshot.connection_errors,
        "sv1_connection_errors": snapshot.sv1_connection_errors,
        "template_failures": snapshot.template_failures,
        "last_template_error": snapshot.last_template_error,
        "sv1_local_rejected": snapshot.sv1_local_rejected,
        "sessions_started": snapshot.sessions_started,
        "device_details": devices,
    })
}

/// Replaces the status file whole, so a reader never sees half of it.
fn write_status(path: &Path, status: &serde_json::Value) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{:016x}", rand::random::<u64>()));
    let temp = PathBuf::from(temp);
    fs::write(&temp, status.to_string())?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// The saved status as `stratum-v2 watch` reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WatchStatus {
    network: String,
    updated: u64,
    ready: bool,
    height: Option<u32>,
    donation: String,
    donation_summary: Option<String>,
    shares_accepted: u64,
    shares_rejected: u64,
    blocks_accepted: u64,
    blocks_pending: usize,
    device_details: Vec<WorkerLine>,
}

impl WatchStatus {
    /// The workers page header, with how old the saved status is.
    fn header(&self, now: u64) -> String {
        let age = now.saturating_sub(self.updated);
        let online = self
            .device_details
            .iter()
            .filter(|row| row.connected)
            .count();
        let rate: f64 = self
            .device_details
            .iter()
            .filter(|row| row.connected)
            .filter_map(|row| row.hashrate_estimate)
            .sum();
        let freshness = if age > 5 {
            format!("Server not updating; last status {}", ago(Some(age)))
        } else {
            "Live".to_owned()
        };
        format!(
            "{} · Node {} · Height {} · Donation {} · {freshness}\n{online} of {} workers online · {} · Shares {} accepted / {} rejected · Blocks {} accepted / {} pending",
            self.network,
            if self.ready { "Ready" } else { "Waiting" },
            self.height
                .map(|height| height.to_string())
                .unwrap_or_else(|| "Waiting".into()),
            self.donation_summary.as_deref().unwrap_or(&self.donation),
            self.device_details.len(),
            crate::telemetry::format_hash_rate(rate),
            self.shares_accepted,
            self.shares_rejected,
            self.blocks_accepted,
            self.blocks_pending,
        )
    }

    /// The rows, with share ages counted to now rather than to the save.
    fn rows(&self, now: u64) -> Vec<WorkerLine> {
        let age = now.saturating_sub(self.updated);
        self.device_details
            .iter()
            .cloned()
            .map(|mut row| {
                row.last_share_seconds = row.last_share_seconds.map(|s| s.saturating_add(age));
                row
            })
            .collect()
    }
}

/// #### PR #40
/// `stratum-v2 watch`: the workers table of a server running elsewhere on
/// this machine, such as a service started with --no-tui. Read-only: it reads
/// the status the server saves each second and never touches the server.
fn watch(path: &Path) -> Result<(), String> {
    let mut terminal = TerminalSession::enter()?;
    let mut offset = 0usize;
    loop {
        let now = unix_now();
        let status = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<WatchStatus>(&text).ok());
        let (header, rows) = match &status {
            Some(status) => (status.header(now), status.rows(now)),
            None => (
                format!(
                    "Waiting for the server's status at {}\nStart the server, or pass the same --config it uses.",
                    path.display()
                ),
                Vec::new(),
            ),
        };
        offset = offset.min(rows.len().saturating_sub(1));
        terminal
            .terminal
            .draw(|frame| render_workers(frame, &header, &rows, offset, WATCH_FOOTER, false))
            .map_err(|_| "cannot draw workers table")?;
        if event::poll(Duration::from_secs(1)).map_err(|_| "cannot read terminal")? {
            if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Up => offset = offset.saturating_sub(1),
                        KeyCode::Down => offset = offset.saturating_add(1),
                        KeyCode::PageUp => offset = offset.saturating_sub(10),
                        KeyCode::PageDown => offset = offset.saturating_add(10),
                        _ => (),
                    }
                }
            }
        }
    }
    Ok(())
}

fn preflight(config: &RuntimeConfig) -> Result<(NativeNodeRpc, BchTemplate), String> {
    let endpoints = config.custom_node_endpoints();
    if endpoints.is_empty() {
        return Err("configure your BCHN RPC connection before starting BCH ASIC mining".into());
    }
    for endpoint in endpoints {
        let mut provider =
            TemplateProvider::new(NativeNodeRpc::new(endpoint.to_owned()), config.network);
        if let Ok((_, template)) = provider.refresh() {
            return Ok((NativeNodeRpc::new(endpoint.to_owned()), template.clone()));
        }
    }
    Err("no configured BCH node supplied a synchronized template for the selected network".into())
}

fn load_authority(path: &Path) -> Result<[u8; 32], String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|_| "cannot create authority directory")?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            // Lock down the empty file before any secret bytes are written.
            config::restrict_private_config(path).map_err(|_| "cannot protect authority file")?;
            let secret = loop {
                let candidate = rand::random::<[u8; 32]>();
                if server::authority_public(&candidate).is_ok() {
                    break candidate;
                }
            };
            file.write_all(&secret)
                .and_then(|()| file.sync_all())
                .map_err(|_| "cannot save authority key")?;
            Ok(secret)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| "cannot inspect authority file")?;
            if !metadata.is_file() || metadata.len() != 32 {
                return Err("invalid authority file; refusing to replace it".into());
            }
            config::restrict_private_config(path).map_err(|_| "cannot protect authority file")?;
            let mut secret = [0; 32];
            fs::File::open(path)
                .and_then(|mut file| file.read_exact(&mut secret))
                .map_err(|_| "cannot read authority key")?;
            server::authority_public(&secret)?;
            Ok(secret)
        }
        Err(_) => Err("cannot create authority file".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn donation_control_persists_only_the_bch_setting_and_fails_closed() {
        let dir = super::super::journal::TestDirectory::new();
        let path = dir.0.join("settings.json");
        let mut original = config::SavedConfig {
            network: Some("chipnet".into()),
            intensity: Some(70),
            ..Default::default()
        };
        original.save(&path).unwrap();
        let rate = "2.01".parse().unwrap();
        save_donation(&path, rate).unwrap();
        original.bch_donation_bps = Some(rate);
        assert_eq!(config::SavedConfig::load(&path).unwrap(), original);
        let mut runtime = RuntimeConfig::default();
        original.apply_to_runtime(&mut runtime).unwrap();
        assert_eq!(runtime.bch_donation, rate);
        assert_eq!(
            runtime.token.fee_policy(runtime.network).scheme.work(),
            [400, 0]
        );
        fs::write(&path, b"broken config").unwrap();
        assert!(save_donation(&path, BchDonation::default()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken config");
    }

    #[test]
    fn workers_page_lists_each_worker_like_a_pool() {
        use super::super::telemetry::{Devices, ShareEvent};
        use ratatui::{backend::TestBackend, Terminal};
        let start = Instant::now();
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:1000".parse().unwrap(), true, start);
        let mut target = [0xff; 32];
        target[26..32].fill(0);
        for second in 1..=40 {
            devices.share(
                first,
                ShareEvent::Accepted(target),
                false,
                start + Duration::from_secs(second),
            );
        }
        devices.share(first, ShareEvent::Rejected("stale job"), true, start);
        let rows: Vec<WorkerLine> = devices
            .snapshots(start + Duration::from_secs(45))
            .iter()
            .map(WorkerLine::from)
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(180, 20)).unwrap();
        terminal
            .draw(|f| {
                render_workers(
                    f,
                    "Chipnet · Node Ready · 1 of 1 workers online",
                    &rows,
                    0,
                    SERVE_FOOTER,
                    true,
                )
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        for column in [
            "Worker",
            "Now (5m)",
            "1 hour",
            "Device says",
            "Temp",
            "Fan",
            "Reject",
            "Last share",
            "Difficulty",
        ] {
            assert!(text.contains(column), "{column}");
        }
        assert!(text.contains(&rows[0].label));
        assert!(text.contains("2.44%"));
        assert!(text.contains("5s ago"));
        assert!(text.contains("Tab  Overview"));
        assert_eq!(format_difficulty(Some(4096.0)), "4.10K");
        assert_eq!(format_difficulty(Some(512.0)), "512");
        assert_eq!(format_difficulty(None), "—");
        assert_eq!(ago(Some(250)), "4m ago");
    }

    #[test]
    fn controls_need_a_choice_and_a_confirmation() {
        use ratatui::{backend::TestBackend, Terminal};
        let fleet = Arc::new(Fleet::new());
        let mut stats = ServerStats::default();
        stats
            .device_stats
            .connect("127.0.0.1:1000".parse().unwrap(), true, Instant::now());
        let mut device = stats.device_stats.snapshots(Instant::now()).remove(0);
        device.model = Some("Avalonminer AvalonNano3s".into());
        device.power_w = Some(140.0);
        let mut view = Controls::new(&device, None, &fleet);
        // A device not identified by asic-rs offers Pickaxe's own actions.
        assert_eq!(view.actions, DeviceAction::OWN.to_vec());
        assert_eq!(view.details, "Avalonminer AvalonNano3s · 140 W");
        // Choosing an action only asks for confirmation; a number past the
        // list chooses nothing.
        assert!(!handle_controls_key(&mut view, KeyCode::Char('9'), &fleet));
        assert_eq!(view.confirming, None);
        assert!(!handle_controls_key(&mut view, KeyCode::Char('2'), &fleet));
        assert_eq!(view.confirming, Some(DeviceAction::LowerPower));
        // Any key other than y cancels.
        handle_controls_key(&mut view, KeyCode::Char('n'), &fleet);
        assert_eq!(view.confirming, None);
        assert!(view.reply.lock().unwrap().is_none());
        // Confirmed, but without a known address nothing is sent.
        handle_controls_key(&mut view, KeyCode::Char('1'), &fleet);
        handle_controls_key(&mut view, KeyCode::Char('y'), &fleet);
        assert!(view
            .reply
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .contains("cannot be controlled"));
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| render_controls(f, &view)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Restart") && text.contains("Raise power"));
        assert!(text.contains("AvalonNano3s"));
        assert!(handle_controls_key(&mut view, KeyCode::Esc, &fleet));
    }

    #[test]
    fn devices_are_told_this_computers_network_address() {
        let lan = Some("192.168.0.160".parse().unwrap());
        assert_eq!(
            device_hint(
                "0.0.0.0:3336".parse().unwrap(),
                Some("0.0.0.0:3333".parse().unwrap()),
                lan
            ),
            "Point devices at: SV1 stratum+tcp://192.168.0.160:3333 · SV2 192.168.0.160:3336"
        );
        // Explicit listeners are shown as configured.
        assert_eq!(
            device_hint("127.0.0.1:3336".parse().unwrap(), None, lan),
            "Point devices at: SV2 127.0.0.1:3336"
        );
        assert_eq!(
            device_hint("0.0.0.0:3336".parse().unwrap(), None, None),
            "Point devices at: SV2 0.0.0.0:3336"
        );
    }

    #[test]
    fn saved_status_shows_the_same_workers_read_only() {
        use super::super::{device_api::DeviceReport, telemetry::ShareEvent};
        use ratatui::{backend::TestBackend, Terminal};
        let start = Instant::now();
        let mut stats = ServerStats::default();
        let id = stats
            .device_stats
            .connect("127.0.0.1:1000".parse().unwrap(), true, start);
        let mut target = [0xff; 32];
        target[26..32].fill(0);
        stats.device_stats.share(
            id,
            ShareEvent::Accepted(target),
            false,
            start + Duration::from_secs(1),
        );
        stats.device_stats.set_report(
            id,
            Some(DeviceReport {
                hashrate: Some(4.0e12),
                temperature_c: Some(61.0),
                fan: Some("40%".into()),
                model: Some("Avalonminer AvalonNano3s".into()),
                ..DeviceReport::default()
            }),
        );
        stats.template_ready = true;
        stats.height = Some(326930);
        stats.shares_accepted = 1;
        let devices = stats
            .device_stats
            .snapshots(start + Duration::from_secs(40));
        let status = status_json("chipnet", &stats, BchDonation::default(), &devices);
        let dir = super::super::journal::TestDirectory::new();
        let path = status_path(&dir.0.join("chipnet.json"));
        write_status(&path, &status).unwrap();
        write_status(&path, &status).unwrap();
        // Replaced whole each time, with no temporary file left behind.
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        let saved: WatchStatus = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let updated = saved.updated;
        assert_eq!(
            saved.rows(updated),
            devices.iter().map(WorkerLine::from).collect::<Vec<_>>()
        );
        let header = saved.header(updated);
        for part in [
            "chipnet",
            "Height 326930",
            "0.50% of work",
            "Live",
            "1 of 1 workers",
        ] {
            assert!(header.contains(part), "{part}");
        }
        assert!(saved.header(updated + 60).contains("Server not updating"));
        // Share ages count on from the save.
        assert_eq!(
            saved.rows(updated + 10)[0].last_share_seconds,
            devices[0].last_share_seconds.map(|s| s + 10)
        );
        let mut terminal = Terminal::new(TestBackend::new(180, 20)).unwrap();
        terminal
            .draw(|f| render_workers(f, &header, &saved.rows(updated), 0, WATCH_FOOTER, false))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains(&devices[0].label));
        assert!(text.contains("61 °C"));
        assert!(text.contains("q  Quit"));
        assert!(!text.contains("Stop server"));
    }

    #[test]
    fn donation_shows_its_parts_and_changes_only_in_advanced_settings() {
        use ratatui::{backend::TestBackend, Terminal};
        assert_eq!(
            donation_summary(BchDonation::default()),
            "1.50% (0.50% of work · 1.00% of block rewards)"
        );
        let two: BchDonation = "2".parse().unwrap();
        assert_eq!(
            donation_summary(two),
            "2.00% (0.67% of work · 1.34% of block rewards)"
        );
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|f| render_advanced(f, BchDonation::default(), Some("Could not save donation")))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Advanced settings"));
        assert!(text.contains("Donation  1.50%"));
        assert!(text.contains("0.50% of mining work and 1.00% of each block reward"));
        assert!(text.contains("from 0% to 100%"));
        assert!(text.contains("Could not save donation"));
    }

    #[test]
    fn device_dashboard_renders_estimates_failures_and_scrolled_sessions() {
        use super::super::telemetry::{Devices, ShareEvent};
        use ratatui::{backend::TestBackend, Terminal};
        let start = Instant::now();
        let mut devices = Devices::default();
        let first = devices.connect("127.0.0.1:1000".parse().unwrap(), true, start);
        let second = devices.connect("127.0.0.1:1001".parse().unwrap(), false, start);
        let mut target = [0; 32];
        target[26] = 1;
        devices.share(first, ShareEvent::Accepted(target), false, start);
        devices.share(second, ShareEvent::Rejected("stale job"), true, start);
        let rows = devices.snapshots(start + Duration::from_secs(60));
        for width in [100, 140] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|f| render_dashboard(f, "Chipnet · Node Ready · Donation 1.5%", &rows, 0))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains(&rows[0].label));
            assert!(text.contains(&rows[1].label));
            assert!(text.contains("Est. 5m"));
            assert!(text.contains("stale job"));
            assert!(text.contains("TH/s"));
            assert!(text.contains("Donation 1.5%"));
            assert!(text.contains("a  Advanced settings"));
            assert!(!text.contains("+/- Donation"));
            assert!(!text.contains("2T/3"));
            terminal
                .draw(|f| render_dashboard(f, "Chipnet · Node Ready", &rows, 1))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(!text.contains(&rows[0].label));
            assert!(text.contains(&rows[1].label));
        }
    }

    #[test]
    fn authority_is_reused_and_invalid_files_are_not_replaced() {
        let path =
            std::env::temp_dir().join(format!("pickaxe-sv2-authority-{}", rand::random::<u64>()));
        let first = load_authority(&path).unwrap();
        assert_eq!(load_authority(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&path, b"bad").unwrap();
        assert!(load_authority(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad");
        fs::remove_file(path).unwrap();
    }
}
