//! Pickaxe Miner - interactive CLI (Stage 2/3).
//!
//! Controls mirror the postcorps WebGPU site (esp. intensity).
//! Donation intent is 2%, but same-transaction splitting stays disabled until covenant-valid.
//! Search/CPU/crypto: Lead Dev. Electrum/win-tx: Dev Assist.

mod backend;
mod cli;
mod config;
mod crypto;
#[cfg(test)]
#[allow(dead_code, clippy::needless_range_loop)]
mod cuda_miner;
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
#[allow(dead_code)]
mod node;
#[allow(dead_code)]
mod protocol;
#[allow(dead_code)]
mod search;
#[cfg(test)]
mod stage_b;
mod tx;

use config::{RuntimeConfig, DONATION_ADDRESS, DONATION_BPS, MINER_BPS};
use electrum::{ElectrumSession, LiveJob};
use search::SearchHandle;
use std::io::{self, Write};

fn print_banner() {
    println!("Pickaxe Miner 0.1.0 - interactive CLI");
    println!(
        "Donation: {DONATION_BPS} bps ({:.2}%) ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ {DONATION_ADDRESS}",
        DONATION_BPS as f64 / 100.0
    );
    println!("Donation split is disabled until a covenant-valid construction is proven.");
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
  donation                     Show donation address and split
  connect                      Electrum/Fulcrum connect (custom then bootstrap)
  fulcrum <wss://ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦>            Set custom Fulcrum/Electrum WSS URL
  fulcrum clear                Clear custom Fulcrum URL
  node <http://ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦>              Set custom native node JSON-RPC URL
  node clear                   Clear custom node URL
  servers                      Show Fulcrum + node try-order (ban-safe)
  nodeprobe                    Probe native node RPC (getblockchaininfo)
  job                          Fetch live PHOTON baton ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ MiningJob
  dryrun                       connect+job + proven 2-output tx preview (no broadcast)
arm                          like dryrun + message SHA256 for Schnorr (no keys)
applysig <nonce> <pk33hex> <sig64hex>  verify+arm proven 2-output winner (no broadcast)
  broadcast                      recheck and submit last verified armed winner
  start                        Start GPU search (uses last job if present)
  stop                         Stop search
  split <reward_raw>           Preview 98%/2% split for a raw reward amount
  quit | exit                  Leave

Donation target is 2%, but same-transaction donation is disabled until covenant
validity is proven. Never skim unrelated wallet funds or keys."#
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
    println!("donation:      {DONATION_BPS} bps ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ {DONATION_ADDRESS}");
    println!("miner share:   {MINER_BPS} bps");
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

fn broadcast_raw_with_fallback(cfg: &RuntimeConfig, raw_hex: &str) -> Result<String, String> {
    let mut failures = Vec::new();
    match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
        Ok(mut session) => match session.broadcast_raw(raw_hex) {
            Ok(txid) => return Ok(format!("fulcrum:{txid}")),
            Err(error) => failures.push(format!("fulcrum broadcast failed: {error}")),
        },
        Err(error) => failures.push(format!("fulcrum connect failed: {error}")),
    }

    let nodes = cfg.node_endpoints();
    if nodes.is_empty() {
        failures.push("no node fallback configured".into());
    } else {
        match node::broadcast_raw(&nodes, raw_hex) {
            Ok((url, txid)) => return Ok(format!("node {}:{txid}", redact_url(&url))),
            Err(error) => failures.push(format!("node broadcast failed: {error}")),
        }
    }
    Err(failures.join(" | "))
}

fn print_donation() {
    println!("Donation target:");
    println!("  intended share: {DONATION_BPS} bps = 2%");
    println!("  address:        {DONATION_ADDRESS}");
    println!("  status:         DISABLED");
    println!("  blocker:        {}", tx::DONATION_SPLIT_BLOCKER);
}

