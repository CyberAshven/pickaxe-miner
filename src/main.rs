//! Pickaxe Miner - interactive CLI (Stage 2/3).
//!
//! Runtime controls preserve the authoritative PHOTON reference semantics.
//! Donation: 2%.
//! Search/CPU/crypto: Lead Dev. Electrum/win-tx: Dev Assist.

mod backend;
mod benchmark;
mod cli;
mod config;
mod crypto;
#[cfg(test)]
#[allow(dead_code, clippy::needless_range_loop)]
mod cuda_miner;
#[allow(dead_code)]
mod cuda_photon;
#[cfg(test)]
mod cuda_stage_a;
#[cfg(test)]
mod cuda_stage_a_ref;
#[cfg(test)]
mod cuda_stage_b;
#[allow(dead_code)]
mod cuda_stage_c;
#[allow(dead_code)]
mod electrum;
mod hip_photon;
#[allow(dead_code)]
mod m29_table;
#[allow(dead_code)]
mod node;
#[allow(dead_code)]
mod protocol;
mod reward;
mod runtime;
#[allow(dead_code)]
mod search;
mod self_test;
#[cfg(test)]
mod stage_b;
mod telemetry;
#[allow(dead_code)]
mod tui;
mod tx;

use config::RuntimeConfig;
use electrum::{ElectrumSession, LiveJob};
use search::SearchHandle;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn print_banner() {
    println!("Pickaxe Miner 0.1.0 - interactive CLI");
    println!("Donation: 2%");
    println!("Type `help` for commands.\n");
}

fn print_help() {
    println!(
        r#"Commands:
  help                         Show this help
  status                       Show intensity, payout, mining, donation, job, rate
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

Donation: 2%"#
    );
}

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
        println!("rate:          {:.0} work/s", s.rate);
    }
    println!("Donation: 2%");
    match &cfg.fulcrum_url {
        Some(u) => println!("fulcrum:       {u} (custom, tried first)"),
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

fn redact_url(url: &str) -> String {
    // Strip userinfo so passwords never hit the terminal/logs.
    if let Some(scheme_end) = url.find("://") {
        let scheme = &url[..scheme_end + 3];
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            return format!("{scheme}***@{}", &rest[at + 1..]);
        }
    }
    url.to_string()
}

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

fn reference_job_context(job: &LiveJob) -> tx::ReferenceJobContext {
    tx::ReferenceJobContext {
        prev_txid: job.baton_txid.clone(),
        prev_vout: job.baton_vout,
        age: job.age,
        target_le_hex: job.target_le_hex.clone(),
        contract_value_sats: job.baton_value_sats,
        contract_token_amount: job.token_amount,
        reward_raw: job.reward_raw,
    }
}

fn refresh_live_job(cfg: &mut RuntimeConfig, live: &mut Option<LiveJob>) -> Result<(), String> {
    let mut session = ElectrumSession::connect_failover(&cfg.electrum_endpoints())?;
    let job = session.fetch_live_job()?;
    publish_live_job(cfg, live, job);
    Ok(())
}

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
    Ok(())
}

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

