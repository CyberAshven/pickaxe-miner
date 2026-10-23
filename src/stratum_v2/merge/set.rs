//! #### PR #42, merge-mining v2
//! A token set (what the server merge-mines now) and an `AuxJob` (one job's
//! leaves, tree and commitment).
//!
//! The layout search runs once per token set: slots depend only on
//! categories. Each job then builds its own leaves, because every leaf
//! binds that job's payout: the miner's script in miner jobs, the
//! donation's in donation-work jobs. A public pool's fee never reaches
//! tokens, and other people's devices get the empty commitment
//! ([`AuxJob::empty`]).

use super::{
    super::template::meets_target,
    commitment::{AuxCommitment, OUTPUT_LEN},
    leaf::Leaf,
    registry::{MergeToken, MAX_TOKENS},
    tree::{AuxTree, Layout, MAX_HEIGHT},
    verify::expand_compact,
    Hash,
};
use crate::config::MiningNetwork;

/// A token's live state: the hashes its next leaf binds and its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenState {
    /// `HASH256` of the baton's locking bytecode.
    pub lock_hash: Hash,
    /// The token-defined state hash; it changes only on a successful claim.
    pub state_hash: Hash,
    pub target_bits: u32,
    /// `target_bits` expanded, little-endian, as share hashes compare.
    pub target: Hash,
}

impl TokenState {
    pub fn new(lock_hash: Hash, state_hash: Hash, target_bits: u32) -> Result<Self, String> {
        Ok(Self {
            lock_hash,
            state_hash,
            target_bits,
            target: expand_compact(target_bits).ok_or("invalid token target")?,
        })
    }
}

/// One token to merge-mine, in its current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetEntry {
    pub token: &'static MergeToken,
    pub state: TokenState,
}

impl SetEntry {
    pub fn new(token: &'static MergeToken, state: TokenState) -> Self {
        Self { token, state }
    }
}

/// What the server merge-mines: the entries that fit, their layout, the
/// easiest target and a serial that grows with every change.
#[derive(Clone, Debug)]
pub struct TokenSet {
    entries: Vec<SetEntry>,
    layout: Layout,
    easiest: Hash,
    serial: u64,
    dropped: Vec<SetEntry>,
}

/// Whether target `a` is easier (a larger number) than `b`.
fn easier(a: &Hash, b: &Hash) -> bool {
    a.iter().rev().cmp(b.iter().rev()).is_gt()
}

