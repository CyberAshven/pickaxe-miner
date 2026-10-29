//! #### PR #42, merge-mining v2
//! `AuxProof` v2: what a token win hands to the proof journal and a claim.
//! Little-endian, canonical (exact lengths, nothing trailing).
//!
//! ```text
//! 0    4     "CTMP"
//! 4    1     02
//! 5    80    header
//! 85   1     h (at most 16)
//! 86   4     salt, u32 LE
//! 90   32h   aux_branch, leaf to root
//! ..   2+n   u16 LE n, cb_head (47+L bytes: the coinbase up to output 0)
//! ..   2+n   u16 LE n, cb_tail (outputs 1.. and the locktime)
//! ..   1     d (at most 25)
//! ..   32d   cb_branch, the coinbase at index 0
//! ```
//!
//! Output 0 and the leaf are never carried: a verifier rebuilds both, the
//! leaf from the claim and output 0 from the root it folds. The limit
//! `d <= 25` belongs to version 02. Version 03 is reserved for a
//! block-required mode.

use super::{commitment::OUTPUT_LEN, set::AuxJob, tree::MAX_HEIGHT, Hash};
use std::fmt;

pub const MAGIC: [u8; 4] = *b"CTMP";
pub const VERSION: u8 = 2;
/// The deepest coinbase branch in version 02: 2^25 transactions.
pub const MAX_COINBASE_BRANCH: usize = 25;
/// The bytes of a proof besides its branches and coinbase parts.
pub const FIXED_LEN: usize = 4 + 1 + 80 + 1 + 4 + 2 + 2 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofError {
    /// The bytes are not a canonical v2 proof.
    Decode,
    /// The parts do not fit their length fields or limits.
    Shape,
    /// The aux tree is taller than the token allows.
    Height,
    /// The token's compact target is not a canonical one.
    Target,
    /// The coinbase does not start with the fixed-shape head.
    CoinbaseHead,
    /// The coinbase has no room for its locktime.
    CoinbaseTail,
    /// The coinbase does not reach the header's merkle root.
    MerkleRoot,
    /// The header's hash does not meet the token's target.
    Work,
    /// The job has no such entry, or the coinbase lacks its commitment.
    NotInJob,
}

impl fmt::Display for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Decode => "not a canonical merge-mining proof",
            Self::Shape => "merge-mining proof parts exceed their limits",
            Self::Height => "merge-mining tree is taller than the token allows",
            Self::Target => "invalid token target",
            Self::CoinbaseHead => "merge-mining coinbase head is malformed",
            Self::CoinbaseTail => "merge-mining coinbase tail is too short",
            Self::MerkleRoot => "merge-mining coinbase does not reach the header",
            Self::Work => "header does not meet the token target",
            Self::NotInJob => "token win does not belong to this job",
        })
    }
}

impl std::error::Error for ProofError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxProof {
    pub header: [u8; 80],
    pub height: u8,
    pub salt: u32,
    pub aux_branch: Vec<Hash>,
    pub cb_head: Vec<u8>,
    pub cb_tail: Vec<u8>,
    pub cb_branch: Vec<Hash>,
}

/// Builds the proof of entry `entry` of `job` from the share's coinbase,
/// its merkle branch at index 0 and its header.
pub fn assemble(
    job: &AuxJob,
    entry: u16,
    coinbase: &[u8],
    branch: &[Hash],
    header: [u8; 80],
) -> Result<AuxProof, ProofError> {
    let item = job
        .entries
        .get(usize::from(entry))
        .ok_or(ProofError::NotInJob)?;
    let head = 47 + usize::from(*coinbase.get(41).ok_or(ProofError::CoinbaseHead)?);
    if coinbase.get(head..head + OUTPUT_LEN) != Some(&job.output[..]) {
        return Err(ProofError::NotInJob);
    }
    let proof = AuxProof {
        header,
        height: job.commitment.height,
        salt: job.commitment.salt,
        aux_branch: job.tree.branch(item.slot),
        cb_head: coinbase[..head].to_vec(),
        cb_tail: coinbase[head + OUTPUT_LEN..].to_vec(),
        cb_branch: branch.to_vec(),
    };
    proof.check_shape()?;
    Ok(proof)
}

impl AuxProof {
    /// The rules every v2 proof keeps: `h` matches the aux branch and is at
    /// most 16, the coinbase branch is at most 25 deep, and the coinbase
    /// parts fit their length fields.
    pub fn check_shape(&self) -> Result<(), ProofError> {
        if self.height <= MAX_HEIGHT
            && self.aux_branch.len() == usize::from(self.height)
            && self.cb_branch.len() <= MAX_COINBASE_BRANCH
            && self.cb_head.len() <= usize::from(u16::MAX)
            && self.cb_tail.len() <= usize::from(u16::MAX)
        {
            Ok(())
        } else {
            Err(ProofError::Shape)
        }
    }

