//! #### PR #38
//! Native node preflight and mining-server lifecycle. Reuse the terminal guard,
//! keep authority secrets private, and never print node credentials or payouts.

use super::{
    provider::{NativeNodeRpc, TemplateProvider},
    server::{self, ServerConfig, ServerStats},
    template::{compact_target, BchTemplate},
};
use crate::{
    cli::StratumV2Command,
    config::{self, RuntimeConfig},
    tui::TerminalSession,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::{Block, Paragraph, Wrap};
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
    time::Duration,
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
    let StratumV2Command::Serve { listen } = action else {
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
    };
    let worker = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || server::run(listener, rpc, settings, stop, stats))
    };
    // SV2 reference authority public-key encoding: version 1 (little endian),
    // 32-byte x-only key, Base58Check. Only the public key is displayed.
    let mut encoded = vec![1, 0];
    encoded.extend(public);
    let authority = stratum_core::bitcoin::base58::encode_check(&encoded);
    let result = (|| {
        if terminal.is_none() {
            println!(
                "{}",
                serde_json::json!({"listen":listen.to_string(),"authority":authority,"network":config.network.as_str()})
            );
        }
        while !stop.load(Ordering::Relaxed) && !worker.is_finished() {
            let snapshot = stats
                .lock()
                .map_err(|_| "mining statistics unavailable")?
                .clone();
            if let Some(terminal) = terminal.as_mut() {
                let status = format!(
                    "Network       {}\nListener      {}\nNode          {}\nHeight        {}\nDevices       {}\nShares        {} accepted / {} rejected\nBlocks        {} accepted / {} unconfirmed\nConnections   {} errors\n\nAuthority public key\n{}\n\nq  Stop mining server",
                    config.network.as_str(), listen, if snapshot.template_ready { "Ready" } else { "Waiting for a valid template" },
                    snapshot.height.map(|height| height.to_string()).unwrap_or_else(|| "Waiting".into()), snapshot.connections,
                    snapshot.shares_accepted, snapshot.shares_rejected, snapshot.blocks_accepted, snapshot.blocks_unconfirmed,
                    snapshot.connection_errors, authority,
                );
                terminal
                    .terminal
                    .draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(status)
                                .block(Block::bordered().title("Pickaxe · BCH ASIC mining"))
                                .wrap(Wrap { trim: false }),
                            frame.area(),
                        );
                    })
                    .map_err(|_| "cannot draw mining dashboard")?;
                if event::poll(Duration::from_millis(500)).map_err(|_| "cannot read terminal")? {
                    if let Event::Key(key) = event::read().map_err(|_| "cannot read terminal")? {
                        if key.kind == KeyEventKind::Press
                            && (key.code == KeyCode::Char('q')
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL)))
                        {
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                }
            } else {
                println!(
                    "{}",
                    serde_json::json!({"ready":snapshot.template_ready,"height":snapshot.height,
                    "devices":snapshot.connections,"shares_accepted":snapshot.shares_accepted,"shares_rejected":snapshot.shares_rejected,
                    "blocks_accepted":snapshot.blocks_accepted,"blocks_unconfirmed":snapshot.blocks_unconfirmed,
                    "connection_errors":snapshot.connection_errors})
                );
                thread::sleep(Duration::from_secs(1));
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    let server_result = worker
        .join()
        .map_err(|_| "mining server stopped unexpectedly")?;
    result.and(server_result)
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
