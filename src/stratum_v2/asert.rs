//! #### PR #48
//! BCH's aserti3-2d difficulty: the bits a node requires of the next block,
//! from the network's anchor and the parent's height and time. Integer-exact
//! as `CalculateASERT` and `GetNextASERTWorkRequired` in Bitcoin Cash Node
//! v29.2.0 (`src/pow.cpp`), written here from that reference and the
//! aserti3-2d specification. Only Chipnet's anchor is used: the server
//! needs it to tell whether the limit's bits are allowed before Chipnet's
//! 20-minute point (see `BchTemplate::wrong_bits`).

use super::template::compact_target;
use num_bigint::BigUint;
use num_traits::Zero;

/// An ASERT anchor as BCHN's chain parameters fix it (`asertAnchorParams`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub height: u32,
    pub bits: u32,
    /// The time of the anchor block's parent.
    pub parent_time: i64,
    /// Seconds behind (ahead of) schedule that double (halve) the target.
    pub half_life: i64,
}

/// Chipnet's anchor, the same as testnet4's (BCHN v29.2.0
/// `src/chainparams.cpp`, `CChipNetParams`): block 16844 on bits
/// 0x1d00ffff, its parent at 1605451779, and a one-hour half-life.
pub const CHIPNET: Anchor = Anchor {
    height: 16_844,
    bits: 0x1d00_ffff,
    parent_time: 1_605_451_779,
    half_life: 60 * 60,
};

/// The target spacing, 10 minutes, on every BCH network.
const SPACING: i64 = 600;

/// The proof-of-work limit on Chipnet and mainnet, 2^224 - 1
/// (`00000000ffff…ff`), whose bits are 0x1d00ffff.
fn pow_limit() -> BigUint {
    (BigUint::from(1u8) << 224u32) - 1u8
}

// #### PR #48: ASERT's next bits
// What: the bits ASERT gives the block after a parent at `parent_height`
// and `parent_time` (before any testnet rule such as Chipnet's 20-minute
// one). None where BCHN would assert instead: a parent below the anchor, or
// a time so far from the schedule that the exponent's fixed point would
// overflow.
// Why: the limit's bits are valid before Chipnet's 20-minute point only when
// ASERT itself reaches the limit.
// Look here if: a Chipnet block on the limit's bits is refused or sent
// wrongly, or BCHN changes ASERT.
pub fn next_bits(anchor: &Anchor, parent_height: u32, parent_time: u32) -> Option<u32> {
    let height_diff = i64::from(parent_height.checked_sub(anchor.height)?);
    let time_diff = i64::from(parent_time) - anchor.parent_time;
    let reference = BigUint::from_bytes_le(&compact_target(anchor.bits).ok()?);
    let target = calculate(
        &reference,
        time_diff,
        height_diff,
        &pow_limit(),
        anchor.half_life,
    )?;
    Some(compact(&target))
}

/// BCHN's `CalculateASERT`: `reference * 2^((time_diff - 600 * (height_diff
/// + 1)) / half_life)` in 16-bit fixed point, with the cubic approximation
/// of 2^x for the fraction, clamped to `limit` and to at least 1.
fn calculate(
    reference: &BigUint,
    time_diff: i64,
    height_diff: i64,
    limit: &BigUint,
    half_life: i64,
) -> Option<BigUint> {
    if reference.is_zero() || reference > limit || half_life <= 0 {
        return None;
    }
    // BCHN asserts |time_diff - spacing * height_diff| < 2^47.
    let span = time_diff.checked_sub(SPACING.checked_mul(height_diff)?)?;
    if span.unsigned_abs() >= 1 << 47 {
        return None;
    }
    // C++ and Rust both divide toward zero, and `>>` on a negative i64 is
    // arithmetic (rounds down) in both, so the integer part and the 16-bit
    // fraction split exactly as BCHN's.
    let exponent = (span - SPACING) * 65_536 / half_life;
    let shifts = (exponent >> 16) - 16;
    let fraction = u64::from(exponent as u16);
    // 65536 * 2^fraction; the sum stays below 2^64 for every 16-bit
    // fraction (at most 18,446,563,080,438,344,768).
    let factor = 65_536
        + ((195_766_423_245_049 * fraction
            + 971_821_376 * fraction * fraction
            + 5_127 * fraction * fraction * fraction
            + (1 << 47))
            >> 48);
    let mut next = reference * factor;
    if shifts <= 0 {
        next >>= shifts.unsigned_abs().min(256);
    } else if next.bits() + shifts.unsigned_abs() > 256 {
        // BCHN's 256-bit shift would lose high bits: the limit.
        next = limit.clone();
    } else {
        next <<= shifts.unsigned_abs();
    }
    if next.is_zero() {
        next = BigUint::from(1u8);
    } else if next > *limit {
        next = limit.clone();
    }
    Some(next)
}

