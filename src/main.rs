//! Pickaxe Miner — interactive CLI (Stage 2).
//!
//! Controls mirror the postcorps WebGPU site (esp. intensity).
//! Donation: 2% coinbase-style split on the win tx only — never skim unrelated funds.

mod config;

use config::{RuntimeConfig, DONATION_ADDRESS, DONATION_BPS, MINER_BPS};
use std::io::{self, Write};

fn print_banner() {
    println!("Pickaxe Miner 0.1.0 — interactive CLI");
    println!("Donation: {DONATION_BPS} bps ({:.2}%) → {DONATION_ADDRESS}", DONATION_BPS as f64 / 100.0);
    println!("Miner keeps {MINER_BPS} bps. Split is on the win tx only (coinbase-style).");
    println!("Type `help` for commands.\n");
}

fn print_help() {
    println!(
        r#"Commands:
  help                         Show this help
  status                       Show intensity, payout, mining, donation
  intensity <0-100>            Set work intensity (default 50)
  payout <cashaddr>            Set miner payout address
  donation                     Show donation address and split
  start                        Start stub mining loop (no GPU yet)
  stop                         Stop stub mining
  split <reward_raw>           Preview 98%/2% split for a raw reward amount
  quit | exit                  Leave

Invariant: distribution builds pay 98% miner + 2% donation on the verified win
transaction itself. Visible before arm. Never call it a "dev fee". Never skim
unrelated wallet funds or keys."#
    );
}

fn print_status(cfg: &RuntimeConfig) {
    println!("intensity:     {}%", cfg.intensity);
    println!(
        "payout:        {}",
        if cfg.payout_address.is_empty() {
            "(not set)"
        } else {
            &cfg.payout_address
        }
    );
    println!("mining:        {}", if cfg.mining { "ON (stub)" } else { "off" });
    println!("donation:      {DONATION_BPS} bps → {DONATION_ADDRESS}");
    println!("miner share:   {MINER_BPS} bps");
    println!("gpu:           not wired yet (Stage 3+)");
}

fn print_donation() {
    println!("Donation (not a \"dev fee\"):");
    println!("  share:   {DONATION_BPS} bps = 2%");
    println!("  address: {DONATION_ADDRESS}");
    println!("  model:   coinbase-style — two outputs on the win tx (98% miner / 2% donation)");
    println!("  never:   skim unrelated balances or keys");
}

fn handle_line(cfg: &mut RuntimeConfig, line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return true;
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("").to_ascii_lowercase();

    match cmd.as_str() {
        "help" | "?" => print_help(),
        "status" => print_status(cfg),
        "donation" => print_donation(),
        "quit" | "exit" => {
            if cfg.mining {
                cfg.mining = false;
                println!("stopped stub mining.");
            }
            println!("bye.");
            return false;
        }
        "intensity" => match parts.next() {
            Some(v) => match v.parse::<u8>() {
                Ok(n) => match cfg.set_intensity(n) {
                    Ok(()) => println!("intensity set to {n}%"),
                    Err(e) => println!("error: {e}"),
                },
                Err(_) => println!("error: intensity must be an integer 0..=100"),
            },
            None => println!("usage: intensity <0-100>  (current {}%)", cfg.intensity),
        },
        "payout" => {
            let rest: Vec<&str> = parts.collect();
            if rest.is_empty() {
                println!("usage: payout <bitcoincash:…>");
            } else {
                match cfg.set_payout(rest.join(" ")) {
                    Ok(()) => println!("payout set to {}", cfg.payout_address),
                    Err(e) => println!("error: {e}"),
                }
            }
        }
        "start" => {
            if cfg.payout_address.is_empty() {
                println!("error: set payout first (`payout bitcoincash:…`)");
            } else if cfg.mining {
                println!("already mining (stub) at {}%", cfg.intensity);
            } else {
                cfg.mining = true;
                println!(
                    "stub mining ON — intensity {}%, payout {}, donation 2% on win tx (GPU not wired)",
                    cfg.intensity, cfg.payout_address
                );
            }
        }
        "stop" => {
            if cfg.mining {
                cfg.mining = false;
                println!("stub mining OFF");
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
                    println!("  donation ({DONATION_BPS} bps): {donation} → {DONATION_ADDRESS}");
                }
                Err(_) => println!("error: split needs a non-negative integer"),
            },
            None => println!("usage: split <reward_raw>"),
        },
        other => println!("unknown command `{other}` — try `help`"),
    }
    true
}

fn main() {
    let mut cfg = RuntimeConfig::default();
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
                if !handle_line(&mut cfg, &line) {
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
