//! #### PR #42
//! A token set (what the server merge-mines now) and an `AuxJob` (one job's
//! leaves, tree, commitment and tickets).
//!
//! The layout search runs once per token set: slots depend only on
//! categories and modes. Each job then builds its own leaves, because every
//! leaf binds that job's payout (the miner's script in miner jobs, the
//! donation's in donation-work jobs, the operator's in fee-work jobs) and
//! the donation's split.

use super::{
    super::template::meets_target,
    commitment::{AuxCommitment, OUTPUT_LEN},
    leaf::{Leaf, Mode, MAX_SPLIT_BPS},
    registry::{MergeToken, MAX_TICKETS, MAX_TOKENS, TICKET_LEN},
    tree::{AuxTree, Layout, Placement, MAX_HEIGHT},
    verify::expand_compact,
    Hash, OutPoint,
};
use crate::config::MiningNetwork;

/// A Case A token's live state: its baton's outpoint and current target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenState {
    pub anchor: OutPoint,
    pub target_bits: u32,
    /// `target_bits` expanded, little-endian, as share hashes compare.
    pub target: Hash,
}

impl TokenState {
    pub fn new(anchor: OutPoint, target_bits: u32) -> Result<Self, String> {
        Ok(Self {
            anchor,
            target_bits,
            target: expand_compact(target_bits).ok_or("invalid token target")?,
        })
    }
}

/// One (token, mode) to merge-mine. Case A needs the token's state; Case B
/// has none (its anchor is its ticket).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetEntry {
    pub token: &'static MergeToken,
    pub mode: Mode,
    pub state: Option<TokenState>,
}

impl SetEntry {
    pub fn share_target(token: &'static MergeToken, state: TokenState) -> Self {
        Self {
            token,
            mode: Mode::ShareTarget,
            state: Some(state),
        }
    }

    pub fn block_required(token: &'static MergeToken) -> Self {
        Self {
            token,
            mode: Mode::BlockRequired,
            state: None,
        }
    }
}

/// What the server merge-mines: the entries that fit, their layout, the
/// easiest Case A target and a serial that grows with every change.
#[derive(Clone, Debug)]
pub struct TokenSet {
    entries: Vec<SetEntry>,
    layout: Layout,
    easiest_a: Option<Hash>,
    serial: u64,
    dropped: Vec<SetEntry>,
}

/// Whether target `a` is easier (a larger number) than `b`.
fn easier(a: &Hash, b: &Hash) -> bool {
    a.iter().rev().cmp(b.iter().rev()).is_gt()
}

impl TokenSet {
    /// Checks every entry against the network and its registry row, then
    /// searches the layout once. If the whole set does not fit, entries join
    /// highest priority first (on a tie, the earlier one), and each entry
    /// that does not fit beside those already kept is left out.
    pub fn new(
        network: MiningNetwork,
        entries: Vec<SetEntry>,
        serial: u64,
    ) -> Result<Self, String> {
        if entries.is_empty() {
            return Err("no token to merge-mine".into());
        }
        for (index, entry) in entries.iter().enumerate() {
            let token = entry.token;
            token.verify()?;
            if token.network != network {
                return Err(format!(
                    "{} is a {} token, not a {} one",
                    token.name,
                    token.network.as_str(),
                    network.as_str()
                ));
            }
            if !token.supports(entry.mode) {
                return Err(format!("{} does not offer that mode", token.name));
            }
            if (entry.mode == Mode::ShareTarget) != entry.state.is_some() {
                return Err(format!(
                    "{}: Case A needs the token's state and Case B takes none",
                    token.name
                ));
            }
            if entries[..index]
                .iter()
                .any(|other| other.token.category == token.category && other.mode == entry.mode)
            {
                return Err(format!("{} is listed twice in one mode", token.name));
            }
        }
        let mut categories: Vec<_> = entries.iter().map(|e| e.token.category).collect();
        categories.sort_unstable();
        categories.dedup();
        let tickets = entries
            .iter()
            .filter(|e| e.mode == Mode::BlockRequired)
            .count();
        if categories.len() > MAX_TOKENS || tickets > MAX_TICKETS {
            return Err(format!(
                "at most {MAX_TOKENS} tokens and {MAX_TICKETS} tickets per coinbase"
            ));
        }
        let search = |kept: &[SetEntry]| {
            let placements: Vec<_> = kept
                .iter()
                .map(|e| Placement {
                    category: e.token.category,
                    mode: e.mode,
                    priority: e.token.priority,
                })
                .collect();
            let max_height = kept
                .iter()
                .map(|e| e.token.max_aux_height)
                .min()
                .unwrap_or(MAX_HEIGHT);
            Layout::search(&placements, max_height).ok()
        };
        // #### PR #42: which entries leave a set that does not fit
        // What: when the whole set has no layout, entries join highest
        // priority first, and one that does not fit beside those kept so far
        // is left out; the others still join.
        // Why: a token with a small max_aux_height caps the tree for every
        // entry. Dropping the lowest priority while nothing fits would push
        // out tokens that fit beside the higher-priority ones (losing their
        // wins) before reaching the capped token that is the cause.
        // Look here if: a token is listed in `dropped` although the set
        // without some higher-priority token would hold it.
        let (entries, layout, dropped) = match search(&entries) {
            Some(layout) => (entries, layout, Vec::new()),
            None => {
                let mut order: Vec<usize> = (0..entries.len()).collect();
                order.sort_by_key(|&index| {
                    (std::cmp::Reverse(entries[index].token.priority), index)
                });
                let mut keep = vec![false; entries.len()];
                let mut layout = None;
                for index in order {
                    keep[index] = true;
                    let trial: Vec<SetEntry> = entries
                        .iter()
                        .zip(&keep)
                        .filter_map(|(entry, &joined)| joined.then_some(*entry))
                        .collect();
                    match search(&trial) {
                        Some(found) => layout = Some(found),
                        None => keep[index] = false,
                    }
                }
                // One entry alone always fits (height 0, nonce 0).
                let layout = layout.ok_or("no token fits a layout")?;
                let mut kept = Vec::new();
                let mut dropped = Vec::new();
                for (entry, joined) in entries.into_iter().zip(keep) {
                    if joined {
                        kept.push(entry);
                    } else {
                        dropped.push(entry);
                    }
                }
                (kept, layout, dropped)
            }
        };
        // #### end PR #42 ####
        let easiest_a = entries
            .iter()
            .filter_map(|e| e.state.map(|state| state.target))
            .reduce(|best, target| if easier(&target, &best) { target } else { best });
        Ok(Self {
            entries,
            layout,
            easiest_a,
            serial,
            dropped,
        })
    }