/// BCHN's `arith_uint256::GetCompact` for a positive target.
fn compact(target: &BigUint) -> u32 {
    let low = |value: &BigUint| value.iter_u64_digits().next().unwrap_or(0);
    let mut size = target.bits().div_ceil(8) as u32;
    let mut word = if size <= 3 {
        (low(target) << (8 * (3 - size))) as u32
    } else {
        low(&(target >> (8 * (size - 3)))) as u32
    };
    if word & 0x0080_0000 != 0 {
        word >>= 8;
        size += 1;
    }
    word | size << 24
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### PR #48
    // What: real Chipnet blocks: the bits each carries are the ones ASERT
    // gives from Chipnet's anchor and its parent's height and time (both at
    // most 1200 s apart, so the 20-minute rule did not apply). From the
    // chaingraph.cash Chipnet explorer: block 300000 (bits 1c2b1b94) on
    // 299999 (time 1775336127); block 300002 (1c34e28b) on the
    // minimum-difficulty block 300001 (1775338388); block 328001 (1a1394ac)
    // on 328000 (1792075028).
    // Look here if: the anchor, the half-life or the fixed point changes.
    #[test]
    fn chipnet_blocks_carry_the_bits_asert_gives_them() {
        for (parent_height, parent_time, bits) in [
            (299_999, 1_775_336_127, 0x1c2b_1b94),
            (300_001, 1_775_338_388, 0x1c34_e28b),
            (328_000, 1_792_075_028, 0x1a13_94ac),
        ] {
            assert_eq!(
                next_bits(&CHIPNET, parent_height, parent_time),
                Some(bits),
                "{parent_height}"
            );
        }
        // On schedule from the anchor the target is the anchor's, the limit;
        // far behind schedule it stays there; below the anchor nothing.
        assert_eq!(
            next_bits(&CHIPNET, 16_845, 1_605_451_779 + 1200),
            Some(0x1d00_ffff)
        );
        assert_eq!(next_bits(&CHIPNET, 16_845, u32::MAX), Some(0x1d00_ffff));
        assert_eq!(next_bits(&CHIPNET, 16_843, 1_605_451_779), None);
    }

    // #### PR #48
    // What: lines of the aserti3-2d specification's test vectors (BCHN's
    // qa-assets repository, test_vectors/aserti3-2d, runs 1 to 6, 11 and
    // 12: the anchor at height 1, a two-day half-life and the limit
    // 00000000ffff…ff); each gives the bits for the block after the one at
    // (height, time). They cover the limit, the smallest target, halving and
    // doubling each block, fractional exponents both ways, and times that
    // go backwards. A time far ahead of schedule clamps to the limit
    // through the shift-overflow path.
    // Look here if: calculate or compact changes.
    #[test]
    fn the_specification_vectors_give_the_same_bits() {
        let two_days = 2 * 24 * 60 * 60;
        let vectors: &[(u32, i64, u32, u32, u32)] = &[
            // (anchor bits, anchor parent time, height, time, bits)
            (0x1d00_ffff, 0, 2, 1200, 0x1d00_ffff),
            (0x1a2b_3c4d, 0, 11, 6600, 0x1a2b_3c4d),
            (0x0101_0000, 0, 2, 1200, 0x0101_0000),
            (0x0101_0000, 0, 2, 174_000, 0x0102_0000),
            (0x0101_0000, 0, 51, 8_670_600, 0x0704_0000),
            (0x0101_0000, 0, 224, 38_668_800, 0x1d00_8000),
            (0x0101_0000, 0, 225, 38_842_200, 0x1d00_ffff),
            (0x1d00_ffff, 0, 2, 0, 0x1d00_fec5),
            (0x1d00_ffff, 0, 290, 0, 0x1c7f_62c0),
            (0x1d00_ffff, 0, 63_074, 0, 0x011f_0000),
            (0x1d00_ffff, 0, 64_514, 0, 0x0101_0000),
            (0x1802_aee8, 0, 3, 1310, 0x1802_ad91),
            (0x1802_aee8, 0, 9, 7099, 0x1802_b39c),
            (0x1802_aee8, 0, 101, 63_201, 0x1802_b620),
            (0x1802_aee8, 0, 1001, 589_738, 0x1802_91ad),
            (0x1802_aee8, 0, 4, 1398, 0x1802_ac28),
            (0x1802_aee8, 0, 1001, 293_713, 0x1800_c896),
            (0x1802_aee8, 10_000, 3, 11_199, 0x1802_ad44),
            (0x1802_aee8, 10_000, 3876, 7326, 0x160f_74a0),
        ];
        for &(anchor_bits, anchor_time, height, time, bits) in vectors {
            let anchor = Anchor {
                height: 1,
                bits: anchor_bits,
                parent_time: anchor_time,
                half_life: two_days,
            };
            assert_eq!(
                next_bits(&anchor, height, time),
                Some(bits),
                "{anchor_bits:#x} {height} {time}"
            );
        }
        let anchor = Anchor {
            height: 1,
            bits: 0x1d00_ffff,
            parent_time: 0,
            half_life: two_days,
        };
        assert_eq!(
            next_bits(&anchor, 2, 40 * two_days as u32),
            Some(0x1d00_ffff)
        );
    }
}
