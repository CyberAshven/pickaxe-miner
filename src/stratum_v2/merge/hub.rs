//! #### PR #42
//! The server's merge-mining hub: the token set jobs carry, the claim
//! worker's proofs and their journal.
//!
//! A winning share reaches the hub through a bounded queue the device thread
//! never waits on. The claim worker builds each won entry's proof, checks it
//! with the reference verifier exactly as a covenant would (a proof that
//! fails is a Pickaxe bug and is never kept as claimable), and saves it to
//! `<config>.sv2-token-proofs.json`, readable by the owner alone since it
//! names payout scripts. No token has a covenant or claim builder yet, so a
//! proof stops at "proven". A journal that cannot be written turns token
//! claims off and leaves BCH mining as it is.
//!
//! No token is registered on either network. The Chipnet test token
//! (`stratum-v2 serve --merge-test-token`) has a simulated baton: its anchor
//! moves to the winning header after each proven Case A win, which changes
//! the set and makes the server re-issue jobs on the same parent.

use super::{
    leaf::Mode,
    registry::{MergeToken, TEST_TOKEN},
    set::{SetEntry, TokenSet, TokenState},
    verify::{verify_a, verify_b, ClaimView},
    Hash, OutPoint,
};
use crate::config::MiningNetwork;
use crate::stratum_v2::{channel::TokenWin, payout, template::double_sha256};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// Proofs the journal keeps, the oldest dropped first.
const KEPT_PROOFS: usize = 256;
/// Token states recently won, so each is proven once.
const RECENT_STATES: usize = 256;

/// One won entry, as the dashboard lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenWin {
    pub token: &'static str,
    pub mode: Mode,
    pub height: u32,
}

/// The test token's simulated baton: where its next Case A state is.
struct TestBaton {
    target_bits: u32,
}

