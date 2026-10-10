//! Pickaxe Miner - interactive CLI (Stage 2/3).
//!
//! Runtime controls preserve the authoritative PHOTON reference semantics.
//! Each supported token declares its fee policy.
//! Search/CPU/crypto: Lead Dev. Electrum/win-tx: Dev Assist.

use pickaxe_miner::{
    backend, benchmark, cli, config, electrum, mine_watch, mining_lock, node, reach, rigs, runtime,
    search, self_test, stratum_v2, telemetry, tui, tx,
};

use config::RuntimeConfig;
use electrum::{ElectrumSession, LiveJob};
use search::SearchHandle;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Prints the miner startup banner.
fn print_banner() {
    println!(
        "Pickaxe Miner {} - interactive CLI",
        env!("CARGO_PKG_VERSION")
    );
    println!("Type `help` for commands.\n");
}

/// Prints available interactive commands and their syntax.
fn print_help() {
    println!(
        r#"Commands:
  help                         Show this help
  status                       Show intensity, payout, mining, job, rate
  intensity <10-100>           Set live GPU intensity (default 100)
  pause | p                    Pause/resume GPU mining
  payout <cashaddr>            Set miner payout address
  donation                     Show donation percentage
  connect                      Electrum/Fulcrum connect (custom then bootstrap)
  fulcrum <wss://...>          Set custom Fulcrum/Electrum WSS URL
  fulcrum clear                Clear custom Fulcrum URL
  node <http://...>            Set custom native node JSON-RPC URL
  node clear                   Clear custom node URL
  servers                      Show Fulcrum + node try-order (ban-safe)
  nodeprobe                    Probe native node RPC (getblockchaininfo)
  job                          Fetch live PHOTON baton job
  preview                      connect+job + proven 2-output tx preview (no mining)
  arm                          like preview + message SHA256 for Schnorr (no keys)
  applysig <nonce> <pk33hex> <sig64hex>  verify+arm proven 2-output winner (no broadcast)
  quit | exit                  Leave

Use `donation` to display the token fee policy."#
    );
}

/// Prints the current configuration, live job, and search status.
fn print_status(cfg: &RuntimeConfig, handle: &Option<SearchHandle>, job: &Option<LiveJob>) {
    println!("intensity:     {}%", cfg.intensity);
    println!(
        "payout:        {}",
        if cfg.payout_address.is_empty() {
            "(not set)"
        } else {
            &cfg.payout_address
        }
    );
    println!(
        "mining:        {}",
        if cfg.mining {
            "ON (GPU M1 rate)"
        } else {
            "off"
        }
    );
    if let Some(h) = handle {
        let s = h.snapshot();
        println!(
            "state:         {}",
            match s.state {
                search::MiningState::Paused => "PAUSED",
                search::MiningState::Mining => "MINING",
                _ => "STOPPED",
            }
        );
        println!("candidates:    {}", s.candidates);
        println!("active intensity: {}%", s.intensity);
        println!("winners:       {}", s.winners);
        println!("elapsed:       {}s", s.elapsed_secs);
        println!(
            "rate:          {}",
            crate::telemetry::format_hash_rate(s.rate)
        );
    }
    match &cfg.fulcrum_url {
        Some(u) => println!("fulcrum:       {} (custom, tried first)", redact_url(u)),
        None => println!("fulcrum:       (bootstrap only)"),
    }
    if let Some(j) = job {
        println!("electrum:      {}", j.url);
        println!("job height:    {}", j.height);
        println!("job baton:     {}:{}", j.baton_txid, j.baton_vout);
        println!("job target:    set ({} hex chars)", j.target_le_hex.len());
        println!("job reward:    {}", j.reward_raw);
    } else {
        println!("electrum/job:  (run `connect` / `job`)");
    }
    println!("PHOTON jobs:   Fulcrum CashToken baton index");
    println!("broadcast pref:{}", cfg.source.as_str());
    println!(
        "gpu pipeline:  {}",
        if search::REFERENCE_GPU_PIPELINE_READY {
            "reference A->B->C ready"
        } else {
            "gated: exact Stage C/full-tx HASH256 not wired yet"
        }
    );
}

