//! #### PR #42, merge-mining v2
//! The server's merge-mining hub: the token set jobs carry, the claim
//! worker's proofs and their journal.
//!
//! A winning share reaches the hub through a bounded queue the device thread
//! never waits on. The claim worker builds each won entry's proof, checks it
//! with the reference verifier exactly as a covenant would (a proof that
//! fails is a Pickaxe bug and is never kept as claimable), and saves it to
//! `<config>.sv2-token-proofs.json`, readable by the owner alone since it
//! names payout scripts. tSAFA's claimer (Chipnet, from a deployment file;
//! see `safa_tick`) claims its rows; any other token's proof stops at
//! "proven". A journal that cannot be written turns token claims off and
//! leaves BCH mining as it is.
//!
//! Privacy: each journal row is one claim, broadcast in its own transaction
//! (never batched with a proof from another share), no earlier than its
//! `broadcast_after` time, a small random delay after the win
//! ([`ClaimDelay`], 5 to 60 seconds by default).
//!
//! No token is registered on either network. The Chipnet test token
//! (`stratum-v2 serve --merge-test-token`) has a simulated baton: a
//! synthetic lock hash, and a state hash that moves to the winning header's
//! hash after each proven win, which changes the set and makes the server
//! re-issue jobs on the same parent.

use super::{
    registry::{MergeToken, TEST_TOKEN},
    safa_auxpow::{self, build_claim, Chain, ClaimTerms, Deployment, Vault},
    set::{SetEntry, TokenSet, TokenState},
    split::Split,
    verify::{verify, ClaimView},
    Hash,
};
use crate::config::MiningNetwork;
use crate::stratum_v2::{channel::TokenWin, payout, template::double_sha256};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// Proofs the journal keeps, the oldest dropped first.
const KEPT_PROOFS: usize = 256;
/// Token states recently won, so each is proven once.
const RECENT_STATES: usize = 256;

/// One proven win, as the dashboard lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenWin {
    pub token: &'static str,
    pub height: u32,
}

/// #### merge-mining v2: the claim broadcast delay
/// What: how long after a win its claim may be broadcast, drawn per claim
/// at random from `min..=max` seconds (5 to 60 by default; `0-0` for none).
/// Why: a claim broadcast the moment a share is found ties the claim to the
/// share's timing and to the sender's connection; a small random delay
/// loosens both. Routing the broadcast through Tor is up to the operator.
/// Look here if: a journal row's `broadcast_after` is before its
/// `found_at`, or `--claim-delay` is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimDelay {
    pub min: u32,
    pub max: u32,
}

impl Default for ClaimDelay {
    fn default() -> Self {
        Self { min: 5, max: 60 }
    }
}

/// The longest claim delay accepted: one day.
const MAX_CLAIM_DELAY: u32 = 86_400;

impl ClaimDelay {
    /// `SECONDS` or `MIN-MAX` seconds, at most a day.
    pub fn parse(text: &str) -> Result<Self, String> {
        let error = || "--claim-delay takes SECONDS or MIN-MAX seconds, at most 86400".to_owned();
        let number = |part: &str| part.trim().parse::<u32>().map_err(|_| error());
        let (min, max) = match text.split_once('-') {
            Some((min, max)) => (number(min)?, number(max)?),
            None => {
                let seconds = number(text)?;
                (seconds, seconds)
            }
        };
        if min > max || max > MAX_CLAIM_DELAY {
            return Err(error());
        }
        Ok(Self { min, max })
    }

    /// A delay drawn from `min..=max`.
    pub fn pick(&self) -> u64 {
        let span = u64::from(self.max - self.min) + 1;
        u64::from(self.min) + rand::random::<u64>() % span
    }
}

/// The test token's simulated baton: its synthetic lock hash and target.
struct TestBaton {
    lock_hash: Hash,
    target_bits: u32,
}

pub struct TokenHub {
    network: MiningNetwork,
    journal_path: PathBuf,
    set: RwLock<Option<Arc<TokenSet>>>,
    changed: AtomicBool,
    test: Option<TestBaton>,
    delay: ClaimDelay,
    /// (category, state hash) of states already won.
    won: Mutex<VecDeque<(Hash, Hash)>>,
    /// Why token claims are off, after a journal failure.
    off: Mutex<Option<String>>,
    /// #### merge-mining v2: tSAFA's vaults and claims, when a deployment
    /// file was given.
    safa: Option<SafaVaults>,
    /// #### merge-mining v2: one writer of the proof journal at a time (the
    /// claim worker adds proofs, the tSAFA claimer moves their states).
    journal: Mutex<()>,
}

// #### merge-mining v2: tSAFA's threads
// What: with a deployment file, the hub merge-mines one tSAFA thread per
// job: of the vaults Fulcrum lists, one whose state is older than the
// job's height, not already won here and whose satoshis pay a claim, with
// the easiest claim target at that height (the lowest thread on a tie).
// The leaf commits the vault's lock hash, its state hash and the claim
// target the covenant computes for that height. The set is built per
// height and rebuilt when the vaults change or a thread's state is won.
// Why: a coinbase holds one leaf per category, and every thread shares
// tSAFA's; a won state is worth one claim, so mining it again is wasted.
// Look here if: no tSAFA leaf reaches jobs, or a won thread is mined
// again.
struct SafaVaults {
    deployment: Arc<Deployment>,
    token: &'static MergeToken,
    endpoints: Vec<String>,
    vaults: Mutex<Vec<Vault>>,
    /// Bumped when the vaults change or a state is won.
    generation: AtomicU64,
    /// The set last built: its height and generation.
    built: Mutex<Option<(u32, u64, Arc<TokenSet>)>>,
    serial: AtomicU64,
    /// The vaults jobs lately carried, so a win knows its thread.
    mined: Mutex<VecDeque<Vault>>,
    claims: Mutex<ClaimCounts>,
}

/// Vaults jobs carried that the hub remembers.
const MINED_VAULTS: usize = 64;

