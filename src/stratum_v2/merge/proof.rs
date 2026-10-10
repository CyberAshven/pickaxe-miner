//! #### PR #42
//! `AuxProof` v1: what a token win hands to the proof journal and a claim
//! builder. Little-endian, canonical (exact lengths, nothing trailing).
//!
//! ```text
//! 0    4     "CTMP"
//! 4    1     01
//! 5    1     mode
//! 6    170   leaf[6..176]
//! 176  1     h (at most 16)
//! 177  4     aux_nonce, u32 LE
//! 181  32h   aux_branch, leaf to root
//! ..   2+n   u16 LE n, cb_head (47+L bytes: the coinbase up to output 0)
//! ..   2+n   u16 LE n, cb_tail (outputs 1.. and the locktime)
//! ..   1+32d d (at most 32), the coinbase's merkle branch at index 0 (Case B: d = 0)
//! ..   80    header (Case A only)
//! ```
//!
//! Output 0 is not carried: a verifier rebuilds it from the aux root.

use super::{
    commitment::OUTPUT_LEN,
    leaf::{Leaf, Mode, FIELDS_LEN},
    set::AuxJob,
    tree::MAX_HEIGHT,
    Hash,
};
use std::fmt;

pub const MAGIC: [u8; 4] = *b"CTMP";
pub const VERSION: u8 = 1;
/// The deepest coinbase branch: 2^32 transactions.
pub const MAX_COINBASE_BRANCH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofError {
    /// The bytes are not a canonical v1 proof.
    Decode,
    /// The parts do not fit the mode (header, branch lengths).
    Shape,
    /// The proof's leaf is not the one the claim builds.
    Leaf,
    /// A split above 100%.
    Split,
    /// The aux tree is taller than the token allows.
    Height,
    /// The token's compact target is not a valid one.
    Target,
    /// The coinbase does not start with one null-prevout input and a
    /// one-byte output count before output 0.
    CoinbaseHead,
    /// The coinbase has no room for its locktime.
    CoinbaseTail,
    /// The coinbase does not reach the header's merkle root.
    MerkleRoot,
    /// The header's hash does not meet the token's target.
    Work,
    /// Case B: the coinbase is not the ticket's transaction.
    Ticket,
    /// Case B: the block is below the token's start height.
    StartHeight,
    /// The job has no such entry, or the coinbase lacks its commitment.
    NotInJob,
}

impl fmt::Display for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Decode => "not a canonical merge-mining proof",
            Self::Shape => "merge-mining proof parts do not fit its mode",
            Self::Leaf => "merge-mining proof leaf does not match the claim",
            Self::Split => "merge-mining split exceeds 100%",
            Self::Height => "merge-mining tree is taller than the token allows",
            Self::Target => "invalid token target",
            Self::CoinbaseHead => "merge-mining coinbase head is malformed",
            Self::CoinbaseTail => "merge-mining coinbase tail is too short",
            Self::MerkleRoot => "merge-mining coinbase does not reach the header",
            Self::Work => "header does not meet the token target",
            Self::Ticket => "coinbase is not the ticket's transaction",
            Self::StartHeight => "block is below the token's start height",
            Self::NotInJob => "token win does not belong to this job",
        })
    }
}

impl std::error::Error for ProofError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxProof {
    pub leaf: Leaf,
    pub height: u8,
    pub nonce: u32,
    pub aux_branch: Vec<Hash>,
    pub cb_head: Vec<u8>,
    pub cb_tail: Vec<u8>,
    pub cb_branch: Vec<Hash>,
    pub header: Option<[u8; 80]>,
}

/// Builds the proof of entry `entry` of `job` from the share's coinbase,
/// its merkle branch at index 0 and, for Case A, its header. Case B proofs
/// carry neither branch nor header: the ticket proves the block.
pub fn assemble(
    job: &AuxJob,
    entry: u16,
    coinbase: &[u8],
    branch: &[Hash],
    header: Option<[u8; 80]>,
) -> Result<AuxProof, ProofError> {
    let item = job
        .entries
        .get(usize::from(entry))
        .ok_or(ProofError::NotInJob)?;
    let head = 47 + usize::from(*coinbase.get(41).ok_or(ProofError::CoinbaseHead)?);
    if coinbase.get(head..head + OUTPUT_LEN) != Some(&job.outputs.commitment[..]) {
        return Err(ProofError::NotInJob);
    }
    let (cb_branch, header) = match item.mode {
        Mode::ShareTarget => (branch.to_vec(), Some(header.ok_or(ProofError::Shape)?)),
        Mode::BlockRequired => (Vec::new(), None),
    };
    let proof = AuxProof {
        leaf: item.leaf,
        height: job.commitment.height,
        nonce: job.commitment.nonce,
        aux_branch: job.tree.branch(item.slot),
        cb_head: coinbase[..head].to_vec(),
        cb_tail: coinbase[head + OUTPUT_LEN..].to_vec(),
        cb_branch,
        header,
    };
    proof.check_shape()?;
    Ok(proof)
}

