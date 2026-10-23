//! #### PR #42, merge-mining v2
//! A token's leaf: a 169-byte preimage, hashed as `SHA256(0x00 ‖ preimage)`.
//! A covenant builds it from introspection, so a proof never carries leaf
//! data it could forge.
//!
//! ```text
//! off len field         value
//! 0   4   "CTML"        43 54 4d 4c
//! 4   1   version       02
//! 5   32  category      the token category
//! 37  32  lock_hash     HASH256(the token baton's locking bytecode)
//! 69  32  state_hash    token-defined; changes only on a successful claim
//! 101 32  payout_hash   HASH256(the payout output's locking bytecode)
//! 133 4   target_bits   the token's compact target, u32 LE
//! 137 32  ext           token-defined (a split or a donation), or 00 x 32
//! ```
//!
//! The state hash stops replays (a claim moves it), the payout hash stops
//! redirection, and the target stops a claim at an easier one. v1's mode
//! byte, outpoint anchor and split fields are gone.

use super::{super::template::double_sha256, sha256, Hash};

pub const TAG: [u8; 4] = *b"CTML";
pub const VERSION: u8 = 2;
pub const LEN: usize = 169;
/// The domain byte a leaf hash starts with (a node's is 0x01).
pub const LEAF_PREFIX: u8 = 0x00;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub category: Hash,
    pub lock_hash: Hash,
    pub state_hash: Hash,
    pub payout_hash: Hash,
    pub target_bits: u32,
    pub ext: Hash,
}

impl Leaf {
    /// The leaf of a token in state `state_hash` behind a baton locked by
    /// `lock_hash`, paying the locking bytecode `payout`.
    pub fn new(
        category: Hash,
        lock_hash: Hash,
        state_hash: Hash,
        payout: &[u8],
        target_bits: u32,
        ext: Hash,
    ) -> Self {
        Self {
            category,
            lock_hash,
            state_hash,
            payout_hash: double_sha256(payout),
            target_bits,
            ext,
        }
    }

    pub fn preimage(&self) -> [u8; LEN] {
        let mut bytes = [0; LEN];
        bytes[..4].copy_from_slice(&TAG);
        bytes[4] = VERSION;
        bytes[5..37].copy_from_slice(&self.category);
        bytes[37..69].copy_from_slice(&self.lock_hash);
        bytes[69..101].copy_from_slice(&self.state_hash);
        bytes[101..133].copy_from_slice(&self.payout_hash);
        bytes[133..137].copy_from_slice(&self.target_bits.to_le_bytes());
        bytes[137..].copy_from_slice(&self.ext);
        bytes
    }

    /// `SHA256(0x00 ‖ preimage)`: the domain byte keeps a leaf from ever
    /// being read as a node.
    pub fn hash(&self) -> Hash {
        let mut bytes = [0; LEN + 1];
        bytes[0] = LEAF_PREFIX;
        bytes[1..].copy_from_slice(&self.preimage());
        sha256(&bytes)
    }
}

/// `HASH256` of a baton's locking bytecode: a leaf's `lock_hash`.
pub fn lock_hash(locking_bytecode: &[u8]) -> Hash {
    double_sha256(locking_bytecode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Leaf {
        Leaf::new(
            [0xc1; 32],
            [0xa2; 32],
            [0xb3; 32],
            &[0x76, 0xa9],
            0x1f10_0000,
            [0xe5; 32],
        )
    }

    // #### merge-mining v2
    #[test]
    fn preimage_layout_and_every_field_changes_the_hash() {
        let leaf = sample();
        let bytes = leaf.preimage();
        assert_eq!(bytes.len(), 169);
        assert_eq!(bytes[..4], *b"CTML");
        assert_eq!(bytes[4], 2);
        assert_eq!(bytes[5..37], [0xc1; 32]);
        assert_eq!(bytes[37..69], [0xa2; 32]);
        assert_eq!(bytes[69..101], [0xb3; 32]);
        assert_eq!(bytes[101..133], double_sha256(&[0x76, 0xa9]));
        assert_eq!(bytes[133..137], [0, 0, 0x10, 0x1f]);
        assert_eq!(bytes[137..169], [0xe5; 32]);
        assert_eq!(leaf.hash(), sha256(&[&[0][..], &bytes[..]].concat()));
        assert_ne!(leaf.hash(), sha256(&bytes));
        assert_eq!(lock_hash(&[0x51]), double_sha256(&[0x51]));
        // Every field is bound.
        let changes: [fn(&mut Leaf); 6] = [
            |l| l.category[31] ^= 1,
            |l| l.lock_hash[0] ^= 1,
            |l| l.state_hash[7] ^= 1,
            |l| l.payout_hash[5] ^= 1,
            |l| l.target_bits ^= 1,
            |l| l.ext[31] ^= 0x80,
        ];
        for (index, change) in changes.into_iter().enumerate() {
            let mut changed = leaf;
            change(&mut changed);
            assert_ne!(changed.hash(), leaf.hash(), "field {index}");
        }
    }
}
