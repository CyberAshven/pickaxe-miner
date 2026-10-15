//! #### PR #51
//! What: `pickaxe electric`: the fee pot's address, the baton's state, the CPU
//! miner that follows the baton through Fulcrum and claims from the pot, and
//! the keyless refund.
//! Why: ELECTRIC is a CPU search beside BCH and PHOTON (docs/electric-token.md).
//! Check: the tests in `electric` and `tools/reward-policy-vm/electric.mjs`
//! cover what is broadcast; this file only reads Fulcrum and sends.

use super::{
    build_claim, build_refund, claim_locktime, parallel_search, script_hash_hex, Baton, Coin,
    Deployment, Pot, SearchJob, Win, POT_MIN_CHANGE_SATS, TOKEN_OUTPUT_SATS,
};
use crate::cli::{ElectricCommand, ElectricDeploymentArgs};
use crate::config::{MiningToken, RuntimeConfig};
use crate::donation::{Recipient, Schedule};
use crate::electrum::ElectrumSession;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How often the miner reads the baton while it searches.
const BATON_POLL: Duration = Duration::from_secs(2);
/// How long it waits for a new block or a funded pot before reading again.
const IDLE_POLL: Duration = Duration::from_secs(10);
/// Hashes per donation-schedule slot.
const SCHEDULE_UNIT: u64 = 1 << 20;

/// Runs one `pickaxe electric` command.
pub fn run(cfg: &RuntimeConfig, command: &ElectricCommand) -> Result<(), String> {
    match command {
        ElectricCommand::Pot { deployment } => {
            let deployment = select(cfg, deployment)?;
            let (pot, addresses) = pot_for(cfg, &deployment)?;
            print_deployment(cfg, &deployment, &addresses);
            println!("pot:        {}", pot.address());
            println!(
                "Fund it with plain BCH from your wallet. Each claim takes {TOKEN_OUTPUT_SATS} sats \
                 (they arrive with the tokens) plus the relay fee, at most {} sats. \
                 `pickaxe electric refund` returns it to {}.",
                super::POT_MAX_FEE_SATS,
                addresses[0]
            );
            Ok(())
        }
        ElectricCommand::Status { deployment } => {
            let deployment = select(cfg, deployment)?;
            let mut session = connect(cfg)?;
            let tip = read_tip(&mut session)?;
            let baton = read_baton(&mut session, &deployment)?;
            println!("server:     {}", session.url);
            println!("tip:        {tip}");
            print_baton(&deployment, &baton, tip);
            if !cfg.payout_address.trim().is_empty() {
                let (pot, _) = pot_for(cfg, &deployment)?;
                let coins = read_pot(&mut session, &pot)?;
                println!(
                    "pot:        {} ({} sats in {} coin{})",
                    pot.address(),
                    coins.iter().map(|coin| coin.value).sum::<u64>(),
                    coins.len(),
                    if coins.len() == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }
        ElectricCommand::Refund {
            deployment,
            broadcast,
        } => {
            let deployment = select(cfg, deployment)?;
            let (pot, addresses) = pot_for(cfg, &deployment)?;
            let refund_lock = crate::tx::cashaddr_to_p2pkh_locking(&addresses[0])?;
            let mut session = connect(cfg)?;
            let fee = read_relay_fee(&mut session)?;
            let coins = read_pot(&mut session, &pot)?;
            if coins.is_empty() {
                println!("The pot {} holds nothing.", pot.address());
            }
            for coin in coins {
                let refund = build_refund(&pot, &coin, &refund_lock, fee)?;
                println!(
                    "refund {}:{} {} sats -> {} (fee {})",
                    coin.txid,
                    coin.vout,
                    coin.value - refund.fee,
                    addresses[0],
                    refund.fee
                );
                if *broadcast {
                    let txid = session.broadcast_raw(&hex::encode(&refund.raw))?;
                    println!("sent {txid}");
                } else {
                    println!("{}", hex::encode(&refund.raw));
                }
            }
            if !broadcast {
                println!("Nothing was sent; add --broadcast to send these.");
            }
            Ok(())
        }
        ElectricCommand::Mine {
            deployment,
            threads,
            max_difficulty,
            dry_run,
        } => {
            let deployment = select(cfg, deployment)?;
            let threads = threads.unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map_or(1, |n| n.get().saturating_sub(1))
                    .max(1)
            });
            mine(cfg, &deployment, threads, *max_difficulty, *dry_run)
        }
    }
}

