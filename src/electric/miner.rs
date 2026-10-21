//! #### PR #51
//! What: `pickaxe electric`: the fee pot's address, the baton's state, the
//! GPU miner that follows the baton through Fulcrum and claims from the pot, and
//! owner-authorized recovery (legacy pots remain explicitly selectable).
//! Why: ELECTRIC shares GPU hashing and device selection with PHOTON.
//! Check: the tests in `electric` and `tools/reward-policy-vm/electric.mjs`
//! cover what is broadcast; this file only reads Fulcrum and sends.

use super::recovery::RecoveryKey;
use super::{
    build_authorized_claim, build_refund, script_hash_hex, Baton, Coin, Deployment, Pot, SearchJob,
    POT_MIN_CHANGE_SATS, TOKEN_OUTPUT_SATS,
};
use crate::cli::{ElectricCommand, ElectricDeploymentArgs};
use crate::config::{MiningToken, RuntimeConfig};
use crate::donation::{Recipient, Schedule};
use crate::electrum::ElectrumSession;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
pub fn run(
    cfg: &RuntimeConfig,
    command: &ElectricCommand,
    config_path: &Path,
    backend: crate::backend::BackendKind,
    devices: &crate::backend::DeviceSelection,
    no_tui: bool,
) -> Result<(), String> {
    match command {
        ElectricCommand::Wallet => {
            let wallet = super::wallet::create(cfg.network, config_path)?;
            println!("Pickaxe payout wallet on {}", cfg.network.as_str());
            println!("{}", wallet.address);
            for row in super::ui::qr_lines(&wallet.address)? {
                println!("{row}");
            }
            println!("Private backup: {}. Back up this file before receiving rewards. Creating this wallet does not move old funds or change the current profile.", wallet.key_path.display());
            Ok(())
        }
        ElectricCommand::Pot { deployment } => {
            let args = deployment;
            let deployment = select(cfg, args)?;
            let (pot, addresses, key_path) = pot_for(cfg, &deployment, args, config_path, true)?;
            if pot.owner_destination {
                println!("Owner-controlled pot: its saved key controls Max/send to a chosen BCH address. This is a separate new pot; old pot funds are not moved.");
            }
            if pot.authorized_claims {
                println!("Signed claim pot: fresh same-height proofs are allowed. Older funded pots retain their address and key; select --restricted-pot on status/refund to recover their BCH. No funds are moved automatically.");
            }
            if let Some(path) = key_path {
                println!(
                    "Recovery key saved: {}. Back up this private file before funding.",
                    path.display()
                );
                println!("The file is owner-only, not encrypted. Losing it prevents refunds.");
            }
            print_deployment(cfg, &deployment, &addresses);
            println!("pot:        {}", pot.address());
            // #### PR #53: the same QR code the setup shows, for a wallet to scan.
            for line in super::ui::qr_lines(&pot.address())? {
                println!("{line}");
            }
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
            let args = deployment;
            let deployment = select(cfg, args)?;
            let mut session = connect(cfg)?;
            let tip = read_tip(&mut session)?;
            let baton = read_baton(&mut session, &deployment)?;
            println!("server:     {}", session.url);
            println!("tip:        {tip}");
            print_baton(
                &deployment,
                &baton,
                tip,
                args.restricted_pot || args.legacy_pot,
            );
            if !cfg.payout_address.trim().is_empty() {
                let (pot, _, _) = pot_for(cfg, &deployment, args, config_path, false)?;
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
            let args = deployment;
            let deployment = select(cfg, args)?;
            let (pot, addresses, key_path) = pot_for(cfg, &deployment, args, config_path, false)?;
            let key = key_path
                .as_deref()
                .map(|path| RecoveryKey::load(path, false))
                .transpose()?;
            let journal_path = claim_journal_path(cfg, &pot, config_path)?;
            let _pot_lock = super::withdrawal::lock(&journal_path)?;
            let mut pkhs = [[0u8; 20]; 3];
            for (pkh, address) in pkhs.iter_mut().zip(&addresses) {
                pkh.copy_from_slice(&crate::tx::cashaddr_to_p2pkh_locking(address)?[3..23]);
            }
            if super::pending::Journal::open_with_key(
                &journal_path,
                &deployment,
                &pot,
                &pkhs,
                key.as_ref().map(RecoveryKey::secret),
            )?
            .pending()
            .is_some()
            {
                return Err("recover the pending ELECTRIC claim before refunding its pot".into());
            }
            let sends = super::withdrawal::Transfers::open(
                &journal_path.with_extension("send.json"),
                &pot,
                key.as_ref(),
            )?;
            if sends.active() {
                return Err("recover the approved Max/send in the TUI before refunding".into());
            }
            let refund_lock = crate::tx::cashaddr_to_p2pkh_locking(&addresses[0])?;
            let mut session = connect(cfg)?;
            let fee = read_relay_fee(&mut session)?;
            let coins = read_pot(&mut session, &pot)?;
            if coins.is_empty() {
                println!("The pot {} holds nothing.", pot.address());
            }
            for coin in coins {
                let refund = build_refund(
                    &pot,
                    &coin,
                    &refund_lock,
                    fee,
                    key.as_ref().map(RecoveryKey::secret),
                )?;
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
        ElectricCommand::Deploy {
            funding,
            funding_sats,
            change,
            supply,
            height,
            electric_reward,
            electric_min_difficulty,
            electric_max_difficulty,
        } => {
            let (txid, vout) = funding
                .rsplit_once(':')
                .and_then(|(txid, vout)| Some((txid, vout.parse::<u32>().ok()?)))
                .ok_or("--funding is TXID:0")?;
            let deployment = Deployment::select(
                cfg.network,
                Some(txid),
                *electric_reward,
                *electric_min_difficulty,
                *electric_max_difficulty,
            )?;
            let change = match change {
                Some(address) => address.clone(),
                None if !cfg.payout_address.trim().is_empty() => cfg.payout_address.clone(),
                None => return Err("give --change or --address for the change".into()),
            };
            let change_lock = crate::tx::cashaddr_to_p2pkh_locking(
                &crate::config::validate_payout_address(cfg.network, &change)?,
            )?;
            let mut session = connect(cfg)?;
            let height = match height {
                Some(height) => *height,
                None => read_tip(&mut session)?,
            };
            let fee = read_relay_fee(&mut session)?;
            let genesis = super::build_genesis(
                &deployment,
                &Coin {
                    txid: txid.to_ascii_lowercase(),
                    vout,
                    value: *funding_sats,
                },
                &change_lock,
                supply.unwrap_or(deployment.reward.saturating_mul(1_000_000)),
                height,
                fee,
            )?;
            println!("ELECTRIC genesis on {}", cfg.network.as_str());
            println!("category:   {}", deployment.category_hex());
            println!(
                "contract:   reward {} units, difficulty {}-{} bytes, baton at height {height}",
                deployment.reward, deployment.min_difficulty, deployment.max_difficulty
            );
            println!("covenant:   {}", hex::encode(deployment.locking_bytecode()));
            println!("fee:        {} sats", genesis.fee);
            println!("unsigned:   {}", hex::encode(&genesis.raw));
            println!(
                "Sign input 0 with the funding coin's wallet and send it. Then mine with \
                 --electric-category {}",
                deployment.category_hex()
            );
            Ok(())
        }
        ElectricCommand::Mine {
            deployment,
            once,
            max_difficulty,
            dry_run,
        } => {
            // Fail on a bad payout or a missing key before taking the GPUs.
            pot_for(
                cfg,
                &select(cfg, deployment)?,
                deployment,
                config_path,
                false,
            )?;
            let _gpu_lock = crate::mining_lock::acquire_gpu_lock()?;
            let devices = crate::backend::resolve_electric_devices(backend, devices)?;
            session(
                cfg,
                deployment,
                &devices,
                config_path,
                Session {
                    max_difficulty: *max_difficulty,
                    dry_run: *dry_run,
                    once: *once,
                    plain: no_tui,
                    create_key: false,
                },
            )
        }
    }
}

/// How one mining session runs.
struct Session {
    max_difficulty: u8,
    dry_run: bool,
    once: bool,
    plain: bool,
    /// Make the pot's recovery key if there is none (the setup's flow).
    create_key: bool,
}

/// Mines the deployment on `devices` on the mining dashboard (or plain
/// lines), following the baton and claiming from the pot.
fn session(
    cfg: &RuntimeConfig,
    args: &ElectricDeploymentArgs,
    devices: &[crate::backend::GpuDevice],
    config_path: &Path,
    options: Session,
) -> Result<(), String> {
    use std::io::IsTerminal;
    let monitor = super::ui::Monitor::new(
        cfg.intensity,
        options.plain || !std::io::stdout().is_terminal(),
        device_names(devices),
    );
    let mut ready = prepare(
        cfg,
        args,
        devices,
        config_path,
        options.create_key,
        &monitor,
    )?;
    let result = super::ui::run(&monitor, devices, cfg.network, || {
        mine(
            cfg,
            &ready.deployment,
            &mut ready.engines,
            options.max_difficulty,
            options.dry_run,
            options.once,
            &ready.pot,
            &ready.addresses,
            &ready.journal_path,
            ready.claim_key.as_ref(),
            &monitor,
        )
    });
    if crate::session_control::profiles_requested() {
        let mut final_cfg = cfg.clone();
        if args.owner_pot {
            final_cfg.electric_pot_mode = crate::config::ElectricPotMode::Owner;
        } else if args.restricted_pot {
            final_cfg.electric_pot_mode = crate::config::ElectricPotMode::Restricted;
        }
        if let Some(value) = monitor.summary().donation {
            final_cfg.token_donations.insert("ELECTRIC".into(), value);
        }
        crate::tui::remember_token_session(&final_cfg, devices, config_path, Vec::new(), true)?;
    }
    result
}

/// The GPUs as the dashboard names them for ELECTRIC.
pub fn device_names(devices: &[crate::backend::GpuDevice]) -> String {
    devices
        .iter()
        .map(|gpu| format!("{} ({})", gpu.name, gpu.backend.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A session ready to mine: the deployment, its pot, the GPU engines and
/// the pending-claim journal's path.
struct Ready {
    deployment: Deployment,
    pot: Pot,
    addresses: [String; 3],
    engines: Vec<super::gpu::Engine>,
    journal_path: PathBuf,
    claim_key: Option<RecoveryKey>,
}

fn prepare(
    cfg: &RuntimeConfig,
    args: &ElectricDeploymentArgs,
    devices: &[crate::backend::GpuDevice],
    config_path: &Path,
    create_key: bool,
    monitor: &super::ui::Monitor,
) -> Result<Ready, String> {
    let deployment = select(cfg, args)?;
    let (pot, addresses, key_path) = pot_for(cfg, &deployment, args, config_path, create_key)?;
    let claim_key = if pot.authorized_claims {
        Some(RecoveryKey::load(
            key_path.as_deref().ok_or("pot claim key unavailable")?,
            false,
        )?)
    } else {
        None
    };
    let engines = devices
        .iter()
        .map(super::gpu::Engine::new)
        .collect::<Result<Vec<_>, _>>()?;
    monitor.set_pot(&pot.address(), key_path.as_deref());
    monitor
        .withdrawal
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .enabled = pot.owner_destination;
    let journal_path = claim_journal_path(cfg, &pot, config_path)?;
    Ok(Ready {
        deployment,
        pot,
        addresses,
        engines,
        journal_path,
        claim_key,
    })
}

fn claim_journal_path(
    cfg: &RuntimeConfig,
    pot: &Pot,
    config_path: &Path,
) -> Result<PathBuf, String> {
    let path = config_path.with_file_name(format!(
        "electric-{}-{}.pending.json",
        cfg.network.as_str(),
        hex::encode(crate::proof::hash256(&pot.redeem)),
    ));
    if pot.owner_destination {
        crate::mining_lock::pot_journal_path(path.file_name().ok_or("pot journal has no name")?)
    } else {
        Ok(path)
    }
}

/// #### PR #53: ELECTRIC as one token of a hash split: the setup's
/// deployment and pot on `gpus`, reporting to `monitor`, whose `held` flag
/// the split sets. The caller holds the GPU lock and the stop signal.
pub fn run_in_split(
    cfg: &RuntimeConfig,
    gpus: &[crate::backend::GpuDevice],
    config_path: &Path,
    monitor: &super::ui::Monitor,
) -> Result<(), String> {
    run_in_split_with_pot_mode(cfg, gpus, config_path, monitor, false, false)
}

pub fn run_in_split_with_pot_mode(
    cfg: &RuntimeConfig,
    gpus: &[crate::backend::GpuDevice],
    config_path: &Path,
    monitor: &super::ui::Monitor,
    restricted_pot: bool,
    owner_pot: bool,
) -> Result<(), String> {
    let devices = crate::backend::electric_engines_for(gpus.to_vec())?;
    let args = ElectricDeploymentArgs {
        restricted_pot,
        owner_pot,
        ..Default::default()
    };
    let mut ready = prepare(cfg, &args, &devices, config_path, true, monitor)?;
    mine(
        cfg,
        &ready.deployment,
        &mut ready.engines,
        MAX_SEARCH_DIFFICULTY,
        false,
        false,
        &ready.pot,
        &ready.addresses,
        &ready.journal_path,
        ready.claim_key.as_ref(),
        monitor,
    )
}

/// #### PR #53: what the setup's fee pot holds now, in plain-BCH sats, and
/// the claims that pays at the current relay fee, read through Fulcrum.
pub fn pot_balance(cfg: &RuntimeConfig, config_path: &Path) -> Result<(u64, u64), String> {
    let args = ElectricDeploymentArgs::default();
    let deployment = select(cfg, &args)?;
    let (pot, _, _) = pot_for(cfg, &deployment, &args, config_path, false)?;
    let mut session = connect(cfg)?;
    let relay_fee = read_relay_fee(&mut session)?;
    let coins = read_pot(&mut session, &pot)?;
    Ok((
        coins.iter().map(|coin| coin.value).sum(),
        claims_paid(&coins, relay_fee),
    ))
}

/// The setup's ELECTRIC choice: mine the network's deployment on the chosen
/// GPUs on the mining dashboard, making the pot's recovery key if needed.
pub fn run_from_setup(
    cfg: &RuntimeConfig,
    gpus: &[crate::backend::GpuDevice],
    config_path: &Path,
) -> Result<(), String> {
    let _gpu_lock = crate::mining_lock::acquire_gpu_lock()?;
    let devices = crate::backend::electric_engines_for(gpus.to_vec())?;
    session(
        cfg,
        &ElectricDeploymentArgs::default(),
        &devices,
        config_path,
        Session {
            max_difficulty: MAX_SEARCH_DIFFICULTY,
            dry_run: false,
            once: false,
            plain: false,
            create_key: true,
        },
    )
}

/// The fee pot for the configured payout on the network's deployment, made
/// with a new recovery key beside `config_path` if there is none: its
/// address and where the key is saved (the setup shows both).
pub fn default_pot(cfg: &RuntimeConfig, config_path: &Path) -> Result<(String, PathBuf), String> {
    let args = ElectricDeploymentArgs::default();
    let deployment = select(cfg, &args)?;
    let (pot, _, key_path) = pot_for(cfg, &deployment, &args, config_path, true)?;
    Ok((
        pot.address(),
        key_path.ok_or("the pot has no recovery key")?,
    ))
}

/// The most difficulty the setup's sessions search (the contract's maximum).
const MAX_SEARCH_DIFFICULTY: u8 = 4;

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
fn pot_for(
    cfg: &RuntimeConfig,
    deployment: &Deployment,
    args: &ElectricDeploymentArgs,
    config_path: &Path,
    create: bool,
) -> Result<(Pot, [String; 3], Option<PathBuf>), String> {
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
    let legacy = Pot::new(deployment, &locks)?;
    if args.legacy_pot {
        return Ok((legacy, addresses, None));
    }
    let owner_pot = args.owner_pot
        || (!args.restricted_pot && cfg.electric_pot_mode == crate::config::ElectricPotMode::Owner);
    let restricted_pot = args.restricted_pot
        || (!args.owner_pot && cfg.electric_pot_mode == crate::config::ElectricPotMode::Restricted);
    let path = args.pot_key.clone().unwrap_or_else(|| {
        config_path.with_file_name(format!(
            "electric-{}-{}{}.pot-key",
            cfg.network.as_str(),
            if owner_pot { "owner-" } else { "" },
            hex::encode(crate::proof::hash256(&legacy.redeem))
        ))
    });
    let key = RecoveryKey::load(&path, create)?;
    let pot = if owner_pot {
        Pot::with_owner_destination(deployment, &locks, key.public())?
    } else if restricted_pot {
        Pot::with_recovery(deployment, &locks, Some(key.public()))?
    } else {
        Pot::with_authorized_claims(deployment, &locks, key.public())?
    };
    Ok((pot, addresses, Some(path)))
}

fn print_deployment(cfg: &RuntimeConfig, deployment: &Deployment, addresses: &[String; 3]) {
    let policy = deployment.donation_policy(
        cfg.token_donations
            .get("ELECTRIC")
            .copied()
            .or(cfg.token_donation),
    );
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

fn print_baton(deployment: &Deployment, baton: &Baton, tip: u32, restricted: bool) {
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
    match if restricted {
        super::claim_locktime(state, tip, deployment)
    } else {
        (tip >= state.last_height).then_some(tip)
    } {
        Some(locktime) if locktime > state.last_height => println!(
            "next:       a claim at difficulty {} moves the baton to height {locktime}",
            state.difficulty
        ),
        Some(_) if restricted => {
            println!("next:       a minimum-difficulty same-height claim (restricted pot)")
        }
        Some(_) => println!(
            "next:       a same-height claim at difficulty {} (authorized pot)",
            state.difficulty
        ),
        None if restricted && tip >= state.last_height => println!(
            "next:       waiting for block {} (restricted pot)",
            state.last_height.saturating_add(1)
        ),
        None => println!(
            "next:       waiting for the tip to reach height {}",
            state.last_height
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

struct WithdrawalLink<'a> {
    session: &'a mut ElectrumSession,
    pot: &'a Pot,
}
impl super::withdrawal::Link for WithdrawalLink<'_> {
    fn coins(&mut self) -> Result<Vec<Coin>, String> {
        read_pot(self.session, self.pot)
    }
    fn relay_fee(&mut self) -> Result<u64, String> {
        read_relay_fee(self.session)
    }
    fn published(&mut self, txid: &str, raw: &str) -> Result<bool, String> {
        match self
            .session
            .rpc("blockchain.transaction.get", json!([txid, false]))
        {
            Ok(value) if value.as_str().is_some_and(|v| v.eq_ignore_ascii_case(raw)) => Ok(true),
            Ok(_) => Err("server returned different send transaction bytes".into()),
            Err(error)
                if error
                    .to_ascii_lowercase()
                    .contains("no such mempool or blockchain transaction") =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    fn broadcast(&mut self, raw: &str) -> Result<String, String> {
        self.session.broadcast_raw(raw)
    }
}

/// What the miner reads each round.
struct Snapshot {
    tip: u32,
    baton: Baton,
    coin: Option<Coin>,
    relay_fee: u64,
    /// All the pot's plain-BCH sats, and the claims they pay.
    pot_sats: u64,
    pot_claims: u64,
}

/// About the size of a claim, for counting the claims a pot pays.
const CLAIM_BYTES_ESTIMATE: u64 = 1_000;

/// The claims `coins` pay, each claim taking its 1,000 sats and fee from one
/// coin and leaving at least the pot's minimum change.
fn claims_paid(coins: &[Coin], relay_fee_sats_per_kb: u64) -> u64 {
    let cost = TOKEN_OUTPUT_SATS
        + (CLAIM_BYTES_ESTIMATE * relay_fee_sats_per_kb.max(1_000)).div_ceil(1_000);
    coins
        .iter()
        .map(|coin| coin.value.saturating_sub(POT_MIN_CHANGE_SATS) / cost)
        .sum()
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
    let coins = read_pot(session, pot)?;
    let pot_sats = coins.iter().map(|coin| coin.value).sum();
    let pot_claims = claims_paid(&coins, relay_fee);
    let coin = coins.into_iter().find(|coin| coin.value >= least);
    Ok(Snapshot {
        tip,
        baton,
        coin,
        relay_fee,
        pot_sats,
        pot_claims,
    })
}

fn recipient_name(recipient: Recipient) -> &'static str {
    match recipient {
        Recipient::Miner => "you",
        Recipient::Project | Recipient::Collaborator => "donation",
    }
}

// #### PR #58: save completed proofs and exact claims before network submission.
#[allow(clippy::too_many_arguments)]
fn mine(
    cfg: &RuntimeConfig,
    deployment: &Deployment,
    engines: &mut [super::gpu::Engine],
    max_difficulty: u8,
    dry_run: bool,
    once: bool,
    pot: &Pot,
    addresses: &[String; 3],
    journal_path: &Path,
    claim_key: Option<&RecoveryKey>,
    monitor: &super::ui::Monitor,
) -> Result<(), String> {
    use super::pending::{Journal, Pending, Prepared};
    let _pot_lock = super::withdrawal::lock(journal_path)?;
    let mut transfers = super::withdrawal::Transfers::open(
        &journal_path.with_extension("send.json"),
        pot,
        claim_key,
    )?;
    let mut pkhs = [[0u8; 20]; 3];
    for (pkh, address) in pkhs.iter_mut().zip(addresses) {
        pkh.copy_from_slice(&crate::tx::cashaddr_to_p2pkh_locking(address)?[3..23]);
    }
    let mut journal = Journal::open_with_key(
        journal_path,
        deployment,
        pot,
        &pkhs,
        claim_key.map(RecoveryKey::secret),
    )?;
    monitor.pending(journal.pending().is_some());
    if transfers.active() && journal.pending().is_some() {
        return Err(
            "both a claim and a send need recovery; journals preserved, no inputs spent".into(),
        );
    }
    if dry_run && (journal.pending().is_some() || transfers.active()) {
        return Err("a pending ELECTRIC claim needs recovery; run without --dry-run".into());
    }
    monitor.event(format!(
        "ELECTRIC on {} · pot {}",
        cfg.network.as_str(),
        pot.address()
    ));
    if dry_run {
        monitor.event("Dry run: no transactions will be sent.");
    }
    let policy = deployment.donation_policy(cfg.token_donation);
    let schedule = Mutex::new(Schedule::new(policy.scheme, SCHEDULE_UNIT, rand::random())?);
    monitor.set_donation_display(policy.scheme);
    let next_nonce = AtomicU64::new(rand::random());
    let mut session: Option<ElectrumSession> = None;
    let mut spent: Option<(String, u32)> = None;
    while !monitor.stopped() {
        let link = match session.as_mut() {
            Some(link) => link,
            None => match connect(cfg) {
                Ok(link) => {
                    monitor.event(format!("Connected: {}", link.url));
                    session.insert(link)
                }
                Err(error) => {
                    monitor.state("Reconnecting");
                    monitor.event(format!("Fulcrum: {error}"));
                    monitor.wait(IDLE_POLL);
                    continue;
                }
            },
        };
        // Owner withdrawals do not need a live ELECTRIC baton. They do wait
        // for any already-verified claim, which owns its reserved pot coin.
        if !dry_run && journal.pending().is_none() {
            let mut transfer_link = WithdrawalLink { session: link, pot };
            match transfers.poll(pot, claim_key, &mut transfer_link, &monitor.withdrawal) {
                Ok(true) => {
                    monitor.state("ELECTRIC pot reserved for Max/send");
                    monitor.wait(Duration::from_millis(500));
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    monitor
                        .withdrawal
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .failure(error, transfers.active());
                    session = None;
                    monitor.wait(BATON_POLL);
                    continue;
                }
            }
        }
        let now = match snapshot(link, deployment, pot) {
            Ok(now) => {
                monitor.pot_balance(now.pot_sats, now.pot_claims);
                now
            }
            Err(error) => {
                monitor.state("Reconnecting; pending proof retained");
                monitor.event(format!("Fulcrum: {error}"));
                session = None;
                monitor.wait(IDLE_POLL);
                continue;
            }
        };
        if let Some(mut pending) = journal.pending() {
            monitor.pending(true);
            monitor.state("Submitting saved claim");
            if let Some(prepared) = &pending.prepared {
                let expected = crate::reward::transaction_id(
                    &hex::decode(&prepared.raw).map_err(|_| "invalid saved claim")?,
                );
                let outcome = claim_outcome(&now.baton, &pending.baton, &expected, |txid| {
                    link.rpc("blockchain.transaction.get", json!([txid, false]))?
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| "server returned invalid transaction data".into())
                });
                match outcome {
                    Ok(ClaimOutcome::Accepted) => {
                        spent = Some((pending.baton.txid, pending.baton.vout));
                        journal.save(None)?;
                        monitor.accepted();
                        monitor.event(format!(
                            "Reward accepted for {}: {expected}",
                            recipient_name(pending.win.recipient)
                        ));
                        if once {
                            return Ok(());
                        }
                        continue;
                    }
                    Ok(ClaimOutcome::Conflicted) => {
                        journal.retire_conflict()?;
                        monitor.event("Another claim advanced the baton; saved claim archived, following new work.");
                        continue;
                    }
                    Ok(ClaimOutcome::Unchanged) => {}
                    Err(error) => {
                        monitor.state("Recovering pending claim");
                        monitor.event(format!("Claim retained: {error}"));
                        session = None;
                        monitor.wait(BATON_POLL);
                        continue;
                    }
                }
            } else if pending.baton != now.baton {
                journal.save(None)?;
                monitor.event("Baton changed before broadcast; unspent pot preserved.");
                continue;
            }
            if pending.prepared.is_none() {
                let Some(coin) = now.coin.as_ref() else {
                    monitor.state("Holding verified proof; waiting for pot funding");
                    monitor.wait(IDLE_POLL);
                    continue;
                };
                let locktime = pot
                    .claim_locktime(now.baton.state, now.tip, deployment)
                    .ok_or("saved proof is not currently eligible for the pot")?;
                let built = build_authorized_claim(
                    deployment,
                    pot,
                    &pending.baton,
                    coin,
                    &pending.win,
                    locktime,
                    now.relay_fee,
                    claim_key.map(RecoveryKey::secret),
                )?;
                pending.prepared = Some(Prepared {
                    coin: coin.clone(),
                    locktime,
                    relay_fee: now.relay_fee,
                    raw: hex::encode(built.raw),
                });
                journal.save(Some(pending.clone()))?;
            }
            let raw = &pending.prepared.as_ref().unwrap().raw;
            let expected = crate::reward::transaction_id(
                &hex::decode(raw).map_err(|_| "invalid saved claim")?,
            );
            match link.broadcast_raw(raw) {
                Ok(txid) if txid.eq_ignore_ascii_case(&expected) => {
                    monitor.event(format!("Claim sent: {expected}; checking propagation"))
                }
                Ok(_) => {
                    return Err(
                        "server returned another transaction id; pending claim preserved".into(),
                    )
                }
                Err(error) => {
                    monitor.event(format!("Claim retained for retry: {error}"));
                    session = None;
                }
            }
            monitor.wait(BATON_POLL);
            continue;
        }
        if spent.as_ref() == Some(&(now.baton.txid.clone(), now.baton.vout)) {
            monitor.state("Waiting for authoritative baton advance");
            monitor.wait(BATON_POLL);
            continue;
        }
        monitor.pending(false);
        if monitor.paused.load(Ordering::Relaxed) {
            monitor.state("Paused");
            monitor.wait(Duration::from_millis(100));
            continue;
        }
        let state = now.baton.state;
        if now.coin.is_none() && !dry_run {
            monitor.state(format!("Waiting for BCH in the fee pot {}", pot.address()));
            monitor.wait(IDLE_POLL);
            continue;
        }
        if pot.claim_locktime(state, now.tip, deployment).is_none() {
            monitor.state(format!(
                "Waiting for block {} · fee-pot replay protection",
                state.last_height + 1,
            ));
            monitor.wait(IDLE_POLL);
            continue;
        }
        if state.difficulty > max_difficulty {
            monitor.state(format!(
                "Waiting for block {} · difficulty {} exceeds configured limit {}",
                state.last_height + 1,
                state.difficulty,
                max_difficulty
            ));
            monitor.wait(IDLE_POLL);
            continue;
        }
        if state.difficulty < deployment.min_difficulty
            || state.difficulty > deployment.max_difficulty
            || now.baton.token_amount < deployment.reward
        {
            return Err("ELECTRIC baton has invalid difficulty or insufficient rewards".into());
        }
        monitor.state(format!(
            "Mining · height {} · difficulty {}",
            now.tip, state.difficulty
        ));
        monitor.job(state.difficulty);
        let job = SearchJob {
            last_height: state.last_height,
            difficulty: state.difficulty,
            pkhs,
        };
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let before = monitor.hashes.load(Ordering::Relaxed);
        let mut moved = false;
        // #### PR #53: the hash split gives ELECTRIC GPU time only while this
        // search runs, so an empty pot or a wait leaves it to other tokens.
        struct Searching<'a>(&'a AtomicBool);
        impl Drop for Searching<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        monitor.searching.store(true, Ordering::Release);
        let searching = Searching(&monitor.searching);
        let win = std::thread::scope(|scope| {
            let search = scope.spawn(|| {
                super::gpu::search_all(engines, job, &schedule, &next_nonce, &stop, monitor)
            });
            let mut polled = Instant::now();
            while !search.is_finished() {
                std::thread::sleep(Duration::from_millis(20));
                if monitor.stopped() {
                    stop.store(true, Ordering::Relaxed);
                }
                if monitor
                    .withdrawal
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .holding()
                {
                    stop.store(true, Ordering::Relaxed);
                }
                if polled.elapsed() < BATON_POLL || monitor.stopped() {
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
                        monitor.event(format!("Fulcrum: {error}"));
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            }
            search
                .join()
                .map_err(|_| "ELECTRIC search thread panicked".to_string())?
        });
        drop(searching);
        let win = win?;
        let rate = (monitor.hashes.load(Ordering::Relaxed) - before) as f64
            / started.elapsed().as_secs_f64().max(1e-3);
        if moved {
            monitor.event("Baton changed during search; following new work.");
        }
        if let Some(win) = win {
            monitor.event(format!(
                "Verified proof for {} · {:.2} MH/s",
                recipient_name(win.recipient),
                rate / 1e6
            ));
            if dry_run {
                if once {
                    return Ok(());
                }
            } else if !moved {
                journal.save(Some(Pending {
                    baton: now.baton,
                    win,
                    prepared: None,
                }))?;
                monitor.pending(true);
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum ClaimOutcome {
    Unchanged,
    Accepted,
    Conflicted,
}

// Transaction visibility alone does not prove a reward belongs to the live baton.
fn claim_outcome(
    current: &Baton,
    old: &Baton,
    expected: &str,
    mut fetch: impl FnMut(&str) -> Result<String, String>,
) -> Result<ClaimOutcome, String> {
    let mut cursor = (current.txid.clone(), current.vout);
    if cursor == (old.txid.clone(), old.vout) {
        return Ok(ClaimOutcome::Unchanged);
    }
    for _ in 0..256 {
        if cursor == (expected.to_owned(), 1) {
            return Ok(ClaimOutcome::Accepted);
        }
        if cursor.1 != 1 {
            return Err("live ELECTRIC baton is not on the saved claim's lineage".into());
        }
        let raw = fetch(&cursor.0)?;
        let bytes = hex::decode(&raw).map_err(|_| "invalid baton transaction hex")?;
        if crate::reward::transaction_id(&bytes) != cursor.0 {
            return Err("baton transaction bytes do not match the requested id".into());
        }
        let previous = crate::runtime::first_input_outpoint(&raw)?;
        if previous == (old.txid.clone(), old.vout) {
            return Ok(ClaimOutcome::Conflicted);
        }
        cursor = previous;
    }
    Err("ELECTRIC recovery exceeded 256 baton advances; saved claim retained".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MiningNetwork;

    #[test]
    fn lost_reply_and_conflicting_claim_follow_verified_baton_lineage() {
        let old = Baton {
            txid: "42".repeat(32),
            vout: 0,
            value: 1000,
            token_amount: 100,
            state: super::super::State {
                difficulty: 1,
                last_height: 1,
                mine_count: 0,
            },
        };
        let child = |parent: &Baton, marker: u8| {
            let mut raw = vec![2, 0, 0, 0, 1];
            let mut id = hex::decode(&parent.txid).unwrap();
            id.reverse();
            raw.extend(id);
            raw.extend(parent.vout.to_le_bytes());
            raw.extend([
                0, 255, 255, 255, 255, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, marker, 0, 0, 0, 0,
            ]);
            (
                Baton {
                    txid: crate::reward::transaction_id(&raw),
                    vout: 1,
                    ..parent.clone()
                },
                hex::encode(raw),
            )
        };
        let (won, _) = child(&old, 0x51);
        let (later, raw) = child(&won, 0x51);
        assert_eq!(
            claim_outcome(&old, &old, &won.txid, |_| panic!(
                "unchanged requires no fetch"
            ))
            .unwrap(),
            ClaimOutcome::Unchanged
        );
        assert_eq!(
            claim_outcome(&won, &old, &won.txid, |_| panic!("own txid")).unwrap(),
            ClaimOutcome::Accepted
        );
        assert_eq!(
            claim_outcome(&later, &old, &won.txid, |_| Ok(raw.clone())).unwrap(),
            ClaimOutcome::Accepted
        );
        let (competitor, rival_raw) = child(&old, 0x52);
        assert_eq!(
            claim_outcome(&competitor, &old, &won.txid, |_| Ok(rival_raw.clone())).unwrap(),
            ClaimOutcome::Conflicted
        );
        assert!(claim_outcome(&competitor, &old, &won.txid, |_| Ok(raw.clone())).is_err());
        assert!(
            claim_outcome(&competitor, &old, &won.txid, |_| Err("disconnected".into())).is_err()
        );
    }

    #[test]
    fn pot_identity_survives_restart_and_backup_and_isolated_from_other_payouts() {
        let dir = std::env::temp_dir().join(format!("pickaxe-pot-{:016x}", rand::random::<u64>()));
        let config = dir.join("config.json");
        let mut cfg = RuntimeConfig {
            payout_address: crate::tx::cashaddr_with_version(
                0,
                &[0x11; 20],
                MiningNetwork::Mainnet,
            ),
            ..RuntimeConfig::default()
        };
        let deploy = Deployment::mainnet();
        let args = ElectricDeploymentArgs::default();
        assert!(pot_for(&cfg, &deploy, &args, &config, false).is_err());
        assert!(!dir.exists());
        let (pot, _, key_path) = pot_for(&cfg, &deploy, &args, &config, true).unwrap();
        let key_path = key_path.unwrap();
        assert_eq!(
            pot,
            pot_for(&cfg, &deploy, &args, &config, false).unwrap().0
        );
        let owner_args = ElectricDeploymentArgs {
            owner_pot: true,
            ..Default::default()
        };
        let (controlled, _, controlled_path) =
            pot_for(&cfg, &deploy, &owner_args, &config, true).unwrap();
        assert!(controlled.owner_destination);
        assert!(controlled.authorized_claims);
        assert_ne!(controlled.address(), pot.address());
        assert_ne!(controlled_path.unwrap(), key_path);
        assert_ne!(controlled.recovery_public_key, pot.recovery_public_key);
        // A profile return retains the exact funded owner address and key;
        // an explicitly selected older pot still overrides the saved mode.
        cfg.electric_pot_mode = crate::config::ElectricPotMode::Owner;
        assert_eq!(
            controlled,
            pot_for(&cfg, &deploy, &args, &config, false).unwrap().0
        );
        let restricted_args = ElectricDeploymentArgs {
            restricted_pot: true,
            ..Default::default()
        };
        let restricted = pot_for(&cfg, &deploy, &restricted_args, &config, false)
            .unwrap()
            .0;
        assert!(!restricted.owner_destination && !restricted.authorized_claims);
        cfg.electric_pot_mode = crate::config::ElectricPotMode::Restricted;
        assert_eq!(
            controlled,
            pot_for(&cfg, &deploy, &owner_args, &config, false)
                .unwrap()
                .0
        );
        cfg.electric_pot_mode = crate::config::ElectricPotMode::Signed;
        let alternate_config = dir.join("other-profile/config.json");
        assert_eq!(
            claim_journal_path(&cfg, &controlled, &config).unwrap(),
            claim_journal_path(&cfg, &controlled, &alternate_config).unwrap()
        );
        assert_ne!(
            claim_journal_path(&cfg, &pot, &config).unwrap(),
            claim_journal_path(&cfg, &pot, &alternate_config).unwrap()
        );
        assert_eq!(
            controlled,
            pot_for(&cfg, &deploy, &owner_args, &config, false)
                .unwrap()
                .0
        );
        let backup = dir.join("backup.pot-key");
        std::fs::copy(&key_path, &backup).unwrap();
        std::fs::remove_file(&key_path).unwrap();
        assert!(pot_for(&cfg, &deploy, &args, &config, false).is_err());
        let restored = ElectricDeploymentArgs {
            pot_key: Some(backup),
            ..args.clone()
        };
        assert_eq!(
            pot,
            pot_for(&cfg, &deploy, &restored, &config, false).unwrap().0
        );
        let legacy_args = ElectricDeploymentArgs {
            legacy_pot: true,
            ..args.clone()
        };
        let (legacy, _, no_key) = pot_for(&cfg, &deploy, &legacy_args, &config, false).unwrap();
        assert!(no_key.is_none());
        assert_ne!(legacy.address(), pot.address());
        cfg.payout_address =
            crate::tx::cashaddr_with_version(0, &[0x12; 20], MiningNetwork::Mainnet);
        let (other, _, other_path) = pot_for(&cfg, &deploy, &args, &config, true).unwrap();
        assert_ne!(other_path.unwrap(), key_path);
        assert_ne!(other.address(), pot.address());
        cfg.network = MiningNetwork::Chipnet;
        assert!(pot_for(&cfg, &deploy, &args, &config, true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
