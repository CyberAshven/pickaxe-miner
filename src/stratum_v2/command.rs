//! #### PR #38
//! Native node preflight and mining-server lifecycle. Reuse the terminal guard,
//! keep authority secrets private, and never print node credentials or payouts.

use super::{
    fleet::Fleet,
    panel::{DevicePanel, Selection},
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
    widgets::{Block, Paragraph, Row, Table, TableState, Wrap},
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
    // #### PR #40
    // Pool mode: SV1 devices mine at a remote SV2 pool through the adapter,
    // and no local node or SV2 server runs.
    let pools = match &action {
        StratumV2Command::Serve {
            upstream,
            upstream_key,
            upstream_user,
            ..
        } if !upstream.is_empty() => {
            pool_upstreams(config, upstream, upstream_key, upstream_user.as_deref())?
        }
        _ => Vec::new(),
    };
    // #### PR #40
    // A public pool: each miner is paid at the address they connect with,
    // and the operator's fee comes off what the donation leaves.
    let public = match &action {
        StratumV2Command::Serve {
            public: true,
            pool_fee,
            pool_fee_mode,
            pool_fee_address,
            ..
        } => Some(super::payout::PublicPool {
            fee: pool_fee.filter(|rate| u16::from(*rate) > 0).map(|rate| {
                crate::donation::bch::PoolFee {
                    rate,
                    mode: pool_fee_mode.unwrap_or(crate::donation::bch::FeeMode::Coinbase),
                }
            }),
            address: config::validate_coinbase_address(
                config.network,
                pool_fee_address
                    .as_deref()
                    .unwrap_or(config.payout_address.as_str()),
            )
            .map_err(|_| "the pool fee address must be a q or p address on this network")?,
        }),
        _ => None,
    };
    if matches!(action, StratumV2Command::Serve { .. }) && pools.is_empty() {
        config::validate_payout_address(config.network, &config.payout_address)
            .map_err(|_| "a valid payout for the selected network is required")?;
    }
    let node = if pools.is_empty() {
        Some(preflight(config)?)
    } else {
        None
    };
    // #### PR #40
    // The node's client and version, such as "Bitcoin Cash Node 29.1.0",
    // for check-node, the dashboard and the saved status.
    let node_client = node
        .as_ref()
        .and_then(|(nodes, _)| nodes.first())
        .and_then(|rpc| rpc.info().ok())
        .map(|info| info.client);
    let StratumV2Command::Serve {
        listen,
        sv1_listen,
        donation,
        pool_tag,
        start_difficulty,
        ..
    } = action
    else {
        if let Some((_, template)) = &node {
            println!(
                "{}",
                serde_json::json!({
                    "network":config.network.as_str(),"node":node_client,"template_height":template.height,
                    "transactions":template.transaction_count(),"bits":format!("{:08x}", template.bits),
                    "size_limit":template.size_limit,"ready":true,"mining":false,
                })
            );
        }
        return Ok(());
    };
    // #### PR #40: where each device's difficulty starts.
    let share_target = match start_difficulty {
        None => compact_target(0x1b0ffff0)?,
        Some(difficulty) => difficulty_target(difficulty)?,
    };
    // #### PR #40: the pool's name, at most 20 printable characters.
    let pool_tag: Vec<u8> = match pool_tag.as_deref().map(str::trim) {
        None | Some("") => Vec::new(),
        Some(tag) if tag.len() <= 20 && tag.chars().all(|c| c.is_ascii_graphic() || c == ' ') => {
            tag.as_bytes().to_vec()
        }
        Some(_) => return Err("--pool-tag must be 1 to 20 printable characters".into()),
    };
    let donation = Arc::new(RwLock::new(donation.unwrap_or(config.bch_donation)));
    // #### PR #40
    // At a pool the donation is the setting's share of mining time under the
    // donation address, on a second channel at the same pool.
    let pools: Vec<super::sv1::Upstream> = pools
        .into_iter()
        .map(|pool| super::sv1::Upstream {
            donation: Some(super::sv1::DonationRoute {
                identity: crate::donation::bch::address(config.network).to_owned(),
                rate: donation.clone(),
            }),
            ..pool
        })
        .collect();
    let listener = node
        .is_some()
        .then(|| TcpListener::bind(listen))
        .transpose()
        .map_err(|_| "cannot bind mining listener")?;
    let bound = listener
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()
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
    let authority_secret = node
        .is_some()
        .then(|| load_authority(&config_path.with_extension("sv2-key")))
        .transpose()?;
    let public_key = authority_secret
        .as_ref()
        .map(server::authority_public)
        .transpose()?;
    // The SV1 adapter's upstreams: the pools in failover order, or this
    // server's own listener. Wildcard listeners are dialed through local
    // loopback, never via an arbitrary network route. The SV2 authority
    // remains pinned.
    let public_pool = public.clone();
    let upstreams = match (bound, public_key) {
        _ if !pools.is_empty() => pools.clone(),
        (Some(mut local), Some(public)) => {
            if local.ip().is_unspecified() {
                local.set_ip(if local.is_ipv4() {
                    std::net::Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                });
            }
            // #### PR #40: a public pool's devices open their channel
            // under their own username.
            vec![if public_pool.is_some() {
                super::sv1::Upstream::local_public(local, public)
            } else {
                super::sv1::Upstream::local(local, public)
            }]
        }
        _ => return Err("mining server unavailable".into()),
    };
    // The pools as the dashboard and status show them; never the identity.
    // #### PR #40: a public pool says so on the dashboard, with its fee.
    let pool_suffix = public_pool_suffix(public.as_ref());
    let pool_address = (!pools.is_empty()).then(|| {
        pools
            .iter()
            .map(|pool| pool.address.as_str())
            .collect::<Vec<_>>()
            .join(" → ")
    });
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
    let worker = match (node, listener, authority_secret) {
        (Some((nodes, _)), Some(listener), Some(authority_secret)) => {
            // Difficulty 4096 is each device's starting target; vardiff then
            // moves it toward 20 shares a minute. No nominal device rate is
            // shown as measured.
            let settings = ServerConfig {
                network: config.network,
                payout: config.payout_address.clone(),
                authority_secret,
                share_target,
                journal_path: config_path.with_extension("sv2-blocks.json"),
                pool_tag: pool_tag.clone(),
                legacy_sources: nodes
                    .iter()
                    .map(NativeNodeRpc::source_identity)
                    .collect::<Result<_, _>>()?,
                public: public.clone(),
                donation: donation.clone(),
                #[cfg(test)]
                allocation_phase: None,
            };
            let stop = stop.clone();
            let stats = stats.clone();
            Some(thread::spawn(move || {
                server::run(listener, nodes, settings, stop, stats)
            }))
        }
        _ => None,
    };
    let firmware = sv1_listener.map(|listener| {
        let stop = stop.clone();
        let stats = stats.clone();
        let upstreams = upstreams.clone();
        thread::spawn(move || super::sv1::run(listener, upstreams, stop, stats))
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
    // SV2 reference authority public-key encoding: version 1 (little endian),
    // 32-byte x-only key, Base58Check. Only the public key is displayed.
    let authority = public_key.map(|key| {
        let mut encoded = vec![1, 0];
        encoded.extend(key);
        stratum_core::bitcoin::base58::encode_check(&encoded)
    });
    // #### PR #40
    // Tell the user where to point devices: a wildcard listener shows this
    // computer's address on the local network instead, and the Connection
    // info page (`i`) every address, Tailscale's included, ready to copy.
    let interfaces = crate::reach::Interfaces::detect();
    let devices_hint = device_hint(bound, sv1_bound, interfaces);
    let serve_mode = if !pools.is_empty() {
        ServeMode::JoinPool
    } else if public_pool.is_some() {
        ServeMode::Public
    } else {
        ServeMode::Solo
    };
    let result = (|| {
        if terminal.is_none() {
            // The pool's identity may be a payout address: never printed.
            println!(
                "{}",
                serde_json::json!({"listen":bound.map(|address| address.to_string()),"sv1_listen":sv1_bound.map(|address| address.to_string()),"authority":authority,"upstream":pool_address,"network":config.network.as_str(),"connect":connect_json(&connect_lines(bound, sv1_bound, authority.as_deref(), interfaces))})
            );
        }
        let mut device_offset = 0usize;
        let mut setting_error = None;
        let mut advanced = false;
        // The workers table opens first; Tab switches to the overview.
        let mut overview = false;
        let status_file = status_path(config_path);
        let mut status_saved: Option<Instant> = None;
        // #### PR #42
        // What: the workers page highlights one row, moved with the arrow
        // keys, PgUp/PgDn, Home and End and kept by its label; Enter (or c)
        // opens the Device panel for that row, online or offline.
        // Why: `c` opened the controls of the scroll position's row, and only
        // while it was online, so an offline device could not be restarted
        // and the row controlled was not always the one the user meant.
        // Look here if: Enter opens another device than the highlighted one,
        // or the highlight jumps when rows re-sort.
        let mut selection = Selection::default();
        let mut panel: Option<DevicePanel> = None;
        // #### end PR #42 ####
        let mut connect: Option<ConnectPage> = None;
        while !stop.load(Ordering::Relaxed)
            && worker.as_ref().is_none_or(|worker| !worker.is_finished())
        {
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
            let status = status_json(
                config.network.as_str(),
                pool_address.as_deref(),
                node_client.as_deref().filter(|_| snapshot.active_node == 0),
                &snapshot,
                donation_value,
                &devices,
            );
            if status_saved.is_none_or(|saved| saved.elapsed() >= Duration::from_secs(1)) {
                let _ = write_status(&status_file, &status);
                status_saved = Some(Instant::now());
            }
            if let Some(terminal) = terminal.as_mut() {
                let overview_text = if let Some(pool) = &pool_address {
                    format!(
                        "{} · Pool {pool} (SV2, encrypted)\nDevices {} · Sessions {} · Shares {} accepted / {} rejected ({} at SV1 adapter)\nThe pool builds the blocks; the donation is that share of mining time under the donation address at the pool.\nConnection errors: SV1 {}\nSV1 {}",
                        config.network.as_str(),
                        snapshot.connections,
                        snapshot.sessions_started,
                        snapshot.shares_accepted,
                        snapshot.shares_rejected,
                        snapshot.sv1_local_rejected,
                        snapshot.sv1_connection_errors,
                        sv1_bound.map(|address| address.to_string()).unwrap_or_else(|| "Off".into()),
                    )
                } else {
                    format!(
                    "{} · Node {}{} · Height {} · Donation {}{}\nDevices {} · Sessions {} · Shares {} accepted / {} rejected ({} at SV1 adapter)\nBlocks {} accepted / {} pending / {} rejected · Retries {} · Last {}\nConnection errors: SV2 {} / SV1 {}\nTemplate errors {} · Last {}\nSV2 {} · SV1 {}\n{}\n{}",
                    config.network.as_str(), if snapshot.template_ready { "Ready" } else { "Waiting" }, node_label(node_client.as_deref(), &snapshot),
                    snapshot.height.map(|height| height.to_string()).unwrap_or_else(|| "Waiting".into()), donation_summary(donation_value), pool_suffix, snapshot.connections, snapshot.sessions_started,
                    snapshot.shares_accepted, snapshot.shares_rejected, snapshot.sv1_local_rejected, snapshot.blocks_accepted, snapshot.blocks_pending,
                    snapshot.blocks_rejected, snapshot.block_retries, snapshot.last_block_result.unwrap_or("Waiting"),
                    snapshot.connection_errors, snapshot.sv1_connection_errors,
                    snapshot.template_failures, snapshot.last_template_error.unwrap_or("None"),
                    bound.map(|address| address.to_string()).unwrap_or_else(|| "Off".into()),
                    sv1_bound.map(|address| address.to_string()).unwrap_or_else(|| "Off".into()),
                    setting_error.map(str::to_owned).unwrap_or_else(|| format!("Authority {}", authority.as_deref().unwrap_or("—"))),
                    records_line(&snapshot),
                    )
                };
                let online = devices.iter().filter(|device| device.connected).count();
                let total_rate: f64 = devices
                    .iter()
                    .filter(|device| device.connected)
                    .filter_map(|device| device.hashrate_estimate)
                    .sum();
                let header = if let Some(pool) = &pool_address {
                    format!(
                        "{} · Pool {pool} (SV2, encrypted) · the pool builds blocks and pays\n{online} of {} workers online · {} · Shares {} accepted / {} rejected\n{devices_hint}",
                        config.network.as_str(),
                        devices.len(),
                        crate::telemetry::format_hash_rate(total_rate),
                        snapshot.shares_accepted,
                        snapshot.shares_rejected,
                    )
                } else {
                    format!(
                        "{} · Node {}{} · Height {} · Donation {}{}\n{online} of {} workers online · {} · Shares {} accepted / {} rejected · Blocks {} accepted / {} pending\n{devices_hint}",
                        config.network.as_str(),
                        if snapshot.template_ready { "Ready" } else { "Waiting" },
                        node_label(node_client.as_deref(), &snapshot),
                        snapshot.height.map(|height| height.to_string()).unwrap_or_else(|| "Waiting".into()),
                        donation_summary(donation_value),
                        pool_suffix,
                        devices.len(),
                        crate::telemetry::format_hash_rate(total_rate),
                        snapshot.shares_accepted,
                        snapshot.shares_rejected,
                        snapshot.blocks_accepted,
                        snapshot.blocks_pending,
                    )
                };
                let lines: Vec<WorkerLine> = devices.iter().map(WorkerLine::from).collect();
                // #### PR #42: the highlight follows its device's label.
                let labels: Vec<&str> = lines.iter().map(|line| line.label.as_str()).collect();
                selection.follow(&labels);
                terminal
                    .terminal
                    .draw(|frame| {
                        if let Some(page) = connect.as_ref() {
                            render_connect(frame, page)
                        } else if let Some(view) = panel.as_ref() {
                            super::panel::render(frame, view)
                        } else if advanced {
                            render_advanced(
                                frame,
                                donation_value,
                                setting_error,
                                pool_address.is_some(),
                            )
                        } else if overview {
                            render_dashboard(frame, &overview_text, &devices, device_offset)
                        } else {
                            render_workers(
                                frame,
                                &header,
                                &lines,
                                &mut selection.table,
                                SERVE_FOOTER,
                            )
                        }
                    })
                    .map_err(|_| "cannot draw mining dashboard")?;
                if event::poll(Duration::from_millis(500)).map_err(|_| "cannot read terminal")? {
                    if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                        if key.kind == KeyEventKind::Press {
                            // #### PR #40: on Connection info, a number copies
                            // that line.
                            if let Some(page) = connect.as_mut() {
                                match key.code {
                                    KeyCode::Char('i') | KeyCode::Char('I') | KeyCode::Esc => {
                                        connect = None
                                    }
                                    KeyCode::Char(digit @ '1'..='9') => {
                                        if let Some(line) =
                                            page.lines.get(digit as usize - '1' as usize)
                                        {
                                            page.note = Some(if crate::reach::copy(&line.url) {
                                                format!("Copied {}", line.url)
                                            } else {
                                                "Could not copy; select the line with the mouse."
                                                    .into()
                                            });
                                        }
                                    }
                                    KeyCode::Char('q') => stop.store(true, Ordering::Relaxed),
                                    KeyCode::Char('c')
                                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        stop.store(true, Ordering::Relaxed)
                                    }
                                    _ => (),
                                }
                                continue;
                            }
                            // #### PR #42
                            // What: the Device panel takes every key, so q
                            // typed there never stops the server; Enter (or
                            // c) on the workers page opens it for the
                            // highlighted row, online or offline, and
                            // Ctrl+C there stops the server instead.
                            // Why: see the selection note above; Ctrl+C on
                            // the workers page opened the controls.
                            // Look here if: a key on the panel reaches the
                            // workers page, or Enter opens the wrong row.
                            if let Some(view) = panel.as_mut() {
                                if view.handle_key(key, &fleet) {
                                    panel = None;
                                }
                                continue;
                            }
                            match key.code {
                                _ if super::panel::opens_panel(&key, !advanced && !overview) => {
                                    // `devices` holds this frame's rows in
                                    // the table's order.
                                    let opened = stats.lock().ok().and_then(|stats| {
                                        DevicePanel::for_selection(
                                            &stats.device_stats,
                                            &devices,
                                            &selection,
                                            Instant::now(),
                                        )
                                    });
                                    if let Some(view) = opened {
                                        view.identify(&fleet);
                                        panel = Some(view);
                                    }
                                }
                                // #### end PR #42 ####
                                KeyCode::Char('a') | KeyCode::Char('A') => advanced = !advanced,
                                KeyCode::Char('i') | KeyCode::Char('I') if !advanced => {
                                    connect = Some(ConnectPage {
                                        lines: connect_lines(
                                            bound,
                                            sv1_bound,
                                            authority.as_deref(),
                                            crate::reach::Interfaces::detect(),
                                        ),
                                        mode: serve_mode,
                                        note: None,
                                    });
                                }
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
                                KeyCode::Up if overview => {
                                    device_offset = device_offset.saturating_sub(1)
                                }
                                KeyCode::Down if overview => {
                                    device_offset = device_offset.saturating_add(1)
                                }
                                KeyCode::PageUp if overview => {
                                    device_offset = device_offset.saturating_sub(10)
                                }
                                KeyCode::PageDown if overview => {
                                    device_offset = device_offset.saturating_add(10)
                                }
                                // #### PR #42: on the workers page these keys
                                // move the highlight (the overview scrolls).
                                KeyCode::Up
                                | KeyCode::Down
                                | KeyCode::PageUp
                                | KeyCode::PageDown
                                | KeyCode::Home
                                | KeyCode::End
                                    if !advanced && !overview =>
                                {
                                    selection.step(key.code, &labels)
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
    let server_result = match worker {
        Some(worker) => worker
            .join()
            .map_err(|_| "mining server stopped unexpectedly")?,
        None => Ok(()),
    };
    result.and(server_result).and(firmware_result)
}

/// #### PR #40
/// The remote SV2 pools from `--upstream`, `--upstream-key` and
/// `--upstream-user`, in failover order: one key per pool, in the same order.
/// A key is the pool's authority public key as SV2 pools publish it:
/// Base58Check of version 1 and the 32-byte x-only key, the form this server
/// prints for its own. The identity, shared by all the pools, defaults to the
/// payout address, which solo SV2 pools pay; it is never printed.
fn pool_upstreams(
    config: &RuntimeConfig,
    addresses: &[String],
    keys: &[String],
    user: Option<&str>,
) -> Result<Vec<super::sv1::Upstream>, String> {
    if !keys.is_empty() && keys.len() != addresses.len() {
        return Err(
            "give one --upstream-key for each --upstream, in the same order, or put each key \
             in its address (stratum2+tcp://HOST:PORT/KEY)"
                .into(),
        );
    }
    let identity = match user.map(str::trim) {
        Some(user) => {
            if user.is_empty() || user.len() > 255 || user.chars().any(char::is_control) {
                return Err("--upstream-user must be 1 to 255 printable characters".into());
            }
            user.to_owned()
        }
        None => {
            config::validate_payout_address(config.network, &config.payout_address).map_err(
                |_| "--upstream-user, or a valid payout address for the selected network, is required",
            )?;
            config.payout_address.clone()
        }
    };
    addresses
        .iter()
        .enumerate()
        .map(|(index, address)| {
            // #### PR #40: the key may come in the address, as pools publish
            // it; given in both places, the two must agree.
            let (address, embedded) = super::split_pool_address(address)
                .map_err(|error| format!("--upstream: {error}"))?;
            let key = match (keys.get(index).map(|key| key.trim()), embedded) {
                (Some(given), Some(embedded)) if given != embedded => {
                    return Err("--upstream-key differs from the key in --upstream".into())
                }
                (Some(given), _) => given.to_owned(),
                (None, Some(embedded)) => embedded,
                (None, None) => {
                    return Err(
                        "give the pool's key with --upstream-key, or in its address as \
                                stratum2+tcp://HOST:PORT/KEY"
                            .into(),
                    )
                }
            };
            let invalid_key = "--upstream-key is not an SV2 authority public key";
            let decoded =
                stratum_core::bitcoin::base58::decode_check(&key).map_err(|_| invalid_key)?;
            let authority: [u8; 32] = match decoded.as_slice() {
                [1, 0, key @ ..] => key.try_into().map_err(|_| invalid_key)?,
                _ => return Err(invalid_key.into()),
            };
            Ok(super::sv1::Upstream {
                address,
                authority,
                identity: identity.clone(),
                remote: true,
                donation: None,
                public: false,
            })
        })
        .collect()
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
fn render_advanced(frame: &mut Frame<'_>, donation: BchDonation, error: Option<&str>, pool: bool) {
    let (work, reward) = donation.shares();
    // #### PR #40: at a pool the whole donation is mining time.
    let split = if pool {
        format!(
            "{donation} of each device's mining time mines at the pool under the Pickaxe \
             donation address; the pool builds the blocks, so none of it is a block reward."
        )
    } else {
        format!(
            "{work} of mining work and {reward} of each block reward go to the Pickaxe donation \
             address."
        )
    };
    let mut text = format!(
        "Donation  {donation}\n\n{split}\nThe default is 1.50%; any setting from 0% to 100% \
         works, in 0.5% steps. Changes apply to new jobs and are saved.\n\n←/→ or +/-  Change \
         donation · a or Esc  Back"
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
        Constraint::Length(10),
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
/// #### PR #42
/// What: the table scrolls itself to keep the highlighted row (`state`'s
/// selection) in view and shows it reversed; with no selection (the
/// read-only watch view) it scrolls from `state`'s offset.
/// Why: the highlight used to be whichever row was at the scroll position.
/// Look here if: the highlighted row is off screen or not the one Enter
/// opens.
fn render_workers(
    frame: &mut Frame<'_>,
    header: &str,
    devices: &[WorkerLine],
    state: &mut TableState,
    footer: &str,
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
    let rows = devices.iter().map(|device| {
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
    });
    let title = match state.selected() {
        Some(index) if index < devices.len() => {
            format!("Workers · {} · row {} selected", devices.len(), index + 1)
        }
        _ => format!(
            "Workers · {} · {} onward",
            devices.len(),
            if devices.is_empty() {
                0
            } else {
                state.offset() + 1
            }
        ),
    };
    frame.render_stateful_widget(
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
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(Block::bordered().title(title)),
        areas[1],
        state,
    );
    frame.render_widget(Paragraph::new(footer), areas[2]);
}

// #### PR #42: Enter opens the Device panel for the highlighted row.
const SERVE_FOOTER: &str = "Tab  Overview · ↑/↓ PgUp/PgDn  Select · Enter  Device panel · i  Connection info · a  Advanced settings · q  Stop server\nNow and 1 hour come from validated shares (30s warm-up); Device says, Temp and Fan are the device's own report.";
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

/// Where devices connect, with wildcard listeners shown at this computer's
/// address on the local network; Connection info (`i`) has every address.
fn device_hint(
    sv2: Option<std::net::SocketAddr>,
    sv1: Option<std::net::SocketAddr>,
    interfaces: crate::reach::Interfaces,
) -> String {
    let shown = |address: std::net::SocketAddr| {
        crate::reach::addresses(address, interfaces)
            .into_iter()
            .next()
            .map_or(address, |(_, shown)| shown)
    };
    match (sv1, sv2) {
        (Some(sv1), Some(sv2)) => format!(
            "Point devices at: SV1 stratum+tcp://{} · SV2 {} · i  Connection info",
            shown(sv1),
            shown(sv2)
        ),
        (Some(sv1), None) => format!(
            "Point devices at: SV1 stratum+tcp://{} · i  Connection info",
            shown(sv1)
        ),
        (None, Some(sv2)) => format!("Point devices at: SV2 {} · i  Connection info", shown(sv2)),
        (None, None) => String::new(),
    }
}

/// #### PR #40
/// What this server is, for what a device's username means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeMode {
    /// Blocks pay this server's payout address.
    Solo,
    /// Each miner's username is their payout address.
    Public,
    /// Devices mine at a remote pool through this computer.
    JoinPool,
}

/// #### PR #40
/// One address devices connect to: where it works from and the line to
/// paste into a device (or, for SV2, into another Pickaxe's Join a pool).
struct ConnectLine {
    place: crate::reach::Place,
    sv2: bool,
    url: String,
}

/// #### PR #40
/// The Connection info page (`i`), like ASICseer's: every address devices
/// connect to, numbered for copying, and what to put as the username.
struct ConnectPage {
    lines: Vec<ConnectLine>,
    mode: ServeMode,
    /// The last copy's result.
    note: Option<String>,
}

/// #### PR #40
/// The addresses devices use: SV1 first, as most firmware speaks only SV1,
/// then SV2 with the authority key in the address, the form ckpool and
/// Braiins publish (stratum2+tcp://HOST:PORT/KEY). Each listener appears at
/// every address this computer is reached at (local network, Tailscale).
fn connect_lines(
    sv2: Option<std::net::SocketAddr>,
    sv1: Option<std::net::SocketAddr>,
    authority: Option<&str>,
    interfaces: crate::reach::Interfaces,
) -> Vec<ConnectLine> {
    let mut lines = Vec::new();
    for (listen, sv2) in [(sv1, false), (sv2, true)] {
        let Some(listen) = listen else {
            continue;
        };
        for (place, address) in crate::reach::addresses(listen, interfaces) {
            let url = match (sv2, authority) {
                (false, _) => format!("stratum+tcp://{address}"),
                (true, Some(key)) => format!("stratum2+tcp://{address}/{key}"),
                (true, None) => format!("stratum2+tcp://{address}"),
            };
            lines.push(ConnectLine { place, sv2, url });
        }
    }
    lines
}

/// #### PR #40: the addresses in the JSON start line; never a payout.
fn connect_json(lines: &[ConnectLine]) -> serde_json::Value {
    lines
        .iter()
        .map(|line| {
            serde_json::json!({
                "place": line.place.label(),
                "protocol": if line.sv2 { "SV2" } else { "SV1" },
                "url": line.url,
            })
        })
        .collect()
}

/// #### PR #40: the Connection info page's text.
fn connect_text(page: &ConnectPage) -> String {
    use crate::reach::Place;
    let mut text = String::from("How devices connect\n");
    let mut group = None;
    for (index, line) in page.lines.iter().enumerate() {
        if group != Some(line.sv2) {
            group = Some(line.sv2);
            text.push_str(if line.sv2 {
                "\nSV2 firmware (Braiins OS, Bitaxe), or another Pickaxe's Join a pool:\n"
            } else {
                "\nMost ASICs speak SV1 (stock Antminer, Avalon and Whatsminer firmware):\n"
            });
        }
        text.push_str(&format!(
            "  {}  {:<14} {}\n",
            index + 1,
            line.place.label(),
            line.url
        ));
    }
    if page.lines.is_empty() {
        text.push_str("\nNo listener is open.\n");
    }
    text.push_str(match page.mode {
        ServeMode::Solo => {
            "\nUsername: any name for the device; it names the device on the workers page. \
             Every block pays this server's payout address.\n"
        }
        ServeMode::Public => {
            "\nUsername: the miner's own BCH address (q or p; the prefix may be left out), \
             optionally followed by .name; the blocks they find pay it. SV2 devices give it \
             as their user identity.\n"
        }
        ServeMode::JoinPool => {
            "\nUsername: any name for the device; it names the device on the workers page. \
             This computer mines at the pool for you.\n"
        }
    });
    text.push_str("Password: anything; it is not checked.\n");
    let reachable = page
        .lines
        .iter()
        .any(|line| line.place != Place::ThisComputer);
    if !reachable && !page.lines.is_empty() {
        text.push_str(
            "\nOnly programs on this computer can connect. For devices on your network, \
             listen on every interface (such as --sv1-listen 0.0.0.0:3333).\n",
        );
    } else if !page.lines.iter().any(|line| line.place == Place::Tailscale) {
        text.push_str(
            "\nTailscale puts computers in other places on one private network without \
             opening router ports; once it runs here, this computer's Tailscale address \
             appears above.\n",
        );
    }
    if page.mode != ServeMode::JoinPool {
        text.push_str(
            "ASICs in another place connect to a computer there running Pickaxe with Join a \
             pool, pointed at an SV2 line above; only encrypted SV2 crosses the internet.\n",
        );
    }
    text.push_str(&format!(
        "\n{}  Copy a line · i or Esc  Back · q  Stop server",
        match page.lines.len() {
            0 | 1 => "1".to_owned(),
            count => format!("1-{count}"),
        }
    ));
    if let Some(note) = &page.note {
        text.push_str(&format!("\n\n{note}"));
    }
    text
}

fn render_connect(frame: &mut Frame<'_>, page: &ConnectPage) {
    frame.render_widget(
        Paragraph::new(connect_text(page))
            .block(Block::bordered().title("Pickaxe · Connection info"))
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
    upstream: Option<&str>,
    node: Option<&str>,
    snapshot: &ServerStats,
    donation: BchDonation,
    devices: &[DeviceSnapshot],
) -> serde_json::Value {
    serde_json::json!({
        "network": network,
        "upstream": upstream,
        "node": node,
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
        "node_active": snapshot.active_node + 1,
        "nodes": snapshot.nodes,
        "node_switches": snapshot.node_switches,
        "best_share": snapshot.best_share.as_ref().map(|(difficulty, worker)| {
            serde_json::json!({"difficulty": difficulty, "worker": worker})
        }),
        "recent_blocks": snapshot.recent_blocks.iter().map(|found| serde_json::json!({
            "height": found.height,
            "hash": found.hash,
            "worker": found.worker,
            "seconds_ago": found.found.elapsed().as_secs(),
            "result": found.result,
        })).collect::<Vec<_>>(),
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

/// #### PR #40: the saved best share.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WatchBest {
    difficulty: f64,
    worker: String,
}

/// #### PR #40: a saved found block.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WatchBlock {
    height: u32,
    worker: String,
    seconds_ago: u64,
    result: Option<String>,
}

/// The saved status as `stratum-v2 watch` reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WatchStatus {
    network: String,
    /// The pool in pool mode.
    upstream: Option<String>,
    /// The node's client and version in local mode.
    node: Option<String>,
    /// #### PR #40: which node of several templates come from (1-based).
    node_active: usize,
    nodes: usize,
    /// #### PR #40: the best share since start and the latest blocks found.
    best_share: Option<WatchBest>,
    recent_blocks: Vec<WatchBlock>,
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
        if let Some(pool) = &self.upstream {
            return format!(
                "{} · Pool {pool} (SV2, encrypted) · {freshness}\n{online} of {} workers online · {} · Shares {} accepted / {} rejected",
                self.network,
                self.device_details.len(),
                crate::telemetry::format_hash_rate(rate),
                self.shares_accepted,
                self.shares_rejected,
            );
        }
        format!(
            "{} · Node {}{} · Height {} · Donation {} · {freshness}\n{online} of {} workers online · {} · Shares {} accepted / {} rejected · Blocks {} accepted / {} pending",
            self.network,
            if self.ready { "Ready" } else { "Waiting" },
            node_suffix(self.node.as_deref())
                + &if self.nodes > 1 {
                    format!(" · node {} of {}", self.node_active, self.nodes)
                } else {
                    String::new()
                },
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
        ) + &self.records(age)
    }

    /// #### PR #40
    /// The third header line: the best share and the latest blocks found,
    /// their ages counted to now.
    fn records(&self, age: u64) -> String {
        let best = self
            .best_share
            .as_ref()
            .map(|best| {
                format!(
                    "{} by {}",
                    format_difficulty(Some(best.difficulty)),
                    best.worker
                )
            })
            .unwrap_or_else(|| "none yet".into());
        let blocks: Vec<String> = self
            .recent_blocks
            .iter()
            .rev()
            .take(3)
            .map(|found| {
                format!(
                    "#{} {} {} {}",
                    found.height,
                    found.worker,
                    found.result.as_deref().unwrap_or("waiting"),
                    ago(Some(found.seconds_ago + age))
                )
            })
            .collect();
        format!(
            "\nBest share {best} · Recent blocks {}",
            if blocks.is_empty() {
                "none yet".to_owned()
            } else {
                blocks.join(", ")
            }
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
        // #### PR #42: read-only, so nothing is highlighted; the table
        // scrolls from the offset as before.
        let mut table = TableState::default().with_offset(offset);
        terminal
            .terminal
            .draw(|frame| render_workers(frame, &header, &rows, &mut table, WATCH_FOOTER))
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

/// #### PR #40
/// A public pool's note after the donation: each miner's blocks pay them,
/// and the operator's fee comes off what the donation leaves.
fn public_pool_suffix(public: Option<&super::payout::PublicPool>) -> String {
    match public.map(|public| public.fee) {
        None => String::new(),
        Some(None) => " · Public pool, no fee".into(),
        Some(Some(fee)) => format!(" · Public pool, fee {} from {}", fee.rate, fee.mode),
    }
}

/// #### PR #40
/// The share target of a pool difficulty: difficulty 1 is the target of
/// compact bits 0x1d00ffff, so 4096 is the default start (0x1b0ffff0).
fn difficulty_target(difficulty: u64) -> Result<super::template::Hash, String> {
    if difficulty == 0 || difficulty > 1 << 48 {
        return Err("--start-difficulty must be from 1 to 2^48".into());
    }
    let target = (num_bigint::BigUint::from(0xffffu32) << 208u32) / difficulty;
    let mut bytes = target.to_bytes_le();
    bytes.resize(32, 0);
    bytes
        .try_into()
        .map_err(|_| "--start-difficulty is out of range".into())
}

/// #### PR #40
/// The overview's records: the best share since start and the latest blocks
/// found, newest first, as "#327035 rig1 accepted 2m ago".
fn records_line(stats: &ServerStats) -> String {
    let best = stats
        .best_share
        .as_ref()
        .map(|(difficulty, worker)| format!("{} by {worker}", format_difficulty(Some(*difficulty))))
        .unwrap_or_else(|| "none yet".into());
    let blocks = stats
        .recent_blocks
        .iter()
        .rev()
        .take(3)
        .map(|found| {
            format!(
                "#{} {} {} {}",
                found.height,
                found.worker,
                found.result.unwrap_or("waiting"),
                ago(Some(found.found.elapsed().as_secs()))
            )
        })
        .collect::<Vec<_>>();
    format!(
        "Best share {best} · Recent blocks {}",
        if blocks.is_empty() {
            "none yet".into()
        } else {
            blocks.join(", ")
        }
    )
}

/// #### PR #40
/// After "Node Ready": the node's client while templates come from the node
/// it was read from (the first in failover order), and which node of
/// several, such as " (Bitcoin Cash Node 29.1.0) · node 1 of 2".
fn node_label(client: Option<&str>, stats: &ServerStats) -> String {
    let mut label = node_suffix(client.filter(|_| stats.active_node == 0));
    if stats.nodes > 1 {
        label.push_str(&format!(
            " · node {} of {}",
            stats.active_node + 1,
            stats.nodes
        ));
    }
    label
}

/// #### PR #40
/// The node's client after "Node Ready", such as " (Bitcoin Cash Node 29.1.0)".
fn node_suffix(client: Option<&str>) -> String {
    client
        .map(|client| format!(" ({client})"))
        .unwrap_or_default()
}

/// The configured nodes in failover order, starting with the first one that
/// gives a synchronized template, and that template.
fn preflight(config: &RuntimeConfig) -> Result<(Vec<NativeNodeRpc>, BchTemplate), String> {
    let endpoints = config.custom_node_endpoints();
    if endpoints.is_empty() {
        return Err("configure your BCHN RPC connection before starting BCH ASIC mining".into());
    }
    let mut reason = "node RPC unavailable";
    for (index, endpoint) in endpoints.iter().enumerate() {
        let mut provider =
            TemplateProvider::new(NativeNodeRpc::new((*endpoint).to_owned()), config.network);
        match provider.refresh() {
            // #### PR #40: the others follow in their order, as the server's
            // failover list.
            Ok((_, template)) => {
                let nodes = endpoints[index..]
                    .iter()
                    .chain(&endpoints[..index])
                    .map(|endpoint| NativeNodeRpc::new((*endpoint).to_owned()))
                    .collect();
                return Ok((nodes, template.clone()));
            }
            // #### PR #40
            // The last node's reason, such as a refused login or a node on
            // another network, instead of a bare failure.
            Err(error) => reason = server::template_reason(&error),
        }
    }
    Err(format!(
        "no configured BCH node supplied a synchronized template for the selected network ({reason})"
    ))
}

pub(crate) fn load_authority(path: &Path) -> Result<[u8; 32], String> {
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
        // #### PR #42: a second worker, highlighted with the arrow keys.
        devices.connect("127.0.0.1:1001".parse().unwrap(), true, start);
        let rows: Vec<WorkerLine> = devices
            .snapshots(start + Duration::from_secs(45))
            .iter()
            .map(WorkerLine::from)
            .collect();
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        let mut selection = Selection::default();
        selection.follow(&labels);
        selection.step(KeyCode::Down, &labels);
        let mut terminal = Terminal::new(TestBackend::new(180, 20)).unwrap();
        terminal
            .draw(|f| {
                render_workers(
                    f,
                    "Chipnet · Node Ready · 2 of 2 workers online",
                    &rows,
                    &mut selection.table,
                    SERVE_FOOTER,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
        // Only the highlighted row is shown reversed.
        let line_of = |label: &str| {
            (0..buffer.area.height)
                .find(|&y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .contains(label)
                })
                .unwrap()
        };
        let reversed = |y: u16| buffer[(2, y)].modifier.contains(Modifier::REVERSED);
        assert!(reversed(line_of(&rows[1].label)));
        assert!(!reversed(line_of(&rows[0].label)));
        assert!(text.contains("Workers · 2 · row 2 selected"), "{text}");
        assert!(text.contains("Enter  Device panel"));
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

    // #### PR #40
    #[test]
    fn a_start_difficulty_is_the_matching_share_target() {
        assert_eq!(
            difficulty_target(4096).unwrap(),
            compact_target(0x1b0ffff0).unwrap()
        );
        assert_eq!(
            difficulty_target(1).unwrap(),
            compact_target(0x1d00ffff).unwrap()
        );
        let fast = difficulty_target(65_536).unwrap();
        assert!(super::super::telemetry::expected_hashes(&fast) > 2f64.powi(47));
        assert!(difficulty_target(0).is_err());
        assert!(difficulty_target(u64::MAX).is_err());
    }

    // #### PR #40
    #[test]
    fn the_overview_records_the_best_share_and_recent_blocks() {
        let mut stats = ServerStats::default();
        assert_eq!(
            records_line(&stats),
            "Best share none yet · Recent blocks none yet"
        );
        stats.best_share = Some((2_500_000.0, "rig1 #3".into()));
        for (height, result) in [(327_034, Some("accepted")), (327_035, None)] {
            stats
                .recent_blocks
                .push_back(super::super::server::FoundBlock {
                    height,
                    hash: "00".repeat(32),
                    worker: "rig1 #3".into(),
                    found: Instant::now(),
                    result,
                });
        }
        let line = records_line(&stats);
        assert!(line.starts_with("Best share 2.50M by rig1 #3"), "{line}");
        assert!(
            line.contains("#327035 rig1 #3 waiting") && line.contains("#327034 rig1 #3 accepted"),
            "{line}"
        );
        assert!(line.find("#327035").unwrap() < line.find("#327034").unwrap());
    }

    #[test]
    fn devices_are_told_this_computers_network_address() {
        let lan = crate::reach::Interfaces {
            local: Some("192.168.0.160".parse().unwrap()),
            tailscale: None,
        };
        assert_eq!(
            device_hint(
                Some("0.0.0.0:3336".parse().unwrap()),
                Some("0.0.0.0:3333".parse().unwrap()),
                lan
            ),
            "Point devices at: SV1 stratum+tcp://192.168.0.160:3333 · SV2 192.168.0.160:3336 · i  Connection info"
        );
        // Explicit listeners are shown as configured.
        assert_eq!(
            device_hint(Some("127.0.0.1:3336".parse().unwrap()), None, lan),
            "Point devices at: SV2 127.0.0.1:3336 · i  Connection info"
        );
        // With no network, only this computer can connect.
        assert_eq!(
            device_hint(
                Some("0.0.0.0:3336".parse().unwrap()),
                None,
                crate::reach::Interfaces::default()
            ),
            "Point devices at: SV2 127.0.0.1:3336 · i  Connection info"
        );
        // Pool mode has no SV2 listener of its own.
        assert_eq!(
            device_hint(None, Some("0.0.0.0:3333".parse().unwrap()), lan),
            "Point devices at: SV1 stratum+tcp://192.168.0.160:3333 · i  Connection info"
        );
    }

    // #### PR #40
    #[test]
    fn connection_info_lists_sv1_and_sv2_lines_and_what_to_type() {
        use crate::reach::{Interfaces, Place};
        let local = Some("192.168.0.160".parse().unwrap());
        let both = Interfaces {
            local,
            tailscale: Some("100.101.102.103".parse().unwrap()),
        };
        let lines = connect_lines(
            Some("0.0.0.0:3336".parse().unwrap()),
            Some("0.0.0.0:3333".parse().unwrap()),
            Some("KEY"),
            both,
        );
        let urls: Vec<_> = lines
            .iter()
            .map(|line| (line.place, line.url.as_str()))
            .collect();
        assert_eq!(
            urls,
            [
                (Place::LocalNetwork, "stratum+tcp://192.168.0.160:3333"),
                (Place::Tailscale, "stratum+tcp://100.101.102.103:3333"),
                (Place::LocalNetwork, "stratum2+tcp://192.168.0.160:3336/KEY"),
                (Place::Tailscale, "stratum2+tcp://100.101.102.103:3336/KEY"),
            ]
        );
        assert_eq!(
            connect_json(&lines)[3],
            serde_json::json!({"place": "Tailscale", "protocol": "SV2", "url": "stratum2+tcp://100.101.102.103:3336/KEY"})
        );
        // What another Pickaxe pastes into Join a pool is understood there.
        assert_eq!(
            super::super::split_pool_address(&lines[2].url).unwrap(),
            ("192.168.0.160:3336".into(), Some("KEY".into()))
        );
        let public = ConnectPage {
            lines,
            mode: ServeMode::Public,
            note: None,
        };
        let text = connect_text(&public);
        assert!(text.contains("Most ASICs speak SV1"), "{text}");
        assert!(
            text.contains("  1  your network   stratum+tcp://192.168.0.160:3333"),
            "{text}"
        );
        assert!(
            text.contains("  4  Tailscale      stratum2+tcp://100.101.102.103:3336/KEY"),
            "{text}"
        );
        assert!(
            text.contains("Username: the miner's own BCH address"),
            "{text}"
        );
        assert!(text.contains("1-4  Copy a line"), "{text}");
        assert!(!text.contains("Tailscale puts computers"), "{text}");
        // Solo on the local network only: any name, and Tailscale is offered.
        let solo = ConnectPage {
            lines: connect_lines(
                None,
                Some("0.0.0.0:3333".parse().unwrap()),
                None,
                Interfaces {
                    local,
                    tailscale: None,
                },
            ),
            mode: ServeMode::Solo,
            note: Some("Copied stratum+tcp://192.168.0.160:3333".into()),
        };
        let text = connect_text(&solo);
        assert!(text.contains("Username: any name for the device"), "{text}");
        assert!(text.contains("Tailscale puts computers"), "{text}");
        assert!(text.contains("with Join a"), "{text}");
        assert!(text.contains("1  Copy a line"), "{text}");
        assert!(
            text.ends_with("Copied stratum+tcp://192.168.0.160:3333"),
            "{text}"
        );
        // Joining a pool: this computer is the device side.
        let join = ConnectPage {
            mode: ServeMode::JoinPool,
            note: None,
            ..solo
        };
        let text = connect_text(&join);
        assert!(text.contains("mines at the pool for you"), "{text}");
        assert!(!text.contains("with Join a"), "{text}");
        // A loopback listener: only this computer.
        let local_only = ConnectPage {
            lines: connect_lines(
                Some("127.0.0.1:3336".parse().unwrap()),
                None,
                Some("KEY"),
                both,
            ),
            mode: ServeMode::Solo,
            note: None,
        };
        let text = connect_text(&local_only);
        assert!(
            text.contains("this computer  stratum2+tcp://127.0.0.1:3336/KEY"),
            "{text}"
        );
        assert!(text.contains("Only programs on this computer"), "{text}");
    }

    #[test]
    fn pool_mode_reads_the_pool_key_and_defaults_its_identity_to_the_payout() {
        let mut config = RuntimeConfig {
            network: crate::config::MiningNetwork::Chipnet,
            ..RuntimeConfig::default()
        };
        let mut encoded = vec![1, 0];
        encoded.extend([7u8; 32]);
        let key = stratum_core::bitcoin::base58::encode_check(&encoded);
        let one = |address: &str, key: &str, user: Option<&str>| {
            pool_upstreams(&config, &[address.to_owned()], &[key.to_owned()], user)
                .map(|mut pools| pools.remove(0))
        };
        // A worker name for an account pool.
        let pool = one(" pool.example:3336 ", &key, Some("me.rig1")).unwrap();
        assert_eq!(pool.address, "pool.example:3336");
        assert_eq!(pool.authority, [7u8; 32]);
        assert_eq!(pool.identity, "me.rig1");
        assert!(pool.remote);
        // Without one, a solo pool's identity is the payout address, which
        // must be valid for the network.
        assert!(one("pool.example:3336", &key, None).is_err());
        config.payout_address = "bchtest:qrzq5f9ltv70u4su7d40agd4nlnp8qlgqcma6x2tvp".into();
        let one = |address: &str, key: &str, user: Option<&str>| {
            pool_upstreams(&config, &[address.to_owned()], &[key.to_owned()], user)
                .map(|mut pools| pools.remove(0))
        };
        assert_eq!(
            one("pool.example:3336", &key, None).unwrap().identity,
            config.payout_address
        );
        // Malformed input fails before any connection.
        assert!(one("pool.example", &key, None).is_err());
        assert!(one("pool.example:3336", "not-a-key", None).is_err());
        assert!(one("pool.example:3336", &key, Some("a\nb")).is_err());
        let mut wrong_version = vec![2, 0];
        wrong_version.extend([7u8; 32]);
        let wrong = stratum_core::bitcoin::base58::encode_check(&wrong_version);
        assert!(one("pool.example:3336", &wrong, None).is_err());
        // #### PR #40: the key may come in the address, as pools publish it.
        let embedded = pool_upstreams(
            &config,
            &[format!("stratum2+tcp://pool.example:3336/{key}")],
            &[],
            None,
        )
        .unwrap();
        assert_eq!(embedded[0].address, "pool.example:3336");
        assert_eq!(embedded[0].authority, [7u8; 32]);
        // Given in both places, the keys must agree; given nowhere, it is
        // missing; an SV1 pool is refused.
        assert!(pool_upstreams(
            &config,
            &[format!("pool.example:3336/{key}")],
            std::slice::from_ref(&wrong),
            None
        )
        .is_err());
        assert!(pool_upstreams(&config, &["pool.example:3336".into()], &[], None).is_err());
        assert!(one("stratum+tcp://pool.example:3333", &key, None).is_err());
        // Backups: one key per pool, in order, sharing the identity.
        let pools = pool_upstreams(
            &config,
            &["a.example:3336".into(), "b.example:3336".into()],
            &[key.clone(), key.clone()],
            Some("me"),
        )
        .unwrap();
        assert_eq!(
            pools
                .iter()
                .map(|pool| pool.address.as_str())
                .collect::<Vec<_>>(),
            ["a.example:3336", "b.example:3336"]
        );
        assert!(pools.iter().all(|pool| pool.identity == "me"));
        assert!(pool_upstreams(
            &config,
            &["a.example:3336".into(), "b.example:3336".into()],
            &[key],
            Some("me"),
        )
        .is_err());
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
        // #### PR #40: the records a service's watch view shows too.
        stats.best_share = Some((5000.0, "rig1 #1".into()));
        stats
            .recent_blocks
            .push_back(super::super::server::FoundBlock {
                height: 326930,
                hash: "00".repeat(32),
                worker: "rig1 #1".into(),
                found: start,
                result: Some("accepted"),
            });
        let devices = stats
            .device_stats
            .snapshots(start + Duration::from_secs(40));
        let status = status_json(
            "chipnet",
            None,
            Some("Bitcoin Cash Node 29.1.0"),
            &stats,
            BchDonation::default(),
            &devices,
        );
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
            "Node Ready (Bitcoin Cash Node 29.1.0)",
            "Height 326930",
            "0.50% of work",
            "Live",
            "1 of 1 workers",
            "Best share 5.00K by rig1 #1",
            "#326930 rig1 #1 accepted",
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
            .draw(|f| {
                render_workers(
                    f,
                    &header,
                    &saved.rows(updated),
                    &mut TableState::default(),
                    WATCH_FOOTER,
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
            .draw(|f| {
                render_advanced(
                    f,
                    BchDonation::default(),
                    Some("Could not save donation"),
                    false,
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