    pub fn entries(&self) -> &[SetEntry] {
        &self.entries
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The easiest Case A target (the largest), if any entry is Case A.
    pub fn easiest_a(&self) -> Option<&Hash> {
        self.easiest_a.as_ref()
    }

    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Entries left out because they did not fit beside higher-priority
    /// ones (their slots collided, or the tree could not be tall enough).
    pub fn dropped(&self) -> &[SetEntry] {
        &self.dropped
    }
}

/// What a coinbase adds for a token set: output 0 and the tickets after the
/// payouts, whose first vout the B leaves bind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxOutputs {
    pub commitment: [u8; OUTPUT_LEN],
    pub tickets: Vec<[u8; TICKET_LEN]>,
    pub first_ticket_vout: u32,
}

/// One entry of a job: where its leaf sits, the leaf, and its Case A
/// target or Case B ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxEntry {
    pub token: &'static MergeToken,
    pub mode: Mode,
    pub slot: u32,
    pub leaf: Leaf,
    pub target: Option<Hash>,
    pub ticket_vout: Option<u32>,
}

/// One job's merge-mining: entries in set order, the tree and its
/// commitment, and the outputs the coinbase adds.
#[derive(Clone, Debug)]
pub struct AuxJob {
    pub commitment: AuxCommitment,
    pub entries: Vec<AuxEntry>,
    pub tree: AuxTree,
    pub outputs: AuxOutputs,
    pub easiest_a: Option<Hash>,
    pub serial: u64,
}

impl AuxJob {
    /// Builds the leaves for a job paying `payout_script`, with the
    /// donation's `split` (share in hundredths of a percent and its locking
    /// script), and tickets from vout `first_ticket_vout` on (1 + the
    /// number of payout outputs, since output 0 is the commitment).
    pub fn build(
        set: &TokenSet,
        payout_script: &[u8],
        split: Option<(u16, &[u8])>,
        first_ticket_vout: u32,
    ) -> Result<Self, String> {
        if split.is_some_and(|(bps, _)| bps > MAX_SPLIT_BPS) {
            return Err("a token split cannot exceed 100%".into());
        }
        let mut tickets = Vec::new();
        let mut entries = Vec::with_capacity(set.entries.len());
        for (entry, slot) in set.entries.iter().zip(&set.layout.slots) {
            let token = entry.token;
            let (leaf, target, ticket_vout) = match (entry.mode, entry.state) {
                (Mode::ShareTarget, Some(state)) => (
                    Leaf::share_target(
                        token.category,
                        state.anchor,
                        payout_script,
                        state.target_bits,
                        split,
                        [0; 32],
                    ),
                    Some(state.target),
                    None,
                ),
                (Mode::BlockRequired, None) => {
                    let vout = u32::try_from(tickets.len())
                        .ok()
                        .and_then(|count| first_ticket_vout.checked_add(count))
                        .ok_or("too many coinbase outputs")?;
                    tickets.push(
                        token
                            .ticket_output()
                            .ok_or_else(|| format!("{} has no ticket", token.name))?,
                    );
                    (
                        Leaf::block_required(token.category, vout, payout_script, split, [0; 32]),
                        None,
                        Some(vout),
                    )
                }
                _ => return Err(format!("{}: entry and mode disagree", token.name)),
            };
            entries.push(AuxEntry {
                token,
                mode: entry.mode,
                slot: *slot,
                leaf,
                target,
                ticket_vout,
            });
        }
        let leaves: Vec<Hash> = entries.iter().map(|e| e.leaf.hash()).collect();
        let tree = AuxTree::build(&set.layout, &leaves);
        let commitment = AuxCommitment {
            root: tree.root(),
            height: set.layout.height,
            nonce: set.layout.nonce,
        };
        Ok(Self {
            commitment,
            entries,
            tree,
            outputs: AuxOutputs {
                commitment: commitment.output(),
                tickets,
                first_ticket_vout,
            },
            easiest_a: set.easiest_a,
            serial: set.serial,
        })
    }