/// #### merge-mining v2: what the tSAFA claimer did, for the dashboard
/// (never an address or a script).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaimCounts {
    pub token: &'static str,
    /// Vaults Fulcrum lists.
    pub vaults: usize,
    /// Proven, waiting for their delay, their height or a retry.
    pub waiting: u64,
    pub sent: u64,
    pub confirmed: u64,
    /// Another claim moved the thread first.
    pub lost: u64,
    pub failed: u64,
    /// The latest error, if the last round had one.
    pub error: Option<String>,
}

impl std::fmt::Debug for TokenHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenHub({})", self.network.as_str())
    }
}

/// The compact target of a share difficulty: difficulty 1 is
/// `0xffff << 208`, written with `3 <= exp <= 32` and `0 < mant < 0x800000`
/// as a token's target is.
pub fn bits_for_difficulty(difficulty: u64) -> Result<u32, String> {
    if difficulty == 0 || difficulty > 1 << 48 {
        return Err("the test token's difficulty must be from 1 to 2^48".into());
    }
    let target = (num_bigint::BigUint::from(0xffffu32) << 208u32) / difficulty;
    let bytes = target.to_bytes_be();
    let mut size = bytes.len() as u32;
    let mut mant = bytes
        .iter()
        .take(3)
        .fold(0u32, |mant, byte| (mant << 8) | u32::from(*byte));
    if bytes.len() < 3 {
        mant <<= 8 * (3 - bytes.len() as u32);
    }
    if mant >= 0x0080_0000 {
        mant >>= 8;
        size += 1;
    }
    Ok((size << 24) | mant)
}

/// #### PR #42: the share difficulty of a compact target, the inverse of
/// `bits_for_difficulty`, for display.
pub fn difficulty_for_bits(bits: u32) -> f64 {
    let mantissa = f64::from(bits & 0x007f_ffff);
    if mantissa == 0.0 {
        return 0.0;
    }
    // Difficulty 1 is 0xffff · 256^26; a target is mantissa · 256^(size - 3).
    let size = i32::try_from(bits >> 24).unwrap_or(0);
    f64::from(0xffffu32) / mantissa * 256f64.powi(29 - size)
}

impl TokenHub {
    // #### PR #42: the Chipnet test token
    // What: a hub that merge-mines the test token, its state anchored to a
    // simulated baton at `target_bits`: a synthetic lock hash and a state
    // hash that each proven win moves.
    // Why: no real token exists; this runs the whole path (commitment,
    // shares, proofs, journal, re-issued jobs) on Chipnet devices.
    // Look here if: the flag is accepted on mainnet, or the test token's
    // jobs never change after a win.
    /// The Chipnet test token at a compact target; refused on any network
    /// the test token's registry row is not on.
    pub fn test_token(
        network: MiningNetwork,
        journal_path: PathBuf,
        target_bits: u32,
        delay: ClaimDelay,
    ) -> Result<Self, String> {
        if TEST_TOKEN.network != network {
            return Err("the merge-mining test token exists only on Chipnet".into());
        }
        let hub = Self {
            network,
            journal_path,
            set: RwLock::new(None),
            changed: AtomicBool::new(false),
            test: Some(TestBaton {
                lock_hash: super::sha256(b"pickaxe merge-mining test token baton"),
                target_bits,
            }),
            delay,
            won: Mutex::new(VecDeque::new()),
            off: Mutex::new(None),
            safa: None,
            journal: Mutex::new(()),
        };
        hub.set_test_state(super::sha256(b"pickaxe merge-mining test token state"), 1)?;
        Ok(hub)
    }

    fn set_test_state(&self, state_hash: Hash, serial: u64) -> Result<(), String> {
        let Some(test) = &self.test else {
            return Ok(());
        };
        let token: &'static MergeToken = &TEST_TOKEN;
        let entries = vec![SetEntry::new(
            token,
            TokenState::new(test.lock_hash, state_hash, test.target_bits)?,
        )];
        let set = TokenSet::new(self.network, entries, serial)?;
        if let Ok(mut current) = self.set.write() {
            *current = Some(Arc::new(set));
        }
        self.changed.store(true, Ordering::Release);
        Ok(())
    }

    /// The set the next jobs carry.
    pub fn current(&self) -> Option<Arc<TokenSet>> {
        self.set.read().ok().and_then(|set| set.clone())
    }

