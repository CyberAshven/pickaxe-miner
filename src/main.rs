//! Pickaxe Miner - interactive CLI (Stage 2/3).
//!
//! Controls mirror the postcorps WebGPU site (esp. intensity).
//! Donation: 2% coinbase-style split on the win tx only - never skim unrelated funds.
//! Search/CPU/crypto: Lead Dev. Electrum/win-tx: Dev Assist.

mod config;
mod crypto;
mod cuda_stage_a;
mod electrum;
mod node;
mod protocol;
mod search;
mod tx;

use config::{RuntimeConfig, DONATION_ADDRESS, DONATION_BPS, MINER_BPS};
use electrum::{ElectrumSession, LiveJob};
use search::SearchHandle;
use std::io::{self, Write};

fn print_banner() {
    println!("Pickaxe Miner 0.1.0 - interactive CLI");
    println!(
        "Donation: {DONATION_BPS} bps ({:.2}%) â†’ {DONATION_ADDRESS}",
        DONATION_BPS as f64 / 100.0
    );
    println!("Miner keeps {MINER_BPS} bps. Split is on the win tx only (coinbase-style).");
    println!("Type `help` for commands.\n");
}

fn print_help() {
    println!(
        r#"Commands:
  help                         Show this help
  status                       Show intensity, payout, mining, donation, job, rate
  intensity <0-100>            Set work intensity (default 50)
  payout <cashaddr>            Set miner payout address
  donation                     Show donation address and split
  connect                      Electrum/Fulcrum connect (custom then bootstrap)
  fulcrum <wss://â€¦>            Set custom Fulcrum/Electrum WSS URL
  fulcrum clear                Clear custom Fulcrum URL
  node <http://â€¦>              Set custom native node JSON-RPC URL
  node clear                   Clear custom node URL
  servers                      Show Fulcrum + node try-order (ban-safe)
  nodeprobe                    Probe native node RPC (getblockchaininfo)
  job                          Fetch live PHOTON baton â†’ MiningJob
  dryrun                       connect+job + 98/2 win-tx preview (no broadcast)
  start                        Start CPU search (uses last job if present)
  stop                         Stop search
  split <reward_raw>           Preview 98%/2% split for a raw reward amount
  quit | exit                  Leave

Invariant: distribution builds pay 98% miner + 2% donation on the verified win
transaction itself. Visible before arm. Never call it a "dev fee". Never skim
unrelated wallet funds or keys."#
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
            "ON (CPU M1 rate)"
        } else {
            "off"
        }
    );
    if let Some(h) = handle {
        let s = h.snapshot(cfg.intensity);
        println!("candidates:    {}", s.candidates);
        println!("elapsed:       {}s", s.elapsed_secs);
        println!("rate:          {:.0} H/s (HASH256 M1)", s.rate);
    }
    println!("donation:      {DONATION_BPS} bps â†’ {DONATION_ADDRESS}");
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
    println!("gpu:           not wired yet");
}

fn redact_url(url: &str) -> String {
    // Strip userinfo so passwords never hit the terminal/logs.
    if let Some(scheme_end) = url.find("://") {
        let scheme = &url[..scheme_end + 3];
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            return format!("{scheme}***@{}" , &rest[at + 1..]);
        }
    }
    url.to_string()
}

fn print_donation() {
    println!("Donation (not a \"dev fee\"):");
    println!("  share:   {DONATION_BPS} bps = 2%");
    println!("  address: {DONATION_ADDRESS}");
    println!("  model:   coinbase-style - two outputs on the win tx (98% miner / 2% donation)");
    println!("  never:   skim unrelated balances or keys");
}

