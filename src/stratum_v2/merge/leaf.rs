//! #### PR #42
//! A token's leaf: 176 bytes, hashed once with SHA-256 (three compression
//! blocks). A covenant builds it from introspection, so a proof never
//! supplies leaf data it could forge.
//!
//! ```text
//! off len field         Case A (share target)              Case B (block required)
//! 0   4   "CTML"        43 54 4d 4c
//! 4   1   version       01
//! 5   1   mode          41                                  42
//! 6   32  category      the baton's token category (input 0)
//! 38  32  anchor_hash   the baton's outpoint hash (input 0) 00 x 32
//! 70  4   anchor_index  the baton's outpoint index, u32 LE  the ticket's vout in the coinbase, u32 LE
//! 74  32  payout_hash   HASH256(output 1 locking bytecode)
//! 106 4   target_bits   the token's compact target, u32 LE  00000000
//! 110 2   split_bps     u16 LE, 0..=10000
//! 112 32  split_hash    HASH256(output 2 locking bytecode), or 00 x 32 when split_bps is 0
//! 144 32  ext           token-defined, 00 x 32 if unused
//! ```
//!
//! The anchor stops replays (an outpoint is spent once), the payout and the
//! split stop redirection, and the target stops a claim at an easier one.

use super::{super::template::double_sha256, sha256, Hash, OutPoint};

pub const TAG: [u8; 4] = *b"CTML";
pub const VERSION: u8 = 1;
pub const LEN: usize = 176;
/// The leaf fields a proof carries: preimage bytes 6..176.
pub const FIELDS_LEN: usize = LEN - 6;
/// The largest split: the whole reward.
pub const MAX_SPLIT_BPS: u16 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Mode {
    /// Case A: any share whose hash meets the token's target wins.
    ShareTarget = 0x41,
    /// Case B: only a found BCH block wins, claimed through its ticket.
    BlockRequired = 0x42,
}

