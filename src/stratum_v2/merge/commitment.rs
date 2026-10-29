//! #### PR #42, merge-mining v2
//! The commitment: coinbase output 0, value 0, an `OP_RETURN` of 42 bytes.
//!
//! ```text
//! off len value
//! 0   8   00 00 00 00 00 00 00 00   value 0
//! 8   1   2c                        script length 44
//! 9   1   6a                        OP_RETURN
//! 10  1   2a                        push 42
//! 11  4   43 54 4d 4d               "CTMM"
//! 15  1   02                        version
//! 16  32  root                      as hashed, not reversed
//! 48  1   h                         0..=16
//! 49  4   salt                      u32 LE
//! ```
//!
//! The empty commitment is the zero root with `h = 0` and `salt = 0`; a zero
//! root with any other `h` or salt is not a commitment.
//!
//! A covenant finds output 0 without a search. The coinbase head is fixed
//! shape: version (4), `01`, 32 zero bytes, `ffffffff`, `L` (at most 100),
//! the script-sig (`L`), the sequence (4) and a one-byte output count
//! (1..=0xfc), so `cb_head = cb[0 .. 47+L]` with `L = cb[41]`, and output 0
//! is `cb[47+L .. 100+L]`. Only a coinbase has the null prevout, and the
//! coinbase is at least 104 bytes, so it never passes as a merkle node.

use super::{tree::MAX_HEIGHT, Hash};

pub const MAGIC: [u8; 4] = *b"CTMM";
pub const VERSION: u8 = 2;
/// The whole output: value, script length and script.
pub const OUTPUT_LEN: usize = 53;
/// The locking script alone.
pub const SCRIPT_LEN: usize = 44;
/// The longest script-sig a coinbase head may carry.
pub const MAX_SCRIPT_SIG: usize = 100;