impl AuxProof {
    /// The rules every v1 proof keeps: `h` matches the aux branch and is at
    /// most 16, the coinbase branch is at most 32 deep, the coinbase parts
    /// fit their length fields, and Case A has a header while Case B has
    /// neither header nor coinbase branch.
    pub fn check_shape(&self) -> Result<(), ProofError> {
        let fits = self.height <= MAX_HEIGHT
            && self.aux_branch.len() == usize::from(self.height)
            && self.cb_branch.len() <= MAX_COINBASE_BRANCH
            && self.cb_head.len() <= usize::from(u16::MAX)
            && self.cb_tail.len() <= usize::from(u16::MAX);
        let mode = match self.leaf.mode {
            Mode::ShareTarget => self.header.is_some(),
            Mode::BlockRequired => self.header.is_none() && self.cb_branch.is_empty(),
        };
        if fits && mode {
            Ok(())
        } else {
            Err(ProofError::Shape)
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, ProofError> {
        self.check_shape()?;
        let mut bytes = Vec::with_capacity(
            181 + 32 * self.aux_branch.len()
                + 5
                + self.cb_head.len()
                + self.cb_tail.len()
                + 32 * self.cb_branch.len()
                + 80,
        );
        bytes.extend_from_slice(&MAGIC);
        bytes.push(VERSION);
        bytes.push(self.leaf.mode.byte());
        bytes.extend_from_slice(&self.leaf.fields());
        bytes.push(self.height);
        bytes.extend_from_slice(&self.nonce.to_le_bytes());
        self.aux_branch
            .iter()
            .for_each(|hash| bytes.extend_from_slice(hash));
        for part in [&self.cb_head, &self.cb_tail] {
            bytes.extend_from_slice(&(part.len() as u16).to_le_bytes());
            bytes.extend_from_slice(part);
        }
        bytes.push(self.cb_branch.len() as u8);
        self.cb_branch
            .iter()
            .for_each(|hash| bytes.extend_from_slice(hash));
        if let Some(header) = &self.header {
            bytes.extend_from_slice(header);
        }
        Ok(bytes)
    }

    /// Reads exactly one canonical v1 proof; anything else is refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProofError> {
        let mut reader = Reader(bytes);
        if reader.take(4)? != MAGIC || reader.byte()? != VERSION {
            return Err(ProofError::Decode);
        }
        let mode = Mode::from_byte(reader.byte()?).ok_or(ProofError::Decode)?;
        let fields: &[u8; FIELDS_LEN] = reader
            .take(FIELDS_LEN)?
            .try_into()
            .map_err(|_| ProofError::Decode)?;
        let leaf = Leaf::from_fields(mode, fields);
        let height = reader.byte()?;
        if height > MAX_HEIGHT {
            return Err(ProofError::Decode);
        }
        let nonce = u32::from_le_bytes(reader.array()?);
        let aux_branch = reader.hashes(usize::from(height))?;
        let cb_head = reader.part()?;
        let cb_tail = reader.part()?;
        let depth = usize::from(reader.byte()?);
        if depth > MAX_COINBASE_BRANCH {
            return Err(ProofError::Decode);
        }
        let cb_branch = reader.hashes(depth)?;
        let header = match mode {
            Mode::ShareTarget => Some(reader.array()?),
            Mode::BlockRequired => None,
        };
        if !reader.0.is_empty() {
            return Err(ProofError::Decode);
        }
        let proof = Self {
            leaf,
            height,
            nonce,
            aux_branch,
            cb_head,
            cb_tail,
            cb_branch,
            header,
        };
        proof.check_shape().map_err(|_| ProofError::Decode)?;
        Ok(proof)
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ProofError> {
        if self.0.len() < count {
            return Err(ProofError::Decode);
        }
        let (taken, rest) = self.0.split_at(count);
        self.0 = rest;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, ProofError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProofError> {
        self.take(N)?.try_into().map_err(|_| ProofError::Decode)
    }

    fn hashes(&mut self, count: usize) -> Result<Vec<Hash>, ProofError> {
        (0..count).map(|_| self.array()).collect()
    }

    fn part(&mut self) -> Result<Vec<u8>, ProofError> {
        let length = u16::from_le_bytes(self.array()?);
        Ok(self.take(usize::from(length))?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stratum_v2::merge::OutPoint;

    fn sample(mode: Mode) -> AuxProof {
        let leaf = match mode {
            Mode::ShareTarget => Leaf::share_target(
                [1; 32],
                OutPoint {
                    txid: [2; 32],
                    vout: 1,
                },
                &[0x51],
                0x1f10_0000,
                None,
                [0; 32],
            ),
            Mode::BlockRequired => Leaf::block_required([1; 32], 3, &[0x51], None, [0; 32]),
        };
        AuxProof {
            leaf,
            height: 2,
            nonce: 0x0102_0304,
            aux_branch: vec![[5; 32], [6; 32]],
            cb_head: vec![7; 80],
            cb_tail: vec![8; 70],
            cb_branch: match mode {
                Mode::ShareTarget => vec![[9; 32]; 3],
                Mode::BlockRequired => Vec::new(),
            },
            header: (mode == Mode::ShareTarget).then_some([10; 80]),
        }
    }

    // #### PR #42
    #[test]
    fn round_trips_and_rejects_trailing_bytes_and_oversize() {
        for mode in [Mode::ShareTarget, Mode::BlockRequired] {
            let proof = sample(mode);
            let bytes = proof.to_bytes().unwrap();
            let header = if mode == Mode::ShareTarget { 80 } else { 0 };
            let depth = proof.cb_branch.len();
            assert_eq!(
                bytes.len(),
                181 + 64 + 2 + 80 + 2 + 70 + 1 + 32 * depth + header
            );
            assert_eq!(bytes[..6], [b'C', b'T', b'M', b'P', 1, mode.byte()]);
            assert_eq!(bytes[6..176], proof.leaf.fields());
            assert_eq!(bytes[176], 2);
            assert_eq!(bytes[177..181], [4, 3, 2, 1]);
            assert_eq!(AuxProof::from_bytes(&bytes).unwrap(), proof);
            // Trailing or missing bytes.
            assert_eq!(
                AuxProof::from_bytes(&[&bytes[..], &[0]].concat()),
                Err(ProofError::Decode)
            );
            for cut in [0, 5, 176, 181, bytes.len() - 1] {
                assert_eq!(AuxProof::from_bytes(&bytes[..cut]), Err(ProofError::Decode));
            }
            // Another magic, version or mode.
            for (index, value) in [(0, b'X'), (4, 2), (5, 0x43)] {
                let mut changed = bytes.clone();
                changed[index] = value;
                assert_eq!(AuxProof::from_bytes(&changed), Err(ProofError::Decode));
            }
            // h above 16, and h that disagrees with the branch.
            let mut tall = bytes.clone();
            tall[176] = 17;
            assert_eq!(AuxProof::from_bytes(&tall), Err(ProofError::Decode));
            let mut short = bytes.clone();
            short[176] = 1;
            assert_eq!(AuxProof::from_bytes(&short), Err(ProofError::Decode));
        }
        // A coinbase branch deeper than 32.
        let mut deep = sample(Mode::ShareTarget);
        deep.cb_branch = vec![[9; 32]; 33];
        assert_eq!(deep.to_bytes(), Err(ProofError::Shape));
        let mut bytes = sample(Mode::ShareTarget).to_bytes().unwrap();
        let depth_at = 181 + 64 + 2 + 80 + 2 + 70;
        assert_eq!(bytes[depth_at], 3);
        bytes[depth_at] = 33;
        bytes.splice(depth_at + 1..depth_at + 1, [9; 30 * 32]);
        assert_eq!(AuxProof::from_bytes(&bytes), Err(ProofError::Decode));
        // Case B carries no header and no coinbase branch.
        let mut b = sample(Mode::BlockRequired);
        b.header = Some([0; 80]);
        assert_eq!(b.to_bytes(), Err(ProofError::Shape));
        let mut b = sample(Mode::BlockRequired);
        b.cb_branch = vec![[0; 32]];
        assert_eq!(b.to_bytes(), Err(ProofError::Shape));
        let mut a = sample(Mode::ShareTarget);
        a.header = None;
        assert_eq!(a.to_bytes(), Err(ProofError::Shape));
        // Oversized coinbase parts do not fit their length fields.
        let mut big = sample(Mode::BlockRequired);
        big.cb_tail = vec![0; usize::from(u16::MAX) + 1];
        assert_eq!(big.to_bytes(), Err(ProofError::Shape));
    }
}
