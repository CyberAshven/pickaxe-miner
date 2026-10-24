//! #### merge-mining v2: leaf disclosure
//! What: the leaves behind a coinbase's output 0, in a small versioned
//! format: what a Job Declaration client puts in DeclareMiningJob's
//! `excess_data`, and what a pool shows a miner who asks for it.
//!
//! ```text
//! off len   field
//! 0   4     "CTMD"
//! 4   1     02
//! 5   1     n, the leaves (0 to 16)
//! then n times:
//!     169   the leaf preimage
//!     1     00: ext is zero; 01: a split follows
//!     69    the split preimage (with 01 only)
//! ```
//!
//! The reader rebuilds the tree from the leaves alone, with the `h` and salt
//! of output 0, and accepts the disclosure only when its root is output 0's:
//! a leaf left out, added or changed gives another root.
//! Why: a pool must see that a declared coinbase's leaves pay its fee, and a
//! miner on someone else's pool that their leaves pay them.
//! Look here if: a declaration with tokens is refused for its leaves, or a
//! miner's disclosure does not rebuild their root.

use super::{
    commitment::AuxCommitment,
    leaf::{Leaf, LEN as LEAF_LEN},
    set::AuxJob,
    split::{Split, PREIMAGE_LEN},
    tree::{slot, AuxTree, Layout, MAX_LEAVES},
};

pub const MAGIC: [u8; 4] = *b"CTMD";
pub const VERSION: u8 = 2;

/// One disclosed leaf and the split its ext commits to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisclosedLeaf {
    pub leaf: Leaf,
    pub split: Split,
}

/// The leaves of one coinbase's output 0.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disclosure {
    pub leaves: Vec<DisclosedLeaf>,
}

impl Disclosure {
    /// A job's leaves.
    pub fn from_job(job: &AuxJob) -> Self {
        Self {
            leaves: job
                .entries
                .iter()
                .map(|entry| DisclosedLeaf {
                    leaf: entry.leaf,
                    split: entry.split,
                })
                .collect(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(VERSION);
        bytes.push(self.leaves.len() as u8);
        for disclosed in &self.leaves {
            bytes.extend_from_slice(&disclosed.leaf.preimage());
            if disclosed.split.is_none() {
                bytes.push(0);
            } else {
                bytes.push(1);
                bytes.extend_from_slice(&disclosed.split.preimage());
            }
        }
        bytes
    }

    /// Reads exactly one disclosure: each leaf canonical, each split
    /// canonical and its ext the leaf's, nothing trailing.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let bad = "not a merge-mining leaf disclosure";
        let rest = bytes.strip_prefix(&MAGIC[..]).ok_or(bad)?;
        let (&version, rest) = rest.split_first().ok_or(bad)?;
        let (&count, mut rest) = rest.split_first().ok_or(bad)?;
        if version != VERSION || usize::from(count) > MAX_LEAVES {
            return Err(bad);
        }
        let mut leaves = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let (preimage, after) = rest.split_at_checked(LEAF_LEN).ok_or(bad)?;
            let leaf = Leaf::parse(preimage).ok_or(bad)?;
            let (&flag, after) = after.split_first().ok_or(bad)?;
            let (split, after) = match flag {
                0 => (Split::NONE, after),
                1 => {
                    let (preimage, after) = after.split_at_checked(PREIMAGE_LEN).ok_or(bad)?;
                    (Split::parse(preimage).map_err(|_| bad)?, after)
                }
                _ => return Err(bad),
            };
            if split.ext() != leaf.ext {
                return Err("a disclosed leaf's ext is not its split's");
            }
            leaves.push(DisclosedLeaf { leaf, split });
            rest = after;
        }
        if !rest.is_empty() {
            return Err(bad);
        }
        Ok(Self { leaves })
    }

    /// Whether these leaves, and no others, make `commitment`'s root at its
    /// height and salt: each in its own slot, every other slot empty.
    pub fn check_root(&self, commitment: &AuxCommitment) -> Result<(), &'static str> {
        if self.leaves.is_empty() {
            return if commitment.is_empty() {
                Ok(())
            } else {
                Err("the disclosure names no leaf for a non-empty root")
            };
        }
        let slots: Vec<u32> = self
            .leaves
            .iter()
            .map(|disclosed| slot(&disclosed.leaf.category, commitment.salt, commitment.height))
            .collect();
        if slots
            .iter()
            .enumerate()
            .any(|(index, placed)| slots[..index].contains(placed))
        {
            return Err("two disclosed leaves share a slot");
        }
        let layout = Layout {
            height: commitment.height,
            salt: commitment.salt,
            slots,
        };
        let hashes: Vec<_> = self.leaves.iter().map(|d| d.leaf.hash()).collect();
        if AuxTree::build(&layout, &hashes).root() != commitment.root {
            return Err("the disclosed leaves do not make output 0's root");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stratum_v2::merge::{
        set::{tests::test_set, LeafTerms},
        split::Slot,
    };

    fn job() -> AuxJob {
        let terms = LeafTerms {
            payout: &[0x51],
            split: Split::new(
                Slot::new(100, &[0x52]).unwrap(),
                Slot::new(200, &[0x53]).unwrap(),
            ),
            plain_payout: &[0x51],
        };
        AuxJob::build(&test_set(), &terms)
    }

    // #### merge-mining v2: the disclosure format
    #[test]
    fn a_disclosure_round_trips_and_rebuilds_its_root() {
        let job = job();
        let disclosure = Disclosure::from_job(&job);
        let bytes = disclosure.encode();
        assert_eq!(bytes[..6], [b'C', b'T', b'M', b'D', 2, 1]);
        assert_eq!(bytes.len(), 6 + 169 + 1 + 69);
        assert_eq!(Disclosure::decode(&bytes), Ok(disclosure.clone()));
        assert_eq!(disclosure.check_root(&job.commitment), Ok(()));
        // No split: the flag alone.
        let plain = AuxJob::build(&test_set(), &LeafTerms::plain(&[0x51]));
        let bytes = Disclosure::from_job(&plain).encode();
        assert_eq!(bytes.len(), 6 + 169 + 1);
        assert_eq!(bytes[6 + 169], 0);
        // Another root, a leaf left out, trailing bytes, another version or
        // count, a split that is not the leaf's ext.
        assert!(disclosure.check_root(&plain.commitment).is_err());
        assert!(Disclosure::default().check_root(&job.commitment).is_err());
        assert_eq!(
            Disclosure::default().check_root(&AuxCommitment::EMPTY),
            Ok(())
        );
        let bytes = disclosure.encode();
        assert!(Disclosure::decode(&[&bytes[..], &[0]].concat()).is_err());
        let mut changed = bytes.clone();
        changed[4] = 3;
        assert!(Disclosure::decode(&changed).is_err());
        let mut changed = bytes.clone();
        changed[5] = 17;
        assert!(Disclosure::decode(&changed).is_err());
        let mut changed = bytes.clone();
        changed[6 + 169 + 1 + 2] ^= 1;
        assert!(Disclosure::decode(&changed).is_err());
        let mut changed = bytes;
        changed[6 + 169] = 0;
        changed.truncate(6 + 169 + 1);
        assert!(Disclosure::decode(&changed).is_err());
    }
}
