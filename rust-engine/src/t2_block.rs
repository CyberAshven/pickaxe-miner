//! Shared transaction-block layout for T2 amount grinding.
#[cfg(target_arch = "spirv")]
use spirv_std::arch::IndexUnchecked;
// #### PR #22
// Keep one layout/padding definition. Only byte access and 64-bit primitives
// differ at the SPIR-V boundary. Native PTX identity is a required gate.
#[cfg(not(target_arch = "spirv"))]
pub type Amount = u64;
#[cfg(target_arch = "spirv")]
#[derive(Clone, Copy)]
pub struct Amount {
    pub lo: u32,
    pub hi: u32,
}
#[cfg(target_arch = "spirv")]
#[inline(always)]
fn add(value: Amount, j: u32) -> Amount {
    let lo = value.lo.wrapping_add(j);
    Amount {
        lo,
        hi: value.hi.wrapping_add(u32::from(lo < value.lo)),
    }
}
#[cfg(target_arch = "spirv")]
#[inline(always)]
fn sub(value: Amount, j: u32) -> Amount {
    Amount {
        lo: value.lo.wrapping_sub(j),
        hi: value.hi.wrapping_sub(u32::from(value.lo < j)),
    }
}
#[cfg(target_arch = "spirv")]
#[inline(always)]
fn amount_byte(value: Amount, offset: usize) -> u32 {
    let word = if offset < 4 { value.lo } else { value.hi };
    (word >> ((offset & 3) * 8)) & 255
}

#[cfg(target_arch = "spirv")]
type Byte = u32;

// One layout definition; native keeps its original captured closure so LLVM
// retains the verified register schedule, while SPIR-V uses a direct function.
macro_rules! byte_body {
    ($tx:ident, $tx_start:ident, $tx_origin:ident, $big_endian:ident, $baton:ident, $reward:ident, $j:ident, $pos:ident, $SHIFT:ident) => {{
        if $pos >= 491 + $SHIFT && $pos < 499 + $SHIFT {
            #[cfg(not(target_arch = "spirv"))]
            {
                (($baton + u64::from($j)) >> (8 * ($pos - 491 - $SHIFT))) as u8
            }
            #[cfg(target_arch = "spirv")]
            {
                amount_byte(add($baton, $j), $pos - 491 - $SHIFT)
            }
        } else if $pos >= 578 + $SHIFT && $pos < 586 + $SHIFT {
            #[cfg(not(target_arch = "spirv"))]
            {
                (($reward - u64::from($j)) >> (8 * ($pos - 578 - $SHIFT))) as u8
            }
            #[cfg(target_arch = "spirv")]
            {
                amount_byte(sub($reward, $j), $pos - 578 - $SHIFT)
            }
        } else if $pos < 615 + $SHIFT {
            #[cfg(not(target_arch = "spirv"))]
            {
                *$tx.add($pos)
            }
            #[cfg(target_arch = "spirv")]
            {
                let byte_index = if $big_endian {
                    3 - ($pos & 3)
                } else {
                    $pos & 3
                };
                // The entry adapter validates the record span once. Avoid a
                // Rust panic branch for each byte of every fixed-width word.
                (*$tx.index_unchecked($tx_start + $pos / 4 - $tx_origin / 4) >> (byte_index * 8))
                    & 255
            }
        } else if $pos == 615 + $SHIFT {
            0x80
        } else if $pos >= 632 {
            #[cfg(not(target_arch = "spirv"))]
            {
                (((615 + $SHIFT) as u64 * 8) >> (8 * (639 - $pos))) as u8
            }
            #[cfg(target_arch = "spirv")]
            {
                if $pos < 636 {
                    0
                } else {
                    ((615 + $SHIFT) as u32 * 8) >> (8 * (639 - $pos)) & 255
                }
            }
        } else {
            0
        }
    }};
}
#[cfg(target_arch = "spirv")]
#[inline(always)]
unsafe fn byte<const SHIFT: usize>(
    tx: &[u32],
    tx_start: usize,
    tx_origin: usize,
    big_endian: bool,
    baton: Amount,
    reward: Amount,
    j: u32,
    pos: usize,
) -> Byte {
    byte_body!(tx, tx_start, tx_origin, big_endian, baton, reward, j, pos, SHIFT)
}

/// Assemble one padded transaction block with the conserved amount coordinate.
/// # Safety
/// Native `tx` must point to at least `615 + SHIFT` readable bytes. The host
/// validates amount arithmetic and layout eligibility before dispatch.
/// Portable records must cover every source byte in the requested block,
/// starting at `tx_origin` (word aligned); `tx_start` is their first word.
#[inline(always)]
pub unsafe fn block<const SHIFT: usize, const BLOCK: usize>(
    #[cfg(not(target_arch = "spirv"))] tx: *const u8,
    #[cfg(target_arch = "spirv")] tx: &[u32],
    #[cfg(target_arch = "spirv")] tx_start: usize,
    #[cfg(target_arch = "spirv")] tx_origin: usize,
    #[cfg(target_arch = "spirv")] big_endian: bool,
    baton: Amount,
    reward: Amount,
    j: u32,
) -> [u32; 16] {
    #[cfg(not(target_arch = "spirv"))]
    let byte =
        |pos: usize| byte_body!(tx, tx_start, tx_origin, big_endian, baton, reward, j, pos, SHIFT);
    macro_rules! byte {
        ($pos:expr) => {{
            #[cfg(not(target_arch = "spirv"))]
            {
                byte($pos)
            }
            #[cfg(target_arch = "spirv")]
            {
                byte::<SHIFT>(tx, tx_start, tx_origin, big_endian, baton, reward, j, $pos)
            }
        }};
    }
    macro_rules! word {
        ($i:literal) => {{
            #[cfg(not(target_arch = "spirv"))]
            {
                u32::from_be_bytes([
                    byte!(BLOCK * 64 + $i * 4),
                    byte!(BLOCK * 64 + $i * 4 + 1),
                    byte!(BLOCK * 64 + $i * 4 + 2),
                    byte!(BLOCK * 64 + $i * 4 + 3),
                ])
            }
            #[cfg(target_arch = "spirv")]
            {
                (byte!(BLOCK * 64 + $i * 4) << 24)
                    | (byte!(BLOCK * 64 + $i * 4 + 1) << 16)
                    | (byte!(BLOCK * 64 + $i * 4 + 2) << 8)
                    | byte!(BLOCK * 64 + $i * 4 + 3)
            }
        }};
    }
    [
        word!(0),
        word!(1),
        word!(2),
        word!(3),
        word!(4),
        word!(5),
        word!(6),
        word!(7),
        word!(8),
        word!(9),
        word!(10),
        word!(11),
        word!(12),
        word!(13),
        word!(14),
        word!(15),
    ]
}
