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

// #### merge-mining v2: disclosure to miners
// What: a Pickaxe SV2 extension (type 0x4D4D, "MM"), negotiated with the
// SV2 extensions negotiation (type 0x0001, RequestExtensions): a server
// sends a client that asked for it, after each job, the job's leaves
// (`JobLeaves`), and after a share that wins a token, its proof
// (`WinProof`). The SV1 adapter relays them to firmware that asked for
// `pickaxe-merge` in mining.configure. Every integer is little-endian.
//
// JobLeaves (message 0x00):
//   channel u32 | job u32 | output 0 (53) | u16 n + disclosure (CTMD, n bytes)
//   | for each disclosed leaf, its aux branch (32 x h bytes, leaf to root)
// WinProof (message 0x01):
//   channel u32 | job u32 | category (32) | u16 n + AuxProof v2 (n bytes)
//   | u8 m (0 or 69) + the split preimage (m bytes)
//
// Why: a miner on someone else's pool checks that their root's leaves pay
// them (and only the fee and donation they agreed to) before hashing, and
// can claim their own wins.
// Look here if: a client that negotiated the extension gets no leaves, or
// firmware without it gets frames it does not know.
/// The Pickaxe merge-mining disclosure extension's type.
pub const EXTENSION: u16 = 0x4D4D;
/// The SV2 extensions negotiation's type and its messages.
pub const NEGOTIATION: u16 = 0x0001;
pub const REQUEST_EXTENSIONS: u8 = 0x00;
pub const REQUEST_EXTENSIONS_SUCCESS: u8 = 0x01;
/// This extension's messages.
pub const JOB_LEAVES: u8 = 0x00;
pub const WIN_PROOF: u8 = 0x01;

/// A job's leaves for a miner, with each leaf's aux branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobLeaves {
    pub channel: u32,
    pub job: u32,
    pub output: [u8; super::commitment::OUTPUT_LEN],
    pub disclosure: Disclosure,
    pub branches: Vec<Vec<super::Hash>>,
}

impl JobLeaves {
    pub fn from_job(channel: u32, job: u32, aux: &AuxJob) -> Self {
        Self {
            channel,
            job,
            output: aux.output,
            disclosure: Disclosure::from_job(aux),
            branches: aux
                .entries
                .iter()
                .map(|entry| aux.tree.branch(entry.slot))
                .collect(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.channel.to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.job.to_le_bytes());
        bytes.extend_from_slice(&self.output);
        let disclosure = self.disclosure.encode();
        bytes.extend_from_slice(&(disclosure.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&disclosure);
        for branch in &self.branches {
            branch.iter().for_each(|hash| bytes.extend_from_slice(hash));
        }
        bytes
    }

    /// Reads exactly one, checking that the leaves and branches make
    /// output 0's root.
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let bad = "not a job's merge-mining leaves";
        let take = |at: usize, len: usize| bytes.get(at..at + len).ok_or(bad);
        let word = |at: usize| -> Result<u32, &'static str> {
            Ok(u32::from_le_bytes(
                take(at, 4)?.try_into().map_err(|_| bad)?,
            ))
        };
        let channel = word(0)?;
        let job = word(4)?;
        let output: [u8; super::commitment::OUTPUT_LEN] = take(8, super::commitment::OUTPUT_LEN)?
            .try_into()
            .map_err(|_| bad)?;
        let commitment = AuxCommitment::parse(&output).ok_or(bad)?;
        let at = 8 + super::commitment::OUTPUT_LEN;
        let length = usize::from(u16::from_le_bytes(
            take(at, 2)?.try_into().map_err(|_| bad)?,
        ));
        let disclosure = Disclosure::decode(take(at + 2, length)?)?;
        disclosure.check_root(&commitment)?;
        let mut rest = bytes.get(at + 2 + length..).ok_or(bad)?;
        let depth = 32 * usize::from(commitment.height);
        let mut branches = Vec::with_capacity(disclosure.leaves.len());
        for disclosed in &disclosure.leaves {
            let (branch, after) = rest.split_at_checked(depth).ok_or(bad)?;
            let branch: Vec<super::Hash> = branch.as_chunks::<32>().0.to_vec();
            let slot = slot(&disclosed.leaf.category, commitment.salt, commitment.height);
            if super::tree::fold(disclosed.leaf.hash(), slot, &branch) != commitment.root {
                return Err("a disclosed branch does not reach output 0's root");
            }
            branches.push(branch);
            rest = after;
        }
        if !rest.is_empty() {
            return Err(bad);
        }
        Ok(Self {
            channel,
            job,
            output,
            disclosure,
            branches,
        })
    }
}

