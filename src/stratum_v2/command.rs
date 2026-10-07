//! #### PR #38
//! Native node preflight and mining-server lifecycle. Reuse the terminal guard,
//! keep authority secrets private, and never print node credentials or payouts.

use super::{
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
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant},
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
    // Difficulty 4096 is a starting target. Device vardiff remains a separate
    // capability; no nominal device rate is reported as measured hashrate.
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
            if let Some(terminal) = terminal.as_mut() {
                let status = format!(
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
                    "{} · Node {} · Height {} · Donation {}\n{online} of {} workers online · {} · Shares {} accepted / {} rejected · Blocks {} accepted / {} pending",
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
                terminal
                    .terminal
                    .draw(|frame| {
                        if advanced {
                            render_advanced(frame, donation_value, setting_error)
                        } else if overview {
                            render_dashboard(frame, &status, &devices, device_offset)
                        } else {
                            render_workers(frame, &header, &devices, device_offset)
                        }
                    })
                    .map_err(|_| "cannot draw mining dashboard")?;
                if event::poll(Duration::from_millis(500)).map_err(|_| "cannot read terminal")? {
                    if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                        if key.kind == KeyEventKind::Press {
                            match key.code {
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
                println!(
                    "{}",
                    serde_json::json!({"ready":snapshot.template_ready,"height":snapshot.height,
                    "donation":donation_value.to_string(),
                    "devices":snapshot.connections,"shares_accepted":snapshot.shares_accepted,"shares_rejected":snapshot.shares_rejected,
                    "blocks_accepted":snapshot.blocks_accepted,"blocks_unconfirmed":snapshot.blocks_pending,
                    "blocks_pending":snapshot.blocks_pending,"blocks_rejected":snapshot.blocks_rejected,
                    "block_retries":snapshot.block_retries,"last_block_result":snapshot.last_block_result,
                    "connection_errors":snapshot.connection_errors,"sv1_connection_errors":snapshot.sv1_connection_errors,
                    "template_failures":snapshot.template_failures,"last_template_error":snapshot.last_template_error,
                    "sv1_local_rejected":snapshot.sv1_local_rejected,"sessions_started":snapshot.sessions_started,
                    "device_details":devices})
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
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
            device
                .hashrate_estimate
                .map(crate::telemetry::format_hash_rate)
                .unwrap_or_else(|| "Measuring".into()),
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
                Constraint::Length(12),
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
                "Est. 5m",
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
fn render_workers(frame: &mut Frame<'_>, header: &str, devices: &[DeviceSnapshot], offset: usize) {
    let areas = Layout::vertical([
        Constraint::Length(4),
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
    let rows = devices.iter().skip(offset).map(|device| {
        let total = device.accepted + device.rejected;
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
            device.accepted.to_string(),
            device.rejected.to_string(),
            if total == 0 {
                "—".to_owned()
            } else {
                format!("{:.2}%", device.rejected as f64 * 100.0 / total as f64)
            },
            ago(device.last_share_seconds),
            format_difficulty(device.difficulty),
            device.protocol.to_owned(),
            device
                .adapter_error
                .or(device.connection_error)
                .or(device.last_rejection)
                .unwrap_or("—")
                .to_owned(),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(20),
                Constraint::Length(7),
                Constraint::Length(11),
                Constraint::Length(11),
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
    frame.render_widget(
        Paragraph::new(
            "Tab  Overview · ↑/↓ PgUp/PgDn  Scroll · a  Advanced settings · q  Stop server\nRates come from validated shares: 30s warm-up, then up to 5 minutes and up to 1 hour.",
        ),
        areas[2],
    );
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
        let rows = devices.snapshots(start + Duration::from_secs(45));
        let mut terminal = Terminal::new(TestBackend::new(150, 20)).unwrap();
        terminal
            .draw(|f| render_workers(f, "Chipnet · Node Ready · 1 of 1 workers online", &rows, 0))
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