/// Removes credentials before displaying an endpoint URL.
fn redact_url(url: &str) -> String {
    url.split(',')
        .map(|endpoint| crate::node::redact_url(endpoint.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Displays the newly fetched live PHOTON job.
fn publish_live_job(cfg: &mut RuntimeConfig, live: &mut Option<LiveJob>, job: LiveJob) {
    let changed = live.as_ref().is_none_or(|current| {
        current.baton_txid != job.baton_txid
            || current.baton_vout != job.baton_vout
            || current.baton_height != job.baton_height
            || current.baton_value_sats != job.baton_value_sats
            || current.height != job.height
            || current.age != job.age
            || current.commitment_hex != job.commitment_hex
            || current.target_le_hex != job.target_le_hex
            || current.token_amount != job.token_amount
            || current.reward_raw != job.reward_raw
            || current.url != job.url
    });
    if changed {
        cfg.bump_generation();
    }
    *live = Some(job);
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ArmedTx {
    raw_hex: String,
    generation_id: u64,
    baton_txid: String,
    baton_vout: u32,
}

impl ArmedTx {
    /// Creates a ArmedTx for the miner CLI.
    fn new(raw_hex: String, cfg: &RuntimeConfig, job: &LiveJob) -> Result<Self, String> {
        if cfg.generation_id == 0 {
            return Err("cannot arm a transaction before a live generation is published".into());
        }
        Ok(Self {
            raw_hex,
            generation_id: cfg.generation_id,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
        })
    }

    #[cfg(test)]
    /// Checks that the current job still matches the live state.
    fn validate_current<'a>(
        &'a self,
        cfg: &RuntimeConfig,
        live: Option<&LiveJob>,
    ) -> Result<&'a str, String> {
        if self.generation_id != cfg.generation_id {
            return Err(format!(
                "armed transaction is stale: generation {} != current {}",
                self.generation_id, cfg.generation_id
            ));
        }
        let live = live.ok_or("armed transaction is stale: no current PHOTON baton job")?;
        if self.baton_txid != live.baton_txid || self.baton_vout != live.baton_vout {
            return Err("armed transaction is stale: PHOTON baton outpoint changed".into());
        }
        Ok(&self.raw_hex)
    }
}

/// Builds reference transaction context for a live job.
fn reference_job_context(job: &LiveJob) -> tx::ReferenceJobContext {
    tx::ReferenceJobContext {
        prev_txid: job.baton_txid.clone(),
        prev_vout: job.baton_vout,
        age: job.age,
        target_le_hex: job.target_le_hex.clone(),
        contract_value_sats: job.baton_value_sats,
        relay_fee_sats_per_kb: job.relay_fee_sats_per_kb,
        contract_token_amount: job.token_amount,
        reward_raw: job.reward_raw,
    }
}

/// Fetches and validates a refreshed live PHOTON job.
fn refresh_live_job(cfg: &mut RuntimeConfig, live: &mut Option<LiveJob>) -> Result<(), String> {
    let mut session = ElectrumSession::connect_failover_for_deployment(
        &cfg.electrum_endpoints(),
        cfg.token.photon_deployment(cfg.network),
    )?;
    let job = session.fetch_live_job()?;
    publish_live_job(cfg, live, job);
    Ok(())
}

/// Synchronizes the GPU search worker with the live job.
fn sync_search_job(
    cfg: &RuntimeConfig,
    handle: Option<&SearchHandle>,
    live: Option<&LiveJob>,
) -> Result<(), String> {
    let Some(handle) = handle else {
        return Ok(());
    };
    if handle.generation_id() == cfg.generation_id {
        return Ok(());
    }
    let live = live.ok_or("cannot update GPU generation without a live PHOTON job")?;
    let job = live.to_mining_job(cfg.generation_id, &cfg.payout_address);
    if let Err(error) = handle.replace_job(job) {
        let _ = handle.apply_control(search::RuntimeCommand::Pause);
        return Err(format!(
            "failed to apply GPU generation {}; search paused: {error}",
            cfg.generation_id
        ));
    }
    Ok(())
}

/// Rechecks a GPU winner against the current live job.
fn validate_verified_winner_current(
    winner: &search::VerifiedWinner,
    cfg: &RuntimeConfig,
    live: Option<&LiveJob>,
) -> Result<(), String> {
    if winner.generation_id != cfg.generation_id {
        return Err(format!(
            "winner generation {} is stale; current generation is {}",
            winner.generation_id, cfg.generation_id
        ));
    }
    let live = live.ok_or("no current PHOTON baton job")?;
    if winner.height != live.height {
        return Err(format!(
            "winner BCH height {} is stale; current height is {}",
            winner.height, live.height
        ));
    }
    if winner.baton_txid != live.baton_txid || winner.baton_vout != live.baton_vout {
        return Err("winner PHOTON baton outpoint is stale".into());
    }
    if winner.job_reward_raw != live.reward_raw {
        return Err("winner PHOTON reward job is stale".into());
    }
    Ok(())
}

/// Processes verified GPU winners before continuing search.
fn process_gpu_winners(
    cfg: &mut RuntimeConfig,
    handle: &Option<SearchHandle>,
    live: &mut Option<LiveJob>,
    armed: &mut Option<ArmedTx>,
) {
    let pending = handle
        .as_ref()
        .map(SearchHandle::drain_winners)
        .unwrap_or_default();
    for winner in pending {
        if winner.generation_id != cfg.generation_id {
            println!(
                "discarded stale GPU winner: generation {} != {}",
                winner.generation_id, cfg.generation_id
            );
            continue;
        }
        if let Err(error) = refresh_live_job(cfg, live) {
            println!("refusing GPU winner: fresh PHOTON state recheck failed: {error}");
            continue;
        }
        if let Err(error) = sync_search_job(cfg, handle.as_ref(), live.as_ref()) {
            println!("error: {error}");
        }
        if let Err(error) = validate_verified_winner_current(&winner, cfg, live.as_ref()) {
            println!("discarded stale GPU winner: {error}");
            continue;
        }
        let current = live
            .as_ref()
            .expect("fresh winner validation requires live job");
        match ArmedTx::new(hex::encode(&winner.transaction), cfg, current) {
            Ok(candidate) => {
                println!(
                    "GPU winner independently verified and fresh at height {}: nonce={} hash={}",
                    winner.height,
                    winner.nonce,
                    hex::encode(winner.digest)
                );
                println!(
                    "winner retained for legacy inspection; live submission uses `pickaxe mine`"
                );
                *armed = Some(candidate);
            }
            Err(error) => println!("refusing GPU winner: {error}"),
        }
    }
}

/// Prints the miner donation policy in effect.
fn print_donation(cfg: &RuntimeConfig) {
    println!("{}", cfg.fee_policy().scheme.description());
}

/// Parses and executes one interactive command.
fn handle_line(
    cfg: &mut RuntimeConfig,
    handle: &mut Option<SearchHandle>,
    live: &mut Option<LiveJob>,
    armed: &mut Option<ArmedTx>,
    last_template: &mut Option<node::BlockTemplate>,
    line: &str,
) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return true;
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();

    match cmd.as_str() {
        "help" | "?" => print_help(),
        "status" => print_status(cfg, handle, live),
        "donation" => print_donation(cfg),

        "broadcast" => println!(
            "legacy REPL submission is unavailable; use pickaxe mine --backend cuda --no-tui"
        ),
        "quit" | "exit" => {
            if let Some(h) = handle.take() {
                let s = h.stop();
                cfg.mining = false;
                println!(
                    "stopped. candidates={} rate={}",
                    s.candidates,
                    crate::telemetry::format_hash_rate(s.rate)
                );
            }
            println!("bye.");
            return false;
        }
        "intensity" => match parts.next() {
            Some(v) => match v.parse::<u8>() {
                Ok(n) => match cfg.set_intensity(n) {
                    Ok(()) => {
                        if let Some(h) = handle.as_ref() {
                            if let Err(e) = h.set_intensity(n) {
                                println!("error: {e}");
                                return true;
                            }
                        }
                        println!("intensity set to {n}%");
                    }
                    Err(e) => println!("error: {e}"),
                },
                Err(_) => println!("error: intensity must be an integer 10..=100"),
            },
            None => println!("usage: intensity <10-100>  (current {}%)", cfg.intensity),
        },
        "pause" | "p" => match handle.as_ref() {
            Some(h) => println!("{}", if h.toggle_pause() { "PAUSED" } else { "MINING" }),
            None => println!("not mining"),
        },
        "payout" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                println!("usage: payout <bitcoincash:...>");
            } else {
                match cfg.set_payout(rest.join(" ")) {
                    Ok(()) => {
                        if let Err(error) = sync_search_job(cfg, handle.as_ref(), live.as_ref()) {
                            println!("error: {error}");
                        }
                        println!("payout set to {}", cfg.payout_address);
                    }
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "servers" => {
            println!("Fulcrum/Electrum WSS try-order (sequential, ban-safe backoff):");
            let fe = cfg.electrum_endpoints();
            if fe.is_empty() {
                println!("  (empty)");
            }
            for (i, u) in fe.iter().enumerate() {
                let tag = if cfg.custom_fulcrum_endpoints().contains(&u.as_str()) {
                    " custom"
                } else {
                    " bootstrap"
                };
                println!("  {}. {}{}", i + 1, redact_url(u), tag);
            }
            println!("Native node JSON-RPC try-order (sequential, ban-safe backoff):");
            let ne = cfg.node_endpoints();
            if ne.is_empty() {
                println!("  (none - set node http://127.0.0.1:8332 for Start9/bitcoincashd)");
            }
            for (i, u) in ne.iter().enumerate() {
                let tag = if cfg.custom_node_endpoints().contains(&u.as_str()) {
                    " custom"
                } else {
                    " bootstrap"
                };
                // Never print embedded basic-auth passwords: redact userinfo.
                let display = redact_url(u);
                println!("  {}. {}{}", i + 1, display, tag);
            }
        }
        "fulcrum" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                match &cfg.fulcrum_url {
                    Some(u) => println!("fulcrum (custom): {}", redact_url(u)),
                    None => println!("fulcrum: (not set - using bootstrap). usage: fulcrum <wss://...> | fulcrum clear"),
                }
            } else if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                cfg.clear_fulcrum_url();
                println!("fulcrum custom URL cleared - bootstrap only");
            } else {
                match cfg.set_fulcrum_url(&rest.join(" ")) {
                    Ok(()) => println!(
                        "fulcrum set to {}",
                        redact_url(cfg.fulcrum_url.as_deref().unwrap_or(""))
                    ),
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "node" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                match &cfg.node_url {
                    Some(u) => println!("node (custom): {}", redact_url(u)),
                    None => println!("node: (not set). usage: node <http://...> | node clear"),
                }
            } else if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                cfg.clear_node_url();
                println!("node custom URL cleared");
            } else {
                match cfg.set_node_url(&rest.join(" ")) {
                    Ok(()) => println!(
                        "node set to {}",
                        redact_url(cfg.node_url.as_deref().unwrap_or(""))
                    ),
                    Err(e) => println!("error: {e}"),
                }
            }
        }

        "source" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                println!(
                    "source: {} (node=templates/submit first-class; fulcrum=auxiliary)",
                    cfg.source.as_str()
                );
            } else if let Err(e) = cfg.set_source(rest[0]) {
                println!("error: {e}");
            } else {
                println!("source set to {}", cfg.source.as_str());
            }
        }
        "template" => {
            let nodes = cfg.node_endpoints();
            if nodes.is_empty() {
                println!("set node first: node http://user:pass@127.0.0.1:8332  (or --node-rpc)");
            } else {
                match node::fetch_block_template(&nodes) {
                    Ok(t) => {
                        t.print_summary();
                        if let Some(v) = t.version {
                            println!("  ver:    {v}");
                        }
                        if let Some(obj) = t.raw.as_object() {
                            println!(
                                "  keys:   {}",
                                obj.keys().take(12).cloned().collect::<Vec<_>>().join(", ")
                            );
                        }
                        *last_template = Some(t);
                    }
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "submitblock" => {
            let args: Vec<&str> = parts.collect();
            if args.is_empty() {
                println!("usage: submitblock <hex> [job_id]");
            } else {
                let hex = args[0];
                let jid = args.get(1).copied();
                let nodes = cfg.node_endpoints();
                if nodes.is_empty() {
                    println!("set node first");
                } else {
                    match node::submit_block(&nodes, hex, jid) {
                        Ok((url, v)) => println!("submit ok via {url}: {v}"),
                        Err(e) => println!("submit error: {e}"),
                    }
                }
            }
        }

        "nodeprobe" => match node::connect_failover(&cfg.node_endpoints()) {
            Ok((url, v)) => {
                println!("node connected: {}", redact_url(&url));
                println!("rpc result: {v}");
            }
            Err(e) => println!("error: {e}"),
        },
        "connect" => match ElectrumSession::connect_failover_for_deployment(
            &cfg.electrum_endpoints(),
            cfg.token.photon_deployment(cfg.network),
        ) {
            Ok(s) => {
                println!("connected: {}", s.url);
                println!("server.version: {}", s.server_version);
                // Drop session; next job reconnects (simple CLI).
                drop(s);
            }
            Err(e) => println!("error: {e}"),
        },
        "job" => match ElectrumSession::connect_failover_for_deployment(
            &cfg.electrum_endpoints(),
            cfg.token.photon_deployment(cfg.network),
        ) {
            Ok(mut s) => match s.fetch_live_job() {
                Ok(j) => {
                    j.print_summary();
                    publish_live_job(cfg, live, j);
                    if let Err(error) = sync_search_job(cfg, handle.as_ref(), live.as_ref()) {
                        println!("error: {error}");
                    }
                }
                Err(e) => println!("error: {e}"),
            },
            Err(e) => println!("error: {e}"),
        },

        "arm" => {
            if cfg.payout_address.is_empty() {
                println!("set payout first: payout bitcoincash:...");
            } else {
                match ElectrumSession::connect_failover_for_deployment(
                    &cfg.electrum_endpoints(),
                    cfg.token.photon_deployment(cfg.network),
                ) {
                    Err(e) => println!("error: {e}"),
                    Ok(mut s) => match s.fetch_live_job() {
                        Err(e) => println!("error: {e}"),
                        Ok(j) => {
                            j.print_summary();
                            let nonce = 0u32;
                            match tx::photon_message_sha256(nonce, &j.target_le_hex) {
                                Ok(h) => {
                                    println!("message_sha256(nonce={nonce}): {}", hex::encode(h))
                                }
                                Err(e) => println!("error: {e}"),
                            }
                            let job_ctx = reference_job_context(&j);
                            match tx::build_unsigned_reference_preview_for_deployment(
                                &job_ctx,
                                &cfg.payout_address,
                                cfg.token.photon_deployment(cfg.network),
                            ) {
                                Ok(bytes) => {
                                    let hx = hex::encode(&bytes);
                                    let _ = tx::print_win_tx_preview(
                                        j.reward_raw,
                                        &cfg.payout_address,
                                        Some(&hx),
                                    );
                                    println!("arm: unsigned PHOTON parent ready; then applysig");
                                }
                                Err(e) => println!("error: {e}"),
                            }
                            publish_live_job(cfg, live, j);
                        }
                    },
                }
            }
        }
        "applysig" => {
            let args: Vec<&str> = parts.collect();
            if args.len() != 3 {
                println!("usage: applysig <nonce> <pubkey33hex> <sig64hex>");
            } else if cfg.payout_address.is_empty() {
                println!("set payout first");
            } else {
                let nonce: u32 = match args[0].parse() {
                    Ok(n) => n,
                    Err(_) => {
                        println!("bad nonce");
                        return true;
                    }
                };
                match live.as_ref() {
                    None => println!("run job or arm first to cache LiveJob"),
                    Some(j) => {
                        let job_ctx = reference_job_context(j);
                        match tx::apply_reference_signature_for_deployment(
                            &job_ctx,
                            &cfg.payout_address,
                            args[1],
                            nonce,
                            args[2],
                            cfg.token.photon_deployment(cfg.network),
                        ) {
                            Ok(bytes) => {
                                let hx = hex::encode(&bytes);
                                match ArmedTx::new(hx.clone(), cfg, j) {
                                    Ok(candidate) => *armed = Some(candidate),
                                    Err(error) => {
                                        println!("error: {error}");
                                        return true;
                                    }
                                }
                                println!(
                                    "armed win-tx {} bytes (cached; no broadcast yet):",
                                    bytes.len()
                                );
                                println!("{hx}");
                                match tx::photon_message_sha256(nonce, &j.target_le_hex) {
                                    Ok(h) => println!("message_sha256: {}", hex::encode(h)),
                                    Err(e) => println!("hash err: {e}"),
                                }
                            }
                            Err(e) => println!("error: {e}"),
                        }
                    }
                }
            }
        }

        "preview" => {
            if cfg.payout_address.is_empty() {
                println!("error: set payout first (payout bitcoincash:...)");
            } else {
                match ElectrumSession::connect_failover_for_deployment(
                    &cfg.electrum_endpoints(),
                    cfg.token.photon_deployment(cfg.network),
                ) {
                    Ok(mut s) => match s.fetch_live_job() {
                        Ok(j) => {
                            j.print_summary();
                            let job_ctx = reference_job_context(&j);
                            match tx::build_unsigned_reference_preview_for_deployment(
                                &job_ctx,
                                &cfg.payout_address,
                                cfg.token.photon_deployment(cfg.network),
                            ) {
                                Ok(bytes) => {
                                    let hx = hex::encode(&bytes);
                                    if let Err(e) = tx::print_win_tx_preview(
                                        j.reward_raw,
                                        &cfg.payout_address,
                                        Some(&hx),
                                    ) {
                                        println!("error: {e}");
                                    }
                                    println!("note: signature is zero placeholder; real win requires Schnorr signing");
                                }
                                Err(e) => println!("error building unsigned template: {e}"),
                            }
                            publish_live_job(cfg, live, j);
                        }
                        Err(e) => println!("error: {e}"),
                    },
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "start" => println!(
            "legacy REPL live search is disabled; use `pickaxe mine` so every verified winner enters the settlement/submission state machine"
        ),

        "stop" => {
            if let Some(h) = handle.take() {
                let s = h.stop();
                cfg.mining = false;
                println!(
                    "stopped. candidates={} elapsed={}s rate={}",
                    s.candidates,
                    s.elapsed_secs,
                    crate::telemetry::format_hash_rate(s.rate)
                );
            } else {
                println!("not mining");
            }
        }
        other => println!("unknown command `{other}` - try `help`"),
    }
    true
}

/// Combines CLI options with a loaded runtime configuration.
fn runtime_config_from_cli_with_base(
    args: &cli::Cli,
    mut cfg: RuntimeConfig,
) -> Result<RuntimeConfig, String> {
    if args.chipnet {
        cfg.set_network(config::MiningNetwork::Chipnet);
    } else if let Some(network) = &args.network {
        cfg.set_network(config::MiningNetwork::parse(network)?);
    }
    if let Some(token) = &args.token {
        cfg.set_token(token)?;
    }
    if let Some(intensity) = args.intensity {
        cfg.set_intensity(intensity)?;
    }
    if let Some(donation) = args.token_donation {
        cfg.token_donation = Some(donation);
    }
    if let Some(address) = &args.address {
        cfg.set_payout(address.clone())?;
    }
    if let Some(url) = &args.fulcrum {
        cfg.set_fulcrum_url(url)?;
    }
    if let Some(url) = &args.node_rpc {
        cfg.set_node_url(url)?;
    }
    if let Some(source) = &args.source {
        cfg.set_source(source)?;
    }
    Ok(cfg)
}

#[cfg(test)]
/// Builds the effective runtime configuration from CLI options.
fn runtime_config_from_cli(args: &cli::Cli) -> Result<RuntimeConfig, String> {
    runtime_config_from_cli_with_base(args, RuntimeConfig::default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MineStartup {
    InteractiveSetup,
    Direct,
}

/// Starts mining after the configured runtime checks.
fn mine_startup(args: &cli::Cli) -> MineStartup {
    if !(args.no_tui || args.json) && args.address.is_none() {
        MineStartup::InteractiveSetup
    } else {
        MineStartup::Direct
    }
}

/// Formats and prints a runtime event in headless mode.
fn print_runtime_event(event: runtime::RuntimeEvent, json: bool) {
    if json {
        let value = match event {
            runtime::RuntimeEvent::JobRefreshed {
                generation_id,
                height,
                baton_txid,
                baton_vout,
            } => serde_json::json!({
                "event": "job_refreshed",
                "generation_id": generation_id,
                "height": height,
                "baton_txid": baton_txid,
                "baton_vout": baton_vout,
                "changed": true,
            }),
            runtime::RuntimeEvent::StateRefreshFailed { error, consecutive } => {
                serde_json::json!({
                    "event": "state_refresh_failed",
                    "error": error,
                    "consecutive": consecutive,
                    "reconnecting": false,
                })
            }
            runtime::RuntimeEvent::Reconnecting(error) => {
                serde_json::json!({"event": "reconnecting", "error": error})
            }
            runtime::RuntimeEvent::Reconnected(endpoint) => {
                serde_json::json!({"event": "reconnected", "endpoint": endpoint})
            }
            runtime::RuntimeEvent::EndpointRotated { from, to } => {
                serde_json::json!({
                    "event": "endpoint_rotated",
                    "from": redact_url(&from),
                    "to": redact_url(&to),
                })
            }
            runtime::RuntimeEvent::StaleWinner {
                winner_generation,
                current_generation,
            } => serde_json::json!({
                "event": "stale_winner",
                "winner_generation": winner_generation,
                "current_generation": current_generation,
            }),
            runtime::RuntimeEvent::VerifiedWinner(winner) => serde_json::json!({
                "event": "verified_winner",
                "generation_id": winner.generation_id,
                "height": winner.height,
                "baton_txid": winner.baton_txid,
                "baton_vout": winner.baton_vout,
                "nonce": winner.nonce,
                "hash256": hex::encode(winner.digest),
            }),
            runtime::RuntimeEvent::SubmissionAccepted {
                parent_txid,
                child_txid,
            } => serde_json::json!({
                "event": "submission_accepted",
                "parent_txid": parent_txid,
                "child_txid": child_txid,
            }),
            runtime::RuntimeEvent::DirectRewardAccepted { txid, recipient } => serde_json::json!({
                "event": "direct_reward_accepted", "txid": txid, "recipient": recipient,
            }),
            runtime::RuntimeEvent::Error(error) => {
                serde_json::json!({"event": "error", "error": error})
            }
        };
        println!("{value}");
        return;
    }

    match event {
        runtime::RuntimeEvent::JobRefreshed {
            generation_id,
            height,
            baton_txid,
            baton_vout,
        } => {
            println!(
                "live PHOTON work updated: generation={generation_id} height={height} baton={baton_txid}:{baton_vout}"
            );
        }
        runtime::RuntimeEvent::StateRefreshFailed { error, consecutive } => {
            eprintln!(
                "PHOTON state check failed; retaining current generation (consecutive={consecutive}): {error}"
            );
        }
        runtime::RuntimeEvent::Reconnecting(error) => {
            eprintln!("Fulcrum reconnect: {error}");
        }
        runtime::RuntimeEvent::Reconnected(endpoint) => {
            eprintln!("PHOTON state source reconnected: {}", redact_url(&endpoint));
        }
        runtime::RuntimeEvent::EndpointRotated { from, to } => {
            eprintln!(
                "PHOTON state source changed: {} -> {}",
                redact_url(&from),
                redact_url(&to)
            );
        }
        runtime::RuntimeEvent::StaleWinner {
            winner_generation,
            current_generation,
        } => println!(
            "discarded stale GPU winner: generation {winner_generation} != current {current_generation}"
        ),
        runtime::RuntimeEvent::VerifiedWinner(winner) => println!(
            "verified fresh GPU winner; mining paused: generation={} height={} nonce={} hash={}",
            winner.generation_id,
            winner.height,
            winner.nonce,
            hex::encode(winner.digest)
        ),
        runtime::RuntimeEvent::SubmissionAccepted {
            parent_txid,
            child_txid,
        } => println!(
            "winner submission accepted: parent={parent_txid} reward={child_txid}"
        ),
        runtime::RuntimeEvent::DirectRewardAccepted { txid, .. } => {
            println!("direct reward accepted: tx={txid}")
        }
        runtime::RuntimeEvent::Error(error) => eprintln!("runtime error: {error}"),
    }
}

/// Serializes the current runtime snapshot as JSON.
fn runtime_snapshot_json(snapshot: &runtime::RuntimeSnapshot) -> serde_json::Value {
    let efficiency = snapshot
        .gpu_telemetry
        .candidates_per_watt(snapshot.search.current_rate);
    let gpus: Vec<serde_json::Value> = snapshot
        .gpus
        .iter()
        .zip(&snapshot.search.gpus)
        .map(|(gpu, search)| {
            serde_json::json!({
                "backend": gpu.backend.as_str(),
                "device": gpu.device,
                "name": gpu.name,
                "status": gpu_status_name(search.status),
                "candidates": search.candidates,
                "rate": search.rate,
                "active_rate": search.active_rate,
                "winners": search.winners,
                "last_error": search.last_error,
                "gpu_telemetry": &gpu.telemetry,
            })
        })
        .collect();
    // Built apart from the status below, which is already near the json!
    // macro's recursion limit.
    let rigs = snapshot.rigs.as_ref().map(|rigs| {
        let each: Vec<serde_json::Value> = rigs
            .rigs
            .iter()
            .map(|rig| {
                serde_json::json!({
                    "name": rig.name,
                    "gpus": rig.gpus,
                    "rate": rig.rate,
                    "winners": rig.winners,
                    "connected_secs": rig.connected_secs,
                })
            })
            .collect();
        serde_json::json!({
            "listen": rigs.listen,
            "coordinator_key": rigs.key,
            "connected": rigs.connected,
            "gpus": rigs.gpus,
            "rate": rigs.rate,
            "winners": rigs.winners,
            "rejected": rigs.rejected,
            "rigs": each,
        })
    });
    serde_json::json!({
        "event": "status",
        "state": format!("{:?}", snapshot.state).to_ascii_lowercase(),
        "waiting_for_job": snapshot.search.waiting_for_job,
        "backend": snapshot.gpu_backend,
        "device": snapshot.gpu_device,
        "generation_id": snapshot.generation_id,
        "endpoint": redact_url(&snapshot.endpoint),
        "height": snapshot.height,
        "baton_txid": snapshot.baton_txid,
        "baton_vout": snapshot.baton_vout,
        "photon_target_le": (!snapshot.photon_target_le.is_empty())
            .then_some(snapshot.photon_target_le.as_str()),
        "payout_address": snapshot.payout_address,
        "intensity": snapshot.search.intensity,
        "candidates": snapshot.search.candidates,
        "fee_policy": snapshot.fee_scheme.description(),
        "work_candidates": { "miner": snapshot.search.work_candidates[0], "fee": snapshot.search.work_candidates[1].saturating_add(snapshot.search.work_candidates[2]) },
        "batches": snapshot.search.batches,
        "rate": snapshot.search.rate,
        "current_rate": snapshot.search.current_rate,
        "average_rate": snapshot.search.rate,
        "peak_rate": snapshot.search.peak_rate,
        "state_checks": snapshot.state_checks,
        "transient_refresh_failures": snapshot.transient_refresh_failures,
        "transport_failures": snapshot.transport_failures,
        "consecutive_refresh_failures": snapshot.consecutive_refresh_failures,
        "source_degraded": snapshot.source_degraded,
        "job_changes": snapshot.job_changes,
        "reconnects": snapshot.reconnects,
        "endpoint_rotations": snapshot.endpoint_rotations,
        "stale_winners": snapshot.stale_winners,
        "verified_winners": snapshot.verified_winners,
        "pending_winners": snapshot.pending_winners,
        "rejected_winners": snapshot.search.rejected_winners,
        "last_error": snapshot.last_error,
        "gpu_telemetry": &snapshot.gpu_telemetry,
        "gpu_efficiency_candidates_per_watt": efficiency,
        "gpus": gpus,
        "rigs": rigs,
    })
}

/// Lower-case name of one GPU's mining status.
fn gpu_status_name(status: search::GpuStatus) -> &'static str {
    match status {
        search::GpuStatus::Mining => "mining",
        search::GpuStatus::Recovering => "recovering",
        search::GpuStatus::Stopped => "stopped",
    }
}

/// Each GPU's engine, recent rate and status, for a status line of a rig.
fn runtime_gpus_text(snapshot: &runtime::RuntimeSnapshot) -> String {
    snapshot
        .gpus
        .iter()
        .zip(&snapshot.search.gpus)
        .map(|(gpu, search)| {
            format!(
                "{}:{} {} {}",
                gpu.backend.as_str(),
                gpu.device,
                crate::telemetry::format_hash_rate(search.active_rate),
                gpu_status_name(search.status)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Formats a runtime metric when telemetry is present.
fn runtime_metric(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.1}{unit}"))
        .unwrap_or_else(|| "N/A".into())
}

/// Prints the current runtime status in headless mode.
fn print_runtime_snapshot(snapshot: &runtime::RuntimeSnapshot, json: bool) {
    if json {
        println!("{}", runtime_snapshot_json(snapshot));
    } else {
        let telemetry = &snapshot.gpu_telemetry;
        let efficiency = telemetry.candidates_per_watt(snapshot.search.current_rate);
        println!(
            "state={:?} backend={} device={} generation={} height={} baton={}:{} target={} intensity={} candidates={} batches={} current={} avg={} peak={} state_checks={} refresh_failures={} transport_failures={} consecutive_refresh_failures={} source_degraded={} job_changes={} reconnects={} rotations={} winners={} rejected={} pending={} gpu_util={} power={} temp={} vram={} efficiency={}",
            snapshot.state,
            snapshot.gpu_backend,
            snapshot.gpu_device,
            snapshot.generation_id,
            snapshot.height,
            snapshot.baton_txid,
            snapshot.baton_vout,
            crate::telemetry::format_photon_target(&snapshot.photon_target_le),
            snapshot.search.intensity,
            snapshot.search.candidates,
            snapshot.search.batches,
            crate::telemetry::format_hash_rate(snapshot.search.current_rate),
            crate::telemetry::format_hash_rate(snapshot.search.rate),
            crate::telemetry::format_hash_rate(snapshot.search.peak_rate),
            snapshot.state_checks,
            snapshot.transient_refresh_failures,
            snapshot.transport_failures,
            snapshot.consecutive_refresh_failures,
            snapshot.source_degraded,
            snapshot.job_changes,
            snapshot.reconnects,
            snapshot.endpoint_rotations,
            snapshot.verified_winners,
            snapshot.search.rejected_winners,
            snapshot.pending_winners,
            runtime_metric(telemetry.gpu_utilization_percent, "%"),
            runtime_metric(telemetry.power_watts, "W"),
            runtime_metric(telemetry.temperature_c, "C"),
            runtime_metric(telemetry.vram_used_mib, "MiB"),
            runtime_metric(efficiency, " cand/s/W"),
        );
        if snapshot.gpus.len() > 1 {
            println!("gpus={}", runtime_gpus_text(snapshot));
        }
    }
}

/// Reads interactive intensity commands for the headless miner.
fn spawn_intensity_commands() -> std::sync::mpsc::Receiver<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Some(raw) = line.trim().strip_prefix("intensity") else {
                continue;
            };
            let Ok(value) = raw.trim().parse::<u8>() else {
                continue;
            };
            if !(10..=100).contains(&value) {
                continue;
            }
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    rx
}

/// Runs the mining supervisor without the terminal UI.
fn run_headless_mining(
    cfg: RuntimeConfig,
    gpus: &[backend::GpuDevice],
    json: bool,
    use_tui: bool,
    rigs: Option<rigs::RigHub>,
    status_file: Option<std::path::PathBuf>,
) -> Result<Option<SessionSettings>, String> {
    cfg.ensure_mining_supported()?;
    // #### PR #40
    // What: a coordinator with no GPU of its own (`--rigs-only`, or Run a
    // pool, GPU pool) does not take the GPU lock.
    // Why: the lock keeps two miners off the same GPUs; such a coordinator uses
    // none, and taking it stopped a farm's coordinator from running on a PC
    // that also mines.
    // Look here if: two processes mine the same GPU (only a process with GPUs
    // takes the lock).
    let _gpu_lock = if gpus.is_empty() && rigs.is_some() {
        None
    } else {
        Some(mining_lock::acquire_gpu_lock()?)
    };
    // Cache device information before the live miner starts so `/devices` never
    // probes drivers or creates temporary GPU contexts in the mining hot path.
    // A coordinator with no GPU (`--rigs-only`) never loads a GPU driver.
    let tui_devices = if use_tui && !gpus.is_empty() {
        backend::list_devices(backend::BackendKind::Auto).unwrap_or_default()
    } else {
        Vec::new()
    };
    let supervisor = runtime::RuntimeSupervisor::start_on_gpus_with_rigs(cfg, gpus, rigs)?;
    if use_tui {
        let final_snapshot = tui::run(supervisor, tui_devices)?;
        print_runtime_snapshot(&final_snapshot, false);
        return Ok(Some(SessionSettings {
            intensity: final_snapshot.search.intensity,
            // #### PR #32: a donation raised in Advanced settings is saved;
            // the token's minimum is not, so a later minimum applies.
            token_donation: (final_snapshot.token_donation != final_snapshot.donation_minimum)
                .then_some(final_snapshot.token_donation),
            address: final_snapshot.payout_address,
        }));
    }
    let intensity_rx = spawn_intensity_commands();
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Relaxed))
        .map_err(|error| format!("install Ctrl+C handler: {error}"))?;

    let mut last_status = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        while let Ok(value) = intensity_rx.try_recv() {
            match supervisor.set_intensity(value) {
                Ok(()) => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "event": "intensity",
                            "intensity": value,
                        })
                    );
                    let _ = io::stdout().flush();
                }
                Err(error) => eprintln!("intensity change rejected: {error}"),
            }
        }
        for event in supervisor.drain_events() {
            print_runtime_event(event, json);
        }
        let snapshot = supervisor.snapshot();
        if last_status.elapsed() >= Duration::from_secs(1) {
            print_runtime_snapshot(&snapshot, json);
            let _ = std::io::stdout().flush();
            // #### PR #40: saved for `pickaxe watch`, without the payout.
            if let Some(path) = &status_file {
                mine_watch::save(path, &runtime_snapshot_json(&snapshot));
            }
            last_status = Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    }

    let final_snapshot = supervisor.stop();
    print_runtime_snapshot(&final_snapshot, json);
    let _ = std::io::stdout().flush();
    Ok(None)
}

/// What a mining session saves back to its profile when it stops.
struct SessionSettings {
    intensity: u8,
    address: String,
    token_donation: Option<pickaxe_miner::donation::TokenDonation>,
}

fn persist_session_profile(
    path: &std::path::Path,
    name: &str,
    session: &SessionSettings,
) -> Result<(), String> {
    let mut profiles = config::MiningProfiles::load_optional(path)?;
    let profile = profiles
        .profiles
        .iter_mut()
        .find(|profile| profile.name.eq_ignore_ascii_case(name))
        .ok_or("mining profile was renamed or removed during this session")?;
    profile.settings.intensity = Some(session.intensity);
    profile.settings.address = Some(session.address.clone());
    profile.settings.token_donation_bps = session.token_donation;
    profiles.save(path)
}

/// Dispatches the requested CLI command and mining mode.
// #### PR #22: starting the miner by double-click
// What: with no command the miner opens the mining setup (as `mine` does)
// instead of the hidden command prompt, now `pickaxe repl`. A console opened
// just for the miner stays open after an error until Enter, and on Linux a
// launch without a terminal reopens the miner inside the desktop's terminal.
// Why: users double-click the executable; the prompt looked like nothing
// happened, Windows closed the window on errors, and Linux file managers run
// terminal programs invisibly.
// Check: double-click on Windows, Linux (GNOME/KDE) and macOS; `pickaxe` from
// a shell must not pause on errors.
#[cfg(not(windows))]
const TERMINAL_RELAUNCH_ENV: &str = "PICKAXE_TERMINAL_LAUNCHED";

/// #### PR #32 / #40
/// Mines as a rig of the given coordinators (from `--coordinator`, or the
/// setup's Join a GPU pool or farm) until stopped. A public GPU pool claims
/// the rig's wins to its payout.
fn run_as_rig(
    coordinators: &[(String, String)],
    name: Option<&str>,
    payout: &str,
    gpus: &[backend::GpuDevice],
    intensity: u8,
    json: bool,
) {
    let result = (|| {
        let _gpu_lock = mining_lock::acquire_gpu_lock()?;
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))
            .map_err(|error| format!("install Ctrl+C handler: {error}"))?;
        rigs::run_rig(
            coordinators,
            name,
            Some(payout).filter(|payout| !payout.trim().is_empty()),
            gpus,
            intensity,
            json,
            stop,
        )
    })();
    if let Err(error) = result {
        eprintln!("error: {error}");
        exit_after_error(1);
    }
}