/// A token win's proof for the miner whose share won it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WinProof {
    pub channel: u32,
    pub job: u32,
    pub category: super::Hash,
    pub proof: Vec<u8>,
    pub split: Split,
}

impl WinProof {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.channel.to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.job.to_le_bytes());
        bytes.extend_from_slice(&self.category);
        bytes.extend_from_slice(&(self.proof.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&self.proof);
        if self.split.is_none() {
            bytes.push(0);
        } else {
            bytes.push(PREIMAGE_LEN as u8);
            bytes.extend_from_slice(&self.split.preimage());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let bad = "not a merge-mining win proof";
        let take = |at: usize, len: usize| bytes.get(at..at + len).ok_or(bad);
        let channel = u32::from_le_bytes(take(0, 4)?.try_into().map_err(|_| bad)?);
        let job = u32::from_le_bytes(take(4, 4)?.try_into().map_err(|_| bad)?);
        let category = take(8, 32)?.try_into().map_err(|_| bad)?;
        let length = usize::from(u16::from_le_bytes(
            take(40, 2)?.try_into().map_err(|_| bad)?,
        ));
        let proof = take(42, length)?.to_vec();
        let at = 42 + length;
        let split = match *take(at, 1)? {
            [0] => Split::NONE,
            [69] => Split::parse(take(at + 1, PREIMAGE_LEN)?).map_err(|_| bad)?,
            _ => return Err(bad),
        };
        let end = at + 1 + if split.is_none() { 0 } else { PREIMAGE_LEN };
        if bytes.len() != end {
            return Err(bad);
        }
        Ok(Self {
            channel,
            job,
            category,
            proof,
            split,
        })
    }
}

/// The extensions a RequestExtensions frame's payload asks for: its request
/// id and the extension types (`u16 request_id | u16 n | n x u16`).
pub fn requested_extensions(payload: &[u8]) -> Option<(u16, Vec<u16>)> {
    let request = u16::from_le_bytes(payload.get(..2)?.try_into().ok()?);
    let count = usize::from(u16::from_le_bytes(payload.get(2..4)?.try_into().ok()?));
    let list = payload.get(4..)?;
    if list.len() != 2 * count {
        return None;
    }
    Some((
        request,
        list.as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect(),
    ))
}

/// A RequestExtensions(.Success) payload: the request id and the types.
pub fn extensions_payload(request: u16, extensions: &[u16]) -> Vec<u8> {
    let mut bytes = request.to_le_bytes().to_vec();
    bytes.extend_from_slice(&(extensions.len() as u16).to_le_bytes());
    extensions
        .iter()
        .for_each(|extension| bytes.extend_from_slice(&extension.to_le_bytes()));
    bytes
}

