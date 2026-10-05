//! #### PR #22: completed-transaction words for portable GPUs
//! What: big-endian words of a signed PHOTON transaction from byte 384 with
//! SHA-256 padding, the message schedule of one block and the PHOTON target
//! comparison, for the portable T2 window preparation and the non-T2 hash and
//! winner stages. Portable GPUs have no byte type.
//! Why: those stages compile from this source instead of hand-written WGSL.
//! Native GPU builds exclude it; CUDA assembles bytes in t2.rs and gpu.rs.
//! Check: tests/arithmetic.rs rebuilds every word from serialized bytes.
use crate::wide::{at, set};

/// Byte of a big-endian packed byte string.
#[inline(always)]
fn byte(words: &[u32], offset: usize) -> u32 {
    (at!(*words, offset / 4) >> ((3 - offset % 4) * 8)) & 0xff
}

/// Byte of 32 bytes held as big-endian words.
#[inline(always)]
fn byte32(words: [u32; 8], offset: usize) -> u32 {
    (at!(words, offset / 4) >> ((3 - offset % 4) * 8)) & 0xff
}

/// Big-endian word `w` (0..64) of transaction bytes 384..640 after the nonce
/// and signature r || s are written into the template at the layout `shift`,
/// padded as SHA-256 pads a `length`-byte message (length <= 631).
/// `template` packs the unsigned transaction big-endian.
#[inline(always)]
pub fn tx_word(
    template: &[u32],
    shift: usize,
    length: usize,
    nonce: u32,
    r: [u32; 8],
    s: [u32; 8],
    w: usize,
) -> u32 {
    let mut word = 0;
    let mut b = 0;
    while b < 4 {
        let offset = 384 + w * 4 + b;
        let value = if offset < length {
            if offset >= 390 + shift && offset < 394 + shift {
                (nonce >> (8 * (offset - 390 - shift))) & 0xff
            } else if offset >= 426 + shift && offset < 458 + shift {
                byte32(r, offset - 426 - shift)
            } else if offset >= 458 + shift && offset < 490 + shift {
                byte32(s, offset - 458 - shift)
            } else {
                byte(template, offset)
            }
        } else if offset == length {
            0x80
        } else if offset >= 636 {
            ((length as u32 * 8) >> ((639 - offset) * 8)) & 0xff
        } else {
            0
        };
        word = (word << 8) | value;
        b += 1;
    }
    word
}

/// The complete 64-word SHA-256 message schedule of one block.
#[inline(always)]
pub fn schedule(block: [u32; 16]) -> [u32; 64] {
    let mut w = [0u32; 64];
    let mut i = 0;
    while i < 16 {
        set!(w, i, at!(block, i));
        i += 1;
    }
    while i < 64 {
        let x = at!(w, i - 15);
        let y = at!(w, i - 2);
        set!(
            w,
            i,
            at!(w, i - 16)
                .wrapping_add(x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3))
                .wrapping_add(at!(w, i - 7))
                .wrapping_add(y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10))
        );
        i += 1;
    }
    w
}

/// The PHOTON proof: the digest (big-endian words of its 32 bytes), read as a
/// little-endian number with the top bit of byte 31 cleared, is strictly below
/// the target (32 bytes at `target_offset` of `template`). The positive rule
/// also rejects a digest with that bit set or with every bit clear.
#[inline(always)]
pub fn below_target(
    digest: [u32; 8],
    template: &[u32],
    target_offset: usize,
    positive: bool,
) -> bool {
    let [d0, d1, d2, d3, d4, d5, d6, d7] = digest;
    if positive && (d7 & 0x80 != 0 || (d0 | d1 | d2 | d3 | d4 | d5 | d6 | d7) == 0) {
        return false;
    }
    let mut i = 32;
    while i > 0 {
        i -= 1;
        let mut value = byte32(digest, i);
        if i == 31 {
            value &= 0x7f;
        }
        let target = byte(template, target_offset + i);
        if value != target {
            return value < target;
        }
    }
    false
}