fn select(cfg: &RuntimeConfig, args: &ElectricDeploymentArgs) -> Result<Deployment, String> {
    Deployment::select(
        cfg.network,
        args.electric_category.as_deref(),
        args.electric_reward,
        args.electric_min_difficulty,
        args.electric_max_difficulty,
    )
}

/// The pot for the configured payout and the donation addresses, and the
/// three recipients' addresses (miner, project, collaborator).
fn pot_for(cfg: &RuntimeConfig, deployment: &Deployment) -> Result<(Pot, [String; 3]), String> {
    if cfg.payout_address.trim().is_empty() {
        return Err("set a payout address with --address (a P2PKH CashAddr)".into());
    }
    let payout = crate::config::validate_payout_address(cfg.network, &cfg.payout_address)?;
    let addresses = deployment
        .donation_policy(cfg.token_donation)
        .payouts(cfg.network, &payout)?;
    let mut locks = Vec::with_capacity(3);
    for address in &addresses {
        locks.push(crate::tx::cashaddr_to_p2pkh_locking(address)?);
    }
    let locks: [Vec<u8>; 3] = locks.try_into().map_err(|_| "three recipients")?;
    Ok((Pot::new(deployment, &locks)?, addresses))
}

fn print_deployment(cfg: &RuntimeConfig, deployment: &Deployment, addresses: &[String; 3]) {
    let policy = deployment.donation_policy(cfg.token_donation);
    println!("ELECTRIC on {}", cfg.network.as_str());
    println!("category:   {}", deployment.category_hex());
    println!(
        "reward:     {} units per claim, difficulty {}-{} bytes",
        deployment.reward, deployment.min_difficulty, deployment.max_difficulty
    );
    println!("payout:     {}", addresses[0]);
    println!(
        "{} of the search work, for {} and {}",
        policy.scheme.description(),
        addresses[1],
        addresses[2]
    );
}

fn print_baton(deployment: &Deployment, baton: &Baton, tip: u32) {
    let state = baton.state;
    println!(
        "baton:      {}:{} difficulty {} height {} claims {} left {} units",
        baton.txid,
        baton.vout,
        state.difficulty,
        state.last_height,
        state.mine_count,
        baton.token_amount
    );
    match claim_locktime(state, tip, deployment) {
        Some(locktime) if locktime > state.last_height => println!(
            "next:       a claim at difficulty {} moves the baton to height {locktime}",
            state.difficulty
        ),
        Some(_) => println!(
            "next:       the cheap claim at difficulty {} on this height",
            state.difficulty
        ),
        None => println!(
            "next:       waiting for block {} (the pot pays no claim above difficulty {} on one height)",
            state.last_height + 1,
            deployment.min_difficulty
        ),
    }
}

fn connect(cfg: &RuntimeConfig) -> Result<ElectrumSession, String> {
    ElectrumSession::connect_failover_for_deployment(
        &cfg.electrum_endpoints(),
        MiningToken::Photon.photon_deployment(cfg.network),
    )
}

fn read_tip(session: &mut ElectrumSession) -> Result<u32, String> {
    let header = session.rpc("blockchain.headers.subscribe", json!([]))?;
    header["height"]
        .as_u64()
        .and_then(|height| u32::try_from(height).ok())
        .ok_or_else(|| "Fulcrum's tip has no height".into())
}

fn read_relay_fee(session: &mut ElectrumSession) -> Result<u64, String> {
    crate::fee::bch_value_to_sats(&session.rpc("blockchain.relayfee", json!([]))?)
}

