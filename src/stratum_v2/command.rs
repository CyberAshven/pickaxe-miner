//! #### PR #38
//! Native node preflight and mining-server lifecycle. Reuse the terminal guard,
//! keep authority secrets private, and never print node credentials or payouts.

use super::{
    fleet::Fleet,
    panel::{DevicePanel, Selection},
    provider::{NativeNodeRpc, TemplateProvider, TemplateSource},
    server::{self, ServerConfig, ServerStats},
    telemetry::{AddressIssue, DeviceSnapshot},
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

/// Runs a `stratum-v2` command. `profile` names the setup profile a server
/// started from (#### PR #42: a donation changed on its Advanced page is
/// saved there too).
pub fn run(
    action: StratumV2Command,
    config: &RuntimeConfig,
    config_path: &Path,
    no_tui: bool,
    json: bool,
    profile: Option<&str>,
) -> Result<(), String> {
    if let StratumV2Command::Status = action {
        print!("{}", super::status_report());
        return Ok(());
    }
    if let StratumV2Command::Watch = action {
        return watch(config_path);
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
    // #### PR #42: solo mining's fallback pools.
    let fallbacks = match &action {
        StratumV2Command::Serve { fallback_pool, .. } if !fallback_pool.is_empty() => {
            fallback_upstreams(config, fallback_pool)?
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
    // #### PR #42: Join a pool with JD runs a node and a local server
    // What: with --job-declaration the miner's own node and a local server
    // run as in solo mining; the local server's jobs pay the pool's outputs
    // and are declared to the first pool, and the SV1 adapter tries the local
    // server first, then the pools' own jobs.
    // Why: the miner chooses the transactions while the pool still pays.
    // Look here if: Join a pool with Job Declaration starts without a node,
    // or devices never reach the local server.
    let declaring = matches!(
        &action,
        StratumV2Command::Serve {
            job_declaration: Some(_),
            ..
        }
    );
    if matches!(action, StratumV2Command::Serve { .. }) && (pools.is_empty() || declaring) {
        config::validate_payout_address(config.network, &config.payout_address)
            .map_err(|_| "a valid payout for the selected network is required")?;
    }
    // #### PR #42: template providers, tried before the nodes, told the
    // coinbase bytes this server may add (with the test token's when asked
    // for; its registry row decides the network, so mainnet refuses it).
    let (providers, test_token) = match &action {
        StratumV2Command::Serve {
            template_provider,
            merge_test_token,
            ..
        } => (
            template_provider
                .iter()
                .map(|text| super::tdp::client::TdpAddress::parse(text))
                .collect::<Result<Vec<_>, _>>()?,
            *merge_test_token,
        ),
        _ => (Vec::new(), None),
    };
    let tokens = match test_token {
        Some(difficulty) => Some(Arc::new(super::merge::hub::TokenHub::test_token(
            config.network,
            config_path.with_extension("sv2-token-proofs.json"),
            super::merge::hub::bits_for_difficulty(difficulty)?,
        )?)),
        None => None,
    };
    // #### PR #42: an ASIC-exclusive token instead of BCH
    // What: `--asic-test-token` mines the Chipnet ASIC test token's simulated
    // thread with no node; `--asic-token` is refused while no token is
    // registered. The server then serves the token's jobs alone.
    // Why: ASIC-exclusive mining needs no BCH node; its jobs come from the
    // token's thread.
    // Look here if: token mode asks for a node, or starts on mainnet.
    let header_work = match &action {
        StratumV2Command::Serve {
            asic_token: Some(name),
            ..
        } => {
            return Err(format!(
                "no ASIC-exclusive token named {name} is registered on {} (none is yet)",
                config.network.as_str()
            ))
        }
        StratumV2Command::Serve {
            asic_test_token: Some(difficulty),
            ..
        } => Some(Arc::new(super::merge::source::HeaderWork::test_token(
            config.network,
            config_path.with_extension("sv2-token-proofs.json"),
            super::merge::hub::bits_for_difficulty(*difficulty)?,
            &config.payout_address,
        )?)),
        _ => None,
    };
    let reserve = super::tdp::reserve(tokens.as_ref().and_then(|hub| hub.current()).as_deref());
    let node = if (pools.is_empty() || declaring) && header_work.is_none() {
        Some(preflight_sources(config, &providers, reserve)?)
    } else {
        None
    };
    let serving = node.is_some() || header_work.is_some();
    // #### PR #40
    // The node's client and version, such as "Bitcoin Cash Node 29.1.0",
    // for check-node, the dashboard and the saved status.
    let node_client = node
        .as_ref()
        .and_then(|(nodes, _, _)| nodes.first())
        .and_then(|rpc| rpc.info().ok())
        .map(|info| info.client);
    // #### PR #42: whether the pool knows this server by another name than
    // the payout address, for the Advanced page.
    let custom_user = matches!(
        &action,
        StratumV2Command::Serve {
            upstream_user: Some(_),
            ..
        }
    );
    let StratumV2Command::Serve {
        listen,
        sv1_listen,
        donation,
        pool_tag,
        start_difficulty,
        tp_listen,
        accept_job_declaration,
        job_declaration,
        ..
    } = action
    else {
        if let Some((_, _, template)) = &node {
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
    // #### PR #42: a public pool that accepts miners' own templates; with
    // Full-Template, its first node checks the declared ones
    // (validateblocktemplate).
    let declarator = match (accept_job_declaration, public.as_ref()) {
        (Some(mode), Some(public)) => {
            let accept = match mode {
                crate::cli::AcceptJobDeclaration::Coinbase => super::jd::AcceptJd::CoinbaseOnly,
                crate::cli::AcceptJobDeclaration::Full => super::jd::AcceptJd::FullTemplate,
                crate::cli::AcceptJobDeclaration::Both => super::jd::AcceptJd::Both,
            };
            let mut declarator = super::jd::server::Declarator::new(
                accept,
                config.network,
                public.clone(),
                donation.clone(),
            );
            if let Some(rpc) = node
                .as_ref()
                .and_then(|(nodes, _, _)| nodes.first())
                .filter(|_| accept.allows(true))
            {
                declarator = declarator
                    .with_validator(Arc::new(super::jd::server::NodeValidator::new(rpc.clone())));
            }
            Some(Arc::new(declarator))
        }
        (Some(_), None) => {
            return Err("accepting Job Declaration needs a public pool (--public)".into())
        }
        (None, _) => None,
    };
    // #### PR #42: the source devices come back to from a pool: the node,
    // or Job Declaration (see `sv1::Preferred`).
    let preferred =
        (declaring || !fallbacks.is_empty()).then(|| Arc::new(super::sv1::Preferred::default()));
    // #### PR #40
    // At a pool the donation is the setting's share of mining time under the
    // donation address, on a second channel at the same pool.
    let at_pools = |pools: Vec<super::sv1::Upstream>| -> Vec<super::sv1::Upstream> {
        pools
            .into_iter()
            .map(|pool| super::sv1::Upstream {
                donation: Some(super::sv1::DonationRoute {
                    identity: crate::donation::bch::address(config.network).to_owned(),
                    rate: donation.clone(),
                }),
                prefer: preferred.clone(),
                ..pool
            })
            .collect()
    };
    let pools = at_pools(pools);
    let fallbacks = at_pools(fallbacks);
    let listener = serving
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
    // #### PR #42: the template listener, off unless asked for, and only
    // beside this server's own node.
    let tp_listener = tp_listen
        .filter(|_| node.is_some())
        .map(TcpListener::bind)
        .transpose()
        .map_err(|_| "cannot bind template listener")?;
    let tp_bound = tp_listener
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()
        .map_err(|_| "cannot read template listener")?;
    let authority_secret = serving
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
        _ if !pools.is_empty() && !declaring => pools.clone(),
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
            let mut list = vec![if public_pool.is_some() {
                super::sv1::Upstream::local_public(local, public)
            } else {
                super::sv1::Upstream::local(local, public)
            }
            // #### PR #42: a token that fixes its version slot.
            .with_fixed_version(header_work.as_ref().is_some_and(|work| {
                work.token().params.version == super::merge::safa::VersionRule::Fixed
            }))];
            // #### PR #42: under Job Declaration the pools' own jobs follow;
            // in solo mining, the fallback pools.
            if declaring {
                list.extend(pools.iter().cloned());
            }
            list.extend(fallbacks.iter().cloned());
            list
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
    // #### PR #42: what the server started with, for its Advanced page.
    let started = started_text(&Started {
        sv2: bound,
        sv1: sv1_bound,
        templates: tp_bound,
        start_difficulty: start_difficulty.unwrap_or(4096),
        pool_tag: String::from_utf8_lossy(&pool_tag).into_owned(),
        pools: pool_address.clone(),
        custom_user,
        fee: public.as_ref().and_then(|public| {
            public.fee.map(|fee| {
                (
                    fee,
                    public.address.eq_ignore_ascii_case(&config.payout_address),
                )
            })
        }),
        job_declaration: declarator.as_ref().map(|declarator| declarator.accept),
        fallbacks: (!fallbacks.is_empty()).then(|| {
            fallbacks
                .iter()
                .map(|pool| pool.address.as_str())
                .collect::<Vec<_>>()
                .join(" → ")
        }),
        declaring: job_declaration.filter(|_| !pools.is_empty()),
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
    // #### PR #42: the Job Declaration uplink, to the pools in order.
    let mode = match job_declaration {
        Some(crate::cli::JobDeclarationMode::Coinbase) => super::jd::JdMode::CoinbaseOnly,
        _ => super::jd::JdMode::FullTemplate,
    };
    let uplink = (declaring && !pools.is_empty()).then(|| {
        super::jd::client::spawn(
            pools
                .iter()
                .map(|pool| super::jd::client::JdTarget {
                    address: pool.address.clone(),
                    authority: pool.authority,
                    identity: pool.identity.clone(),
                    retry: Duration::from_secs(30),
                    mode,
                })
                .collect(),
            stop.clone(),
        )
    });
    let uplink_handle = uplink.as_ref().map(|(handle, _)| handle.clone());
    // #### PR #42: the server's work: the nodes' (or providers') templates,
    // or an ASIC-exclusive token's thread.
    let work = match (node, header_work.as_ref()) {
        (Some((nodes, sources, _)), _) => Some((
            nodes
                .iter()
                .map(NativeNodeRpc::source_identity)
                .collect::<Result<Vec<_>, _>>()?,
            sources,
        )),
        (None, Some(work)) => Some((
            Vec::new(),
            vec![
                Box::new(super::merge::source::TokenSource::new(work.clone()))
                    as Box<dyn super::provider::TemplateSource>,
            ],
        )),
        (None, None) => None,
    };
    let worker = match (work, listener, authority_secret) {
        (Some((legacy_sources, sources)), Some(listener), Some(authority_secret)) => {
            // Difficulty 4096 is each device's starting target; vardiff then
            // moves it toward 20 shares a minute. No nominal device rate is
            // shown as measured.
            let settings = ServerConfig {
                network: config.network,
                payout: config.payout_address.clone(),
                authority_secret,
                share_target,
                journal_path: config_path.with_extension(if declaring {
                    "sv2-jd-blocks.json"
                } else {
                    "sv2-blocks.json"
                }),
                pool_tag: pool_tag.clone(),
                legacy_sources,
                public: public.clone(),
                donation: donation.clone(),
                tokens: tokens.clone(),
                relay_journal_path: Some(config_path.with_extension("sv2-relay-blocks.json")),
                // #### PR #42: a pool's JD journal, for blocks found on
                // Full-Template declared jobs.
                declared_journal_path: declarator
                    .as_ref()
                    .map(|_| config_path.with_extension("sv2-jd-blocks.json")),
                declarator: declarator.clone(),
                uplink: uplink_handle.clone(),
                preferred: preferred.clone(),
                // #### PR #42: the token's donation, never below its
                // minimum.
                token_donation: header_work.as_ref().map(|work| {
                    config
                        .token_donation
                        .unwrap_or(work.token().donation_minimum)
                        .at_least(work.token().donation_minimum)
                }),
                header_work: header_work.clone(),
                #[cfg(test)]
                allocation_phase: None,
            };
            let stop = stop.clone();
            let stats = stats.clone();
            Some(thread::spawn(move || {
                server::run_with(
                    server::Listeners {
                        devices: listener,
                        templates: tp_listener,
                    },
                    sources,
                    settings,
                    stop,
                    stats,
                )
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
    // #### PR #42: the device logins the owner saved on the Device panel. A
    // logins file that cannot be read leaves the devices on their default
    // logins; mining is not affected.
    if let Err(error) = fleet.load_logins(config_path) {
        eprintln!("Device logins not loaded: {error}");
    }
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
    // #### PR #42: for the Connection info page.
    let job_declaration_on = declaring || declarator.is_some();
    let result = (|| {
        if terminal.is_none() {
            // The pool's identity may be a payout address: never printed.
            println!(
                "{}",
                serde_json::json!({"listen":bound.map(|address| address.to_string()),"sv1_listen":sv1_bound.map(|address| address.to_string()),"tp_listen":tp_bound.map(|address| address.to_string()),"authority":authority,"upstream":pool_address,"network":config.network.as_str(),"connect":connect_json(&connect_lines(bound, sv1_bound, authority.as_deref(), interfaces)),"templates":template_lines(tp_bound, interfaces).iter().map(|line| line.url.clone()).collect::<Vec<_>>()})
            );
        }
        let mut device_offset = 0usize;
        let mut setting_error = None;
        let mut advanced = false;
        // The workers table opens first; Tab switches to the overview.
        let mut overview = false;
        let status_file = status_path(config_path);
        let mut status_saved: Option<Instant> = None;
        // #### PR #42: the owner-only devices file the watch view controls
        // devices from; written whole, and only when it changes.
        let devices_file = devices_path(config_path);
        let devices_server = connect_lines(bound, sv1_bound, authority.as_deref(), interfaces);
        let mut devices_saved: Option<Vec<u8>> = None;
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
                let list = devices_json(
                    &devices_server,
                    &snapshot.device_stats.private_addresses(Instant::now()),
                );
                if devices_saved.as_deref() != Some(list.as_slice())
                    && crate::config::write_private_atomic(&devices_file, &list).is_ok()
                {
                    devices_saved = Some(list);
                }
            }
            if let Some(terminal) = terminal.as_mut() {
                let overview_text = if let Some(pool) = pool_address.as_ref().filter(|_| !declaring)
                {
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
                    "{} · Node {}{} · Height {} · Donation {}{}\nDevices {} · Sessions {} · Shares {} accepted / {} rejected ({} at SV1 adapter)\nBlocks {} accepted / {} pending / {} rejected · Retries {} · Last {}\nConnection errors: SV2 {} / SV1 {}\nTemplate errors {} · Last {}\nSV2 {} · SV1 {}\n{}\n{}{}{}{}",
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
                    templates_line(&snapshot),
                    jd_line(&snapshot),
                    jd_client_line(&snapshot, pool_address.as_deref()),
                    )
                };
                let online = devices.iter().filter(|device| device.connected).count();
                let total_rate: f64 = devices
                    .iter()
                    .filter(|device| device.connected)
                    .filter_map(|device| device.hashrate_estimate)
                    .sum();
                let header = if let Some(pool) = pool_address.as_ref().filter(|_| !declaring) {
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
                                &started,
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
                                        if let Some(line) = page
                                            .lines
                                            .iter()
                                            .chain(&page.templates)
                                            .nth(digit as usize - '1' as usize)
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
                                    if let Some(mut view) = opened {
                                        // #### PR #42: where devices reach
                                        // this server, for the pool pages.
                                        view.set_server_urls(
                                            connect_lines(
                                                bound,
                                                sv1_bound,
                                                authority.as_deref(),
                                                crate::reach::Interfaces::detect(),
                                            )
                                            .into_iter()
                                            .map(|line| (line.place, line.sv2, line.url))
                                            .collect(),
                                        );
                                        view.identify(&fleet);
                                        panel = Some(view);
                                    }
                                }
                                // #### end PR #42 ####
                                KeyCode::Char('a') | KeyCode::Char('A') => advanced = !advanced,
                                KeyCode::Char('i') | KeyCode::Char('I') if !advanced => {
                                    let interfaces = crate::reach::Interfaces::detect();
                                    connect = Some(ConnectPage {
                                        lines: connect_lines(
                                            bound,
                                            sv1_bound,
                                            authority.as_deref(),
                                            interfaces,
                                        ),
                                        templates: template_lines(tp_bound, interfaces),
                                        key: authority.clone(),
                                        mode: serve_mode,
                                        job_declaration: job_declaration_on,
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
                                        match save_donation(config_path, profile, next) {
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
    if let Some((_, uplink)) = uplink {
        let _ = uplink.join();
    }
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
            let authority = authority_key(&key)
                .map_err(|_| "--upstream-key is not an SV2 authority public key")?;
            Ok(super::sv1::Upstream {
                address,
                authority,
                identity: identity.clone(),
                remote: true,
                donation: None,
                public: false,
                prefer: None,
                fixed_version: false,
            })
        })
        .collect()
}

/// A pool's SV2 authority key from its published base58 form.
fn authority_key(key: &str) -> Result<[u8; 32], ()> {
    let decoded = stratum_core::bitcoin::base58::decode_check(key).map_err(|_| ())?;
    match decoded.as_slice() {
        [1, 0, key @ ..] => key.try_into().map_err(|_| ()),
        _ => Err(()),
    }
}

/// #### PR #42: solo mining's fallback pools, each with its key in its
/// address; devices open their channels there with the payout address.
fn fallback_upstreams(
    config: &RuntimeConfig,
    addresses: &[String],
) -> Result<Vec<super::sv1::Upstream>, String> {
    config::validate_payout_address(config.network, &config.payout_address)
        .map_err(|_| "--fallback-pool needs a valid payout address for the selected network")?;
    addresses
        .iter()
        .map(|address| {
            let (address, key) = super::split_pool_address(address)
                .map_err(|error| format!("--fallback-pool: {error}"))?;
            let key =
                key.ok_or("give each --fallback-pool with its key: stratum2+tcp://HOST:PORT/KEY")?;
            Ok(super::sv1::Upstream {
                address,
                authority: authority_key(&key)
                    .map_err(|_| "a --fallback-pool key is not an SV2 authority public key")?,
                identity: config.payout_address.clone(),
                remote: true,
                donation: None,
                public: false,
                prefer: None,
                fixed_version: false,
            })
        })
        .collect()
}

fn save_donation(path: &Path, profile: Option<&str>, value: BchDonation) -> Result<(), ()> {
    // Preserve unrelated saved settings. Saving must succeed before new jobs
    // use the changed percentage; in-flight jobs retain their original policy.
    let mut saved = config::SavedConfig::load_optional(path)
        .map_err(|_| ())?
        .unwrap_or_default();
    saved.bch_donation_bps = Some(value);
    saved.save(path).map_err(|_| ())?;
    // #### PR #42
    // What: a server started from a setup profile saves the donation into
    // that profile too.
    // Why: starting from a profile takes the profile's settings, so a change
    // made here was lost at the next start.
    // Look here if: a donation changed on the server's Advanced page comes
    // back changed after a restart from a profile.
    if let Some(name) = profile {
        let profiles_path = config::profiles_path(path);
        let mut profiles = config::MiningProfiles::load_optional(&profiles_path).map_err(|_| ())?;
        let entry = profiles
            .profiles
            .iter_mut()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .ok_or(())?;
        entry.settings.bch_donation_bps = Some(value);
        profiles.save(&profiles_path).map_err(|_| ())?;
    }
    Ok(())
}

/// #### PR #42: what the server started with.
struct Started {
    sv2: Option<std::net::SocketAddr>,
    sv1: Option<std::net::SocketAddr>,
    /// #### PR #42: where templates are served, if they are.
    templates: Option<std::net::SocketAddr>,
    start_difficulty: u64,
    pool_tag: String,
    /// The pools in failover order, when joining.
    pools: Option<String>,
    /// The pool knows this server by another name than the payout address.
    custom_user: bool,
    /// A public pool's fee, and whether it goes to the payout address.
    fee: Option<(crate::donation::bch::PoolFee, bool)>,
    /// #### PR #42: the Job Declaration modes the pool accepts, if any.
    job_declaration: Option<super::jd::AcceptJd>,
    /// #### PR #42: solo mining's fallback pools, in order.
    fallbacks: Option<String>,
    /// #### PR #42: how this server declares its node's templates to the
    /// first pool, when it does.
    declaring: Option<crate::cli::JobDeclarationMode>,
}

/// #### PR #42
/// What: the server's start values for its Advanced page: listening
/// addresses, the start difficulty and the vardiff rule, the pool's name, a
/// public pool's fee, and the pools joined. It never shows an address or key:
/// the fee says "your payout address" or "another address".
/// Why: these are set in the setup or on the command line, and apply after a
/// restart; the operator could not see them on a running server.
/// Look here if: the Advanced page shows a wrong start value or an address.
fn started_text(started: &Started) -> String {
    let mut text = String::from(
        "Set when the server started (change them in the setup's Advanced section or on the \
         command line; they apply after a restart):\n",
    );
    let listen = |name: &str, address: Option<std::net::SocketAddr>| {
        address.map(|address| format!("{name} {address}"))
    };
    let listeners: Vec<String> = [
        listen("SV2", started.sv2),
        listen("SV1", started.sv1),
        listen("Templates", started.templates),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !listeners.is_empty() {
        text.push_str(&format!("Listening  {}\n", listeners.join(" · ")));
    }
    match &started.pools {
        Some(pools) => text.push_str(&format!(
            "Pools  {pools} (in failover order), username: {}\n",
            if started.custom_user {
                "your own"
            } else {
                "your payout address"
            }
        )),
        None => {
            let digits = started.start_difficulty.to_string();
            let mut grouped = String::new();
            for (index, digit) in digits.chars().enumerate() {
                if index > 0 && (digits.len() - index).is_multiple_of(3) {
                    grouped.push(',');
                }
                grouped.push(digit);
            }
            text.push_str(&format!(
                "Start difficulty  {grouped}; vardiff then moves each device toward 20 shares a \
                 minute\n"
            ));
        }
    }
    if let Some(fallbacks) = &started.fallbacks {
        text.push_str(&format!(
            "Fallback pools  {fallbacks} (in order), while your node gives no work; devices \
             come back 30 seconds after it answers again\n"
        ));
    }
    if let Some(mode) = started.declaring {
        let mode = match mode {
            crate::cli::JobDeclarationMode::Full => "Full-Template",
            crate::cli::JobDeclarationMode::Coinbase => "Coinbase-only",
        };
        text.push_str(&format!(
            "Your templates  {mode} Job Declaration at the first pool, from your node; devices \
             mine the pools' own jobs while it refuses them\n"
        ));
    }
    if !started.pool_tag.is_empty() {
        text.push_str(&format!("Pool name  {}\n", started.pool_tag));
    }
    if let Some((fee, to_payout)) = &started.fee {
        text.push_str(&format!(
            "Pool fee  {} from {}, to {}\n",
            fee.rate,
            fee.mode,
            if *to_payout {
                "your payout address"
            } else {
                "another address"
            }
        ));
    }
    if let Some(accept) = started.job_declaration {
        let modes = match accept {
            super::jd::AcceptJd::CoinbaseOnly => "Coinbase-only",
            super::jd::AcceptJd::FullTemplate => "Full-Template",
            super::jd::AcceptJd::Both => "Full-Template and Coinbase-only",
        };
        text.push_str(&format!(
            "Miners' own templates  {modes} Job Declaration on the SV2 port; their coinbase \
             pays your fee and the donation in full\n"
        ));
    }
    text
}

/// The donation as the dashboard shows it, with its two parts.
fn donation_summary(donation: BchDonation) -> String {
    let (work, reward) = donation.shares();
    format!("{donation} ({work} of work · {reward} of block rewards)")
}

/// Advanced settings: the donation, adjustable from 0% to 100%.
fn render_advanced(
    frame: &mut Frame<'_>,
    donation: BchDonation,
    error: Option<&str>,
    pool: bool,
    started: &str,
) {
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
         works, in 0.5% steps. Changes apply to new jobs and are saved.\n\n{started}\n←/→ or \
         +/-  Change donation · a or Esc  Back"
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
const WATCH_FOOTER: &str = "↑/↓ PgUp/PgDn  Select · Enter  Device panel · q  Quit (the server keeps running)\nThe server's saved status; Now and 1 hour come from validated shares. Device actions go straight to the device.";

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
    /// #### PR #42: where pools take this node's templates, numbered after
    /// `lines`, and the key they pin.
    templates: Vec<ConnectLine>,
    key: Option<String>,
    mode: ServeMode,
    /// #### PR #42: Job Declaration is on: joining, this server declares its
    /// node's templates to the pool; a public pool accepts miners' own.
    job_declaration: bool,
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

/// #### PR #42: where Template Distribution clients (SV2 pools, P2Pool)
/// reach this node's templates, at every address this computer is reached
/// at, as `HOST:PORT`.
fn template_lines(
    listen: Option<std::net::SocketAddr>,
    interfaces: crate::reach::Interfaces,
) -> Vec<ConnectLine> {
    listen
        .map(|listen| {
            crate::reach::addresses(listen, interfaces)
                .into_iter()
                .map(|(place, address)| ConnectLine {
                    place,
                    sv2: true,
                    url: address.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
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
        // #### PR #42: with Job Declaration, this node builds the blocks.
        ServeMode::JoinPool if page.job_declaration => {
            "\nUsername: any name for the device; it names the device on the workers page. \
             This computer mines at the pool for you with your node's templates (Job \
             Declaration), and the pool's own jobs while the pool refuses them.\nMerge-mined \
             tokens are not added under Job Declaration yet. Mine solo or run your own pool \
             to merge-mine.\n"
        }
        ServeMode::JoinPool => {
            "\nUsername: any name for the device; it names the device on the workers page. \
             This computer mines at the pool for you.\nMerge-mined tokens are off at a pool: \
             the pool builds the blocks, so this computer cannot add token commitments. Mine \
             solo or run your own pool to merge-mine.\n"
        }
    });
    // #### PR #42: where miners' own templates go.
    if page.mode == ServeMode::Public && page.job_declaration {
        text.push_str(
            "Miners' own templates: a Job Declaration client (another Pickaxe's Join a pool \
             with Your templates on) connects to an SV2 line above, with the same key.\n",
        );
    }
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
    // #### PR #42: where pools take this node's templates.
    if !page.templates.is_empty() {
        text.push_str(
            "\nSV2 pools and P2Pool take this node's templates here (SV2 Template \
             Distribution, with the same key):\n",
        );
        for (index, line) in page.templates.iter().enumerate() {
            text.push_str(&format!(
                "  {}  {:<14} {}\n",
                page.lines.len() + index + 1,
                line.place.label(),
                line.url
            ));
        }
        if let (Some(first), Some(key)) = (page.templates.first(), page.key.as_deref()) {
            text.push_str(&format!(
                "An SRI pool or Job Declaration client takes them with:\n  \
                 [template_provider_type.Sv2Tp]\n  address = \"{}\"\n  public_key = \"{key}\"\n",
                first.url
            ));
        }
    }
    text.push_str(&format!(
        "\n{}  Copy a line · i or Esc  Back · q  Stop server",
        match page.lines.len() + page.templates.len() {
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
/// #### PR #42: the Job Declaration client's part of the status file.
fn jd_client_json(jd: &super::jd::client::JdClientSummary) -> serde_json::Value {
    serde_json::json!({
        "state": jd.state,
        "mode": jd.mode,
        "custom_jobs": jd.custom_jobs,
        "refused": jd.refused,
        "forwarded": jd.forwarded,
        "accepted": jd.accepted,
        "rejected": jd.rejected,
        "fallbacks": jd.fallbacks,
        "last_error": jd.last_error,
        "declared": jd.declared,
        "provided": jd.provided,
        "dropped": jd.dropped,
        "pushed": jd.pushed,
    })
}

/// #### PR #42: the Job Declaration server's part of the status file; its
/// Full-Template counts while it accepts Full-Template.
fn jd_server_json(jd: &server::JdServerStats) -> serde_json::Value {
    serde_json::json!({
        "clients": jd.clients,
        "tokens": jd.tokens,
        "custom_jobs": jd.custom_jobs,
        "refused": jd.refused,
        "last_refusal": jd.last_refusal,
        "blocks": jd.blocks,
        "full_template": jd.validator.map(|validator| serde_json::json!({
            "validator": validator,
            "declared": jd.declared,
            "missing_rounds": jd.missing_rounds,
            "validations": jd.validations,
            "pushed": jd.pushed,
            "push_unmatched": jd.push_unmatched,
            "blocks": {
                "pending": jd.declared_pending,
                "accepted": jd.declared_accepted,
                "rejected": jd.declared_rejected,
            },
        })),
    })
}

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
        // #### PR #42: the kind of the active template source.
        "template_source": snapshot.template_source.map(|kind| serde_json::json!({
            "kind": match kind {
                super::provider::SourceKind::NodeRpc => "rpc",
                super::provider::SourceKind::TemplateProvider => "tdp",
                super::provider::SourceKind::Token => "token",
            },
            "index": snapshot.active_node,
            "count": snapshot.nodes,
        })),
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
        // #### PR #42: merge-mined token wins, with no address or script.
        "tokens": serde_json::json!({
            "wins": snapshot.token_wins,
            "dropped": snapshot.token_wins_dropped,
            "off": snapshot.tokens_off,
            "recent": snapshot.recent_token_wins.iter().map(|win| serde_json::json!({
                "token": win.token,
                "mode": win.mode.to_string(),
                "height": win.height,
                "worker": win.worker,
                "seconds_ago": win.found.elapsed().as_secs(),
            })).collect::<Vec<_>>(),
        }),
        // #### PR #42: the Job Declaration client's state and counts, and the
        // server's, with no identity, token or address.
        "jd_client": snapshot.jd_client.as_ref().map(jd_client_json),
        "jd_server": snapshot.jd_server.as_ref().map(jd_server_json),
        // #### PR #42: an ASIC-exclusive token's wins, never a script or an
        // address.
        "header_token": snapshot.header_token.as_ref().map(|token| serde_json::json!({
            "token": token.token,
            "bits": format!("{:08x}", token.bits),
            "wins": token.wins,
            "proven": token.proven,
            "stale": token.stale,
            "dropped": token.dropped,
            "off": token.off,
        })),
        // #### PR #42: the template server's counts, with no address.
        "template_server": snapshot.template_server.as_ref().map(|templates| serde_json::json!({
            "clients": templates.clients,
            "sent": templates.sent,
            "withheld": templates.withheld,
            "solutions": {
                "received": templates.solutions,
                "invalid": templates.invalid,
                "refused_locally": templates.refused_locally,
                "unsaved": templates.unsaved,
            },
            "relayed": {
                "pending": templates.relay_pending,
                "accepted": templates.relay_accepted,
                "rejected": templates.relay_rejected,
            },
            "sent_once": {
                "sent": templates.once_sent,
                "accepted": templates.once_accepted,
            },
        })),
        "sv1_local_rejected": snapshot.sv1_local_rejected,
        "sessions_started": snapshot.sessions_started,
        "device_details": devices,
    })
}

// #### PR #42: the devices file
// What: the server writes `<config>.sv2-devices.json`, readable by its owner
// alone: where devices reach it, and each worker's label with its local
// address (and whether that address is shared). `stratum-v2 watch` reads it
// to open a Device panel for a row; every address is checked again before
// use, so an edited file cannot point Pickaxe at a public address.
// Why: the status file is readable by everyone and its lines are printed to
// service logs, so device addresses never go there.
// Look here if: watch cannot control a device, or an address appears in the
// status file.
/// The devices file beside a server's config.
fn devices_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("sv2-devices.json")
}

fn place_name(place: crate::reach::Place) -> &'static str {
    match place {
        crate::reach::Place::ThisComputer => "this-computer",
        crate::reach::Place::LocalNetwork => "local-network",
        crate::reach::Place::Tailscale => "tailscale",
    }
}

fn devices_json(server: &[ConnectLine], devices: &[(String, std::net::IpAddr, bool)]) -> Vec<u8> {
    serde_json::json!({
        "version": 1,
        "server": server
            .iter()
            .map(|line| serde_json::json!({
                "place": place_name(line.place),
                "sv2": line.sv2,
                "url": line.url,
            }))
            .collect::<Vec<_>>(),
        "devices": devices
            .iter()
            .map(|(label, ip, shared)| serde_json::json!({
                "label": label,
                "ip": ip.to_string(),
                "shared": shared,
            }))
            .collect::<Vec<_>>(),
    })
    .to_string()
    .into_bytes()
}

/// The devices file as the watch view reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DevicesFile {
    server: Vec<ServerLine>,
    devices: Vec<DeviceLine>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServerLine {
    place: String,
    sv2: bool,
    url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DeviceLine {
    label: String,
    ip: Option<std::net::IpAddr>,
    shared: bool,
}

impl DevicesFile {
    fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    /// The address of the worker with this label, only on the local network
    /// or Tailscale, and never a shared one.
    fn target(&self, label: &str) -> Result<std::net::IpAddr, AddressIssue> {
        let line = self
            .devices
            .iter()
            .find(|line| line.label == label)
            .ok_or(AddressIssue::Unknown)?;
        if line.shared {
            return Err(AddressIssue::Shared);
        }
        line.ip
            .filter(|ip| super::device_api::queryable(*ip))
            .ok_or(AddressIssue::Unknown)
    }

    fn server_urls(&self) -> Vec<(crate::reach::Place, bool, String)> {
        self.server
            .iter()
            .filter_map(|line| {
                let place = match line.place.as_str() {
                    "this-computer" => crate::reach::Place::ThisComputer,
                    "local-network" => crate::reach::Place::LocalNetwork,
                    "tailscale" => crate::reach::Place::Tailscale,
                    _ => return None,
                };
                Some((place, line.sv2, line.url.clone()))
            })
            .collect()
    }
}

/// #### PR #42: the Device panel for a row of the watch view, from the
/// server's devices file; refused when the file cannot be read.
fn watch_panel(devices_file: &Path, row: &WorkerLine) -> DevicePanel {
    let file = DevicesFile::read(devices_file);
    let target = match &file {
        Some(file) => file.target(&row.label),
        None => Err(AddressIssue::Unlisted),
    };
    let mut view = DevicePanel::for_line(
        row.label.clone(),
        row.connected,
        row.model.as_deref(),
        row.firmware.as_deref(),
        row.power_w,
        target,
    );
    if let Some(file) = &file {
        view.set_server_urls(file.server_urls());
    }
    view
}
// #### end PR #42 ####

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
    /// #### PR #42: Job Declaration, as the client and as a pool.
    jd_client: Option<WatchJdClient>,
    jd_server: Option<WatchJdServer>,
}

/// #### PR #42: the Job Declaration client's part of the saved status.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WatchJdClient {
    state: String,
    mode: String,
    custom_jobs: u64,
    refused: u64,
    fallbacks: u64,
}

/// #### PR #42: the Job Declaration server's part of the saved status.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct WatchJdServer {
    clients: u64,
    custom_jobs: u64,
    refused: u64,
    blocks: u64,
}

impl WatchStatus {
    /// #### PR #42: Job Declaration's lines, when it is on.
    fn job_declaration(&self) -> String {
        let mut text = String::new();
        if let Some(jd) = &self.jd_client {
            text.push_str(&format!(
                "\nJob Declaration ({}): {} · {} custom jobs · {} refused · {} fallbacks",
                jd.mode, jd.state, jd.custom_jobs, jd.refused, jd.fallbacks
            ));
        }
        if let Some(jd) = &self.jd_server {
            text.push_str(&format!(
                "\nJob Declaration clients {} · {} custom jobs · {} refused · {} blocks",
                jd.clients, jd.custom_jobs, jd.refused, jd.blocks
            ));
        }
        text
    }

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
            ) + &self.job_declaration();
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
            + &self.job_declaration()
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
// #### PR #42: the watch view controls devices
// What: `stratum-v2 watch` highlights a row like the server's workers page,
// and Enter opens the same Device panel. It acts on the devices itself, with
// the addresses from the server's owner-only devices file and the server's
// saved logins; it never talks to the server.
// Why: a server running as a service had no way to control its devices.
// Look here if: watch opens the wrong device, or cannot control any.
fn watch(config_path: &Path) -> Result<(), String> {
    let path = status_path(config_path);
    let devices_file = devices_path(config_path);
    let fleet = Arc::new(Fleet::new());
    // Without the server's logins, devices answer their default ones.
    let _ = fleet.load_logins(config_path);
    let mut terminal = TerminalSession::enter()?;
    let mut selection = Selection::default();
    let mut panel: Option<DevicePanel> = None;
    loop {
        let now = unix_now();
        let status = fs::read_to_string(&path)
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
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        selection.follow(&labels);
        terminal
            .terminal
            .draw(|frame| match panel.as_ref() {
                Some(view) => super::panel::render(frame, view),
                None => render_workers(frame, &header, &rows, &mut selection.table, WATCH_FOOTER),
            })
            .map_err(|_| "cannot draw workers table")?;
        if event::poll(Duration::from_millis(500)).map_err(|_| "cannot read terminal")? {
            if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // The panel takes every key, so `q` there never quits.
                if let Some(view) = panel.as_mut() {
                    if view.handle_key(key, &fleet) {
                        panel = None;
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ if super::panel::opens_panel(&key, true) => {
                        if let Some(row) = selection.index().and_then(|index| rows.get(index)) {
                            let view = watch_panel(&devices_file, row);
                            view.identify(&fleet);
                            panel = Some(view);
                        }
                    }
                    code => selection.step(code, &labels),
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
/// #### PR #42: the Job Declaration client's line on the overview, when this
/// server declares its templates to a pool.
fn jd_client_line(stats: &ServerStats, pool: Option<&str>) -> String {
    let Some(jd) = &stats.jd_client else {
        return String::new();
    };
    let pool = pool
        .and_then(|pools| pools.split(" → ").next())
        .unwrap_or("the pool");
    // Full-Template: the declarations, those dropped and the blocks pushed.
    let full = if jd.mode == super::jd::JdMode::FullTemplate.as_str() {
        format!(
            " · {} declared · {} dropped · {} blocks pushed",
            jd.declared, jd.dropped, jd.pushed
        )
    } else {
        String::new()
    };
    format!(
        "\nJob Declaration at {pool} ({}): {} · {} custom jobs · {} refused{full} · {} shares \
         sent ({} accepted, {} rejected) · {} fallbacks{}",
        jd.mode,
        jd.state,
        jd.custom_jobs,
        jd.refused,
        jd.forwarded,
        jd.accepted,
        jd.rejected,
        jd.fallbacks,
        jd.last_error
            .as_deref()
            .map(|error| format!(" (last: {error})"))
            .unwrap_or_default(),
    )
}

/// #### PR #42: the Job Declaration server's line on the overview, when the
/// pool accepts miners' own templates.
fn jd_line(stats: &ServerStats) -> String {
    let Some(jd) = &stats.jd_server else {
        return String::new();
    };
    // Full-Template: the declarations and what checks them, and the pool's
    // node's answers on their blocks.
    let (declared, blocks) = match jd.validator {
        Some(validator) => (
            format!(
                " · {} declared ({} missing-transaction rounds, {} checked: {validator})",
                jd.declared, jd.missing_rounds, jd.validations
            ),
            format!(
                "{} blocks · pool's node: {} accepted / {} pending / {} rejected",
                jd.blocks, jd.declared_accepted, jd.declared_pending, jd.declared_rejected
            ),
        ),
        None => (
            String::new(),
            format!("{} blocks (their nodes submit them)", jd.blocks),
        ),
    };
    format!(
        "\nJob Declaration clients {} · {} tokens{declared} · {} custom jobs · {} refused{} · \
         {blocks}",
        jd.clients,
        jd.tokens,
        jd.custom_jobs,
        jd.refused,
        jd.last_refusal
            .map(|code| format!(" (last: {code})"))
            .unwrap_or_default(),
    )
}

/// #### PR #42: the template server's line on the overview, while it serves
/// templates or holds relayed blocks.
fn templates_line(stats: &ServerStats) -> String {
    let Some(templates) = &stats.template_server else {
        return String::new();
    };
    let refused = templates.invalid + templates.refused_locally;
    format!(
        "\nTemplate clients {} · {} templates sent · {} withheld · Pool blocks relayed {} \
         accepted / {} pending / {} rejected{}",
        templates.clients,
        templates.sent,
        templates.withheld,
        templates.relay_accepted + templates.once_accepted,
        templates.relay_pending,
        templates.relay_rejected,
        if refused > 0 {
            format!(" · {refused} solutions refused")
        } else {
            String::new()
        }
    )
}

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
    // #### PR #42: merge-mined token wins, when a token is merge-mined.
    let tokens = match (&stats.tokens_off, stats.recent_token_wins.back()) {
        (Some(off), _) => format!(" · Tokens off: {off}"),
        (None, Some(last)) => format!(
            " · Token wins {} (last: {} case {} by {}{})",
            stats.recent_token_wins.len(),
            last.token,
            last.mode,
            last.worker,
            if stats.token_wins_dropped > 0 {
                format!("; {} dropped", stats.token_wins_dropped)
            } else {
                String::new()
            }
        ),
        (None, None) => String::new(),
    };
    // #### PR #42: an ASIC-exclusive token's wins.
    let header = stats
        .header_token
        .as_ref()
        .map(|token| match &token.off {
            Some(off) => format!(" · {} off: {off}", token.token),
            None => format!(
                " · {} wins {} ({} proven, {} stale, {} dropped)",
                token.token, token.wins, token.proven, token.stale, token.dropped
            ),
        })
        .unwrap_or_default();
    format!(
        "Best share {best} · Recent blocks {}{tokens}{header}",
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
    // #### PR #42: an ASIC-exclusive token needs no node.
    if stats.template_source == Some(super::provider::SourceKind::Token) {
        return " · no node: an ASIC-exclusive token instead of BCH".into();
    }
    // #### PR #42: templates from a template provider say so.
    if stats.template_source == Some(super::provider::SourceKind::TemplateProvider) {
        return format!(
            " · templates from template provider {} of {}",
            stats.active_node + 1,
            stats.nodes
        );
    }
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

/// #### PR #42: what preflight found: the nodes (for their identity and
/// client), every template source in failover order, and its first template.
type Preflight = (
    Vec<NativeNodeRpc>,
    Vec<Box<dyn TemplateSource>>,
    BchTemplate,
);

// #### PR #42: mixed failover
// What: the template providers (in their order), then the nodes (in theirs)
// are tried, and the first that gives a verified template goes first; the
// server then fails over across all of them. A provider's new parent is
// confirmed on this network by the first node, or by the network's Fulcrum
// servers without one.
// Why: a template provider can stand in for the miner's RPC login, and a
// node keeps mining going when the provider is down.
// Look here if: the server starts on another source than expected, or a
// provider is refused at start.
fn preflight_sources(
    config: &RuntimeConfig,
    providers: &[super::tdp::client::TdpAddress],
    reserve: u32,
) -> Result<Preflight, String> {
    if providers.is_empty() {
        let (nodes, template) = preflight(config)?;
        let sources = server::rpc_sources(nodes.clone(), config.network);
        return Ok((nodes, sources, template));
    }
    let endpoints = config.custom_node_endpoints();
    let mut sources: Vec<Box<dyn TemplateSource>> = providers
        .iter()
        .map(|address| {
            Box::new(super::tdp::client::TdpSource::new(
                address.clone(),
                reserve,
                chain_guard(config),
            )) as Box<dyn TemplateSource>
        })
        .collect();
    sources.extend(server::rpc_sources(
        endpoints
            .iter()
            .map(|endpoint| NativeNodeRpc::new((*endpoint).to_owned()))
            .collect(),
        config.network,
    ));
    let nodes = endpoints
        .iter()
        .map(|endpoint| NativeNodeRpc::new((*endpoint).to_owned()))
        .collect();
    let mut reason = "template provider sent no template";
    for index in 0..sources.len() {
        match sources[index].refresh() {
            Ok((_, template)) => {
                let template = template.clone();
                sources.rotate_left(index);
                return Ok((nodes, sources, template));
            }
            Err(error) => reason = server::template_reason(&error),
        }
    }
    Err(format!(
        "no configured template provider or BCH node supplied a synchronized template for the \
         selected network ({reason})"
    ))
}

/// #### PR #42: confirms a provider's parent on the selected network: by the
/// first configured node, or by the network's Fulcrum servers.
fn chain_guard(config: &RuntimeConfig) -> super::tdp::client::Guard {
    let network = config.network;
    let node = config
        .custom_node_endpoints()
        .first()
        .map(|node| node.to_string());
    let fulcrum = config.electrum_endpoints();
    Box::new(move |previous, height| {
        let mut display = *previous;
        display.reverse();
        let display = hex::encode(display);
        let parent = height.checked_sub(1).ok_or("no parent")?;
        if let Some(node) = &node {
            let header =
                crate::node::rpc_call(node, "getblockheader", serde_json::json!([display, true]))?;
            return (header.get("hash").and_then(serde_json::Value::as_str)
                == Some(display.as_str())
                && header.get("height").and_then(serde_json::Value::as_u64)
                    == Some(u64::from(parent)))
            .then_some(())
            .ok_or_else(|| "the parent is not on this network".to_owned());
        }
        let mut session = crate::electrum::ElectrumSession::connect_failover_for_deployment(
            &fulcrum,
            crate::config::MiningToken::Photon.photon_deployment(network),
        )?;
        let header = session.rpc("blockchain.block.header", serde_json::json!([parent]))?;
        let bytes = hex::decode(header.as_str().ok_or("malformed header")?)
            .map_err(|_| "malformed header")?;
        let mut hash = super::template::double_sha256(&bytes);
        hash.reverse();
        (hex::encode(hash) == display)
            .then_some(())
            .ok_or_else(|| "the parent is not on this network".to_owned())
    })
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

    // #### PR #42
    // What: merge-mined token wins show on the dashboard's records line and
    // in the status file (token, case, height, worker; never an address),
    // and a journal failure says token claims are off.
    // Look here if: records_line or status_json's tokens change.
    #[test]
    fn token_wins_show_on_the_dashboard_and_in_the_status() {
        let mut stats = ServerStats::default();
        assert!(!records_line(&stats).contains("Token"));
        stats.token_wins = 2;
        stats.token_wins_dropped = 1;
        stats.recent_token_wins.push_back(server::FoundTokenWin {
            token: "Pickaxe test token",
            mode: 'A',
            height: 325_909,
            worker: "rig1 #1".into(),
            found: Instant::now(),
        });
        let line = records_line(&stats);
        assert!(
            line.contains("Token wins 1 (last: Pickaxe test token case A by rig1 #1; 1 dropped)"),
            "{line}"
        );
        let status = status_json("chipnet", None, None, &stats, BchDonation::default(), &[]);
        assert_eq!(status["tokens"]["wins"], 2);
        assert_eq!(status["tokens"]["dropped"], 1);
        assert_eq!(status["tokens"]["recent"][0]["mode"], "A");
        assert_eq!(status["tokens"]["recent"][0]["height"], 325_909);
        stats.tokens_off = Some("token proofs cannot be saved: disk full".into());
        assert!(records_line(&stats).contains("Tokens off: token proofs cannot be saved"));
        assert!(super::super::status_report().contains("merge mining: commitment v1 (draft)"));
    }

    // #### PR #42
    // What: an ASIC-exclusive token's counts show on the overview and in the
    // status file (kind "token"), with no script or address, and the
    // overview says no node is needed.
    // Look here if: the token's dashboard or status lines change.
    #[test]
    fn status_json_reports_token_mode_without_scripts_or_addresses() {
        let mut stats = ServerStats {
            template_source: Some(super::super::provider::SourceKind::Token),
            header_token: Some(super::super::merge::source::HeaderSummary {
                token: "Pickaxe ASIC test token",
                bits: 0x207f_ffff,
                wins: 3,
                proven: 2,
                stale: 1,
                dropped: 0,
                off: None,
            }),
            ..ServerStats::default()
        };
        let status = status_json("chipnet", None, None, &stats, BchDonation::default(), &[]);
        assert_eq!(status["header_token"]["proven"], 2);
        assert_eq!(status["header_token"]["bits"], "207fffff");
        assert_eq!(status["template_source"]["kind"], "token");
        let text = status.to_string();
        assert!(!text.contains("bchtest") && !text.contains("76a914"));
        assert!(records_line(&stats)
            .contains("Pickaxe ASIC test token wins 3 (2 proven, 1 stale, 0 dropped)"));
        assert!(node_label(None, &stats).contains("an ASIC-exclusive token instead of BCH"));
        if let Some(token) = stats.header_token.as_mut() {
            token.off = Some("a token win failed its own check".into());
        }
        assert!(records_line(&stats)
            .contains("Pickaxe ASIC test token off: a token win failed its own check"));
    }

    // #### PR #42
    // What: the template server's counts show on the overview and in the
    // status file, which never holds an address; Connection info lists where
    // pools take the templates, numbered after the device lines, with SRI's
    // configuration lines and the key.
    // Look here if: templates_line, status_json's template_server or
    // connect_text's template section changes.
    #[test]
    fn the_template_server_shows_its_counts_and_where_pools_connect() {
        use crate::reach::{Interfaces, Place};
        let mut stats = ServerStats::default();
        assert_eq!(templates_line(&stats), "");
        let status = status_json("chipnet", None, None, &stats, BchDonation::default(), &[]);
        assert!(status["template_server"].is_null());
        assert!(status["jd_server"].is_null());
        assert_eq!(jd_line(&stats), "");
        stats.jd_server = Some(server::JdServerStats {
            clients: 1,
            tokens: 3,
            custom_jobs: 2,
            refused: 1,
            last_refusal: Some("stale-chain-tip"),
            blocks: 1,
            ..Default::default()
        });
        assert!(jd_line(&stats).contains(
            "Job Declaration clients 1 · 3 tokens · 2 custom jobs · 1 refused (last: \
             stale-chain-tip) · 1 blocks (their nodes submit them)"
        ));
        let status = status_json("chipnet", None, None, &stats, BchDonation::default(), &[]);
        assert_eq!(status["jd_server"]["custom_jobs"], 2);
        assert_eq!(status["jd_server"]["last_refusal"], "stale-chain-tip");
        assert!(status["jd_server"]["full_template"].is_null());
        // #### PR #42: a pool that accepts Full-Template shows its
        // declarations, its node check and its node's answers.
        if let Some(jd) = stats.jd_server.as_mut() {
            jd.validator = Some("validateblocktemplate");
            jd.declared = 4;
            jd.missing_rounds = 1;
            jd.validations = 2;
            jd.declared_accepted = 1;
        }
        assert!(jd_line(&stats).contains(
            "3 tokens · 4 declared (1 missing-transaction rounds, 2 checked: \
             validateblocktemplate) · 2 custom jobs"
        ));
        assert!(
            jd_line(&stats).contains("1 blocks · pool's node: 1 accepted / 0 pending / 0 rejected")
        );
        let status = status_json("chipnet", None, None, &stats, BchDonation::default(), &[]);
        assert_eq!(status["jd_server"]["full_template"]["declared"], 4);
        assert_eq!(
            status["jd_server"]["full_template"]["validator"],
            "validateblocktemplate"
        );
        stats.template_server = Some(server::TemplateServerStats {
            clients: 2,
            sent: 341,
            withheld: 1,
            solutions: 3,
            invalid: 1,
            refused_locally: 1,
            unsaved: 0,
            relay_pending: 0,
            relay_accepted: 1,
            relay_rejected: 0,
            once_sent: 1,
            once_accepted: 0,
        });
        let line = templates_line(&stats);
        assert!(
            line.contains(
                "Template clients 2 · 341 templates sent · 1 withheld · Pool blocks relayed 1 \
                 accepted / 0 pending / 0 rejected · 2 solutions refused"
            ),
            "{line}"
        );
        let status = status_json("chipnet", None, None, &stats, BchDonation::default(), &[]);
        let templates = &status["template_server"];
        assert_eq!(templates["clients"], 2);
        assert_eq!(templates["sent"], 341);
        assert_eq!(templates["solutions"]["refused_locally"], 1);
        assert_eq!(templates["relayed"]["accepted"], 1);
        assert_eq!(templates["sent_once"]["sent"], 1);
        assert!(!templates.to_string().contains("192.168"));
        let local: std::net::IpAddr = "192.168.0.160".parse().unwrap();
        let interfaces = Interfaces {
            local: Some(local),
            tailscale: None,
        };
        let page = ConnectPage {
            lines: connect_lines(
                Some("0.0.0.0:3336".parse().unwrap()),
                Some("0.0.0.0:3333".parse().unwrap()),
                Some("KEY"),
                interfaces,
            ),
            templates: template_lines(Some("0.0.0.0:48442".parse().unwrap()), interfaces),
            key: Some("KEY".into()),
            mode: ServeMode::Solo,
            job_declaration: false,
            note: None,
        };
        assert_eq!(page.templates[0].place, Place::LocalNetwork);
        let text = connect_text(&page);
        let number = page.lines.len() + 1;
        assert!(
            text.contains(&format!("  {number}  your network   192.168.0.160:48442")),
            "{text}"
        );
        assert!(
            text.contains(
                "[template_provider_type.Sv2Tp]\n  address = \"192.168.0.160:48442\"\n  \
                 public_key = \"KEY\""
            ),
            "{text}"
        );
        assert!(text.contains(&format!("1-{number}  Copy a line")), "{text}");
    }

    // #### PR #42
    // What: the devices file carries this server's addresses and each
    // worker's local address; the watch view takes a worker's address from
    // it only when it is local and not shared, refuses an edited public one,
    // and explains how to run watch when the file cannot be read.
    // Look here if: devices_json, DevicesFile or watch_panel changes.
    #[test]
    fn the_watch_view_controls_devices_only_through_the_owner_only_devices_file() {
        let dir = super::super::journal::TestDirectory::new();
        let config = dir.0.join("chipnet.json");
        let lines = vec![ConnectLine {
            place: crate::reach::Place::LocalNetwork,
            sv2: false,
            url: "stratum+tcp://192.168.0.55:3333".into(),
        }];
        let devices = vec![
            ("rig1 #1".to_owned(), "192.168.0.80".parse().unwrap(), false),
            ("rig2 #2".to_owned(), "100.64.0.9".parse().unwrap(), true),
        ];
        let bytes = devices_json(&lines, &devices);
        crate::config::write_private_atomic(&devices_path(&config), &bytes).unwrap();
        let file = DevicesFile::read(&devices_path(&config)).unwrap();
        assert_eq!(file.target("rig1 #1"), Ok("192.168.0.80".parse().unwrap()));
        assert_eq!(file.target("rig2 #2"), Err(AddressIssue::Shared));
        assert_eq!(file.target("rig9 #9"), Err(AddressIssue::Unknown));
        assert_eq!(
            file.server_urls(),
            vec![(
                crate::reach::Place::LocalNetwork,
                false,
                "stratum+tcp://192.168.0.55:3333".to_owned()
            )]
        );
        // An edited file cannot point at a public address.
        let edited = String::from_utf8(bytes)
            .unwrap()
            .replace("192.168.0.80", "8.8.8.8");
        fs::write(devices_path(&config), edited).unwrap();
        let file = DevicesFile::read(&devices_path(&config)).unwrap();
        assert_eq!(file.target("rig1 #1"), Err(AddressIssue::Unknown));
        // The panel for a watch row; without the file, it explains.
        let row = WorkerLine {
            label: "rig1 #1".into(),
            connected: true,
            model: Some("Avalonminer AvalonNano3s".into()),
            ..WorkerLine::default()
        };
        let missing = dir.0.join("other.json");
        let view = watch_panel(&devices_path(&missing), &row);
        let text = {
            let backend = ratatui::backend::TestBackend::new(100, 40);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| super::super::panel::render(frame, &view))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(
            text.contains("cannot read the server's list of device"),
            "{text}"
        );
        assert!(text.contains("Run watch as that user"), "{text}");
    }

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
        save_donation(&path, None, rate).unwrap();
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
        assert!(save_donation(&path, None, BchDonation::default()).is_err());
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
            templates: Vec::new(),
            key: None,
            mode: ServeMode::Public,
            job_declaration: false,
            note: None,
        };
        let text = connect_text(&public);
        assert!(!text.contains("Miners' own templates"), "{text}");
        // #### PR #42: a pool accepting Job Declaration says where it goes.
        let accepting = connect_text(&ConnectPage {
            job_declaration: true,
            lines: connect_lines(
                Some("0.0.0.0:3336".parse().unwrap()),
                None,
                Some("KEY"),
                both,
            ),
            templates: Vec::new(),
            key: None,
            mode: ServeMode::Public,
            note: None,
        });
        assert!(
            accepting.contains("Miners' own templates: a Job Declaration client"),
            "{accepting}"
        );
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
            templates: Vec::new(),
            key: None,
            mode: ServeMode::Solo,
            job_declaration: false,
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
        // #### PR #42: with Job Declaration this node builds the blocks.
        let declaring = connect_text(&ConnectPage {
            job_declaration: true,
            note: None,
            ..join
        });
        assert!(
            declaring.contains("with your node's templates (Job Declaration)"),
            "{declaring}"
        );
        assert!(
            !declaring.contains("the pool builds the blocks"),
            "{declaring}"
        );
        // A loopback listener: only this computer.
        let local_only = ConnectPage {
            lines: connect_lines(
                Some("127.0.0.1:3336".parse().unwrap()),
                None,
                Some("KEY"),
                both,
            ),
            templates: Vec::new(),
            key: None,
            mode: ServeMode::Solo,
            job_declaration: false,
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
        assert!(!header.contains("Job Declaration"), "{header}");
        // #### PR #42: Job Declaration's lines, as the client and as a pool.
        stats.jd_client = Some(super::super::jd::client::JdClientSummary {
            state: "active",
            mode: "full-template",
            custom_jobs: 12,
            refused: 1,
            fallbacks: 2,
            ..Default::default()
        });
        stats.jd_server = Some(server::JdServerStats {
            clients: 3,
            custom_jobs: 5,
            refused: 0,
            blocks: 1,
            ..Default::default()
        });
        let status = status_json(
            "chipnet",
            Some("pool.example:3336"),
            None,
            &stats,
            BchDonation::default(),
            &devices,
        );
        let saved: WatchStatus = serde_json::from_str(&status.to_string()).unwrap();
        let header = saved.header(saved.updated);
        assert!(
            header.contains(
                "Job Declaration (full-template): active · 12 custom jobs · 1 refused · 2 \
                 fallbacks"
            ),
            "{header}"
        );
        assert!(
            header.contains("Job Declaration clients 3 · 5 custom jobs · 0 refused · 1 blocks"),
            "{header}"
        );
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
                    &started_text(&Started {
                        sv2: Some("0.0.0.0:3336".parse().unwrap()),
                        sv1: Some("0.0.0.0:3338".parse().unwrap()),
                        templates: Some("0.0.0.0:48442".parse().unwrap()),
                        start_difficulty: 65_536,
                        pool_tag: "/MyPool/".into(),
                        pools: None,
                        custom_user: false,
                        fee: Some((
                            crate::donation::bch::PoolFee {
                                rate: "1.5".parse().unwrap(),
                                mode: crate::donation::bch::FeeMode::Coinbase,
                            },
                            false,
                        )),
                        job_declaration: Some(crate::stratum_v2::jd::AcceptJd::Both),
                        fallbacks: None,
                        declaring: None,
                    }),
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
        // #### PR #42: the start values, read-only, never an address.
        for expected in [
            "apply after a restart",
            "SV1 0.0.0.0:3338",
            "Templates 0.0.0.0:48442",
            "65,536",
            "/MyPool/",
            "1.50% from",
            "to another address",
            "Miners' own templates  Full-Template and Coinbase-only",
        ] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        let joined = started_text(&Started {
            sv2: None,
            sv1: Some("0.0.0.0:3333".parse().unwrap()),
            templates: None,
            start_difficulty: 4096,
            pool_tag: String::new(),
            pools: Some("pool.example:3336 → b1.example:3336".into()),
            custom_user: true,
            fee: None,
            job_declaration: None,
            fallbacks: None,
            declaring: Some(crate::cli::JobDeclarationMode::Coinbase),
        });
        assert!(joined.contains("pool.example:3336 → b1.example:3336 (in failover order)"));
        assert!(joined.contains("username: your own"));
        assert!(!joined.contains("Start difficulty"));
        // #### PR #42: Join a pool with the node's own templates says so.
        assert!(joined.contains(
            "Your templates  Coinbase-only Job Declaration at the first pool, from your node"
        ));
        // #### PR #42: solo mining names its fallback pools.
        let solo = started_text(&Started {
            sv2: Some("0.0.0.0:3336".parse().unwrap()),
            sv1: Some("0.0.0.0:3333".parse().unwrap()),
            templates: None,
            start_difficulty: 4096,
            pool_tag: String::new(),
            pools: None,
            custom_user: false,
            fee: None,
            job_declaration: None,
            fallbacks: Some("a.example:3336 → b.example:3336".into()),
            declaring: None,
        });
        assert!(solo.contains(
            "Fallback pools  a.example:3336 → b.example:3336 (in order), while your node gives \
             no work"
        ));
    }

    // #### PR #42
    // What: a donation changed on the server's Advanced page is saved into
    // the profile the server started from, and fails closed when that
    // profile is gone.
    // Look here if: save_donation changes.
    #[test]
    fn a_servers_donation_is_saved_to_its_profile() {
        let dir = super::super::journal::TestDirectory::new();
        let path = dir.0.join("chipnet.json");
        config::SavedConfig {
            network: Some("chipnet".into()),
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        let mut profiles = config::MiningProfiles::default();
        profiles
            .upsert(
                None,
                "Pool",
                config::SavedConfig {
                    network: Some("chipnet".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        profiles.save(&config::profiles_path(&path)).unwrap();
        let rate: BchDonation = "0.5".parse().unwrap();
        save_donation(&path, Some("pool"), rate).unwrap();
        let saved = config::MiningProfiles::load_optional(&config::profiles_path(&path)).unwrap();
        assert_eq!(saved.profiles[0].settings.bch_donation_bps, Some(rate));
        assert_eq!(
            config::SavedConfig::load(&path).unwrap().bch_donation_bps,
            Some(rate)
        );
        assert!(save_donation(&path, Some("Gone"), rate).is_err());
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