fn selected_armed_hex(args: &[&str], armed: Option<&ArmedTx>) -> Option<String> {
    if args.is_empty() {
        armed.map(|candidate| candidate.raw_hex.clone())
    } else {
        None
    }
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

        "broadcast" => {
            let args: Vec<&str> = parts.collect();
            let hex_opt = selected_armed_hex(&args, armed.as_ref());
            if args.is_empty() && armed.is_some() {
                let cached = armed.as_ref().expect("checked above").clone();
                if let Err(error) = refresh_live_job(cfg, live) {
                    println!(
                        "refusing cached broadcast: live PHOTON baton recheck failed: {error}"
                    );
                    return true;
                }
                if let Err(error) = cached.validate_current(cfg, live.as_ref()) {
                    println!("refusing cached broadcast: {error}");
                    return true;
                }
            }
            match hex_opt {
                None => {
                    println!("nothing to broadcast ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â run applysig first")
                }
                Some(hx) => match broadcast_raw_with_fallback(cfg, &hx) {
                    Ok(result) => {
                        println!("broadcast ok ({result})");
                        if args.is_empty() {
                            *armed = None;
                        }
                    }
                    Err(error) => println!("broadcast failed: {error}"),
                },
            }
        }

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
                println!("usage: payout <bitcoincash:ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦>");
            } else {
                match cfg.set_payout(rest.join(" ")) {
                    Ok(()) => println!("payout set to {}", cfg.payout_address),
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
                println!("  (none ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â set `node http://127.0.0.1:8332` for Start9/bitcoincashd)");
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
                    None => println!("fulcrum: (not set ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â using bootstrap). usage: fulcrum <wss://ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦> | fulcrum clear"),
                }
            } else if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                cfg.clear_fulcrum_url();
                println!("fulcrum custom URL cleared ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â bootstrap only");
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
                    None => println!("node: (not set). usage: node <http://ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦> | node clear"),
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
                                    println!("arm: unsigned 98/2 ready ÃƒÂ¢Ã¢â€šÂ¬Ã¢â‚¬Â Lead Dev signs message_sha256; then applysig");
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

        "dryrun" => {
            if cfg.payout_address.is_empty() {
                println!("error: set payout first (`payout bitcoincash:ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦`)");
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
                                    println!("note: signature is zero placeholder ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â Lead Dev Schnorr fills real win");
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
        "start" => {
            if cfg.payout_address.is_empty() {
                println!("error: set payout first: payout bitcoincash:...");
            } else if handle.is_some() {
                println!("already mining - status for rate");
            } else {
                // Codex sec15: PHOTON jobs = Fulcrum CashToken baton only. Never GBT->baton.
                if live.is_none() {
                    println!("PHOTON job: fetching CashToken baton via Fulcrum (Codex sec15)...");
                    match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
                        Ok(mut s) => match s.fetch_live_job() {
                            Ok(j) => {
                                j.print_summary();
                                publish_live_job(cfg, live, j);
                            }
                            Err(e) => println!("error: baton fetch failed: {e}"),
                        },
                        Err(e) => println!("error: fulcrum connect failed: {e}"),
                    }
                }
                let Some(job) = live
                    .as_ref()
                    .map(|j| j.to_mining_job(cfg.generation_id, &cfg.payout_address))
                else {
                    println!("error: no PHOTON baton job - check Fulcrum, then job / start");
                    return true;
                };
                if job.target_le_hex.is_empty() {
                    println!("error: baton job missing target - refuse easy-target fallback");
                    return true;
                }
                println!(
                    "using PHOTON baton height={} baton={} (broadcast pref={})",
                    job.height,
                    job.baton_txid,
                    cfg.source.as_str()
                );
                match SearchHandle::start(cfg.intensity, job) {
                    Ok(h) => {
                        *handle = Some(h);
                        cfg.mining = true;
                        println!(
                            "GPU search ON - intensity {}%, payout {}",
                            cfg.intensity, cfg.payout_address
                        );
                        println!("mode: CUDA vs live PHOTON target; node=validate/broadcast only");
                    }
                    Err(e) => println!("error starting CUDA search: {e}"),
                }
            }
        }

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
        "split" => match parts.next() {
            Some(v) => match v.parse::<u128>() {
                Ok(raw) => {
                    let (miner, donation) = RuntimeConfig::split_reward(raw);
                    println!("reward_raw={raw}");
                    println!("  miner    ({MINER_BPS} bps): {miner}");
                    println!("  donation ({DONATION_BPS} bps): {donation} ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ {DONATION_ADDRESS}");
                }
                Err(_) => println!("error: split needs a non-negative integer"),
            },
            None => println!("usage: split <reward_raw>"),
        },
        other => println!("unknown command `{other}` - try `help`"),
    }
    true
}

fn runtime_config_from_cli(args: &cli::Cli) -> Result<RuntimeConfig, String> {
    let mut cfg = RuntimeConfig::default();
    cfg.set_intensity(args.intensity)?;
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
        cli::Commands::Benchmark => {
            eprintln!(
                "error: benchmark is gated until the exact PHOTON GPU RFC6979/kG/Schnorr/full-transaction pipeline is production-ready"
            );
            std::process::exit(2);
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
                            "dry_run": args.dry_run,
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
                    println!("dry_run: {}", args.dry_run);
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
            let mode = if args.json {
                "JSON headless"
            } else if args.no_tui {
                "headless"
            } else {
                "TUI"
            };
            let device = args
                .device
                .map_or_else(|| "auto".to_string(), |index| index.to_string());
            eprintln!(
                "error: {mode} mining on backend={} device={device} dry_run={} is gated until the exact PHOTON GPU RFC6979/kG/Schnorr/full-transaction HASH256 pipeline is production-ready",
                args.backend, args.dry_run
            );
            std::process::exit(2);
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

    fn live_job() -> LiveJob {
        LiveJob {
            url: "wss://fulcrum.invalid".into(),
            server_version: serde_json::json!(["Fulcrum", "1.5"]),
            height: 1_000,
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
    fn explicit_payload_is_not_selected() {
        let armed = ArmedTx {
            raw_hex: "abcd".into(),
            generation_id: 1,
            baton_txid: "11".repeat(32),
            baton_vout: 0,
        };
        assert_eq!(
            selected_armed_hex(&[], Some(&armed)).as_deref(),
            Some("abcd")
        );
        assert!(selected_armed_hex(&["00"], Some(&armed)).is_none());
    }
}