fn print_donation() {
    println!("Donation: 2%");
}

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
        "donation" => print_donation(),

        "broadcast" => println!(
            "legacy REPL submission is unavailable; use pickaxe mine --backend cuda --no-tui"
        ),
        "quit" | "exit" => {
            if let Some(h) = handle.take() {
                let s = h.stop();
                cfg.mining = false;
                println!(
                    "stopped. candidates={} rate={:.0} H/s",
                    s.candidates, s.rate
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
                let tag = if cfg.fulcrum_url.as_ref() == Some(u) {
                    " custom"
                } else {
                    " bootstrap"
                };
                println!("  {}. {}{}", i + 1, u, tag);
            }
            println!("Native node JSON-RPC try-order (sequential, ban-safe backoff):");
            let ne = cfg.node_endpoints();
            if ne.is_empty() {
                println!("  (none - set node http://127.0.0.1:8332 for Start9/bitcoincashd)");
            }
            for (i, u) in ne.iter().enumerate() {
                let tag = if cfg.node_url.as_ref() == Some(u) {
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
                    Some(u) => println!("fulcrum (custom): {u}"),
                    None => println!("fulcrum: (not set - using bootstrap). usage: fulcrum <wss://...> | fulcrum clear"),
                }
            } else if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                cfg.clear_fulcrum_url();
                println!("fulcrum custom URL cleared - bootstrap only");
            } else {
                match cfg.set_fulcrum_url(&rest.join(" ")) {
                    Ok(()) => println!(
                        "fulcrum set to {}",
                        cfg.fulcrum_url.as_deref().unwrap_or("")
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
        "connect" => match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
            Ok(s) => {
                println!("connected: {}", s.url);
                println!("server.version: {}", s.server_version);
                // Drop session; next job reconnects (simple CLI).
                drop(s);
            }
            Err(e) => println!("error: {e}"),
        },
        "job" => match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
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
                match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
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
                            match tx::build_unsigned_reference_preview(
                                &job_ctx,
                                &cfg.payout_address,
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
                        return false;
                    }
                };
                match live.as_ref() {
                    None => println!("run job or arm first to cache LiveJob"),
                    Some(j) => {
                        let job_ctx = reference_job_context(j);
                        match tx::apply_reference_signature(
                            &job_ctx,
                            &cfg.payout_address,
                            args[1],
                            nonce,
                            args[2],
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
                match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
                    Ok(mut s) => match s.fetch_live_job() {
                        Ok(j) => {
                            j.print_summary();
                            let job_ctx = reference_job_context(&j);
                            match tx::build_unsigned_reference_preview(
                                &job_ctx,
                                &cfg.payout_address,
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
                    "stopped. candidates={} elapsed={}s rate={:.0} H/s",
                    s.candidates, s.elapsed_secs, s.rate
                );
            } else {
                println!("not mining");
            }
        }
        other => println!("unknown command `{other}` - try `help`"),
    }
    true
}

fn runtime_config_from_cli(args: &cli::Cli) -> Result<RuntimeConfig, String> {
    let mut cfg = RuntimeConfig::default();
    if let Some(intensity) = args.intensity {
        cfg.set_intensity(intensity)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MineStartup {
    InteractiveSetup,
    Direct,
}

fn mine_startup(args: &cli::Cli, cfg: &RuntimeConfig) -> MineStartup {
    if !(args.no_tui || args.json) && cfg.payout_address.trim().is_empty() {
        MineStartup::InteractiveSetup
    } else {
        MineStartup::Direct
    }
}

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
            runtime::RuntimeEvent::Reconnecting(error) => {
                serde_json::json!({"event": "reconnecting", "error": error})
            }
            runtime::RuntimeEvent::Reconnected(endpoint) => {
                serde_json::json!({"event": "reconnected", "endpoint": endpoint})
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
        runtime::RuntimeEvent::Reconnecting(error) => {
            eprintln!("PHOTON state refresh failed; GPU held at batch boundary: {error}");
        }
        runtime::RuntimeEvent::Reconnected(endpoint) => {
            eprintln!("PHOTON state source reconnected: {}", redact_url(&endpoint));
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
        runtime::RuntimeEvent::Error(error) => eprintln!("runtime error: {error}"),
    }
}

fn runtime_snapshot_json(snapshot: &runtime::RuntimeSnapshot) -> serde_json::Value {
    let efficiency = snapshot
        .gpu_telemetry
        .candidates_per_watt(snapshot.search.current_rate);
    serde_json::json!({
        "event": "status",
        "state": format!("{:?}", snapshot.state).to_ascii_lowercase(),
        "backend": snapshot.gpu_backend,
        "device": snapshot.gpu_device,
        "generation_id": snapshot.generation_id,
        "endpoint": redact_url(&snapshot.endpoint),
        "height": snapshot.height,
        "baton_txid": snapshot.baton_txid,
        "baton_vout": snapshot.baton_vout,
        "payout_address": snapshot.payout_address,
        "intensity": snapshot.search.intensity,
        "candidates": snapshot.search.candidates,
        "batches": snapshot.search.batches,
        "rate": snapshot.search.rate,
        "current_rate": snapshot.search.current_rate,
        "average_rate": snapshot.search.rate,
        "peak_rate": snapshot.search.peak_rate,
        "state_checks": snapshot.state_checks,
        "job_changes": snapshot.job_changes,
        "reconnects": snapshot.reconnects,
        "stale_winners": snapshot.stale_winners,
        "verified_winners": snapshot.verified_winners,
        "pending_winners": snapshot.pending_winners,
        "last_error": snapshot.last_error,
        "gpu_telemetry": &snapshot.gpu_telemetry,
        "gpu_efficiency_candidates_per_watt": efficiency,
    })
}

fn runtime_metric(value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{value:.1}{unit}"))
        .unwrap_or_else(|| "N/A".into())
}

fn print_runtime_snapshot(snapshot: &runtime::RuntimeSnapshot, json: bool) {
    if json {
        println!("{}", runtime_snapshot_json(snapshot));
    } else {
        let telemetry = &snapshot.gpu_telemetry;
        let efficiency = telemetry.candidates_per_watt(snapshot.search.current_rate);
        println!(
            "state={:?} backend={} device={} generation={} height={} baton={}:{} intensity={} candidates={} batches={} current={:.0}/s avg={:.0}/s peak={:.0}/s state_checks={} job_changes={} reconnects={} winners={} pending={} gpu_util={} power={} temp={} vram={} efficiency={}",
            snapshot.state,
            snapshot.gpu_backend,
            snapshot.gpu_device,
            snapshot.generation_id,
            snapshot.height,
            snapshot.baton_txid,
            snapshot.baton_vout,
            snapshot.search.intensity,
            snapshot.search.candidates,
            snapshot.search.batches,
            snapshot.search.current_rate,
            snapshot.search.rate,
            snapshot.search.peak_rate,
            snapshot.state_checks,
            snapshot.job_changes,
            snapshot.reconnects,
            snapshot.verified_winners,
            snapshot.pending_winners,
            runtime_metric(telemetry.gpu_utilization_percent, "%"),
            runtime_metric(telemetry.power_watts, "W"),
            runtime_metric(telemetry.temperature_c, "C"),
            runtime_metric(telemetry.vram_used_mib, "MiB"),
            runtime_metric(efficiency, " cand/s/W"),
        );
    }
}

fn run_headless_mining(
    cfg: RuntimeConfig,
    backend: backend::BackendKind,
    device_ordinal: u32,
    json: bool,
    use_tui: bool,
) -> Result<(), String> {
    // Cache device information before the live miner starts so `/devices` never
    // probes drivers or creates temporary GPU contexts in the mining hot path.
    let tui_devices = if use_tui {
        backend::list_devices(backend::BackendKind::Auto).unwrap_or_default()
    } else {
        Vec::new()
    };
    let supervisor =
        runtime::RuntimeSupervisor::start_on_backend_device(cfg, backend, device_ordinal)?;
    if use_tui {
        let final_snapshot = tui::run(supervisor, tui_devices)?;
        print_runtime_snapshot(&final_snapshot, false);
        return Ok(());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Relaxed))
        .map_err(|error| format!("install Ctrl+C handler: {error}"))?;

    let mut last_status = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        for event in supervisor.drain_events() {
            print_runtime_event(event, json);
        }
        let snapshot = supervisor.snapshot();
        if last_status.elapsed() >= Duration::from_secs(1) {
            print_runtime_snapshot(&snapshot, json);
            last_status = Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    }

    let final_snapshot = supervisor.stop();
    print_runtime_snapshot(&final_snapshot, json);
    Ok(())
}

fn main() {
    let args = cli::parse();
    let backend_kind = match backend::BackendKind::parse(&args.backend) {
        Ok(kind) => kind,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };
    let cfg = match runtime_config_from_cli(&args) {
        Ok(cfg) => cfg,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };

    match args.command.unwrap_or(cli::Commands::Repl) {
        cli::Commands::Devices => {
            if let Err(error) = backend::print_devices(backend_kind) {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
        cli::Commands::SelfTest => {
            let selected = match backend::resolve_mining_device(backend_kind, args.device) {
                Ok(device) => device,
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(2);
                }
            };
            match self_test::run_self_test(selected.backend, selected.index) {
                Ok(report) => self_test::print_report(&report, args.json),
                Err(error) => {
                    eprintln!("error: self-test failed: {error}");
                    std::process::exit(1);
                }
            }
        }
        cli::Commands::Benchmark {
            seconds,
            ui_compare,
        } => {
            let selected = match backend::resolve_mining_device(backend_kind, args.device) {
                Ok(device) => device,
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(2);
                }
            };
            if !matches!(selected.backend, backend::BackendKind::Cuda) {
                eprintln!(
                    "error: benchmark currently requires the validated native CUDA PHOTON backend"
                );
                std::process::exit(2);
            }
            match benchmark::run_cuda_benchmark(
                selected.index,
                selected.name,
                seconds,
                args.intensity,
                ui_compare,
            ) {
                Ok(report) => benchmark::print_report(&report, args.json),
                Err(error) => {
                    eprintln!("error: benchmark failed: {error}");
                    std::process::exit(1);
                }
            }
        }
        cli::Commands::Config { command } => match command {
            cli::ConfigCommand::Show => {
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "backend": args.backend,
                            "device": args.device,
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
                    println!("backend: {}", args.backend);
                    println!("device: {:?}", args.device);
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
        },
        cli::Commands::Mine => {
            let startup = mine_startup(&args, &cfg);
            if matches!(startup, MineStartup::Direct)
                && (args.no_tui || args.json)
                && cfg.payout_address.trim().is_empty()
            {
                eprintln!("error: --address is required with --no-tui or --json");
                std::process::exit(2);
            }
            let selected = match backend::resolve_mining_device(backend_kind, args.device) {
                Ok(device) => device,
                Err(error) => {
                    eprintln!("error: {error}");
                    std::process::exit(2);
                }
            };
            let (cfg, selected_backend, selected_device) = match startup {
                MineStartup::InteractiveSetup => {
                    let devices = match backend::list_devices(backend_kind) {
                        Ok(devices) => devices,
                        Err(error) => {
                            eprintln!("error: {error}");
                            std::process::exit(2);
                        }
                    };
                    let setup = match tui::run_setup(cfg, devices, &selected) {
                        Ok(Some(setup)) => setup,
                        Ok(None) => return,
                        Err(error) => {
                            eprintln!("error: {error}");
                            std::process::exit(1);
                        }
                    };
                    (setup.config, setup.backend, setup.device)
                }
                MineStartup::Direct => (cfg, selected.backend, selected.index),
            };
            let use_tui = !(args.no_tui || args.json);
            if let Err(error) =
                run_headless_mining(cfg, selected_backend, selected_device, args.json, use_tui)
            {
                eprintln!("error: {error}");
                std::process::exit(1);
            }
        }
        cli::Commands::Repl => run_repl(cfg),
    }
}

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
            commitment_hex: "00".repeat(101),
            token_amount: 2_099_905_002_035_715,
            age: 1,
            target_le_hex: "ff".repeat(32),
            reward_raw: 4_999_773_813,
        }
    }

    #[test]
    fn startup_enters_setup_by_default() {
        let args = cli::Cli::try_parse_from(["pickaxe", "mine"]).unwrap();
        let cfg = runtime_config_from_cli(&args).unwrap();
        assert_eq!(mine_startup(&args, &cfg), MineStartup::InteractiveSetup);
    }

    #[test]
    fn startup_flag_keeps_direct_mode() {
        let args = cli::Cli::try_parse_from(["pickaxe", "mine", "--no-tui"]).unwrap();
        let cfg = runtime_config_from_cli(&args).unwrap();
        assert_eq!(mine_startup(&args, &cfg), MineStartup::Direct);
    }

    #[test]
    fn headless_status_distinguishes_state_checks_from_job_changes() {
        let snapshot = runtime::RuntimeSnapshot {
            state: runtime::SupervisorState::Mining,
            gpu_backend: "cuda".into(),
            gpu_device: 0,
            generation_id: 2,
            payout_address: crate::config::DONATION_ADDRESS.into(),
            endpoint: "wss://fulcrum.invalid".into(),
            height: 1_000,
            baton_txid: "11".repeat(32),
            baton_vout: 0,
            state_checks: 7,
            job_changes: 1,
            reconnects: 0,
            stale_winners: 0,
            verified_winners: 0,
            pending_winners: 0,
            last_error: None,
            search: search::SearchStats {
                candidates: 65_536,
                batches: 1,
                intensity: 30,
                state: search::MiningState::Mining,
                elapsed_secs: 1,
                rate: 65_536.0,
                current_rate: 65_536.0,
                peak_rate: 65_536.0,
                winners: 0,
            },
            gpu_telemetry: telemetry::GpuTelemetry {
                samples: 3,
                gpu_utilization_percent: Some(77.0),
                power_watts: Some(65.536),
                temperature_c: Some(71.0),
                vram_used_mib: Some(512.0),
                graphics_clock_mhz: Some(2_400.0),
                memory_clock_mhz: Some(8_000.0),
            },
        };

        let status = runtime_snapshot_json(&snapshot);
        assert_eq!(status["state_checks"], 7);
        assert_eq!(status["job_changes"], 1);
        assert!(status.get("refreshes").is_none());
        assert!(status.get("stale_rebuilds").is_none());
        assert_eq!(status["gpu_telemetry"]["samples"], 3);
        assert_eq!(status["gpu_telemetry"]["gpu_utilization_percent"], 77.0);
        assert_eq!(status["gpu_efficiency_candidates_per_watt"], 1_000.0);
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
        assert_eq!(mine_startup(&args, &cfg), MineStartup::Direct);
        assert_eq!(args.device, Some(0));
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
            nonce: 7,
            digest: [0u8; 32],
            public_key: [0u8; 33],
            signature: [0u8; 64],
            transaction: Vec::new(),
        };
        assert!(validate_verified_winner_current(&winner, &cfg, live.as_ref()).is_ok());

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
}