    // #### merge-mining v2: tSAFA, from its deployment file
    // What: a hub that merge-mines tSAFA's threads (see `SafaVaults`) and
    // claims its wins through Fulcrum (`safa_tick`). Chipnet only.
    // Why: tSAFA is a test token; only an explicit deployment file starts
    // it.
    // Look here if: `--safa-deployment` is accepted on mainnet.
    /// A hub mining the tSAFA deployment, its vaults read and its claims
    /// sent through `endpoints` (Fulcrum).
    pub fn safa(
        network: MiningNetwork,
        journal_path: PathBuf,
        deployment: Deployment,
        delay: ClaimDelay,
        endpoints: Vec<String>,
    ) -> Result<Self, String> {
        if network != MiningNetwork::Chipnet || deployment.network != MiningNetwork::Chipnet {
            return Err("tSAFA is mined on Chipnet only".into());
        }
        let token: &'static MergeToken = Box::leak(Box::new(MergeToken {
            name: Box::leak(deployment.name.clone().into_boxed_str()),
            network,
            category: deployment.category,
            deployment: None,
            verifier_sha256: None,
            max_aux_height: super::tree::MAX_HEIGHT,
            priority: 0,
            split: true,
        }));
        token.verify()?;
        Ok(Self {
            network,
            journal_path,
            set: RwLock::new(None),
            changed: AtomicBool::new(false),
            test: None,
            delay,
            won: Mutex::new(VecDeque::new()),
            off: Mutex::new(None),
            safa: Some(SafaVaults {
                deployment: Arc::new(deployment),
                token,
                endpoints,
                vaults: Mutex::new(Vec::new()),
                generation: AtomicU64::new(0),
                built: Mutex::new(None),
                serial: AtomicU64::new(0),
                mined: Mutex::new(VecDeque::new()),
                claims: Mutex::new(ClaimCounts {
                    token: token.name,
                    ..ClaimCounts::default()
                }),
            }),
            journal: Mutex::new(()),
        })
    }

    /// Whether this hub mines tSAFA (and runs its claimer).
    pub fn is_safa(&self) -> bool {
        self.safa.is_some()
    }

    /// The chain tSAFA's claimer uses: the configured Fulcrum servers.
    pub fn safa_chain(&self) -> Option<safa_auxpow::Fulcrum> {
        let safa = self.safa.as_ref()?;
        Some(safa_auxpow::Fulcrum::new(
            safa.endpoints.clone(),
            self.network,
        ))
    }

    /// What the tSAFA claimer did.
    pub fn claim_counts(&self) -> Option<ClaimCounts> {
        let safa = self.safa.as_ref()?;
        safa.claims.lock().ok().map(|counts| counts.clone())
    }

    /// The set a job at coinbase height `height` carries: tSAFA's thread
    /// for that height, otherwise the current set.
    pub fn current_for(&self, height: u32) -> Option<Arc<TokenSet>> {
        let Some(safa) = &self.safa else {
            return self.current();
        };
        let generation = safa.generation.load(Ordering::Acquire);
        let mut built = safa.built.lock().ok()?;
        if let Some((at, seen, set)) = built.as_ref() {
            if *at == height && *seen == generation {
                return Some(set.clone());
            }
        }
        let vaults = safa.vaults.lock().ok()?.clone();
        let won = self.won.lock().ok()?.clone();
        let deployment = &safa.deployment;
        let chosen = vaults
            .iter()
            .filter(|vault| !won.contains(&(deployment.category, vault.state_hash())))
            .filter(|vault| vault.value >= claim_reserve(deployment))
            .filter_map(|vault| {
                let state = vault.state()?;
                let delta = height
                    .checked_sub(state.height)
                    .filter(|delta| *delta > 0)?;
                let bits =
                    safa_auxpow::claim_target_bits(state.target_bits, delta, deployment.threads)?;
                Some((safa_auxpow::expand_token_target(bits)?, bits, vault))
            })
            .max_by(|a, b| a.0.cmp(&b.0).then(b.2.thread().cmp(&a.2.thread())));
        let set = chosen.and_then(|(_, bits, vault)| {
            let state = TokenState::new(deployment.lock_hash, vault.state_hash(), bits).ok()?;
            let serial = safa.serial.fetch_add(1, Ordering::AcqRel) + 1;
            let set =
                TokenSet::new(self.network, vec![SetEntry::new(safa.token, state)], serial).ok()?;
            if let Ok(mut mined) = safa.mined.lock() {
                if !mined
                    .iter()
                    .any(|known| known.commitment == vault.commitment)
                {
                    if mined.len() >= MINED_VAULTS {
                        mined.pop_front();
                    }
                    mined.push_back(vault.clone());
                }
            }
            Some(Arc::new(set))
        });
        if let Ok(mut current) = self.set.write() {
            *current = set.clone();
        }
        *built = set.as_ref().map(|set| (height, generation, set.clone()));
        set
    }

    /// Takes Fulcrum's vaults; jobs change when they do.
    fn set_vaults(&self, vaults: Vec<Vault>) {
        let Some(safa) = &self.safa else {
            return;
        };
        let Ok(mut known) = safa.vaults.lock() else {
            return;
        };
        if *known != vaults {
            *known = vaults;
            safa.generation.fetch_add(1, Ordering::AcqRel);
            self.changed.store(true, Ordering::Release);
        }
    }

    // #### merge-mining v2: the tSAFA claimer
    // What: one round of claims: Fulcrum's vaults (jobs follow them), the
    // tip, then each tSAFA row of the journal moves on: a proven win past
    // its `broadcast_after`, once the chain has reached its coinbase height
    // (its lock time), is built against its thread's vault, checked with
    // `check_claim` and broadcast ("broadcast", with its txid); a vault
    // already in another state makes it "stale"; a claim the covenant would
    // refuse makes it "failed"; a passing failure (Fulcrum, a refused
    // broadcast) is retried with a growing wait, up to 10 times. A sent
    // claim is "confirmed" once its vault output is in a block (or the
    // thread moved on with the claim known), "stale" if another claim took
    // the thread, and sent again if it left the mempool.
    // Why: a win is only worth something once its claim is mined, and a
    // thread's state can be claimed once: the first claim mined wins.
    // Look here if: tSAFA claims stay "proven", or are sent twice.
    /// One round of the tSAFA claimer at unix time `now`.
    pub fn safa_tick(&self, chain: &mut dyn Chain, now: u64) -> Result<(), String> {
        let Some(safa) = &self.safa else {
            return Ok(());
        };
        let round = (|| {
            let vaults = chain.vaults(&safa.deployment)?;
            self.set_vaults(vaults.clone());
            let tip = chain.tip_height()?;
            let category = hex::encode(safa.deployment.category);
            let mut counts = ClaimCounts {
                token: safa.token.name,
                vaults: vaults.len(),
                ..ClaimCounts::default()
            };
            self.update_journal(|entries| {
                let mut changed = false;
                for row in entries.iter_mut() {
                    if row["category"].as_str() != Some(category.as_str()) {
                        continue;
                    }
                    changed |= advance_claim(&safa.deployment, &vaults, tip, now, chain, row);
                    match row["status"].as_str() {
                        Some("proven") => counts.waiting += 1,
                        Some("broadcast") => counts.sent += 1,
                        Some("confirmed") => counts.confirmed += 1,
                        Some("stale") => counts.lost += 1,
                        Some("failed") => counts.failed += 1,
                        _ => {}
                    }
                }
                changed
            })?;
            Ok::<_, String>(counts)
        })();
        if let Ok(mut claims) = safa.claims.lock() {
            match &round {
                Ok(counts) => *claims = counts.clone(),
                Err(error) => claims.error = Some(error.clone()),
            }
        }
        round.map(|_| ())
    }

    /// Whether the set changed since the last call, so jobs are re-issued.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// Why token claims are off, if they are.
    pub fn off(&self) -> Option<String> {
        self.off.lock().ok().and_then(|off| off.clone())
    }

    fn turn_off(&self, reason: String) {
        if let Ok(mut off) = self.off.lock() {
            *off = Some(reason);
        }
    }

    /// Whether this win holds a token state not won before; checked on the
    /// device thread, with no disk or network.
    pub fn first(&self, win: &TokenWin) -> bool {
        if self.off().is_some() {
            return false;
        }
        let Ok(mut won) = self.won.lock() else {
            return false;
        };
        let mut fresh = false;
        for state in states(win) {
            if !won.contains(&state) {
                if won.len() >= RECENT_STATES {
                    won.pop_front();
                }
                won.push_back(state);
                fresh = true;
            }
        }
        // #### merge-mining v2: a won tSAFA state is claimed once; the next
        // jobs mine another thread.
        if let (true, Some(safa)) = (fresh, &self.safa) {
            safa.generation.fetch_add(1, Ordering::AcqRel);
            self.changed.store(true, Ordering::Release);
        }
        fresh
    }

    /// The claim worker's part for one win: each won entry's proof, checked
    /// from its bytes as a covenant checks it, then saved, one claim per
    /// row. Returns the entries proven.
    pub fn record(&self, win: &TokenWin) -> Vec<ProvenWin> {
        let mut proven = Vec::new();
        let mut kept = Vec::new();
        let found_at = unix_now();
        for (entry, payout) in claim_payouts(self.network, win) {
            let item = &win.aux.entries[usize::from(entry)];
            let view = ClaimView {
                category: item.token.category,
                lock_hash: item.leaf.lock_hash,
                state_hash: item.leaf.state_hash,
                payout: &payout,
                target_bits: item.leaf.target_bits,
                ext: item.leaf.ext,
                max_aux_height: item.token.max_aux_height,
            };
            let checked = win
                .proof(entry)
                .and_then(|proof| proof.to_bytes())
                .and_then(|bytes| verify(&bytes, &view).map(|_| bytes));
            match checked {
                Ok(bytes) => {
                    let mut row = serde_json::json!({
                        "token": item.token.name,
                        "category": hex::encode(item.token.category),
                        "state": hex::encode(item.leaf.state_hash),
                        "format": "AuxProof v2",
                        // #### merge-mining v2: the split preimage the
                        // claim reveals, when the leaf carries one.
                        "split": (!item.split.is_none())
                            .then(|| hex::encode(item.split.preimage())),
                        "height": win.height,
                        "found_at": found_at,
                        "broadcast_after": found_at + self.delay.pick(),
                        "proof": hex::encode(bytes),
                        "status": "proven",
                    });
                    // #### merge-mining v2: what a tSAFA claim needs: the
                    // vault's state, the payout and the split's scripts.
                    if let Some(safa) = self
                        .safa
                        .as_ref()
                        .filter(|safa| safa.token.category == item.token.category)
                    {
                        let vault = safa.mined.lock().ok().and_then(|mined| {
                            mined
                                .iter()
                                .find(|vault| vault.state_hash() == item.leaf.state_hash)
                                .cloned()
                        });
                        let (donation, fee) = split_scripts(self.network, win, &item.split);
                        row["commitment"] = vault.map(|vault| hex::encode(vault.commitment)).into();
                        row["payout_script"] = hex::encode(&payout).into();
                        row["donation_script"] = donation.map(hex::encode).into();
                        row["fee_script"] = fee.map(hex::encode).into();
                    }
                    kept.push(row);
                    proven.push(ProvenWin {
                        token: item.token.name,
                        height: win.height,
                    });
                }
                // A proof Pickaxe built that fails its own check is a bug:
                // it is never kept as claimable.
                Err(_) => self.turn_off("a token proof failed its own check".into()),
            }
        }
        if !kept.is_empty() {
            if let Err(error) = self.save(kept) {
                self.turn_off(format!("token proofs cannot be saved: {error}"));
                return Vec::new();
            }
        }
        // The test token's state moves to the winning header's hash.
        if self.test.is_some() && !proven.is_empty() {
            let serial = self.current().map_or(1, |set| set.serial() + 1);
            if let Err(error) = self.set_test_state(double_sha256(&win.header), serial) {
                self.turn_off(error);
            }
        }
        proven
    }

    /// Adds proofs to the journal, keeping the newest `KEPT_PROOFS`.
    fn save(&self, kept: Vec<serde_json::Value>) -> Result<(), String> {
        self.update_journal(|entries| {
            entries.extend(kept);
            true
        })
    }

    /// Reads the journal, lets `change` edit its rows, and writes it back
    /// (the newest `KEPT_PROOFS`) when `change` says it changed them.
    fn update_journal(
        &self,
        change: impl FnOnce(&mut Vec<serde_json::Value>) -> bool,
    ) -> Result<(), String> {
        let _writer = self
            .journal
            .lock()
            .map_err(|_| "the token proof journal lock failed")?;
        let mut entries = match std::fs::read(&self.journal_path) {
            Ok(bytes) => {
                let saved: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| "the token proof journal is unreadable")?;
                if saved["version"] != 1 || saved["network"] != self.network.as_str() {
                    return Err("the token proof journal is for another network or version".into());
                }
                saved["entries"].as_array().cloned().unwrap_or_default()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        if !change(&mut entries) {
            return Ok(());
        }
        let skip = entries.len().saturating_sub(KEPT_PROOFS);
        let entries: Vec<_> = entries.into_iter().skip(skip).collect();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "network": self.network.as_str(),
            "entries": entries,
        }))
        .map_err(|error| error.to_string())?;
        crate::config::write_private_atomic(&self.journal_path, &bytes)
    }
}