/// The coinbase head's bytes 4..41: one input with the null prevout.
pub const NULL_INPUT: [u8; 37] = {
    let mut input = [0xff; 37];
    input[0] = 1;
    let mut index = 1;
    while index < 33 {
        input[index] = 0;
        index += 1;
    }
    input
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuxCommitment {
    pub root: Hash,
    pub height: u8,
    pub salt: u32,
}

impl AuxCommitment {
    /// The empty commitment: no token. Jobs on other people's devices carry
    /// it (see docs/merge-mining.md, "Pool and role rules").
    pub const EMPTY: Self = Self {
        root: [0; 32],
        height: 0,
        salt: 0,
    };

    pub fn is_empty(&self) -> bool {
        *self == Self::EMPTY
    }

    /// The 44-byte locking script.
    pub fn script(&self) -> [u8; SCRIPT_LEN] {
        let mut script = [0; SCRIPT_LEN];
        script[0] = 0x6a;
        script[1] = 0x2a;
        script[2..6].copy_from_slice(&MAGIC);
        script[6] = VERSION;
        script[7..39].copy_from_slice(&self.root);
        script[39] = self.height;
        script[40..].copy_from_slice(&self.salt.to_le_bytes());
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
    /// magic, a version, `h > 16`, or a zero root with a height or salt) is
    /// not a commitment.
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
        let commitment = Self {
            root: script[7..39].try_into().ok()?,
            height: script[39],
            salt: u32::from_le_bytes(script[40..].try_into().ok()?),
        };
        if commitment.root == [0; 32] && !commitment.is_empty() {
            return None;
        }
        Some(commitment)
    }
}

/// A coinbase head that is not the fixed shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadError;

/// Checks a coinbase head (`cb[0 .. 47+L]`): its length is `47 + head[41]`,
/// `head[41]` is at most 100, `head[4..41]` is one input with the null
/// prevout, and its last byte (the output count) is 1..=0xfc.
pub fn check_head(head: &[u8]) -> Result<(), HeadError> {
    let length = usize::from(*head.get(41).ok_or(HeadError)?);
    if length > MAX_SCRIPT_SIG
        || head.len() != 47 + length
        || head[4..41] != NULL_INPUT
        || !(1..0xfd).contains(&head[head.len() - 1])
    {
        return Err(HeadError);
    }
    Ok(())
}

/// The length of a coinbase's fixed-shape head (`47 + L`), or `None` when
/// the coinbase does not start with one.
pub fn head_len(coinbase: &[u8]) -> Option<usize> {
    let length = 47 + usize::from(*coinbase.get(41)?);
    check_head(coinbase.get(..length)?).ok()?;
    Some(length)
}

/// Output 0 of a coinbase, read right after its fixed-shape head.
pub fn output_zero(coinbase: &[u8]) -> Option<&[u8]> {
    let head = head_len(coinbase)?;
    coinbase.get(head..head + OUTPUT_LEN)
}

/// #### merge-mining v2: the own-root check
/// What: before devices hash a job, the coinbase they will hash must carry
/// exactly `expected` as output 0, after a fixed-shape head.
/// Why: Pickaxe merge-mines only its own root; a coinbase whose output 0 is
/// anything else (another root, a moved output, a malformed head) cannot
/// prove a win for this server's leaves, and must not count as token work.
/// Look here if: token merge mining is refused for a job and the dashboard
/// says output 0 is not this server's commitment.
pub fn check_own_root(coinbase: &[u8], expected: &AuxCommitment) -> Result<(), &'static str> {
    match output_zero(coinbase) {
        Some(output) if output == expected.output() => Ok(()),
        Some(_) => Err("output 0 is not this server's token commitment"),
        None => Err("the coinbase head is not the fixed merge-mining shape"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### merge-mining v2
    // What: output 0's bytes for a sample root; every fixed byte is checked.
    // Look here if: the commitment layout changes.
    #[test]
    fn output_matches_the_golden_bytes() {
        let commitment = AuxCommitment {
            root: [0x11; 32],
            height: 3,
            salt: 7,
        };
        let golden = concat!(
            "0000000000000000",
            "2c",
            "6a",
            "2a",
            "43544d4d",
            "02",
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
            (15, 1),
            (15, 3),
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

    // #### merge-mining v2: the empty commitment (golden vector, repeated in
    // docs/merge-mining.md)
    #[test]
    fn the_empty_commitment_has_a_zero_root_height_and_salt() {
        let golden = concat!(
            "0000000000000000",
            "2c6a2a43544d4d02",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "00",
            "00000000"
        );
        assert_eq!(hex::encode(AuxCommitment::EMPTY.output()), golden);
        assert!(AuxCommitment::EMPTY.is_empty());
        assert_eq!(
            AuxCommitment::parse(&AuxCommitment::EMPTY.output()),
            Some(AuxCommitment::EMPTY)
        );
        // A zero root is valid only with h = 0 and salt = 0.
        for (height, salt) in [(1, 0), (0, 1), (16, 9)] {
            let zero = AuxCommitment {
                root: [0; 32],
                height,
                salt,
            };
            assert_eq!(
                AuxCommitment::parse(&zero.output()),
                None,
                "{height} {salt}"
            );
        }
    }

    // #### merge-mining v2: the fixed-shape head and the own-root check
    #[test]
    fn output_zero_follows_the_fixed_shape_head() {
        let commitment = AuxCommitment {
            root: [0x42; 32],
            height: 1,
            salt: 5,
        };
        let mut coinbase = 2u32.to_le_bytes().to_vec();
        coinbase.extend(NULL_INPUT);
        coinbase.push(5);
        coinbase.extend([3, 1, 2, 3, 0]);
        coinbase.extend(u32::MAX.to_le_bytes());
        coinbase.push(2);
        coinbase.extend(commitment.output());
        coinbase.extend([0x10, 0, 0, 0, 0, 0, 0, 0, 1, 0x51]);
        coinbase.extend([0; 4]);
        assert_eq!(head_len(&coinbase), Some(52));
        assert_eq!(output_zero(&coinbase), Some(&commitment.output()[..]));
        assert_eq!(check_own_root(&coinbase, &commitment), Ok(()));
        let other = AuxCommitment {
            salt: 6,
            ..commitment
        };
        assert!(check_own_root(&coinbase, &other).is_err());
        // A non-null prevout, an output count of 0 or 0xfd, L above 100.
        for (index, value) in [(5, 1), (51, 0), (51, 0xfd), (41, 101)] {
            let mut changed = coinbase.clone();
            changed[index] = value;
            assert!(check_own_root(&changed, &commitment).is_err(), "{index}");
        }
        assert!(check_head(&coinbase[..52]).is_ok());
        assert!(check_head(&coinbase[..51]).is_err());
        assert!(check_head(&coinbase[..53]).is_err());
    }
}