impl Mode {
    pub fn byte(self) -> u8 {
        self as u8
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x41 => Some(Self::ShareTarget),
            0x42 => Some(Self::BlockRequired),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Leaf {
    pub mode: Mode,
    pub category: Hash,
    pub anchor_hash: Hash,
    pub anchor_index: u32,
    pub payout_hash: Hash,
    pub target_bits: u32,
    pub split_bps: u16,
    pub split_hash: Hash,
    pub ext: Hash,
}

/// The split fields: the share and `HASH256` of its locking bytecode, or
/// zeros when there is no split.
fn split_fields(split: Option<(u16, &[u8])>) -> (u16, Hash) {
    match split {
        Some((bps, script)) if bps > 0 => (bps, double_sha256(script)),
        _ => (0, [0; 32]),
    }
}

impl Leaf {
    /// A Case A leaf: anchored to the baton's outpoint, judged at
    /// `target_bits`.
    pub fn share_target(
        category: Hash,
        anchor: OutPoint,
        payout: &[u8],
        target_bits: u32,
        split: Option<(u16, &[u8])>,
        ext: Hash,
    ) -> Self {
        let (split_bps, split_hash) = split_fields(split);
        Self {
            mode: Mode::ShareTarget,
            category,
            anchor_hash: anchor.txid,
            anchor_index: anchor.vout,
            payout_hash: double_sha256(payout),
            target_bits,
            split_bps,
            split_hash,
            ext,
        }
    }

    /// A Case B leaf: anchored to its ticket's vout in this coinbase.
    pub fn block_required(
        category: Hash,
        ticket_vout: u32,
        payout: &[u8],
        split: Option<(u16, &[u8])>,
        ext: Hash,
    ) -> Self {
        let (split_bps, split_hash) = split_fields(split);
        Self {
            mode: Mode::BlockRequired,
            category,
            anchor_hash: [0; 32],
            anchor_index: ticket_vout,
            payout_hash: double_sha256(payout),
            target_bits: 0,
            split_bps,
            split_hash,
            ext,
        }
    }

    pub fn preimage(&self) -> [u8; LEN] {
        let mut bytes = [0; LEN];
        bytes[..4].copy_from_slice(&TAG);
        bytes[4] = VERSION;
        bytes[5] = self.mode.byte();
        bytes[6..].copy_from_slice(&self.fields());
        bytes
    }

    /// Single SHA-256 of the preimage. The tree's height is fixed by the
    /// commitment, so a leaf can never be read as a node.
    pub fn hash(&self) -> Hash {
        sha256(&self.preimage())
    }

    /// Preimage bytes 6..176, as a proof carries them.
    pub fn fields(&self) -> [u8; FIELDS_LEN] {
        let mut bytes = [0; FIELDS_LEN];
        bytes[..32].copy_from_slice(&self.category);
        bytes[32..64].copy_from_slice(&self.anchor_hash);
        bytes[64..68].copy_from_slice(&self.anchor_index.to_le_bytes());
        bytes[68..100].copy_from_slice(&self.payout_hash);
        bytes[100..104].copy_from_slice(&self.target_bits.to_le_bytes());
        bytes[104..106].copy_from_slice(&self.split_bps.to_le_bytes());
        bytes[106..138].copy_from_slice(&self.split_hash);
        bytes[138..].copy_from_slice(&self.ext);
        bytes
    }

    pub fn from_fields(mode: Mode, bytes: &[u8; FIELDS_LEN]) -> Self {
        let hash = |at: usize| -> Hash {
            let mut out = [0; 32];
            out.copy_from_slice(&bytes[at..at + 32]);
            out
        };
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        Self {
            mode,
            category: hash(0),
            anchor_hash: hash(32),
            anchor_index: word(64),
            payout_hash: hash(68),
            target_bits: word(100),
            split_bps: u16::from_le_bytes([bytes[104], bytes[105]]),
            split_hash: hash(106),
            ext: hash(138),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Leaf {
        Leaf::share_target(
            [0xc1; 32],
            OutPoint {
                txid: [0xa2; 32],
                vout: 0x0403_0201,
            },
            &[0x76, 0xa9],
            0x1f10_0000,
            Some((100, &[0x51])),
            [0xe5; 32],
        )
    }

    // #### PR #42
    #[test]
    fn preimage_layout_and_every_field_changes_the_hash() {
        let leaf = sample();
        let bytes = leaf.preimage();
        assert_eq!(bytes.len(), 176);
        assert_eq!(bytes[..4], *b"CTML");
        assert_eq!(bytes[4], 1);
        assert_eq!(bytes[5], 0x41);
        assert_eq!(bytes[6..38], [0xc1; 32]);
        assert_eq!(bytes[38..70], [0xa2; 32]);
        assert_eq!(bytes[70..74], [1, 2, 3, 4]);
        assert_eq!(bytes[74..106], double_sha256(&[0x76, 0xa9]));
        assert_eq!(bytes[106..110], [0, 0, 0x10, 0x1f]);
        assert_eq!(bytes[110..112], [100, 0]);
        assert_eq!(bytes[112..144], double_sha256(&[0x51]));
        assert_eq!(bytes[144..176], [0xe5; 32]);
        assert_eq!(leaf.hash(), sha256(&bytes));
        assert_eq!(Leaf::from_fields(Mode::ShareTarget, &leaf.fields()), leaf);
        // Case B: no anchor hash and no target; the ticket's vout instead.
        let b = Leaf::block_required([0xc1; 32], 3, &[0x76, 0xa9], None, [0; 32]);
        let b_bytes = b.preimage();
        assert_eq!(b_bytes[5], 0x42);
        assert_eq!(b_bytes[38..70], [0; 32]);
        assert_eq!(b_bytes[70..74], [3, 0, 0, 0]);
        assert_eq!(b_bytes[106..144], [0; 38]);
        // A zero split carries no script hash.
        assert_eq!(
            Leaf::block_required([0xc1; 32], 3, &[0x76, 0xa9], Some((0, &[0x51])), [0; 32]),
            b
        );
        // Every field is bound.
        let changes: [fn(&mut Leaf); 9] = [
            |l| l.mode = Mode::BlockRequired,
            |l| l.category[31] ^= 1,
            |l| l.anchor_hash[0] ^= 1,
            |l| l.anchor_index ^= 1 << 31,
            |l| l.payout_hash[5] ^= 1,
            |l| l.target_bits ^= 1,
            |l| l.split_bps ^= 1,
            |l| l.split_hash[9] ^= 1,
            |l| l.ext[31] ^= 0x80,
        ];
        for (index, change) in changes.into_iter().enumerate() {
            let mut changed = leaf;
            change(&mut changed);
            assert_ne!(changed.hash(), leaf.hash(), "field {index}");
        }
        assert_eq!(Mode::from_byte(0x41), Some(Mode::ShareTarget));
        assert_eq!(Mode::from_byte(0x42), Some(Mode::BlockRequired));
        assert_eq!(Mode::from_byte(0x43), None);
    }
}