/// The satoshis a vault needs before its thread is mined: a claim's fee
/// at 1 sat/byte with a large proof (4,000 bytes), its three share outputs
/// and the vault's dust.
fn claim_reserve(deployment: &Deployment) -> u64 {
    deployment.fee_k + 4_000 + 3 * safa_auxpow::OUTPUT_SATS + 800
}

/// The split's donation and fee scripts among the scripts the win's job
/// paid: its miner, the Pickaxe donation and an operator (or a declared
/// plan's outputs).
fn split_scripts(
    network: MiningNetwork,
    win: &TokenWin,
    split: &Split,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut scripts = match win.declared.as_deref() {
        Some(plan) => plan.scripts.clone(),
        None => payout::scripts(network, &win.miner, win.operator.as_deref()).unwrap_or_default(),
    };
    if let Ok(donation) = payout::scripts(network, &win.miner, None) {
        scripts.extend(donation);
    }
    let find = |slot: &super::split::Slot| {
        (!slot.is_empty())
            .then(|| {
                scripts
                    .iter()
                    .find(|script| double_sha256(script) == *slot.script_hash())
                    .cloned()
            })
            .flatten()
    };
    (find(split.donation_slot()), find(split.fee_slot()))
}

/// Retries a passing failure later, at most 10 times.
const CLAIM_ATTEMPTS: u64 = 10;

