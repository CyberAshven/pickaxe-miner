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
        Arc, Mutex,
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
    let StratumV2Command::Serve { listen, sv1_listen } = action else {
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
            if let Some(terminal) = terminal.as_mut() {
                let status = format!(
                    "{} · Node {} · Height {}\nDevices {} · Sessions {} · Shares {} accepted / {} rejected ({} at SV1 adapter)\nBlocks {} accepted / {} pending / {} rejected · Retries {} · Last {}\nConnection errors: SV2 {} / SV1 {}\nTemplate errors {} · Last {}\nSV2 {} · SV1 {}\nAuthority {}",
                    config.network.as_str(), if snapshot.template_ready { "Ready" } else { "Waiting" },
                    snapshot.height.map(|height| height.to_string()).unwrap_or_else(|| "Waiting".into()), snapshot.connections, snapshot.sessions_started,
                    snapshot.shares_accepted, snapshot.shares_rejected, snapshot.sv1_local_rejected, snapshot.blocks_accepted, snapshot.blocks_pending,
                    snapshot.blocks_rejected, snapshot.block_retries, snapshot.last_block_result.unwrap_or("Waiting"),
                    snapshot.connection_errors, snapshot.sv1_connection_errors,
                    snapshot.template_failures, snapshot.last_template_error.unwrap_or("None"), bound,
                    sv1_bound.map(|address| address.to_string()).unwrap_or_else(|| "Off".into()), authority,
                );
                terminal
                    .terminal
                    .draw(|frame| render_dashboard(frame, &status, &devices, device_offset))
                    .map_err(|_| "cannot draw mining dashboard")?;
                if event::poll(Duration::from_millis(500)).map_err(|_| "cannot read terminal")? {
                    if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                        if key.kind == KeyEventKind::Press {
                            match key.code {
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
    frame.render_widget(Paragraph::new("↑/↓ PgUp/PgDn  Devices · q  Stop server\nRate uses validated shares; 30s warm-up, up to 5m window."), areas[2]);
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
                .draw(|f| render_dashboard(f, "Chipnet · Node Ready", &rows, 0))
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