    /// The aux branch of entry `index`, from its leaf to the root.
    pub fn branch(&self, index: usize) -> Option<Vec<Hash>> {
        Some(self.tree.branch(self.entries.get(index)?.slot))
    }

    /// The entries a share whose header hashes to `hash` wins, as indices
    /// in set order: Case A entries whose target the hash meets (`hash <=
    /// target`, the header rule) and, when the share is a BCH block, every
    /// Case B entry.
    pub fn wins(&self, hash: &Hash, block: bool) -> Vec<u16> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| match (entry.mode, &entry.target) {
                (Mode::ShareTarget, Some(target)) => meets_target(hash, target),
                (Mode::BlockRequired, _) => block,
                (Mode::ShareTarget, None) => false,
            })
            .filter_map(|(index, _)| u16::try_from(index).ok())
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stratum_v2::merge::{registry::TEST_TOKEN, tree::fold};

    /// The test token's Case A state: baton `[0x22; 32]:0`, about one share
    /// in 4,096 wins.
    pub(crate) fn test_state() -> TokenState {
        TokenState::new(
            OutPoint {
                txid: [0x22; 32],
                vout: 0,
            },
            0x1f10_0000,
        )
        .unwrap()
    }

    pub(crate) fn test_set(modes: &[Mode]) -> TokenSet {
        let entries = modes
            .iter()
            .map(|mode| match mode {
                Mode::ShareTarget => SetEntry::share_target(&TEST_TOKEN, test_state()),
                Mode::BlockRequired => SetEntry::block_required(&TEST_TOKEN),
            })
            .collect();
        TokenSet::new(MiningNetwork::Chipnet, entries, 1).unwrap()
    }

    // #### PR #42
    #[test]
    fn a_set_checks_its_entries_and_a_job_binds_payout_split_and_ticket_vouts() {
        // The test token exists only on Chipnet.
        let a = SetEntry::share_target(&TEST_TOKEN, test_state());
        let b = SetEntry::block_required(&TEST_TOKEN);
        assert!(TokenSet::new(MiningNetwork::Mainnet, vec![a], 1)
            .unwrap_err()
            .contains("chipnet"));
        assert!(TokenSet::new(MiningNetwork::Chipnet, vec![], 1).is_err());
        assert!(TokenSet::new(MiningNetwork::Chipnet, vec![a, a], 1).is_err());
        let stateless = SetEntry { state: None, ..a };
        assert!(TokenSet::new(MiningNetwork::Chipnet, vec![stateless], 1).is_err());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![a, b], 7).unwrap();
        assert_eq!(set.serial(), 7);
        assert_eq!(set.layout().height, 1);
        assert_eq!(set.easiest_a(), Some(&test_state().target));
        assert!(set.dropped().is_empty());
        assert_eq!(test_set(&[Mode::BlockRequired]).easiest_a(), None);
        assert!(TokenState::new(test_state().anchor, 0x0200_0001).is_err());

        let payout = [0x76, 0xa9, 0x14];
        let donation = [0xa9, 0x14];
        let job = AuxJob::build(&set, &payout, Some((100, &donation)), 3).unwrap();
        assert_eq!(job.outputs.first_ticket_vout, 3);
        assert_eq!(
            job.outputs.tickets,
            vec![TEST_TOKEN.ticket_output().unwrap()]
        );
        assert_eq!(job.outputs.commitment, job.commitment.output());
        assert_eq!(job.commitment.root, job.tree.root());
        assert_eq!(job.entries[0].leaf.anchor_hash, [0x22; 32]);
        assert_eq!(job.entries[0].target, Some(test_state().target));
        assert_eq!(job.entries[1].ticket_vout, Some(3));
        assert_eq!(job.entries[1].leaf.anchor_index, 3);
        for (index, entry) in job.entries.iter().enumerate() {
            assert_eq!(entry.leaf.payout_hash, double(&payout));
            assert_eq!(entry.leaf.split_bps, 100);
            assert_eq!(entry.leaf.split_hash, double(&donation));
            let branch = job.branch(index).unwrap();
            assert_eq!(
                fold(entry.leaf.hash(), entry.slot, &branch),
                job.commitment.root
            );
        }
        assert_eq!(job.branch(2), None);
        // Another payout, split or ticket vout gives another root, same layout.
        let roots = [
            AuxJob::build(&set, &donation, Some((100, &donation)), 3).unwrap(),
            AuxJob::build(&set, &payout, None, 3).unwrap(),
            AuxJob::build(&set, &payout, Some((100, &donation)), 4).unwrap(),
        ];
        for other in roots {
            assert_ne!(other.commitment.root, job.commitment.root);
            assert_eq!(other.commitment.nonce, job.commitment.nonce);
        }
        assert!(AuxJob::build(&set, &payout, Some((10_001, &donation)), 3).is_err());
    }

    // #### PR #42: which entries leave a set that does not fit
    #[test]
    fn entries_join_a_set_highest_priority_first_and_sets_are_capped() {
        let token = |byte: u8, priority: u8, max_aux_height: u8| -> &'static MergeToken {
            Box::leak(Box::new(MergeToken {
                category: [byte; 32],
                priority,
                max_aux_height,
                modes: &[Mode::ShareTarget],
                ticket: None,
                ..TEST_TOKEN
            }))
        };
        // A token whose covenant takes no aux branch (h = 0) leaves room
        // for one entry: the lower priority one leaves the set.
        let high = SetEntry::share_target(token(1, 9, MAX_HEIGHT), test_state());
        let low = SetEntry::share_target(token(2, 1, 0), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![low, high], 1).unwrap();
        assert_eq!(set.entries(), &[high]);
        assert_eq!(set.dropped(), &[low]);
        assert_eq!(set.layout().height, 0);
        // A capped token in the middle leaves alone: the lowest priority one
        // still fits beside the highest at h = 1.
        let capped = SetEntry::share_target(token(3, 1, 0), test_state());
        let lowest = SetEntry::share_target(token(4, 0, MAX_HEIGHT), test_state());
        let top = SetEntry::share_target(token(5, 5, MAX_HEIGHT), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![capped, lowest, top], 1).unwrap();
        assert_eq!(set.entries(), &[lowest, top]);
        assert_eq!(set.dropped(), &[capped]);
        assert_eq!(set.layout().height, 1);
        // Priority still rules: a capped top-priority token stays alone.
        let capped_top = SetEntry::share_target(token(6, 9, 0), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![lowest, capped_top, top], 1).unwrap();
        assert_eq!(set.entries(), &[capped_top]);
        assert_eq!(set.dropped(), &[lowest, top]);
        // At most 16 tokens per set (checked before any layout search).
        let many: Vec<_> = (1..=17)
            .map(|byte| SetEntry::share_target(token(byte, 0, MAX_HEIGHT), test_state()))
            .collect();
        assert!(TokenSet::new(MiningNetwork::Chipnet, many, 1)
            .unwrap_err()
            .contains("at most 16"));
    }

    // #### PR #42: token wins on the share path
    #[test]
    fn a_share_wins_case_a_by_target_and_case_b_only_with_a_block() {
        let set = test_set(&[Mode::ShareTarget, Mode::BlockRequired]);
        let job = AuxJob::build(&set, &[0x51], None, 2).unwrap();
        let target = test_state().target;
        // The header rule: a hash equal to the target wins, one above misses.
        assert_eq!(target[0], 0);
        let mut above = target;
        above[0] = 1;
        assert_eq!(job.wins(&target, false), [0]);
        assert_eq!(job.wins(&[0; 32], false), [0]);
        assert_eq!(job.wins(&above, false), Vec::<u16>::new());
        // Case B needs a block, whatever the hash.
        assert_eq!(job.wins(&target, true), [0, 1]);
        assert_eq!(job.wins(&above, true), [1]);
        let b_only = AuxJob::build(&test_set(&[Mode::BlockRequired]), &[0x51], None, 2).unwrap();
        assert_eq!(b_only.easiest_a, None);
        assert_eq!(b_only.wins(&[0; 32], false), Vec::<u16>::new());
        assert_eq!(b_only.wins(&[0xff; 32], true), [0]);
    }

    fn double(bytes: &[u8]) -> Hash {
        crate::stratum_v2::template::double_sha256(bytes)
    }
}