// #### merge-mining v2: the public pool audit rule
// What: on a public pool, every leaf of a miner's job pays that miner's
// registered payout script, except a work job's, which pays the donation or
// the operator whole; a split pays only the donation and the operator.
// Why: anyone holding a disclosure (or the leaf log) can check a pool never
// put a root on someone's device that pays another miner or itself beyond
// its fee.
// Look here if: a public pool job is refused its tokens by this check.
/// Checks a public pool job's leaves against the miner, donation and
/// operator scripts.
pub fn audit_public(
    disclosure: &Disclosure,
    miner: &[u8],
    donation: &[u8],
    operator: &[u8],
) -> Result<(), &'static str> {
    use super::super::template::double_sha256;
    let (miner, donation, operator) = (
        double_sha256(miner),
        double_sha256(donation),
        double_sha256(operator),
    );
    for disclosed in &disclosure.leaves {
        let payout = disclosed.leaf.payout_hash;
        let pays = if disclosed.split.is_none() {
            [miner, donation, operator].contains(&payout)
        } else {
            payout == miner
        };
        let slots = [disclosed.split.donation_slot(), disclosed.split.fee_slot()];
        let split = slots[0].is_empty() || *slots[0].script_hash() == donation;
        let fee = slots[1].is_empty() || *slots[1].script_hash() == operator;
        if !(pays && split && fee) {
            return Err("a public pool leaf pays someone other than its miner, the donation and the operator");
        }
    }
    Ok(())
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

    // #### merge-mining v2: disclosure to miners
    // What: JobLeaves and WinProof round-trip; JobLeaves refuses a branch
    // that misses output 0's root; the negotiation payloads read back.
    #[test]
    fn job_leaves_and_win_proofs_round_trip() {
        let job = job();
        let leaves = JobLeaves::from_job(3, 9, &job);
        let bytes = leaves.encode();
        assert_eq!(JobLeaves::decode(&bytes), Ok(leaves.clone()));
        assert!(JobLeaves::decode(&[&bytes[..], &[0]].concat()).is_err());
        // A branch that misses the root: the test set has one leaf at h = 0,
        // so take a two-token job.
        let two = AuxJob::build(
            &crate::stratum_v2::merge::set::TokenSet::new(
                crate::config::MiningNetwork::Chipnet,
                vec![
                    crate::stratum_v2::merge::set::SetEntry::new(
                        &crate::stratum_v2::merge::registry::TEST_TOKEN,
                        crate::stratum_v2::merge::set::tests::test_state(),
                    ),
                    crate::stratum_v2::merge::set::SetEntry::new(
                        Box::leak(Box::new(crate::stratum_v2::merge::registry::MergeToken {
                            category: [7; 32],
                            ..crate::stratum_v2::merge::registry::TEST_TOKEN
                        })),
                        crate::stratum_v2::merge::set::tests::test_state(),
                    ),
                ],
                1,
            )
            .unwrap(),
            &LeafTerms::plain(&[0x51]),
        );
        let leaves = JobLeaves::from_job(3, 9, &two);
        assert_eq!(leaves.branches.len(), 2);
        let mut bytes = leaves.encode();
        assert_eq!(JobLeaves::decode(&bytes), Ok(leaves));
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(JobLeaves::decode(&bytes).is_err());
        let proof = WinProof {
            channel: 3,
            job: 9,
            category: [1; 32],
            proof: vec![5; 200],
            split: job.entries[0].split,
        };
        let bytes = proof.encode();
        assert_eq!(WinProof::decode(&bytes), Ok(proof.clone()));
        assert!(WinProof::decode(&bytes[..bytes.len() - 1]).is_err());
        let plain = WinProof {
            split: Split::NONE,
            ..proof
        };
        assert_eq!(WinProof::decode(&plain.encode()), Ok(plain));
        assert_eq!(
            requested_extensions(&extensions_payload(4, &[EXTENSION, 2])),
            Some((4, vec![EXTENSION, 2]))
        );
        assert_eq!(requested_extensions(&[4, 0, 2, 0, 1]), None);
    }

    // #### merge-mining v2: the public pool audit rule
    #[test]
    fn a_public_pool_leaf_pays_its_miner_or_a_work_job_s_recipient() {
        let (miner, donation, operator) = (vec![0x51], vec![0x52], vec![0x53]);
        let split = Split::new(
            Slot::new(100, &donation).unwrap(),
            Slot::new(200, &operator).unwrap(),
        );
        let build = |payout: &[u8], split| {
            Disclosure::from_job(&AuxJob::build(
                &test_set(),
                &LeafTerms {
                    payout,
                    split,
                    plain_payout: payout,
                },
            ))
        };
        let audit =
            |disclosure: &Disclosure| audit_public(disclosure, &miner, &donation, &operator);
        assert_eq!(audit(&build(&miner, split)), Ok(()));
        // Work jobs pay the donation or the operator whole.
        assert_eq!(audit(&build(&donation, Split::NONE)), Ok(()));
        assert_eq!(audit(&build(&operator, Split::NONE)), Ok(()));
        // Another miner, a split leaf paying the operator, a slot paying
        // someone else.
        assert!(audit(&build(&[0x54], split)).is_err());
        assert!(audit(&build(&operator, split)).is_err());
        let stray = Split::new(
            Slot::new(100, &donation).unwrap(),
            Slot::new(200, &[0x54]).unwrap(),
        );
        assert!(audit(&build(&miner, stray)).is_err());
    }
}
