//! #### PR #42
//! The commitment: coinbase output 0, value 0, an `OP_RETURN` of 42 bytes.
//!
//! ```text
//! off len value
//! 0   8   00 00 00 00 00 00 00 00   value 0
//! 8   1   2c                        script length 44
//! 9   1   6a                        OP_RETURN
//! 10  1   2a                        push 42
//! 11  4   43 54 4d 4d               "CTMM"
//! 15  1   01                        version
//! 16  32  aux_root                  as hashed, not reversed
//! 48  1   h                         0..=16
//! 49  4   aux_nonce                 u32 LE
//! ```
//!
//! A covenant finds it without a search: `cb_head = cb[0 .. 47+L]` with
//! `L = cb[41] <= 100` (version, one input with the null prevout, the
//! script, the sequence and a one-byte output count below 0xfd), and output
//! 0 is `cb[47+L .. 100+L]`. Only a coinbase has the null prevout, and the
//! coinbase is at least 104 bytes, so it never passes as a merkle node.

use super::{tree::MAX_HEIGHT, Hash};

pub const MAGIC: [u8; 4] = *b"CTMM";
pub const VERSION: u8 = 1;
/// The whole output: value, script length and script.
pub const OUTPUT_LEN: usize = 53;
/// The locking script alone.
pub const SCRIPT_LEN: usize = 44;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuxCommitment {
    pub root: Hash,
    pub height: u8,
    pub nonce: u32,
}

impl AuxCommitment {
    /// The 44-byte locking script.
    pub fn script(&self) -> [u8; SCRIPT_LEN] {
        let mut script = [0; SCRIPT_LEN];
        script[0] = 0x6a;
        script[1] = 0x2a;
        script[2..6].copy_from_slice(&MAGIC);
        script[6] = VERSION;
        script[7..39].copy_from_slice(&self.root);
        script[39] = self.height;
        script[40..].copy_from_slice(&self.nonce.to_le_bytes());
        script
    }

    /// The 53-byte output as it appears in the coinbase.
    pub fn output(&self) -> [u8; OUTPUT_LEN] {
        let mut output = [0; OUTPUT_LEN];
        output[8] = SCRIPT_LEN as u8;
        output[9..].copy_from_slice(&self.script());
        output
    }

    /// Reads a 53-byte output; anything else (another length, a value, a
    /// magic, a version or `h > 16`) is not a commitment.
    pub fn parse(output: &[u8]) -> Option<Self> {
        let (value, rest) = output.split_first_chunk::<8>()?;
        let (&length, script) = rest.split_first()?;
        if *value != [0; 8] || usize::from(length) != SCRIPT_LEN {
            return None;
        }
        Self::parse_script(script)
    }

    /// Reads a 44-byte locking script.
    pub fn parse_script(script: &[u8]) -> Option<Self> {
        let script: &[u8; SCRIPT_LEN] = script.try_into().ok()?;
        if script[..2] != [0x6a, 0x2a]
            || script[2..6] != MAGIC
            || script[6] != VERSION
            || script[39] > MAX_HEIGHT
        {
            return None;
        }
        Some(Self {
            root: script[7..39].try_into().ok()?,
            height: script[39],
            nonce: u32::from_le_bytes(script[40..].try_into().ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #42
    #[test]
    fn output_matches_the_golden_bytes() {
        let commitment = AuxCommitment {
            root: [0x11; 32],
            height: 3,
            nonce: 7,
        };
        let golden = concat!(
            "0000000000000000",
            "2c",
            "6a",
            "2a",
            "43544d4d",
            "01",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "03",
            "07000000"
        );
        assert_eq!(hex::encode(commitment.output()), golden);
        assert_eq!(commitment.output()[9..], commitment.script());
        assert_eq!(AuxCommitment::parse(&commitment.output()), Some(commitment));
        assert_eq!(
            AuxCommitment::parse_script(&commitment.script()),
            Some(commitment)
        );
        let output = commitment.output();
        // Another length, value, script length, magic, version or height.
        assert_eq!(AuxCommitment::parse(&output[..52]), None);
        assert_eq!(AuxCommitment::parse(&[&output[..], &[0]].concat()), None);
        for (index, value) in [
            (0, 1),
            (8, 0x2d),
            (9, 0x6b),
            (10, 0x2b),
            (13, b'X'),
            (15, 2),
        ] {
            let mut changed = output;
            changed[index] = value;
            assert_eq!(AuxCommitment::parse(&changed), None, "byte {index}");
        }
        let mut tall = output;
        tall[48] = MAX_HEIGHT + 1;
        assert_eq!(AuxCommitment::parse(&tall), None);
        tall[48] = MAX_HEIGHT;
        assert!(AuxCommitment::parse(&tall).is_some());
    }
}