fn handle_line(
    cfg: &mut RuntimeConfig,
    handle: &mut Option<SearchHandle>,
    live: &mut Option<LiveJob>,
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
                        println!("intensity set to {n}%");
                        if handle.is_some() {
                            println!("note: restart `start` to apply intensity to the search thread");
                        }
                    }
                    Err(e) => println!("error: {e}"),
                },
                Err(_) => println!("error: intensity must be an integer 0..=100"),
            },
            None => println!("usage: intensity <0-100>  (current {}%)", cfg.intensity),
        },
        "payout" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                println!("usage: payout <bitcoincash:â€¦>");
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
                println!("  (none â€” set `node http://127.0.0.1:8332` for Start9/bitcoincashd)");
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
                    None => println!("fulcrum: (not set â€” using bootstrap). usage: fulcrum <wss://â€¦> | fulcrum clear"),
                }
            } else if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                cfg.clear_fulcrum_url();
                println!("fulcrum custom URL cleared â€” bootstrap only");
            } else {
                match cfg.set_fulcrum_url(&rest.join(" ")) {
                    Ok(()) => println!("fulcrum set to {}", cfg.fulcrum_url.as_deref().unwrap_or("")),
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "node" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                match &cfg.node_url {
                    Some(u) => println!("node (custom): {}", redact_url(u)),
                    None => println!("node: (not set). usage: node <http://â€¦> | node clear"),
                }
            } else if rest.len() == 1 && rest[0].eq_ignore_ascii_case("clear") {
                cfg.clear_node_url();
                println!("node custom URL cleared");
            } else {
                match cfg.set_node_url(&rest.join(" ")) {
                    Ok(()) => println!("node set to {}", redact_url(cfg.node_url.as_deref().unwrap_or(""))),
                    Err(e) => println!("error: {e}"),
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
                    *live = Some(j);
                }
                Err(e) => println!("error: {e}"),
            },
            Err(e) => println!("error: {e}"),
        },
        "dryrun" => {
            if cfg.payout_address.is_empty() {
                println!("error: set payout first (`payout bitcoincash:â€¦`)");
            } else {
                match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
                    Ok(mut s) => match s.fetch_live_job() {
                        Ok(j) => {
                            j.print_summary();
                            match tx::build_unsigned_donation_preview(
                                &j.baton_txid,
                                j.baton_vout,
                                j.age,
                                &j.target_le_hex,
                                j.baton_value_sats,
                                j.token_amount,
                                j.reward_raw,
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
                                    println!("note: signature is zero placeholder â€” Lead Dev Schnorr fills real win");
                                }
                                Err(e) => println!("error building unsigned template: {e}"),
                            }
                            *live = Some(j);
                        }
                        Err(e) => println!("error: {e}"),
                    },
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "start" => {
            if cfg.payout_address.is_empty() {
                println!("error: set payout first (`payout bitcoincash:â€¦`)");
            } else if handle.is_some() {
                println!("already mining â€” `status` for rate");
            } else {
                // Prefer last live job; otherwise fetch once so start is one-shot usable.
                if live.is_none() {
                    println!("no cached job â€” fetching via Electrumâ€¦");
                    match ElectrumSession::connect_failover(&cfg.electrum_endpoints()) {
                        Ok(mut s) => match s.fetch_live_job() {
                            Ok(j) => {
                                j.print_summary();
                                *live = Some(j);
                            }
                            Err(e) => println!("error: job fetch failed: {e}"),
                        },
                        Err(e) => println!("error: electrum connect failed: {e}"),
                    }
                }
                let Some(job) = live.as_ref().map(|j| j.to_mining_job()) else {
                    println!("error: no live job â€” fix Electrum then `job` / `start` again");
                    return true;
                };
                if job.target_le_hex.is_empty() {
                    println!("error: live job missing target â€” refuse easy-target fallback");
                    return true;
                }
                println!(
                    "using live job height={} baton={}",
                    job.height, job.baton_txid
                );
                *handle = Some(SearchHandle::start(cfg.clone(), job));
                cfg.mining = true;
                println!(
                    "CPU search ON â€” intensity {}%, payout {}",
                    cfg.intensity, cfg.payout_address
                );
                println!("mode: HASH256 M1 rate against live target (full PHOTON Schnorr next)");
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
                    println!("  donation ({DONATION_BPS} bps): {donation} â†’ {DONATION_ADDRESS}");
                }
                Err(_) => println!("error: split needs a non-negative integer"),
            },
            None => println!("usage: split <reward_raw>"),
        },
        other => println!("unknown command `{other}` - try `help`"),
    }
    true
}

fn main() {
    let mut cfg = RuntimeConfig::default();
    let mut handle: Option<SearchHandle> = None;
    let mut live: Option<LiveJob> = None;
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
                if !handle_line(&mut cfg, &mut handle, &mut live, &line) {
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
        assert!(c.set_intensity(100).is_ok());
        assert!(c.set_intensity(101).is_err());
    }
}