pub struct TokenHub {
    network: MiningNetwork,
    journal_path: PathBuf,
    set: RwLock<Option<Arc<TokenSet>>>,
    changed: AtomicBool,
    test: Option<TestBaton>,
    /// (category, mode, anchor) of states already won.
    won: Mutex<VecDeque<(Hash, u8, OutPoint)>>,
    /// Why token claims are off, after a journal failure.
    off: Mutex<Option<String>>,
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
    // What: a hub that merge-mines the test token in both modes, its Case A
    // state anchored to a simulated baton at `target_bits`.
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
    ) -> Result<Self, String> {
        if TEST_TOKEN.network != network {
            return Err("the merge-mining test token exists only on Chipnet".into());
        }
        let hub = Self {
            network,
            journal_path,
            set: RwLock::new(None),
            changed: AtomicBool::new(false),
            test: Some(TestBaton { target_bits }),
            won: Mutex::new(VecDeque::new()),
            off: Mutex::new(None),
        };
        let anchor = OutPoint {
            txid: super::sha256(b"pickaxe merge-mining test token anchor"),
            vout: 0,
        };
        hub.set_test_state(anchor, 1)?;
        Ok(hub)
    }

    fn set_test_state(&self, anchor: OutPoint, serial: u64) -> Result<(), String> {
        let Some(test) = &self.test else {
            return Ok(());
        };
        let token: &'static MergeToken = &TEST_TOKEN;
        let entries = vec![
            SetEntry::share_target(token, TokenState::new(anchor, test.target_bits)?),
            SetEntry::block_required(token),
        ];
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

    /// Whether the set changed since the last call, so jobs are re-issued.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// Why token claims are off, if they are.
    pub fn off(&self) -> Option<String> {
        self.off.lock().ok().and_then(|off| off.clone())
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
        fresh
    }

    /// The claim worker's part for one win: each won entry's proof, checked
    /// as a covenant checks it, then saved. Returns the entries proven.
    pub fn record(&self, win: &TokenWin) -> Vec<ProvenWin> {
        let mut proven = Vec::new();
        let mut kept = Vec::new();
        for (entry, view) in claim_views(self.network, win) {
            let item = &win.aux.entries[usize::from(entry)];
            let checked = win.proof(entry).and_then(|proof| {
                match item.mode {
                    Mode::ShareTarget => verify_a(&proof, &view.view()),
                    Mode::BlockRequired => verify_b(&proof, &view.view()),
                }?;
                proof.to_bytes()
            });
            match checked {
                Ok(bytes) => {
                    kept.push(serde_json::json!({
                        "token": item.token.name,
                        "category": hex::encode(item.token.category),
                        "mode": if item.mode == Mode::ShareTarget { "A" } else { "B" },
                        "anchor": format!("{}:{}", hex::encode(view.anchor.txid), view.anchor.vout),
                        "height": win.height,
                        "found_at": unix_now(),
                        "proof": hex::encode(bytes),
                        "status": "proven",
                    }));
                    proven.push(ProvenWin {
                        token: item.token.name,
                        mode: item.mode,
                        height: win.height,
                    });
                }
                // A proof Pickaxe built that fails its own check is a bug:
                // it is never kept as claimable.
                Err(_) => {
                    if let Ok(mut off) = self.off.lock() {
                        *off = Some("a token proof failed its own check".into());
                    }
                }
            }
        }
        if !kept.is_empty() {
            if let Err(error) = self.save(kept) {
                if let Ok(mut off) = self.off.lock() {
                    *off = Some(format!("token proofs cannot be saved: {error}"));
                }
                return Vec::new();
            }
        }
        // The test token's baton moves to the winning header.
        if self.test.is_some() && proven.iter().any(|win| win.mode == Mode::ShareTarget) {
            let serial = self.current().map_or(1, |set| set.serial() + 1);
            let anchor = OutPoint {
                txid: double_sha256(&win.header),
                vout: 0,
            };
            if let Err(error) = self.set_test_state(anchor, serial) {
                if let Ok(mut off) = self.off.lock() {
                    *off = Some(error);
                }
            }
        }
        proven
    }

    /// Adds proofs to the journal, keeping the newest `KEPT_PROOFS`.
    fn save(&self, kept: Vec<serde_json::Value>) -> Result<(), String> {
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
        entries.extend(kept);
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The token states a win holds: Case A's baton, Case B's ticket.
fn states(win: &TokenWin) -> Vec<(Hash, u8, OutPoint)> {
    let txid = double_sha256(&win.coinbase);
    win.entries
        .iter()
        .filter_map(|entry| win.aux.entries.get(usize::from(*entry)))
        .map(|item| {
            let anchor = match (item.mode, item.ticket_vout) {
                (Mode::BlockRequired, Some(vout)) => OutPoint { txid, vout },
                _ => OutPoint {
                    txid: item.leaf.anchor_hash,
                    vout: item.leaf.anchor_index,
                },
            };
            (item.token.category, item.mode.byte(), anchor)
        })
        .collect()
}

/// What a claim of one won entry would show its covenant.
struct View {
    category: Hash,
    anchor: OutPoint,
    ticket: Option<OutPoint>,
    payout: Vec<u8>,
    split: Option<(u16, Vec<u8>)>,
    target_bits: u32,
    max_aux_height: u8,
    start_height: u32,
}

impl View {
    fn view(&self) -> ClaimView<'_> {
        ClaimView {
            category: self.category,
            anchor: self.anchor,
            ticket: self.ticket,
            payout: &self.payout,
            split: self
                .split
                .as_ref()
                .map(|(bps, script)| (*bps, script.as_slice())),
            target_bits: self.target_bits,
            ext: [0; 32],
            max_aux_height: self.max_aux_height,
            start_height: self.start_height,
        }
    }
}

/// Each won entry with the claim view its proof is checked against: the
/// job's beneficiary and split, the Case A baton or the Case B ticket.
fn claim_views(network: MiningNetwork, win: &TokenWin) -> Vec<(u16, View)> {
    // #### PR #42: a Job Declaration job's leaves bind its plan's terms: the
    // miner's own script and the pool's whole donation rate.
    let (beneficiary, split) = match win.declared.as_deref() {
        Some(plan) => {
            let Some((miner, split)) = plan.token_terms() else {
                return Vec::new();
            };
            (
                miner.to_vec(),
                split.map(|(bps, script)| (bps, script.to_vec())),
            )
        }
        None => {
            let Ok(scripts) = payout::scripts(network, &win.miner, win.operator.as_deref()) else {
                return Vec::new();
            };
            (
                payout::beneficiary(&scripts, win.payout).to_vec(),
                payout::token_split(win.payout, &scripts[1])
                    .map(|(bps, script)| (bps, script.to_vec())),
            )
        }
    };
    let txid = double_sha256(&win.coinbase);
    win.entries
        .iter()
        .filter_map(|entry| {
            let item = win.aux.entries.get(usize::from(*entry))?;
            let baton = OutPoint {
                txid: item.leaf.anchor_hash,
                vout: item.leaf.anchor_index,
            };
            let (anchor, ticket) = match (item.mode, item.ticket_vout) {
                (Mode::BlockRequired, Some(vout)) => (baton, Some(OutPoint { txid, vout })),
                (Mode::BlockRequired, None) => return None,
                (Mode::ShareTarget, _) => (baton, None),
            };
            Some((
                *entry,
                View {
                    category: item.token.category,
                    anchor,
                    ticket,
                    payout: beneficiary.clone(),
                    split: split.clone(),
                    target_bits: item.leaf.target_bits,
                    max_aux_height: item.token.max_aux_height,
                    start_height: item.token.start_height,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
    // What: the test token's difficulty becomes a compact target that
    // expands back; the test token is refused off Chipnet; a new hub carries
    // a set with both modes and reports it changed once.
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
        assert!(
            TokenHub::test_token(MiningNetwork::Mainnet, path.clone(), 0x1d00ffff)
                .unwrap_err()
                .contains("only on Chipnet")
        );
        let hub = TokenHub::test_token(MiningNetwork::Chipnet, path, 0x1d00ffff).unwrap();
        let set = hub.current().unwrap();
        assert_eq!(set.entries().len(), 2);
        assert!(hub.take_changed());
        assert!(!hub.take_changed());
        assert!(hub.off().is_none());
    }
}