    /// The encoded length.
    pub fn encoded_len(&self) -> usize {
        FIXED_LEN
            + 32 * self.aux_branch.len()
            + self.cb_head.len()
            + self.cb_tail.len()
            + 32 * self.cb_branch.len()
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, ProofError> {
        self.check_shape()?;
        let mut bytes = Vec::with_capacity(self.encoded_len());
        bytes.extend_from_slice(&MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&self.header);
        bytes.push(self.height);
        bytes.extend_from_slice(&self.salt.to_le_bytes());
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
        Ok(bytes)
    }

    /// Reads exactly one canonical v2 proof; anything else is refused.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProofError> {
        let mut reader = Reader(bytes);
        if reader.take(4)? != MAGIC || reader.byte()? != VERSION {
            return Err(ProofError::Decode);
        }
        let header = reader.array()?;
        let height = reader.byte()?;
        if height > MAX_HEIGHT {
            return Err(ProofError::Decode);
        }
        let salt = u32::from_le_bytes(reader.array()?);
        let aux_branch = reader.hashes(usize::from(height))?;
        let cb_head = reader.part()?;
        let cb_tail = reader.part()?;
        let depth = usize::from(reader.byte()?);
        if depth > MAX_COINBASE_BRANCH {
            return Err(ProofError::Decode);
        }
        let cb_branch = reader.hashes(depth)?;
        if !reader.0.is_empty() {
            return Err(ProofError::Decode);
        }
        Ok(Self {
            header,
            height,
            salt,
            aux_branch,
            cb_head,
            cb_tail,
            cb_branch,
        })
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

    fn sample() -> AuxProof {
        AuxProof {
            header: [10; 80],
            height: 2,
            salt: 0x0102_0304,
            aux_branch: vec![[5; 32], [6; 32]],
            cb_head: vec![7; 80],
            cb_tail: vec![8; 70],
            cb_branch: vec![[9; 32]; 3],
        }
    }

    // #### merge-mining v2
    #[test]
    fn round_trips_and_rejects_trailing_bytes_and_oversize() {
        let proof = sample();
        let bytes = proof.to_bytes().unwrap();
        assert_eq!(bytes.len(), 95 + 64 + 80 + 70 + 96);
        assert_eq!(bytes.len(), proof.encoded_len());
        assert_eq!(bytes[..5], [b'C', b'T', b'M', b'P', 2]);
        assert_eq!(bytes[5..85], [10; 80]);
        assert_eq!(bytes[85], 2);
        assert_eq!(bytes[86..90], [4, 3, 2, 1]);
        assert_eq!(bytes[90..122], [5; 32]);
        assert_eq!(bytes[154..156], [80, 0]);
        assert_eq!(AuxProof::from_bytes(&bytes).unwrap(), proof);
        // Trailing or missing bytes.
        assert_eq!(
            AuxProof::from_bytes(&[&bytes[..], &[0]].concat()),
            Err(ProofError::Decode)
        );
        for cut in [0, 4, 5, 85, 90, bytes.len() - 1] {
            assert_eq!(AuxProof::from_bytes(&bytes[..cut]), Err(ProofError::Decode));
        }
        // Another magic or version.
        for (index, value) in [(0, b'X'), (4, 1), (4, 3)] {
            let mut changed = bytes.clone();
            changed[index] = value;
            assert_eq!(AuxProof::from_bytes(&changed), Err(ProofError::Decode));
        }
        // h above 16, and h that disagrees with the branch.
        let mut tall = bytes.clone();
        tall[85] = 17;
        assert_eq!(AuxProof::from_bytes(&tall), Err(ProofError::Decode));
        let mut short = bytes.clone();
        short[85] = 1;
        assert_eq!(AuxProof::from_bytes(&short), Err(ProofError::Decode));
        // A coinbase branch deeper than 25.
        let mut deep = sample();
        deep.cb_branch = vec![[9; 32]; 26];
        assert_eq!(deep.to_bytes(), Err(ProofError::Shape));
        deep.cb_branch.pop();
        assert!(deep.to_bytes().is_ok());
        let mut bytes = sample().to_bytes().unwrap();
        let depth_at = bytes.len() - 97;
        assert_eq!(bytes[depth_at], 3);
        bytes[depth_at] = 26;
        bytes.extend([9; 23 * 32]);
        assert_eq!(AuxProof::from_bytes(&bytes), Err(ProofError::Decode));
        // Oversized coinbase parts do not fit their length fields.
        let mut big = sample();
        big.cb_tail = vec![0; usize::from(u16::MAX) + 1];
        assert_eq!(big.to_bytes(), Err(ProofError::Shape));
        // A branch that disagrees with h.
        let mut odd = sample();
        odd.height = 3;
        assert_eq!(odd.to_bytes(), Err(ProofError::Shape));
    }
}