/// Exits after an error, keeping a console opened just for the miner open.
fn exit_after_error(code: i32) -> ! {
    if console_closes_on_exit() {
        eprintln!();
        eprintln!("Press Enter to close.");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }
    std::process::exit(code)
}

/// A Windows console whose only process is the miner was opened by Explorer.
#[cfg(windows)]
fn console_closes_on_exit() -> bool {
    let mut processes = [0u32; 2];
    let attached = unsafe {
        windows_sys::Win32::System::Console::GetConsoleProcessList(
            processes.as_mut_ptr(),
            processes.len() as u32,
        )
    };
    attached == 1
}

/// The Linux launcher marks the terminal it opened for the miner.
#[cfg(not(windows))]
fn console_closes_on_exit() -> bool {
    std::env::var_os(TERMINAL_RELAUNCH_ENV).is_some()
}

/// Started from a Linux file manager without a terminal: reopen in one.
#[cfg(target_os = "linux")]
fn relaunch_in_terminal() {
    use std::io::IsTerminal;
    let desktop =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if std::io::stdin().is_terminal()
        || std::io::stdout().is_terminal()
        || std::env::var_os(TERMINAL_RELAUNCH_ENV).is_some()
        || !desktop
    {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // Each terminal with the arguments that precede the command it runs.
    const TERMINALS: [(&str, &[&str]); 9] = [
        ("x-terminal-emulator", &["-e"]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("xfce4-terminal", &["-x"]),
        ("mate-terminal", &["-x"]),
        ("kitty", &[]),
        ("alacritty", &["-e"]),
        ("foot", &[]),
        ("xterm", &["-e"]),
    ];
    for (terminal, prefix) in TERMINALS {
        let launched = std::process::Command::new(terminal)
            .args(prefix)
            .arg(&executable)
            .args(&args)
            .env(TERMINAL_RELAUNCH_ENV, "1")
            .spawn();
        if launched.is_ok() {
            std::process::exit(0);
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    relaunch_in_terminal();
    let args = cli::parse();
    let config_path = match config::config_path(args.config.as_deref()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: {error}");
            exit_after_error(2);
        }
    };
    let saved_config = match config::SavedConfig::load_optional(&config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            exit_after_error(2);
        }
    };
    let mut effective_backend = "auto".to_string();
    let mut effective_device = backend::DeviceSelection::Default;
    let mut include_integrated = args.include_integrated;
    if let Some(config) = &saved_config {
        if let Some(value) = &config.backend {
            effective_backend = value.clone();
        }
        if let Some(saved) = &config.device {
            effective_device = saved.selection();
        }
        include_integrated |= config.include_integrated == Some(true);
    }
    if let Some(value) = &args.backend {
        effective_backend = value.clone();
    }
    if let Some(value) = &args.device {
        effective_device = value.clone();
    }
    let effective_device = effective_device.with_integrated(include_integrated);
    let backend_kind = match backend::BackendKind::parse(&effective_backend) {
        Ok(kind) => kind,
        Err(error) => {
            eprintln!("error: {error}");
            exit_after_error(2);
        }
    };
    let mut base_cfg = RuntimeConfig::default();
    if let Some(config) = &saved_config {
        if let Err(error) = config.apply_to_runtime(&mut base_cfg) {
            eprintln!("error: {error}");
            exit_after_error(2);
        }
    }
    let cfg = match runtime_config_from_cli_with_base(&args, base_cfg) {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("error: {error}");
            exit_after_error(2);
        }
    };

    match args.command.clone().unwrap_or(cli::Commands::Mine) {
        // #### PR #40: the read-only view of a miner without a screen.
        cli::Commands::Watch => {
            if let Err(error) = mine_watch::run(&mine_watch::status_path(&config_path)) {
                eprintln!("error: {error}");
                exit_after_error(1);
            }
        }
        cli::Commands::Devices => {
            if let Err(error) = backend::print_devices(backend_kind) {
                eprintln!("error: {error}");
                exit_after_error(1);
            }
        }
        cli::Commands::SelfTest => {
            let selected = match effective_device
                .single_index()
                .and_then(|index| backend::resolve_mining_device(backend_kind, index))
            {
                Ok(device) => device,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
            };
            match self_test::run_self_test(selected.backend, selected.index) {
                Ok(report) => self_test::print_report(&report, args.json),
                Err(error) => {
                    eprintln!("error: self-test failed: {error}");
                    exit_after_error(1);
                }
            }
        }
        cli::Commands::Benchmark {
            seconds,
            ui_compare,
        } => {
            let selected = match effective_device
                .single_index()
                .and_then(|index| backend::resolve_mining_device(backend_kind, index))
            {
                Ok(device) => device,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
            };
            match benchmark::run_gpu_benchmark(&selected, seconds, args.intensity, ui_compare) {
                Ok(report) => {
                    let passed = report.passed();
                    benchmark::print_report(&report, args.json);
                    if !passed {
                        exit_after_error(1);
                    }
                }
                Err(error) => {
                    eprintln!("error: benchmark failed: {error}");
                    exit_after_error(1);
                }
            }
        }
        cli::Commands::Config { command } => match command {
            cli::ConfigCommand::Show => {
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "backend": effective_backend,
                            "network": cfg.network.as_str(),
                            "token": cfg.token.as_str(),
                            "device": config::SavedDevices::from_selection(&effective_device),
                            "include_integrated":
                                effective_device == backend::DeviceSelection::WithIntegrated,
                            "intensity": cfg.intensity,
                            "address": cfg.payout_address,
                            "fulcrum": cfg.fulcrum_url,
                            "node_rpc": cfg.node_url.as_deref().map(redact_url),
                            "source": cfg.source.as_str(),
                            "no_tui": args.no_tui,
                            "generation_id": cfg.generation_id,
                        })
                    );
                } else {
                    println!("backend: {}", effective_backend);
                    println!("network: {}", cfg.network.as_str());
                    println!("token: {}", cfg.token.as_str());
                    println!("device: {effective_device}");
                    println!("intensity: {}%", cfg.intensity);
                    println!("address: {}", cfg.payout_address);
                    println!("source: {}", cfg.source.as_str());
                    println!("no_tui: {}", args.no_tui);
                }
            }
            cli::ConfigCommand::Validate => {
                if args.json {
                    println!("{}", serde_json::json!({"valid": true}));
                } else {
                    println!("configuration syntax and CashAddr validation: OK");
                }
            }
            cli::ConfigCommand::Save => {
                let saved = config::SavedConfig::from_effective(
                    &effective_backend,
                    &effective_device,
                    &cfg,
                );
                if let Err(error) = saved.save(&config_path) {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "saved": true,
                            "path": config_path,
                        })
                    );
                } else {
                    println!("saved configuration: {}", config_path.display());
                }
            }
        },
        cli::Commands::StratumV2 { command } => match command {
            cli::StratumV2Command::Status => {
                print!("{}", stratum_v2::status_report());
            }
            action => {
                #[cfg(feature = "stratum-v2")]
                let result = stratum_v2::command::run(
                    action,
                    &cfg,
                    &config_path,
                    args.no_tui,
                    args.json,
                    None,
                );
                #[cfg(not(feature = "stratum-v2"))]
                let result: Result<(), String> = {
                    let _ = action;
                    Err(
                        "this build does not include Stratum V2; build with --features stratum-v2"
                            .into(),
                    )
                };
                if let Err(error) = result {
                    eprintln!("error: {error}");
                    exit_after_error(1);
                }
            }
        },
        cli::Commands::Mine => {
            let profiles_path = config::profiles_path(&config_path);
            let sources_path = config::sources_path(&config_path);
            let mut sources = match config::SharedSources::load_optional(&sources_path) {
                Ok(sources) => sources,
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
            };
            if saved_config
                .as_ref()
                .is_some_and(|saved| sources.adopt_saved_config(saved))
            {
                if let Err(error) = sources.save(&sources_path) {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
            }
            let startup = mine_startup(&args);
            let mut cfg = cfg;
            if matches!(startup, MineStartup::Direct) {
                // Saved per-network connections apply unless the command line
                // or the base configuration named its own.
                let fulcrum = sources.list(cfg.network, config::ConnectionKind::Fulcrum);
                let node = sources.list(cfg.network, config::ConnectionKind::Node);
                let applied = (if cfg.fulcrum_url.is_none() {
                    cfg.set_fulcrum_url(&fulcrum.join(","))
                } else {
                    Ok(())
                })
                .and_then(|()| {
                    if cfg.node_url.is_none() {
                        cfg.set_node_url(&node.join(","))
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = applied {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
            }
            // #### PR #32: a rig claims nothing, so it needs no address.
            if matches!(startup, MineStartup::Direct)
                && (args.no_tui || args.json)
                && args.coordinator.is_empty()
                && cfg.payout_address.trim().is_empty()
            {
                eprintln!("error: --address is required with --no-tui or --json");
                exit_after_error(2);
            }
            if matches!(startup, MineStartup::Direct) {
                if let Err(error) = cfg.ensure_mining_supported() {
                    eprintln!("error: {error}");
                    exit_after_error(2);
                }
            }
            // #### PR #32: `--rigs-only` uses no GPU on this computer.
            let selected_gpus = if args.rigs_only {
                Vec::new()
            } else {
                match backend::resolve_mining_devices(backend_kind, &effective_device) {
                    Ok(gpus) => gpus,
                    Err(error) => {
                        eprintln!("error: {error}");
                        exit_after_error(2);
                    }
                }
            };
            // #### PR #32
            // A rig mines its coordinator's jobs: no setup, payout, Fulcrum or
            // node of its own, and it never claims; the coordinator does.
            if !args.coordinator.is_empty() {
                if args.coordinator.len() != args.coordinator_key.len() {
                    eprintln!(
                        "error: give one --coordinator-key for each --coordinator, in the same order"
                    );
                    exit_after_error(2);
                }
                let coordinators: Vec<(String, String)> = args
                    .coordinator
                    .iter()
                    .cloned()
                    .zip(args.coordinator_key.iter().cloned())
                    .collect();
                run_as_rig(
                    &coordinators,
                    args.rig_name.as_deref(),
                    &cfg.payout_address,
                    &selected_gpus,
                    cfg.intensity,
                    args.json,
                );
                return;
            }
            // #### PR #32 / #40: the rig hub's settings, from the flags or
            // from the setup's Run a pool → GPU pool.
            let mut rigs_listen = args.rigs_listen;
            let mut rigs_public = args.rigs_public;
            let mut rigs_fee = args.rigs_fee;
            let mut rigs_fee_address = args.rigs_fee_address.clone();
            let (cfg, gpus, profile_name) = match startup {
                MineStartup::InteractiveSetup => {
                    // Setup lists each physical GPU once, numbered as
                    // `--device` numbers them; a named backend lists its own.
                    let devices = if backend_kind == backend::BackendKind::Auto {
                        Ok(backend::mining_gpus())
                    } else {
                        backend::list_devices(backend_kind)
                    };
                    let devices = match devices {
                        Ok(devices) => devices,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_after_error(2);
                        }
                    };
                    let mut profiles = match config::MiningProfiles::load_optional(&profiles_path) {
                        Ok(profiles) => profiles,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_after_error(2);
                        }
                    };
                    // Servers and nodes once saved inside profiles move to the
                    // shared per-network lists; save those before the profiles.
                    if sources.adopt_profile_sources(&mut profiles) {
                        if let Err(error) = sources
                            .save(&sources_path)
                            .and_then(|()| profiles.save(&profiles_path))
                        {
                            eprintln!("error: {error}");
                            exit_after_error(2);
                        }
                    }
                    let overrides = tui::SetupOverrides {
                        network: if args.chipnet {
                            Some(config::MiningNetwork::Chipnet)
                        } else {
                            args.network.as_deref().map(|value| {
                                config::MiningNetwork::parse(value).expect("validated CLI network")
                            })
                        },
                        token: args.token.clone(),
                        intensity: args.intensity,
                        fulcrum: args.fulcrum.clone(),
                        node_rpc: args.node_rpc.clone(),
                        source: args.source.clone(),
                        gpus: (args.backend.is_some()
                            || args.device.is_some()
                            || args.include_integrated)
                            .then(|| {
                                selected_gpus
                                    .iter()
                                    .map(|gpu| (gpu.backend, gpu.index))
                                    .collect()
                            }),
                    };
                    let setup = match tui::run_setup(
                        cfg,
                        devices,
                        backend_kind,
                        &selected_gpus,
                        &profiles_path,
                        profiles,
                        &sources_path,
                        sources,
                        overrides,
                    ) {
                        Ok(Some(setup)) => setup,
                        Ok(None) => return,
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_after_error(1);
                        }
                    };
                    // #### PR #40
                    // Run a pool → GPU pool: this computer coordinates the
                    // pool's rigs on every interface (port 3340) with no GPU
                    // of its own, as `--rigs-listen 0.0.0.0:3340 --rigs-only
                    // --rigs-public` would.
                    // #### PR #40: Join a GPU pool or farm: these GPUs mine as a
                    // rig of its coordinator, as `--coordinator` does.
                    if let Some(tui::ServerSetup::JoinGpuPool { address, key }) =
                        setup.server.clone()
                    {
                        println!(
                            "Mining as a rig of {address} on {} GPU(s); Ctrl+C stops.",
                            setup.gpus.len()
                        );
                        run_as_rig(
                            &[(address, key)],
                            None,
                            &setup.config.payout_address,
                            &setup.gpus,
                            setup.config.intensity,
                            false,
                        );
                        return;
                    }
                    if let Some(tui::ServerSetup::GpuPool { fee, address }) = setup.server.clone() {
                        rigs_listen = Some(std::net::SocketAddr::from(([0, 0, 0, 0], 3340)));
                        rigs_public = true;
                        rigs_fee = Some(fee);
                        rigs_fee_address = address;
                        (setup.config, Vec::new(), Some(setup.profile_name))
                    } else {
                        // #### PR #40
                        // ASIC mode from setup runs the BCH ASIC server for devices
                        // on the local network (SV1 on 3333, SV2 on 3336): solo
                        // on the miner's node, at someone's pool, or as a public
                        // pool for other miners.
                        if let Some(server) = setup.server.clone() {
                            // #### PR #42: the command the setup's rows
                            // make, with the Advanced section's values
                            // (see `tui::asic_serve_command`).
                            let Some(action) = tui::asic_serve_command(&server, &setup.options)
                            else {
                                // Started above, as a GPU coordinator or rig.
                                unreachable!()
                            };
                            #[cfg(feature = "stratum-v2")]
                            let result = stratum_v2::command::run(
                                action,
                                &setup.config,
                                &config_path,
                                false,
                                false,
                                Some(&setup.profile_name),
                            );
                            #[cfg(not(feature = "stratum-v2"))]
                            let result: Result<(), String> = {
                                let _ = action;
                                Err("this build does not include Stratum V2; build with --features stratum-v2"
                                .into())
                            };
                            if let Err(error) = result {
                                eprintln!("error: {error}");
                                exit_after_error(1);
                            }
                            return;
                        }
                        (setup.config, setup.gpus, Some(setup.profile_name))
                    }
                }
                MineStartup::Direct => (cfg, selected_gpus, None),
            };
            let use_tui = !(args.no_tui || args.json);
            // #### PR #32
            // --rigs-listen makes this miner the coordinator its rigs follow.
            let rig_hub = match rigs_listen {
                None => None,
                Some(listen) => {
                    match rigs::RigHub::start(listen, &config_path.with_extension("rigs-key")) {
                        Ok(hub) => {
                            // #### PR #32: a public GPU pool.
                            if rigs_public {
                                let address = match config::validate_payout_address(
                                    cfg.network,
                                    rigs_fee_address
                                        .as_deref()
                                        .unwrap_or(cfg.payout_address.as_str()),
                                ) {
                                    Ok(address) => address,
                                    Err(error) => {
                                        eprintln!("error: pool fee address: {error}");
                                        exit_after_error(2);
                                    }
                                };
                                hub.set_public(Some(rigs::PublicRigs {
                                    fee_bps: rigs_fee.map_or(0, u16::from),
                                    address,
                                }));
                            }
                            // #### PR #40: where rigs reach this coordinator.
                            let interfaces = reach::Interfaces::detect();
                            let connect: Vec<_> = hub
                                .listen()
                                .parse()
                                .map(|listen| reach::addresses(listen, interfaces))
                                .unwrap_or_default()
                                .into_iter()
                                .map(|(place, address)| {
                                    serde_json::json!({
                                        "place": place.label(),
                                        "address": address.to_string(),
                                    })
                                })
                                .collect();
                            println!(
                                "{}",
                                serde_json::json!({
                                    "event": "rigs",
                                    "listen": hub.listen(),
                                    "coordinator_key": hub.key(),
                                    "connect": connect,
                                })
                            );
                            if !args.json && !use_tui {
                                for (place, command) in
                                    rigs::join_lines(&hub.summary(), cfg.network, interfaces)
                                {
                                    println!("rigs join ({}): {command}", place.label());
                                }
                            }
                            Some(hub)
                        }
                        Err(error) => {
                            eprintln!("error: {error}");
                            exit_after_error(2);
                        }
                    }
                }
            };
            let status_file = (!use_tui).then(|| mine_watch::status_path(&config_path));
            match run_headless_mining(cfg, &gpus, args.json, use_tui, rig_hub, status_file) {
                Ok(Some(session)) => {
                    if let Some(name) = profile_name {
                        if let Err(error) = persist_session_profile(&profiles_path, &name, &session)
                        {
                            eprintln!("error: save mining profile: {error}");
                            exit_after_error(1);
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!("error: {error}");
                    exit_after_error(1);
                }
            }
        }
        cli::Commands::Repl => run_repl(cfg),
    }
}

/// Runs the interactive command loop until exit.
fn run_repl(mut cfg: RuntimeConfig) {
    let mut handle: Option<SearchHandle> = None;
    let mut live: Option<LiveJob> = None;
    let mut armed: Option<ArmedTx> = None;
    let mut last_template: Option<node::BlockTemplate> = None;
    print_banner();

    let stdin = io::stdin();
    loop {
        process_gpu_winners(&mut cfg, &handle, &mut live, &mut armed);
        print!("pickaxe> ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {
                if !handle_line(
                    &mut cfg,
                    &mut handle,
                    &mut live,
                    &mut armed,
                    &mut last_template,
                    &line,
                ) {
                    break;
                }
            }
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn normal_session_exit_saves_the_latest_profile_intensity() {
        let path = std::env::temp_dir().join(format!(
            "pickaxe-session-profile-{}.json",
            std::process::id()
        ));
        let mut runtime = RuntimeConfig::default();
        runtime.set_payout(config::DONATION_ADDRESS.into()).unwrap();
        runtime.set_intensity(70).unwrap();
        let mut profiles = config::MiningProfiles::default();
        profiles
            .upsert(
                None,
                "Rig A",
                config::SavedConfig::from_effective(
                    "cuda",
                    &backend::DeviceSelection::Indices(vec![0]),
                    &runtime,
                ),
            )
            .unwrap();
        profiles.save(&path).unwrap();

        let mut session = SessionSettings {
            intensity: 40,
            address: config::DONATION_ADDRESS.into(),
            token_donation: Some(pickaxe_miner::donation::TokenDonation::from_bps(550)),
        };
        persist_session_profile(&path, "Rig A", &session).unwrap();
        let saved = config::MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(saved.profiles[0].settings.intensity, Some(40));
        // #### PR #32: a raised donation is saved and applies at the next start.
        assert_eq!(
            saved.profiles[0].settings.token_donation_bps,
            session.token_donation
        );
        let mut next = RuntimeConfig::default();
        saved.profiles[0]
            .settings
            .apply_to_runtime(&mut next)
            .unwrap();
        assert_eq!(next.token_donation().bps(), 550);
        assert_eq!(next.fee_policy().scheme.work(), [350, 200]);
        // Back at the minimum, nothing is saved.
        session.token_donation = None;
        persist_session_profile(&path, "Rig A", &session).unwrap();
        let saved = config::MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(saved.profiles[0].settings.token_donation_bps, None);
        let _ = std::fs::remove_file(path);
    }

    fn live_job() -> LiveJob {
        LiveJob {
            url: "wss://fulcrum.invalid".into(),
            server_version: serde_json::json!(["Fulcrum", "1.5"]),
            height: 1_000,
            tip_hash: "22".repeat(32),
            baton_txid: "11".repeat(32),
            baton_vout: 0,
            baton_height: 999,
            baton_value_sats: 15_971_500,
            relay_fee_sats_per_kb: 1_000,
            commitment_hex: "00".repeat(101),
            token_amount: 2_099_905_002_035_715,
            age: 1,
            target_le_hex: "ff".repeat(32),
            reward_raw: 4_999_773_813,
        }
    }

    #[test]
    fn bad_applysig_nonce_keeps_the_repl_running() {
        let mut cfg = config::RuntimeConfig::default();
        cfg.set_payout(config::DONATION_ADDRESS.into()).unwrap();
        let mut handle = None;
        let mut live = Some(live_job());
        let mut armed = None;
        let mut last_template = None;
        let keep_running = handle_line(
            &mut cfg,
            &mut handle,
            &mut live,
            &mut armed,
            &mut last_template,
            "applysig 12x 00 00",
        );
        assert!(keep_running);
        assert!(live.is_some());
        assert!(armed.is_none());
        assert!(!handle_line(
            &mut cfg,
            &mut handle,
            &mut live,
            &mut armed,
            &mut last_template,
            "quit",
        ));
    }

    #[test]
    fn release_workflow_prepares_pickaxe_miner_v0_0_3() {
        let workflow = include_str!("../.github/workflows/release.yml");
        let version = cargo_package_version(include_str!("../Cargo.toml"));
        assert_eq!(version, "0.0.3");

        let pattern = release_tag_pattern(workflow);
        assert!(
            release_tag_matches(&pattern, "pickaxe-miner-v0.0.3"),
            "{pattern}"
        );
        assert!(!release_tag_matches(&pattern, "v0.0.3"), "{pattern}");
        assert!(release_tag_matches(&pattern, "pickaxe-miner-v0.0.3-rc.1"));
        assert!(workflow.contains("pickaxe-miner-v*.*.*"));
        assert!(!workflow.lines().any(|line| line.trim() == "- \"v*.*.*\""));
        assert!(workflow.contains("if [[ \"pickaxe-miner-v${version}\" != \"${TAG}\" ]]; then"));
        let tag = format!("pickaxe-miner-v{version}");
        assert_eq!(tag, "pickaxe-miner-v0.0.3");
        assert_ne!(tag, format!("v{version}"));

        assert!(workflow.contains("release_title=\"Pickaxe Miner v${version}\""));
        assert!(workflow.contains("--title \"${release_title}\""));
        let title = format!("Pickaxe Miner v{version}");
        assert_eq!(title, "Pickaxe Miner v0.0.3");

        // #### PR #32: Linux and Windows packages are named by the matrix's
        // arch, x86_64 and arm64.
        assert!(workflow.contains("pickaxe-miner-v${version}-linux-${ARCH}"));
        assert!(workflow.contains("pickaxe-miner-v$version-windows-$env:ARCH"));
        assert!(workflow.contains("arch: x86_64"));
        assert!(workflow.contains("arch: arm64"));
        assert!(workflow.contains("\"${TAG}-linux-x86_64.tar.gz\""));
        assert!(workflow.contains("\"${TAG}-windows-x86_64.zip\""));
        assert!(!workflow.contains("pickaxe-${TAG}"));
        assert!(!workflow.contains("pickaxe-$env:TAG"));
        assert!(!workflow.contains("pickaxe-pickaxe-miner"));

        let linux = format!("pickaxe-miner-v{version}-linux-x86_64");
        let windows = format!("pickaxe-miner-v{version}-windows-x86_64");
        assert!(linux.starts_with(&tag));
        assert!(windows.starts_with(&tag));
        assert_eq!(
            linux.trim_end_matches("-linux-x86_64"),
            windows.trim_end_matches("-windows-x86_64")
        );

        // #### PR #22: macOS ARM64 comes from the shared portable package.
        // #### PR #32: Linux and Windows ARM64 packages join it, built on
        // native ARM64 runners.
        assert!(workflow.contains("\"${TAG}-macos-arm64.tar.gz\""));
        assert!(workflow.contains("\"${TAG}-browser.tar.gz\""));
        assert!(workflow.contains("name: Verify portable package provenance"));
        assert!(workflow.contains("\"${TAG}-linux-arm64.tar.gz\""));
        assert!(workflow.contains("\"${TAG}-windows-arm64.zip\""));
        assert!(workflow.contains("GH_REPO: ${{ github.repository }}"));
        assert!(workflow.contains("--repo \"${GH_REPO}\""));
    }

    #[test]
    fn startup_enters_setup_by_default() {
        let args = cli::Cli::try_parse_from(["pickaxe", "mine"]).unwrap();
        assert_eq!(mine_startup(&args), MineStartup::InteractiveSetup);
    }

    #[test]
    fn startup_flag_keeps_direct_mode() {
        let args = cli::Cli::try_parse_from(["pickaxe", "mine", "--no-tui"]).unwrap();
        assert_eq!(mine_startup(&args), MineStartup::Direct);
    }

    #[test]
    fn headless_status_distinguishes_state_checks_from_job_changes() {
        let snapshot = runtime::RuntimeSnapshot {
            state: runtime::SupervisorState::Mining,
            gpu_backend: "cuda".into(),
            gpu_device: 0,
            gpus: vec![
                runtime::RuntimeGpu {
                    backend: backend::BackendKind::Cuda,
                    device: 0,
                    name: "NVIDIA GeForce RTX 5070 Ti Laptop GPU".into(),
                    telemetry: telemetry::GpuTelemetry::default(),
                },
                runtime::RuntimeGpu {
                    backend: backend::BackendKind::Wgpu,
                    device: 1,
                    name: "AMD Radeon(TM) 610M".into(),
                    telemetry: telemetry::GpuTelemetry::default(),
                },
            ],
            generation_id: 2,
            network: config::MiningNetwork::Mainnet,
            fee_scheme: config::MiningToken::Photon
                .fee_policy(config::MiningNetwork::Mainnet)
                .scheme,
            payout_address: crate::config::DONATION_ADDRESS.into(),
            endpoint: "wss://fulcrum.invalid".into(),
            height: 1_000,
            baton_txid: "11".repeat(32),
            baton_vout: 0,
            photon_target_le: "ff".repeat(32),
            state_checks: 7,
            transient_refresh_failures: 2,
            transport_failures: 1,
            consecutive_refresh_failures: 1,
            source_degraded: true,
            job_changes: 1,
            reconnects: 0,
            endpoint_rotations: 1,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: search::SearchStats {
                candidates: 65_536,
                work_candidates: [65_536, 0, 0],
                batches: 1,
                intensity: 30,
                state: search::MiningState::Mining,
                elapsed_secs: 1,
                rate: 65_536.0,
                current_rate: 65_536.0,
                active_rate: 65_536.0,
                peak_rate: 65_536.0,
                winners: 0,
                rejected_winners: 2,
                waiting_for_job: false,
                key_rotations: 0,
                last_error: Some(
                    "GPU winner rejected by host verification: HASH256 mismatch".into(),
                ),
                gpus: vec![
                    search::GpuSearchStats {
                        backend: backend::BackendKind::Cuda,
                        device: 0,
                        candidates: 60_000,
                        rate: 60_000.0,
                        active_rate: 2_400_000_000.0,
                        winners: 0,
                        status: search::GpuStatus::Mining,
                        last_error: None,
                    },
                    search::GpuSearchStats {
                        backend: backend::BackendKind::Wgpu,
                        device: 1,
                        candidates: 5_536,
                        rate: 5_536.0,
                        active_rate: 30_000_000.0,
                        winners: 0,
                        status: search::GpuStatus::Recovering,
                        last_error: Some("device lost".into()),
                    },
                ],
            },
            gpu_telemetry: telemetry::GpuTelemetry {
                samples: 3,
                gpu_utilization_percent: Some(77.0),
                power_watts: Some(65.536),
                temperature_c: Some(71.0),
                vram_used_mib: Some(512.0),
                graphics_clock_mhz: Some(2_400.0),
                memory_clock_mhz: Some(8_000.0),
                fan_percent: None,
            },
            rigs: None,
            token_donation: pickaxe_miner::donation::TokenDonation::from_bps(400),
            donation_minimum: pickaxe_miner::donation::TokenDonation::from_bps(400),
        };

        let status = runtime_snapshot_json(&snapshot);
        assert_eq!(status["state_checks"], 7);
        assert_eq!(status["transient_refresh_failures"], 2);
        assert_eq!(status["transport_failures"], 1);
        assert_eq!(status["consecutive_refresh_failures"], 1);
        assert_eq!(status["source_degraded"], true);
        assert_eq!(status["job_changes"], 1);
        assert_eq!(status["reconnects"], 0);
        assert_eq!(status["endpoint_rotations"], 1);
        assert_eq!(status["rejected_winners"], 2);
        assert_eq!(status["fee_policy"], "Donation: 4%");
        assert_eq!(status["work_candidates"]["miner"], 65_536);
        assert_eq!(status["work_candidates"]["fee"], 0);
        assert!(status["work_candidates"].get("project").is_none());
        assert!(status["work_candidates"].get("collaborator").is_none());
        assert_eq!(status["photon_target_le"], "ff".repeat(32));
        assert!(status.get("refreshes").is_none());
        assert!(status.get("stale_rebuilds").is_none());
        assert_eq!(status["gpu_telemetry"]["samples"], 3);
        assert_eq!(status["gpu_telemetry"]["gpu_utilization_percent"], 77.0);
        assert_eq!(status["gpu_efficiency_candidates_per_watt"], 1_000.0);
        assert_eq!(status["gpus"][1]["backend"], "wgpu");
        assert_eq!(status["gpus"][1]["name"], "AMD Radeon(TM) 610M");
        assert_eq!(status["gpus"][1]["status"], "recovering");
        assert_eq!(status["gpus"][1]["last_error"], "device lost");
        assert_eq!(status["gpus"][0]["active_rate"], 2_400_000_000.0);
        assert_eq!(
            runtime_gpus_text(&snapshot),
            "cuda:0 2.40 GH/s mining; wgpu:1 30.00 MH/s recovering"
        );
    }

    #[test]
    fn explicit_startup_values_feed_runtime_config() {
        let args = cli::Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--backend",
            "cuda",
            "--device",
            "0",
            "--intensity",
            "60",
            "--address",
            crate::config::DONATION_ADDRESS,
        ])
        .unwrap();
        let cfg = runtime_config_from_cli(&args).unwrap();
        assert_eq!(mine_startup(&args), MineStartup::Direct);
        assert_eq!(
            args.device,
            Some(backend::DeviceSelection::Indices(vec![0]))
        );
        assert_eq!(cfg.intensity, 60);
        assert_eq!(cfg.payout_address, crate::config::DONATION_ADDRESS);
    }

    #[test]
    fn cli_rejects_foreign_network_payouts() {
        for (network, other) in [
            (
                config::MiningNetwork::Mainnet,
                config::MiningNetwork::Chipnet,
            ),
            (
                config::MiningNetwork::Chipnet,
                config::MiningNetwork::Mainnet,
            ),
        ] {
            let address = tx::p2pkh_hash_to_cashaddr_for_network(&[0x42; 20], other).unwrap();
            let args = cli::Cli::try_parse_from([
                "pickaxe",
                "mine",
                "--network",
                network.as_str(),
                "--address",
                &address,
            ])
            .unwrap();
            assert!(runtime_config_from_cli(&args).is_err());
        }
    }

    #[test]
    fn explicit_cli_values_override_saved_runtime_defaults() {
        let args = cli::Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--intensity",
            "60",
            "--address",
            crate::config::DONATION_ADDRESS,
        ])
        .unwrap();
        let mut saved = RuntimeConfig::default();
        saved.set_intensity(40).unwrap();
        let cfg = runtime_config_from_cli_with_base(&args, saved).unwrap();
        assert_eq!(cfg.intensity, 60);
        assert_eq!(cfg.payout_address, crate::config::DONATION_ADDRESS);
    }

    #[test]
    fn split_2_percent_floor() {
        let (m, d) = RuntimeConfig::split_reward(100);
        assert_eq!(d, 2);
        assert_eq!(m, 98);
    }

    #[test]
    fn split_zero() {
        let (m, d) = RuntimeConfig::split_reward(0);
        assert_eq!((m, d), (0, 0));
    }

    #[test]
    fn intensity_bounds() {
        let mut c = RuntimeConfig::default();
        assert_eq!(c.intensity, 100);
        assert!(c.set_intensity(10).is_ok());
        assert!(c.set_intensity(9).is_err());
        assert!(c.set_intensity(100).is_ok());
        assert!(c.set_intensity(101).is_err());
    }

    #[test]
    fn omitted_cli_intensity_keeps_mining_default_at_100() {
        let args = cli::Cli::try_parse_from(["pickaxe", "mine"]).unwrap();
        assert_eq!(args.intensity, None);
        let cfg = runtime_config_from_cli(&args).unwrap();
        assert_eq!(cfg.intensity, 100);
    }

    #[test]
    fn live_job_publication_changes_generation_only_when_job_changes() {
        let mut cfg = RuntimeConfig::default();
        let mut live = None;
        let job = live_job();

        publish_live_job(&mut cfg, &mut live, job.clone());
        assert_eq!(cfg.generation_id, 1);
        publish_live_job(&mut cfg, &mut live, job.clone());
        assert_eq!(cfg.generation_id, 1);

        let mut changed = job;
        changed.target_le_hex = "fe".repeat(32);
        publish_live_job(&mut cfg, &mut live, changed);
        assert_eq!(cfg.generation_id, 2);
    }

    #[test]
    fn armed_transaction_rejects_generation_or_baton_drift() {
        let mut cfg = RuntimeConfig::default();
        let mut live = None;
        let job = live_job();
        publish_live_job(&mut cfg, &mut live, job.clone());

        let armed = ArmedTx::new("00".into(), &cfg, &job).unwrap();
        assert!(armed.validate_current(&cfg, live.as_ref()).is_ok());

        cfg.bump_generation();
        assert!(armed.validate_current(&cfg, live.as_ref()).is_err());

        let mut same_generation = cfg.clone();
        same_generation.generation_id = armed.generation_id;
        let mut different_baton = job;
        different_baton.baton_txid = "22".repeat(32);
        assert!(armed
            .validate_current(&same_generation, Some(&different_baton))
            .is_err());
    }

    #[test]
    fn verified_gpu_winner_requires_same_generation_height_and_baton() {
        let mut cfg = RuntimeConfig::default();
        let mut live = None;
        let job = live_job();
        publish_live_job(&mut cfg, &mut live, job.clone());
        let winner = search::VerifiedWinner {
            generation_id: cfg.generation_id,
            height: job.height,
            baton_txid: job.baton_txid.clone(),
            baton_vout: job.baton_vout,
            job_reward_raw: job.reward_raw,
            nonce: 7,
            digest: [0u8; 32],
            public_key: [0u8; 33],
            signature: [0u8; 64],
            transaction: Vec::new(),
            payout: None,
        };
        assert!(validate_verified_winner_current(&winner, &cfg, live.as_ref()).is_ok());

        let mut stale_reward = winner.clone();
        stale_reward.job_reward_raw += 1;
        assert!(validate_verified_winner_current(&stale_reward, &cfg, live.as_ref()).is_err());

        let mut stale_generation = winner.clone();
        stale_generation.generation_id += 1;
        assert!(validate_verified_winner_current(&stale_generation, &cfg, live.as_ref()).is_err());

        let mut stale_height = job.clone();
        stale_height.height += 1;
        assert!(validate_verified_winner_current(&winner, &cfg, Some(&stale_height)).is_err());

        let mut stale_baton = job;
        stale_baton.baton_txid = "22".repeat(32);
        assert!(validate_verified_winner_current(&winner, &cfg, Some(&stale_baton)).is_err());
    }

    fn cargo_package_version(cargo_toml: &str) -> String {
        cargo_toml
            .lines()
            .find_map(|line| {
                let rest = line.strip_prefix("version = \"")?;
                rest.strip_suffix('"').map(str::to_string)
            })
            .expect("Cargo.toml version")
    }

    fn release_tag_pattern(workflow: &str) -> String {
        let marker = "=~ ";
        let start = workflow.find(marker).expect("release workflow tag matcher") + marker.len();
        let rest = &workflow[start..];
        let end = rest.find(" ]];").expect("release workflow tag matcher end");
        rest[..end].trim().to_string()
    }

    fn release_tag_matches(pattern: &str, tag: &str) -> bool {
        assert_eq!(
            pattern,
            "^pickaxe-miner-v[0-9]+\\.[0-9]+\\.[0-9]+([.-][0-9A-Za-z.-]+)?$"
        );
        let Some(rest) = tag.strip_prefix("pickaxe-miner-v") else {
            return false;
        };
        let mut index = 0;
        let bytes = rest.as_bytes();
        for part in 0..3 {
            if index >= bytes.len() || !bytes[index].is_ascii_digit() {
                return false;
            }
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if part < 2 {
                if index >= bytes.len() || bytes[index] != b'.' {
                    return false;
                }
                index += 1;
            }
        }
        if index == bytes.len() {
            return true;
        }
        let suffix = &rest[index..];
        let mut chars = suffix.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if first != '.' && first != '-' {
            return false;
        }
        let tail = chars.as_str();
        !tail.is_empty()
            && tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    }
}