fn retry(row: &mut serde_json::Value, now: u64, error: &str) {
    let attempts = row["attempts"].as_u64().unwrap_or(0) + 1;
    row["attempts"] = attempts.into();
    row["error"] = error.into();
    if attempts >= CLAIM_ATTEMPTS {
        row["status"] = "failed".into();
    } else {
        row["status"] = "proven".into();
        row["next_attempt"] = (now + (30u64 << attempts.min(7)).min(3_600)).into();
    }
}

/// One journal row's next step (see `TokenHub::safa_tick`); whether it
/// changed.
fn advance_claim(
    deployment: &Deployment,
    vaults: &[Vault],
    tip: u32,
    now: u64,
    chain: &mut dyn Chain,
    row: &mut serde_json::Value,
) -> bool {
    let thread = row["commitment"]
        .as_str()
        .and_then(|text| hex::decode(text).ok())
        .and_then(|bytes| safa_auxpow::ThreadState::parse(&bytes));
    let vault = thread.and_then(|state| vaults.iter().find(|vault| vault.thread() == state.thread));
    let state = row["state"].as_str().unwrap_or_default().to_owned();
    match row["status"].as_str() {
        Some("proven") => {
            let due = row["broadcast_after"]
                .as_u64()
                .unwrap_or(0)
                .max(row["next_attempt"].as_u64().unwrap_or(0));
            let height = row["height"].as_u64().unwrap_or(u64::MAX);
            if now < due || u64::from(tip) < height {
                return false;
            }
            if thread.is_none() {
                row["status"] = "failed".into();
                row["error"] = "the win's thread is unknown".into();
                return true;
            }
            let Some(vault) = vault else {
                retry(row, now, "Fulcrum does not list the thread's vault");
                return true;
            };
            if hex::encode(vault.state_hash()) != state {
                row["status"] = "stale".into();
                row["error"] = "another claim moved the thread first".into();
                return true;
            }
            let script = |key: &str| row[key].as_str().and_then(|text| hex::decode(text).ok());
            let split = match row["split"].as_str() {
                Some(text) => match hex::decode(text)
                    .ok()
                    .and_then(|bytes| Split::parse(&bytes).ok())
                {
                    Some(split) => split,
                    None => {
                        row["status"] = "failed".into();
                        row["error"] = "the win's split preimage is unreadable".into();
                        return true;
                    }
                },
                None => Split::NONE,
            };
            let terms = ClaimTerms {
                payout: script("payout_script").unwrap_or_default(),
                split,
                donation: script("donation_script"),
                fee: script("fee_script"),
            };
            let proof = script("proof").unwrap_or_default();
            let claim = match build_claim(deployment, vault, &proof, &terms, 1, None) {
                Ok(claim) => claim,
                Err(error) => {
                    row["status"] = "failed".into();
                    row["error"] = error.into();
                    return true;
                }
            };
            match chain.broadcast(&claim.raw) {
                Ok(_) => {
                    row["status"] = "broadcast".into();
                    row["txid"] = claim.txid.into();
                    row["claimed_at"] = now.into();
                    row["fee_sats"] = claim.plan.fee.into();
                    row["error"] = serde_json::Value::Null;
                }
                Err(error) => retry(row, now, &format!("the claim was not accepted: {error}")),
            }
            true
        }
        Some("broadcast") => {
            let txid = row["txid"].as_str().unwrap_or_default().to_owned();
            let Some(vault) = vault else {
                return false;
            };
            let mut outpoint = vault.outpoint.txid;
            outpoint.reverse();
            if hex::encode(outpoint) == txid {
                if vault.confirmed {
                    row["status"] = "confirmed".into();
                    return true;
                }
                return false;
            }
            if hex::encode(vault.state_hash()) == state {
                // The claim left the mempool; the vault still waits for it.
                retry(row, now, "the claim left the mempool; it is sent again");
                if row["status"] == "proven" {
                    row["next_attempt"] = now.into();
                }
                return true;
            }
            match chain.known(&txid) {
                Ok(true) => row["status"] = "confirmed".into(),
                Ok(false) => {
                    row["status"] = "stale".into();
                    row["error"] = "another claim moved the thread first".into();
                }
                Err(_) => return false,
            }
            true
        }
        _ => false,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The token states a win holds: (category, state hash) per won entry.
fn states(win: &TokenWin) -> Vec<(Hash, Hash)> {
    win.entries
        .iter()
        .filter_map(|entry| win.aux.entries.get(usize::from(*entry)))
        .map(|item| (item.token.category, item.leaf.state_hash))
        .collect()
}

// #### merge-mining v2: each entry's own payout
// What: each won entry is claimed with the payout its own leaf binds, as
// `BchTemplate::aux_job` chose it (`payout::leaf_terms`): a split token's
// or a token without split support's, which differ in a job that pays the
// latter's token work; under Job Declaration both are the miner's own
// script.
// Why: one payout for the whole win failed the proof check of the other
// kind of token in a mixed set and turned token claims off.
// Look here if: a mixed set's win fails its own proof check.
/// Each won entry with the payout locking bytecode its claim would pay.
/// The verifier hashes it again, so a job whose leaf bound anything else
/// fails its own check.
fn claim_payouts(network: MiningNetwork, win: &TokenWin) -> Vec<(u16, Vec<u8>)> {
    let (split, plain) = match win.declared.as_deref() {
        Some(plan) => match plan.token_payout() {
            Some(miner) => (miner.to_vec(), miner.to_vec()),
            None => return Vec::new(),
        },
        // #### merge-mining v2: on a public pool, the miner's own leaves
        // (`payout::leaf_terms` with the operator).
        None => {
            let Ok(scripts) = payout::scripts(network, &win.miner, win.operator.as_deref()) else {
                return Vec::new();
            };
            let Ok(terms) = payout::leaf_terms(&scripts, win.payout) else {
                return Vec::new();
            };
            (terms.payout.to_vec(), terms.plain_payout.to_vec())
        }
    };
    win.entries
        .iter()
        .filter_map(|entry| {
            let item = win.aux.entries.get(usize::from(*entry))?;
            let payout = if item.token.split { &split } else { &plain };
            Some((*entry, payout.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### merge-mining v2
    // What: in a mixed set (a split token and one without split support)
    // each won entry is claimed with the payout its own leaf binds: in a
    // miner job, in the plain token's donation work and in BCH donation
    // work.
    // Look here if: claim_payouts or BchTemplate::aux_job changes.
    #[test]
    fn a_mixed_set_claims_each_entry_with_its_own_leaf_payout() {
        use crate::donation::bch::BchPayout;
        use crate::stratum_v2::{
            merge::{
                registry::{MergeToken, TEST_TOKEN},
                set::{tests::test_state, SetEntry, TokenSet},
            },
            template::BchTemplate,
            template_tests::{payout, rpc_template},
        };
        use std::sync::Arc;
        let plain = Box::leak(Box::new(MergeToken {
            category: [8; 32],
            split: false,
            ..TEST_TOKEN
        }));
        let set = TokenSet::new(
            MiningNetwork::Chipnet,
            vec![
                SetEntry::new(&TEST_TOKEN, test_state()),
                SetEntry::new(plain, test_state()),
            ],
            1,
        )
        .unwrap();
        let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        template.commit(Arc::new(set));
        let policies = [
            BchPayout::default(),
            BchPayout {
                token_donation_work: Some(true),
                ..BchPayout::default()
            },
            BchPayout {
                donation_work: true,
                token_donation_work: Some(true),
                ..BchPayout::default()
            },
        ];
        for (case, policy) in policies.into_iter().enumerate() {
            let aux = Arc::new(
                template
                    .aux_job(MiningNetwork::Chipnet, &payout(), None, policy)
                    .unwrap()
                    .unwrap(),
            );
            let win = TokenWin {
                aux: aux.clone(),
                entries: vec![0, 1],
                coinbase: Vec::new(),
                merkle_path: Vec::new(),
                header: [0; 80],
                hash: [0; 32],
                height: 0,
                payout: policy,
                miner: payout(),
                operator: None,
                declared: None,
            };
            let claims = claim_payouts(MiningNetwork::Chipnet, &win);
            assert_eq!(claims.len(), 2, "case {case}");
            for (entry, script) in claims {
                assert_eq!(
                    double_sha256(&script),
                    aux.entries[usize::from(entry)].leaf.payout_hash,
                    "case {case}, entry {entry}"
                );
            }
        }
    }

    // #### PR #42
    // What: the test token's difficulty becomes a compact target that
    // expands back; the test token is refused off Chipnet; a new hub carries
    // a one-token set and reports it changed once.
    // Look here if: bits_for_difficulty or TokenHub::test_token changes.
    #[test]
    fn the_test_token_is_chipnet_only_and_its_difficulty_round_trips() {
        for difficulty in [1u64, 2, 4096, 65_536, 1 << 48] {
            let bits = bits_for_difficulty(difficulty).unwrap();
            let target = super::super::verify::expand_compact(bits)
                .unwrap_or_else(|| panic!("{difficulty}: {bits:08x}"));
            let expected = (num_bigint::BigUint::from(0xffffu32) << 208u32) / difficulty;
            let got = num_bigint::BigUint::from_bytes_le(&target);
            // Compact form keeps the top 16 to 23 bits.
            assert!(got <= expected, "{difficulty}");
            assert!(&expected - &got <= &expected >> 15u32, "{difficulty}");
            // #### PR #42: and the difficulty shown comes back from it.
            let shown = difficulty_for_bits(bits);
            let wanted = difficulty as f64;
            assert!(
                (shown - wanted).abs() <= wanted / 30_000.0,
                "{difficulty}: {shown}"
            );
        }
        assert_eq!(difficulty_for_bits(0x1d00_ffff), 1.0);
        assert_eq!(difficulty_for_bits(0x2000_0000), 0.0);
        assert!(bits_for_difficulty(0).is_err());
        let path = std::env::temp_dir().join("pickaxe-hub-unused.json");
        assert!(TokenHub::test_token(
            MiningNetwork::Mainnet,
            path.clone(),
            0x1d00ffff,
            ClaimDelay::default()
        )
        .unwrap_err()
        .contains("only on Chipnet"));
        let hub = TokenHub::test_token(
            MiningNetwork::Chipnet,
            path,
            0x1d00ffff,
            ClaimDelay::default(),
        )
        .unwrap();
        let set = hub.current().unwrap();
        assert_eq!(set.entries().len(), 1);
        let state = set.entries()[0].state;
        assert_eq!(
            state.lock_hash,
            super::super::sha256(b"pickaxe merge-mining test token baton")
        );
        assert_eq!(
            state.state_hash,
            super::super::sha256(b"pickaxe merge-mining test token state")
        );
        assert!(hub.take_changed());
        assert!(!hub.take_changed());
        assert!(hub.off().is_none());
    }

    /// The chain as the tSAFA claimer sees it, with no network.
    struct FakeChain {
        tip: u32,
        vaults: Vec<Vault>,
        sent: Vec<Vec<u8>>,
        refuse: Option<String>,
        known: bool,
    }

    impl Chain for FakeChain {
        fn tip_height(&mut self) -> Result<u32, String> {
            Ok(self.tip)
        }

        fn vaults(&mut self, _: &Deployment) -> Result<Vec<Vault>, String> {
            Ok(self.vaults.clone())
        }

        fn broadcast(&mut self, raw: &[u8]) -> Result<String, String> {
            if let Some(error) = &self.refuse {
                return Err(error.clone());
            }
            self.sent.push(raw.to_vec());
            Ok(String::new())
        }

        fn known(&mut self, _: &str) -> Result<bool, String> {
            Ok(self.known)
        }
    }

    fn safa_vault(thread: u32, height: u32, target_bits: u32, value: u64) -> Vault {
        use super::super::safa_auxpow::{tests::chipnet_block, ThreadState};
        Vault {
            outpoint: super::super::OutPoint {
                txid: double_sha256(&[0x77, thread as u8]),
                vout: 0,
            },
            value,
            tokens: 420_000_000_000,
            commitment: ThreadState {
                thread,
                height,
                target_bits,
                ref_bits: chipnet_block().2,
            }
            .encode(),
            confirmed: true,
        }
    }

    fn safa_hub(name: &str) -> (TokenHub, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("pickaxe-safa-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let deployment = super::super::safa_auxpow::tests::deployment(42_000_000_000);
        let hub = TokenHub::safa(
            MiningNetwork::Chipnet,
            path.clone(),
            deployment,
            ClaimDelay { min: 0, max: 0 },
            Vec::new(),
        )
        .unwrap();
        (hub, path)
    }

    // #### merge-mining v2: tSAFA's threads
    // What: jobs at a height mine the easiest thread whose state is older,
    // whose vault pays a claim and whose state was not won here; a win
    // moves the next jobs to another thread; the hub is Chipnet only.
    // Look here if: `TokenHub::current_for` changes.
    #[test]
    fn tsafa_jobs_mine_the_easiest_open_thread() {
        let (height, _, _) = super::super::safa_auxpow::tests::chipnet_block();
        let (hub, path) = safa_hub("threads");
        assert!(hub.current_for(height).is_none(), "no vaults read yet");
        hub.set_vaults(vec![
            safa_vault(0, height - 1, 0x1d00_ffff, 2_000_000),
            safa_vault(1, height - 10, 0x2000_ffff, 2_000_000),
            safa_vault(2, height, 0x207f_ffff, 2_000_000),
            safa_vault(3, height - 10, 0x207f_ffff, 1_000),
        ]);
        assert!(hub.take_changed());
        let set = hub.current_for(height).unwrap();
        let entry = set.entries()[0];
        assert_eq!(
            entry.state.state_hash,
            safa_vault(1, height - 10, 0x2000_ffff, 2_000_000).state_hash()
        );
        assert_eq!(entry.state.target_bits, 0x2001_bffe);
        assert!(
            Arc::ptr_eq(&set, &hub.current_for(height).unwrap()),
            "built once per height"
        );
        // A win of thread 1's state moves jobs to thread 0.
        if let Ok(mut won) = hub.won.lock() {
            won.push_back((entry.token.category, entry.state.state_hash));
        }
        hub.safa
            .as_ref()
            .unwrap()
            .generation
            .fetch_add(1, Ordering::AcqRel);
        let set = hub.current_for(height).unwrap();
        assert_eq!(
            set.entries()[0].state.state_hash,
            safa_vault(0, height - 1, 0x1d00_ffff, 2_000_000).state_hash()
        );
        let _ = std::fs::remove_file(path);
        assert!(TokenHub::safa(
            MiningNetwork::Mainnet,
            PathBuf::from("unused.json"),
            super::super::safa_auxpow::tests::deployment(42_000_000_000),
            ClaimDelay::default(),
            Vec::new(),
        )
        .is_err());
    }

    /// A share of `hub`'s job at `height` that wins its tSAFA leaf, paid
    /// as a solo miner's default job pays.
    fn safa_win(hub: &TokenHub, height: u32) -> TokenWin {
        use super::super::{
            safa_auxpow::tests::{chipnet_block, coinbase},
            set::AuxJob,
        };
        use crate::{donation::bch::BchPayout, stratum_v2::template_tests::payout};
        let set = hub.current_for(height).unwrap();
        let scripts = payout::scripts(MiningNetwork::Chipnet, &payout(), None).unwrap();
        let terms = payout::leaf_terms(&scripts, BchPayout::default()).unwrap();
        let aux = Arc::new(AuxJob::build(&set, &terms));
        let (head, tail) = coinbase(height, &[(312_500_000, scripts[0].clone())]);
        let coinbase = [&head[..], &aux.output, &tail].concat();
        let (_, time, bits) = chipnet_block();
        let merkle = double_sha256(&coinbase);
        let target =
            super::super::verify::expand_compact(set.entries()[0].state.target_bits).unwrap();
        let mut header = [0u8; 80];
        header[..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        header[36..68].copy_from_slice(&merkle);
        header[68..72].copy_from_slice(&time.to_le_bytes());
        header[72..76].copy_from_slice(&bits.to_le_bytes());
        for nonce in 0u32.. {
            header[76..].copy_from_slice(&nonce.to_le_bytes());
            if crate::stratum_v2::template::meets_target(&double_sha256(&header), &target) {
                break;
            }
        }
        TokenWin {
            aux,
            entries: vec![0],
            coinbase,
            merkle_path: Vec::new(),
            header,
            hash: double_sha256(&header),
            height,
            payout: BchPayout::default(),
            miner: payout(),
            operator: None,
            declared: None,
        }
    }

    fn journal_rows(path: &PathBuf) -> Vec<serde_json::Value> {
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        saved["entries"].as_array().cloned().unwrap()
    }

    // #### merge-mining v2: a tSAFA win, from share to claim
    // What: a win of the hub's tSAFA job is proven and journaled with its
    // vault state, payout and split scripts; once its delay has passed and
    // the chain reaches its coinbase height, the claimer builds the claim
    // (which the covenant check accepts), broadcasts it, and marks it
    // confirmed when its vault output is in a block. The next jobs mine the
    // thread's new state.
    // Look here if: record, safa_tick or advance_claim change.
    #[test]
    fn a_tsafa_win_is_claimed_and_confirmed() {
        use super::super::safa_auxpow::{check_claim, tests::chipnet_block};
        let (height, _, _) = chipnet_block();
        let (hub, path) = safa_hub("claim");
        let vault = safa_vault(1, height - 10, 0x2000_ffff, 2_000_000);
        let mut chain = FakeChain {
            tip: height - 1,
            vaults: vec![vault.clone()],
            sent: Vec::new(),
            refuse: None,
            known: false,
        };
        hub.safa_tick(&mut chain, 0).unwrap();
        let win = safa_win(&hub, height);
        assert!(hub.first(&win));
        assert!(!hub.first(&win), "a state is won once");
        assert_eq!(hub.record(&win).len(), 1);
        assert!(hub.off().is_none(), "{:?}", hub.off());
        let row = &journal_rows(&path)[0];
        assert_eq!(row["commitment"], hex::encode(vault.commitment));
        assert!(row["payout_script"].as_str().is_some());
        assert!(
            row["donation_script"].as_str().is_some(),
            "the default 1% donation slot"
        );
        assert!(row["fee_script"].is_null());
        // The chain is one block short of the coinbase height: no claim yet
        // (its lock time would not be final).
        let now = unix_now() + 1;
        hub.safa_tick(&mut chain, now).unwrap();
        assert!(chain.sent.is_empty());
        assert_eq!(hub.claim_counts().unwrap().waiting, 1);
        chain.tip = height;
        hub.safa_tick(&mut chain, now).unwrap();
        assert_eq!(chain.sent.len(), 1);
        let deployment = super::super::safa_auxpow::tests::deployment(42_000_000_000);
        assert_eq!(check_claim(&deployment, &vault, &chain.sent[0]), Ok(()));
        let row = &journal_rows(&path)[0];
        assert_eq!(row["status"], "broadcast");
        let txid = row["txid"].as_str().unwrap().to_owned();
        assert_eq!(hub.claim_counts().unwrap().sent, 1);
        // Mined: the vault's output is the claim's output 0, confirmed.
        let mut internal: Hash = hex::decode(&txid).unwrap().try_into().unwrap();
        internal.reverse();
        let next = Vault {
            outpoint: super::super::OutPoint {
                txid: internal,
                vout: 0,
            },
            ..vault.clone()
        };
        chain.vaults = vec![next];
        hub.safa_tick(&mut chain, now).unwrap();
        assert_eq!(journal_rows(&path)[0]["status"], "confirmed");
        let counts = hub.claim_counts().unwrap();
        assert_eq!((counts.confirmed, counts.sent, counts.vaults), (1, 0, 1));
        assert_eq!(chain.sent.len(), 1, "sent once");
        let _ = std::fs::remove_file(path);
    }

    // #### merge-mining v2: tSAFA claims that cannot go
    // What: a win whose thread another claim moved first is "stale"; a
    // broadcast the server refuses is retried later, not at once, and a
    // sent claim whose vault moved to another claim's state is "stale".
    #[test]
    fn tsafa_claims_lose_races_and_retry_refusals() {
        use super::super::safa_auxpow::tests::chipnet_block;
        let (height, _, _) = chipnet_block();
        for case in ["stale", "retry", "lost"] {
            let (hub, path) = safa_hub(case);
            let vault = safa_vault(1, height - 10, 0x2000_ffff, 2_000_000);
            let mut chain = FakeChain {
                tip: height,
                vaults: vec![vault.clone()],
                sent: Vec::new(),
                refuse: None,
                known: false,
            };
            hub.safa_tick(&mut chain, 0).unwrap();
            let win = safa_win(&hub, height);
            assert!(hub.first(&win));
            assert_eq!(hub.record(&win).len(), 1);
            let now = unix_now() + 1;
            let moved = safa_vault(1, height - 2, 0x2000_ffff, 1_990_000);
            match case {
                "stale" => {
                    chain.vaults = vec![moved];
                    hub.safa_tick(&mut chain, now).unwrap();
                    assert_eq!(journal_rows(&path)[0]["status"], "stale");
                    assert!(chain.sent.is_empty());
                    assert_eq!(hub.claim_counts().unwrap().lost, 1);
                }
                "retry" => {
                    chain.refuse = Some("txn-mempool-conflict".into());
                    hub.safa_tick(&mut chain, now).unwrap();
                    let row = &journal_rows(&path)[0];
                    assert_eq!(row["status"], "proven");
                    assert_eq!(row["attempts"], 1);
                    let next = row["next_attempt"].as_u64().unwrap();
                    assert!(next > now);
                    chain.refuse = None;
                    hub.safa_tick(&mut chain, now).unwrap();
                    assert!(chain.sent.is_empty(), "not before its next attempt");
                    hub.safa_tick(&mut chain, next).unwrap();
                    assert_eq!(chain.sent.len(), 1);
                    assert_eq!(journal_rows(&path)[0]["status"], "broadcast");
                }
                _ => {
                    hub.safa_tick(&mut chain, now).unwrap();
                    assert_eq!(chain.sent.len(), 1);
                    chain.vaults = vec![moved];
                    hub.safa_tick(&mut chain, now).unwrap();
                    assert_eq!(journal_rows(&path)[0]["status"], "stale");
                }
            }
            let _ = std::fs::remove_file(path);
        }
    }

    // #### merge-mining v2: the claim broadcast delay
    #[test]
    fn claim_delays_parse_and_stay_in_range() {
        assert_eq!(ClaimDelay::default(), ClaimDelay { min: 5, max: 60 });
        assert_eq!(ClaimDelay::parse("5-60"), Ok(ClaimDelay::default()));
        assert_eq!(ClaimDelay::parse("0"), Ok(ClaimDelay { min: 0, max: 0 }));
        assert_eq!(
            ClaimDelay::parse(" 10 - 20 "),
            Ok(ClaimDelay { min: 10, max: 20 })
        );
        for bad in ["", "-", "60-5", "a-b", "-5", "86401", "1-86401"] {
            assert!(ClaimDelay::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(ClaimDelay { min: 0, max: 0 }.pick(), 0);
        let delay = ClaimDelay::default();
        for _ in 0..1_000 {
            assert!((5..=60).contains(&delay.pick()));
        }
    }
}