impl TokenSet {
    /// Checks every entry against the network and its registry row, then
    /// searches the layout once. If the whole set does not fit (a token's
    /// own height limit), entries join highest priority first (on a tie,
    /// the earlier one), and each entry that does not fit beside those
    /// already kept is left out.
    pub fn new(
        network: MiningNetwork,
        entries: Vec<SetEntry>,
        serial: u64,
    ) -> Result<Self, String> {
        if entries.is_empty() {
            return Err("no token to merge-mine".into());
        }
        if entries.len() > MAX_TOKENS {
            return Err(format!("at most {MAX_TOKENS} tokens per coinbase"));
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
            if entries[..index]
                .iter()
                .any(|other| other.token.category == token.category)
            {
                return Err(format!(
                    "{} is listed twice: one leaf per token per coinbase",
                    token.name
                ));
            }
        }
        let search = |kept: &[SetEntry]| {
            let categories: Vec<Hash> = kept.iter().map(|e| e.token.category).collect();
            let max_height = kept
                .iter()
                .map(|e| e.token.max_aux_height)
                .min()
                .unwrap_or(MAX_HEIGHT);
            Layout::search(&categories, max_height).ok()
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
                // One entry alone always fits (height 0, salt 0).
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
        let easiest = entries
            .iter()
            .map(|e| e.state.target)
            .reduce(|best, target| if easier(&target, &best) { target } else { best })
            .ok_or("no token to merge-mine")?;
        Ok(Self {
            entries,
            layout,
            easiest,
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

    /// The easiest token target (the largest).
    pub fn easiest(&self) -> &Hash {
        &self.easiest
    }

    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Entries left out because they did not fit beside higher-priority
    /// ones (the tree could not be tall enough for them all).
    pub fn dropped(&self) -> &[SetEntry] {
        &self.dropped
    }
}

/// One entry of a job: where its leaf sits, the leaf and its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxEntry {
    pub token: &'static MergeToken,
    pub slot: u32,
    pub leaf: Leaf,
    pub target: Hash,
}

/// One job's merge-mining: entries in set order, the tree, its commitment
/// and output 0 as the coinbase carries it.
#[derive(Clone, Debug)]
pub struct AuxJob {
    pub commitment: AuxCommitment,
    pub entries: Vec<AuxEntry>,
    pub tree: AuxTree,
    pub output: [u8; OUTPUT_LEN],
    /// The easiest entry target; `None` for the empty commitment.
    pub easiest: Option<Hash>,
    pub serial: u64,
}

impl AuxJob {
    /// Builds the leaves for a job paying `payout_script`. `ext` is 32 zero
    /// bytes: no registered token defines one yet.
    pub fn build(set: &TokenSet, payout_script: &[u8]) -> Self {
        let entries: Vec<AuxEntry> = set
            .entries
            .iter()
            .zip(&set.layout.slots)
            .map(|(entry, slot)| AuxEntry {
                token: entry.token,
                slot: *slot,
                leaf: Leaf::new(
                    entry.token.category,
                    entry.state.lock_hash,
                    entry.state.state_hash,
                    payout_script,
                    entry.state.target_bits,
                    [0; 32],
                ),
                target: entry.state.target,
            })
            .collect();
        let leaves: Vec<Hash> = entries.iter().map(|e| e.leaf.hash()).collect();
        let tree = AuxTree::build(&set.layout, &leaves);
        let commitment = AuxCommitment {
            root: tree.root(),
            height: set.layout.height,
            salt: set.layout.salt,
        };
        Self {
            commitment,
            entries,
            tree,
            output: commitment.output(),
            easiest: Some(set.easiest),
            serial: set.serial,
        }
    }

    /// #### merge-mining v2: the empty commitment, which jobs for other
    /// people's devices carry: no leaf, no win, no token target.
    pub fn empty() -> Self {
        let layout = Layout {
            height: 0,
            salt: 0,
            slots: Vec::new(),
        };
        Self {
            commitment: AuxCommitment::EMPTY,
            entries: Vec::new(),
            tree: AuxTree::build(&layout, &[]),
            output: AuxCommitment::EMPTY.output(),
            easiest: None,
            serial: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The aux branch of entry `index`, from its leaf to the root.
    pub fn branch(&self, index: usize) -> Option<Vec<Hash>> {
        Some(self.tree.branch(self.entries.get(index)?.slot))
    }

    /// The entries a share whose header hashes to `hash` wins, as indices
    /// in set order: those whose target the hash meets (`hash <= target`,
    /// the header rule).
    pub fn wins(&self, hash: &Hash) -> Vec<u16> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| meets_target(hash, &entry.target))
            .filter_map(|(index, _)| u16::try_from(index).ok())
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::stratum_v2::merge::{registry::TEST_TOKEN, tree::fold};

    /// The test token's state: a synthetic lock and state hash, about one
    /// share in 4,096 wins.
    pub(crate) fn test_state() -> TokenState {
        TokenState::new([0x22; 32], [0x33; 32], 0x1f10_0000).unwrap()
    }

    pub(crate) fn test_set() -> TokenSet {
        TokenSet::new(
            MiningNetwork::Chipnet,
            vec![SetEntry::new(&TEST_TOKEN, test_state())],
            1,
        )
        .unwrap()
    }

    fn token(byte: u8, priority: u8, max_aux_height: u8) -> &'static MergeToken {
        Box::leak(Box::new(MergeToken {
            category: [byte; 32],
            priority,
            max_aux_height,
            ..TEST_TOKEN
        }))
    }

    // #### merge-mining v2
    #[test]
    fn a_set_checks_its_entries_and_a_job_binds_its_payout() {
        // The test token exists only on Chipnet.
        let a = SetEntry::new(&TEST_TOKEN, test_state());
        assert!(TokenSet::new(MiningNetwork::Mainnet, vec![a], 1)
            .unwrap_err()
            .contains("chipnet"));
        assert!(TokenSet::new(MiningNetwork::Chipnet, vec![], 1).is_err());
        assert!(TokenSet::new(MiningNetwork::Chipnet, vec![a, a], 1)
            .unwrap_err()
            .contains("one leaf per token"));
        let b = SetEntry::new(token(7, 0, MAX_HEIGHT), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![a, b], 7).unwrap();
        assert_eq!(set.serial(), 7);
        assert_eq!(set.layout().height, 1);
        assert_eq!(set.easiest(), &test_state().target);
        assert!(set.dropped().is_empty());
        assert!(TokenState::new([0; 32], [0; 32], 0x0200_0001).is_err());

        let payout = [0x76, 0xa9, 0x14];
        let job = AuxJob::build(&set, &payout);
        assert_eq!(job.output, job.commitment.output());
        assert_eq!(job.commitment.root, job.tree.root());
        assert_eq!(job.entries[0].leaf.lock_hash, [0x22; 32]);
        assert_eq!(job.entries[0].leaf.state_hash, [0x33; 32]);
        assert_eq!(job.entries[0].target, test_state().target);
        for (index, entry) in job.entries.iter().enumerate() {
            assert_eq!(
                entry.leaf.payout_hash,
                crate::stratum_v2::template::double_sha256(&payout)
            );
            assert_eq!(entry.leaf.ext, [0; 32]);
            let branch = job.branch(index).unwrap();
            assert_eq!(
                fold(entry.leaf.hash(), entry.slot, &branch),
                job.commitment.root
            );
        }
        assert_eq!(job.branch(2), None);
        // Another payout gives another root on the same layout.
        let other = AuxJob::build(&set, &[0x51]);
        assert_ne!(other.commitment.root, job.commitment.root);
        assert_eq!(other.commitment.salt, job.commitment.salt);
        // The empty commitment: no entry, no win, no target.
        let empty = AuxJob::empty();
        assert!(empty.is_empty() && empty.commitment.is_empty());
        assert_eq!(empty.output, AuxCommitment::EMPTY.output());
        assert_eq!(empty.easiest, None);
        assert!(empty.wins(&[0; 32]).is_empty());
    }

    // #### PR #42: which entries leave a set that does not fit
    #[test]
    fn entries_join_a_set_highest_priority_first_and_sets_are_capped() {
        // A token whose covenant takes no aux branch (h = 0) leaves room
        // for one entry: the lower priority one leaves the set.
        let high = SetEntry::new(token(1, 9, MAX_HEIGHT), test_state());
        let low = SetEntry::new(token(2, 1, 0), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![low, high], 1).unwrap();
        assert_eq!(set.entries(), &[high]);
        assert_eq!(set.dropped(), &[low]);
        assert_eq!(set.layout().height, 0);
        // A capped token in the middle leaves alone: the lowest priority one
        // still fits beside the highest at h = 1.
        let capped = SetEntry::new(token(3, 1, 0), test_state());
        let lowest = SetEntry::new(token(4, 0, MAX_HEIGHT), test_state());
        let top = SetEntry::new(token(5, 5, MAX_HEIGHT), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![capped, lowest, top], 1).unwrap();
        assert_eq!(set.entries(), &[lowest, top]);
        assert_eq!(set.dropped(), &[capped]);
        assert_eq!(set.layout().height, 1);
        // Priority still rules: a capped top-priority token stays alone.
        let capped_top = SetEntry::new(token(6, 9, 0), test_state());
        let set = TokenSet::new(MiningNetwork::Chipnet, vec![lowest, capped_top, top], 1).unwrap();
        assert_eq!(set.entries(), &[capped_top]);
        assert_eq!(set.dropped(), &[lowest, top]);
        // At most 16 tokens per set (checked before any layout search).
        let many: Vec<_> = (1..=17)
            .map(|byte| SetEntry::new(token(byte, 0, MAX_HEIGHT), test_state()))
            .collect();
        assert!(TokenSet::new(MiningNetwork::Chipnet, many, 1)
            .unwrap_err()
            .contains("at most 16"));
    }

    // #### PR #42: token wins on the share path
    #[test]
    fn a_share_wins_each_token_whose_target_it_meets() {
        let easy = TokenState::new([0x22; 32], [0x33; 32], 0x2001_0000).unwrap();
        let set = TokenSet::new(
            MiningNetwork::Chipnet,
            vec![
                SetEntry::new(&TEST_TOKEN, test_state()),
                SetEntry::new(token(9, 0, MAX_HEIGHT), easy),
            ],
            1,
        )
        .unwrap();
        assert_eq!(set.easiest(), &easy.target);
        let job = AuxJob::build(&set, &[0x51]);
        let target = test_state().target;
        // The header rule: a hash equal to the target wins, one above misses.
        let mut above = target;
        above[0] = 1;
        assert_eq!(job.wins(&target), [0, 1]);
        assert_eq!(job.wins(&[0; 32]), [0, 1]);
        assert_eq!(job.wins(&above), [1]);
        assert_eq!(job.wins(&[0xff; 32]), Vec::<u16>::new());
    }
}