fn unspent(session: &mut ElectrumSession, locking: &[u8]) -> Result<Vec<Value>, String> {
    match session.rpc(
        "blockchain.scripthash.listunspent",
        json!([script_hash_hex(locking), "include_tokens"]),
    )? {
        Value::Array(items) => Ok(items),
        _ => Err("Fulcrum's listunspent is not a list".into()),
    }
}

fn outpoint(item: &Value) -> Result<(String, u32, u64), String> {
    let txid = item["tx_hash"]
        .as_str()
        .ok_or("unspent output without tx_hash")?;
    let vout = item["tx_pos"]
        .as_u64()
        .and_then(|vout| u32::try_from(vout).ok())
        .ok_or("unspent output without tx_pos")?;
    let value = item["value"]
        .as_u64()
        .ok_or("unspent output without value")?;
    Ok((txid.to_ascii_lowercase(), vout, value))
}

/// The one unspent mutable NFT of the deployment's category at its covenant.
fn read_baton(session: &mut ElectrumSession, deployment: &Deployment) -> Result<Baton, String> {
    let category = deployment.category_hex();
    let mut batons = Vec::new();
    for item in unspent(session, &deployment.locking_bytecode())? {
        let token = &item["token_data"];
        if token["category"].as_str() != Some(category.as_str())
            || token["nft"]["capability"].as_str() != Some("mutable")
        {
            continue;
        }
        let (txid, vout, value) = outpoint(&item)?;
        let commitment = hex::decode(token["nft"]["commitment"].as_str().unwrap_or_default())
            .map_err(|_| "baton commitment is not hex")?;
        let token_amount = match &token["amount"] {
            Value::String(text) => text.parse().ok(),
            other => other.as_u64(),
        }
        .ok_or("baton token amount is not a number")?;
        batons.push(Baton {
            txid,
            vout,
            value,
            token_amount,
            state: super::State::decode(&commitment)?,
        });
    }
    match batons.len() {
        1 => Ok(batons.remove(0)),
        0 => Err("no ELECTRIC baton is unspent at the covenant".into()),
        _ => Err("more than one ELECTRIC baton is unspent; waiting for one".into()),
    }
}

/// The pot's plain-BCH coins, largest first.
fn read_pot(session: &mut ElectrumSession, pot: &Pot) -> Result<Vec<Coin>, String> {
    let mut coins = Vec::new();
    for item in unspent(session, &pot.locking_bytecode())? {
        if !item["token_data"].is_null() {
            continue;
        }
        let (txid, vout, value) = outpoint(&item)?;
        coins.push(Coin { txid, vout, value });
    }
    coins.sort_by_key(|coin| std::cmp::Reverse(coin.value));
    Ok(coins)
}

/// What the miner reads each round.
struct Snapshot {
    tip: u32,
    baton: Baton,
    coin: Option<Coin>,
    relay_fee: u64,
}

fn snapshot(
    session: &mut ElectrumSession,
    deployment: &Deployment,
    pot: &Pot,
) -> Result<Snapshot, String> {
    let tip = read_tip(session)?;
    let baton = read_baton(session, deployment)?;
    let relay_fee = read_relay_fee(session)?;
    // A claim is about 800 bytes; a coin that cannot pay one is skipped.
    let least = TOKEN_OUTPUT_SATS + POT_MIN_CHANGE_SATS + relay_fee.max(1_000);
    let coin = read_pot(session, pot)?
        .into_iter()
        .find(|coin| coin.value >= least);
    Ok(Snapshot {
        tip,
        baton,
        coin,
        relay_fee,
    })
}

fn recipient_name(recipient: Recipient) -> &'static str {
    match recipient {
        Recipient::Miner => "you",
        Recipient::Project => "the project donation",
        Recipient::Collaborator => "the collaborator donation",
    }
}

/// Searches each baton the pot pays for and broadcasts the claims. Never
/// sends a proof twice: the contract would take a proof again while the
/// baton's height stays put, and that reward is not Pickaxe's to take.
fn mine(
    cfg: &RuntimeConfig,
    deployment: &Deployment,
    threads: usize,
    max_difficulty: u8,
    dry_run: bool,
) -> Result<(), String> {
    let (pot, addresses) = pot_for(cfg, deployment)?;
    print_deployment(cfg, deployment, &addresses);
    println!("pot:        {}", pot.address());
    println!("threads:    {threads}, up to difficulty {max_difficulty}");
    if dry_run {
        println!("Dry run: nothing is sent and the pot is not needed.");
    }
    let mut pkhs = [[0u8; 20]; 3];
    for (pkh, address) in pkhs.iter_mut().zip(&addresses) {
        pkh.copy_from_slice(&crate::tx::cashaddr_to_p2pkh_locking(address)?[3..23]);
    }
    let policy = deployment.donation_policy(cfg.token_donation);
    let schedule = Mutex::new(Schedule::new(policy.scheme, SCHEDULE_UNIT, rand::random())?);
    let next_nonce = AtomicU64::new(rand::random());
    let hashes = AtomicU64::new(0);
    let mut sent: HashSet<(u32, [u8; 20], u64)> = HashSet::new();
    let mut pending: Option<(Baton, Win)> = None;
    let mut session: Option<ElectrumSession> = None;
    let mut waiting = String::new();
    let (mut claims, mut lost) = (0u64, 0u64);
    // The baton our last claim spent, until Fulcrum lists its successor.
    let mut spent: Option<(String, u32)> = None;
    loop {
        let link = match session.as_mut() {
            Some(link) => link,
            None => match connect(cfg) {
                Ok(link) => {
                    println!("server:     {}", link.url);
                    session.insert(link)
                }
                Err(error) => {
                    eprintln!("Fulcrum: {error}");
                    std::thread::sleep(IDLE_POLL);
                    continue;
                }
            },
        };
        let now = match snapshot(link, deployment, &pot) {
            Ok(now) => now,
            Err(error) => {
                eprintln!("Fulcrum: {error}");
                session = None;
                std::thread::sleep(IDLE_POLL);
                continue;
            }
        };
        let note = |waiting: &mut String, text: String| {
            if *waiting != text {
                println!("{text}");
                *waiting = text;
            }
        };
        // A win found while the pot was empty, if its baton is still there.
        if let Some((baton, win)) = pending.take() {
            if baton != now.baton {
                println!("The baton moved on before the pot was funded; that proof is gone.");
            } else if now.coin.is_none() {
                pending = Some((baton, win));
                note(
                    &mut waiting,
                    format!("Holding a win: fund the pot {}", pot.address()),
                );
                std::thread::sleep(IDLE_POLL);
                continue;
            } else {
                let outcome = claim(link, deployment, &pot, &now, &win, &mut sent);
                if report(outcome, &win, &mut claims, &mut lost) {
                    spent = Some((now.baton.txid.clone(), now.baton.vout));
                }
                continue;
            }
        }
        if spent.as_ref() == Some(&(now.baton.txid.clone(), now.baton.vout)) {
            std::thread::sleep(BATON_POLL);
            continue;
        }
        let state = now.baton.state;
        if now.coin.is_none() && !dry_run {
            note(
                &mut waiting,
                format!("Waiting for BCH in the pot {}", pot.address()),
            );
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        if claim_locktime(state, now.tip, deployment).is_none() {
            note(
                &mut waiting,
                format!(
                    "Waiting for block {}: the baton is at difficulty {} on height {}",
                    state.last_height + 1,
                    state.difficulty,
                    state.last_height
                ),
            );
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        if state.difficulty > max_difficulty {
            note(
                &mut waiting,
                format!(
                    "Waiting: the baton needs difficulty {} (above --max-difficulty {max_difficulty})",
                    state.difficulty
                ),
            );
            std::thread::sleep(IDLE_POLL);
            continue;
        }
        waiting.clear();
        println!(
            "Searching difficulty {} for height {} (claim {})",
            state.difficulty,
            state.last_height,
            state.mine_count + 1
        );
        let job = SearchJob {
            last_height: state.last_height,
            difficulty: state.difficulty,
            pkhs,
        };
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let before = hashes.load(Ordering::Relaxed);
        let mut moved = false;
        let win = std::thread::scope(|scope| {
            let search = scope
                .spawn(|| parallel_search(job, &schedule, threads, &next_nonce, &stop, &hashes));
            let mut polled = Instant::now();
            while !search.is_finished() {
                std::thread::sleep(Duration::from_millis(100));
                if polled.elapsed() < BATON_POLL {
                    continue;
                }
                polled = Instant::now();
                match read_baton(link, deployment) {
                    Ok(baton) if baton == now.baton => {}
                    Ok(_) => {
                        moved = true;
                        stop.store(true, Ordering::Relaxed);
                    }
                    Err(error) => {
                        eprintln!("Fulcrum: {error}");
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            }
            search.join().unwrap_or(None)
        });
        let rate = (hashes.load(Ordering::Relaxed) - before) as f64
            / started.elapsed().as_secs_f64().max(1e-3);
        let Some(win) = win else {
            if moved {
                lost += 1;
                println!(
                    "The baton moved after {:.1}s at {:.1} MH/s; searching the new one",
                    started.elapsed().as_secs_f64(),
                    rate / 1e6
                );
            }
            continue;
        };
        println!(
            "Found a proof for {} after {:.1}s at {:.1} MH/s",
            recipient_name(win.recipient),
            started.elapsed().as_secs_f64(),
            rate / 1e6
        );
        if dry_run {
            continue;
        }
        // Claim against what is there now, not what was there at the start.
        let now = match snapshot(link, deployment, &pot) {
            Ok(fresh) if fresh.baton == now.baton => fresh,
            Ok(_) => {
                lost += 1;
                println!("Too late: the baton moved before the claim was sent");
                continue;
            }
            Err(error) => {
                eprintln!("Fulcrum: {error}");
                session = None;
                pending = Some((now.baton, win));
                continue;
            }
        };
        if now.coin.is_none() {
            println!(
                "The pot cannot pay this claim; fund {} to send it",
                pot.address()
            );
            pending = Some((now.baton, win));
            continue;
        }
        let outcome = claim(link, deployment, &pot, &now, &win, &mut sent);
        if report(outcome, &win, &mut claims, &mut lost) {
            spent = Some((now.baton.txid.clone(), now.baton.vout));
        }
    }
}

/// Builds and sends one claim, once per proof.
fn claim(
    session: &mut ElectrumSession,
    deployment: &Deployment,
    pot: &Pot,
    now: &Snapshot,
    win: &Win,
    sent: &mut HashSet<(u32, [u8; 20], u64)>,
) -> Result<String, String> {
    let state = now.baton.state;
    let locktime = claim_locktime(state, now.tip, deployment)
        .ok_or("the pot does not pay a claim on this baton yet")?;
    let coin = now.coin.as_ref().ok_or("the pot is empty")?;
    if !sent.insert((state.last_height, win.pkh, win.nonce)) {
        return Err("this proof was sent before; Pickaxe does not replay proofs".into());
    }
    let built = build_claim(
        deployment,
        pot,
        &now.baton,
        coin,
        win,
        locktime,
        now.relay_fee,
    )?;
    session.broadcast_raw(&hex::encode(&built.raw))
}

/// Prints a claim's outcome; true when the server took it.
fn report(outcome: Result<String, String>, win: &Win, claims: &mut u64, lost: &mut u64) -> bool {
    match outcome {
        Ok(txid) => {
            *claims += 1;
            println!(
                "Claimed for {}: {txid} ({claims} claimed, {lost} lost)",
                recipient_name(win.recipient)
            );
            true
        }
        Err(error) => {
            *lost += 1;
            println!("Claim not accepted: {error}");
            false
        }
    }
}
