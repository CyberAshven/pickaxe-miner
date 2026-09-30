//! Scalar SHA-256 compression for GPU registers, checked against RustCrypto.
pub const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

// LLVM can split (a & b) ^ (!a & c) into disjoint integer-add operands.
// Preserve the single ternary GPU instruction used by the native kernel.
#[inline(always)]
fn choose(a: u32, b: u32, c: u32) -> u32 {
    #[cfg(target_os = "cuda")]
    unsafe {
        let result;
        core::arch::asm!(
            "lop3.b32 {result}, {a}, {b}, {c}, 0xca;",
            result = out(reg32) result,
            a = in(reg32) a, b = in(reg32) b, c = in(reg32) c,
            options(pure, nomem, nostack),
        );
        result
    }
    #[cfg(not(target_os = "cuda"))]
    {
        (a & b) ^ (!a & c)
    }
}

#[inline(always)]
pub fn compress(state: &mut [u32; 8], w: [u32; 16]) {
    compress_from::<0>(state, w, *state);
}

/// Resume compression with an already computed round state.
#[inline(always)]
pub fn compress_from<const START: usize>(state: &mut [u32; 8], w: [u32; 16], head: [u32; 8]) {
    compress_from_limited::<START>(state, w, head, None);
}

#[inline(always)]
fn compress_from_limited<const START: usize>(
    state: &mut [u32; 8],
    mut w: [u32; 16],
    head: [u32; 8],
    high_byte: Option<u8>,
) -> bool {
    // Unrolled rounds rotate variable roles rather than copying eight words.
    // Map a resumed logical state back to those roles at START.
    let head: [u32; 8] = core::array::from_fn(|i| head[(i + START) & 7]);
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = head;
    // Constant ring indices let the GPU compiler retain the schedule in registers.
    macro_rules! round {
        ($i:expr, $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => {{
            if $i >= 16 {
                let x = w[($i + 1) & 15];
                let y = w[($i + 14) & 15];
                let s0 = x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3);
                let s1 = y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10);
                w[$i & 15] = w[$i & 15]
                    .wrapping_add(s0)
                    .wrapping_add(w[($i + 9) & 15])
                    .wrapping_add(s1);
            }
            if $i >= START {
                let s1 = $e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25);
                let ch = choose($e, $f, $g);
                let t = $h
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[$i])
                    .wrapping_add(w[$i & 15]);
                let s0 = $a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22);
                let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);
                $d = $d.wrapping_add(t);
                if $i == 60 {
                    // Logical e after round 60 becomes final h after round 63.
                    // Its low byte is digest[31], PHOTON's first comparison byte.
                    if let Some(limit) = high_byte {
                        if (state[7].wrapping_add($d) as u8 & 0x7f) > limit {
                            return false;
                        }
                    }
                }
                $h = t.wrapping_add(s0).wrapping_add(maj);
            }
        }};
    }
    macro_rules! eight {
        ($i:literal) => {
            round!($i, a, b, c, d, e, f, g, h);
            round!($i + 1, h, a, b, c, d, e, f, g);
            round!($i + 2, g, h, a, b, c, d, e, f);
            round!($i + 3, f, g, h, a, b, c, d, e);
            round!($i + 4, e, f, g, h, a, b, c, d);
            round!($i + 5, d, e, f, g, h, a, b, c);
            round!($i + 6, c, d, e, f, g, h, a, b);
            round!($i + 7, b, c, d, e, f, g, h, a);
        };
    }
    eight!(0);
    eight!(8);
    eight!(16);
    eight!(24);
    eight!(32);
    eight!(40);
    eight!(48);
    eight!(56);
    // Keep the feed-forward in registers too; the NVPTX backend does not
    // unroll the iterator/zip form and otherwise stores each block's state locally.
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
    true
}

#[inline(always)]
pub fn compress_scheduled(state: &mut [u32; 8], w: &[u32; 64]) {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    // Constant ring indices let the GPU compiler retain the schedule in registers.
    macro_rules! round {
        ($i:expr, $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => {{
            {
                let s1 = $e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25);
                let ch = choose($e, $f, $g);
                let t = $h
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[$i])
                    .wrapping_add(w[$i]);
                let s0 = $a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22);
                let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);
                $d = $d.wrapping_add(t);
                $h = t.wrapping_add(s0).wrapping_add(maj);
            }
        }};
    }
    macro_rules! eight {
        ($i:literal) => {
            round!($i, a, b, c, d, e, f, g, h);
            round!($i + 1, h, a, b, c, d, e, f, g);
            round!($i + 2, g, h, a, b, c, d, e, f);
            round!($i + 3, f, g, h, a, b, c, d, e);
            round!($i + 4, e, f, g, h, a, b, c, d);
            round!($i + 5, d, e, f, g, h, a, b, c);
            round!($i + 6, c, d, e, f, g, h, a, b);
            round!($i + 7, b, c, d, e, f, g, h, a);
        };
    }
    eight!(0);
    eight!(8);
    eight!(16);
    eight!(24);
    eight!(32);
    eight!(40);
    eight!(48);
    eight!(56);
    // Keep the feed-forward in registers too; the NVPTX backend does not
    // unroll the iterator/zip form and otherwise stores each block's state locally.
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// The first ten rounds precede every variable T2 amount byte.
#[inline(always)]
pub fn head10(mut state: [u32; 8], w: [u32; 16]) -> [u32; 8] {
    for i in 0..10 {
        let [a, b, c, d, e, f, g, h] = state;
        let t = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add(choose(e, f, g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        state = [t.wrapping_add(t2), a, b, c, d.wrapping_add(t), e, f, g];
    }
    state
}

#[inline(always)]
pub fn compress_bytes(state: &mut [u32; 8], bytes: [u8; 64]) {
    compress(
        state,
        core::array::from_fn(|i| {
            u32::from_be_bytes([
                bytes[i * 4],
                bytes[i * 4 + 1],
                bytes[i * 4 + 2],
                bytes[i * 4 + 3],
            ])
        }),
    );
}

#[inline(always)]
pub fn bytes(state: [u32; 8]) -> [u8; 32] {
    core::array::from_fn(|i| (state[i / 4] >> ((3 - i % 4) * 8)) as u8)
}

#[inline(always)]
pub fn hash_state(first_hash: [u32; 8]) -> [u8; 32] {
    let mut block = [0; 16];
    block[..8].copy_from_slice(&first_hash);
    block[8] = 0x80000000;
    block[15] = 256;
    let mut state = INITIAL;
    compress(&mut state, block);
    bytes(state)
}

/// Reject a PHOTON hash as soon as its highest comparison byte is known.
/// Surviving hashes still require the complete strict target comparison.
#[inline(always)]
pub fn hash_state_filtered(first_hash: [u32; 8], high_byte: u8) -> Option<[u8; 32]> {
    let mut block = [0; 16];
    block[..8].copy_from_slice(&first_hash);
    block[8] = 0x80000000;
    block[15] = 256;
    let mut state = INITIAL;
    compress_from_limited::<0>(&mut state, block, INITIAL, Some(high_byte)).then(|| bytes(state))
}

#[inline(always)]
pub fn challenge(r: [u8; 32], public: [u8; 33], message: [u8; 32]) -> [u8; 32] {
    let mut state = INITIAL;
    let mut block = [0; 64];
    block[..32].copy_from_slice(&r);
    block[32..].copy_from_slice(&public[..32]);
    compress_bytes(&mut state, block);
    block = [0; 64];
    block[0] = public[32];
    block[1..33].copy_from_slice(&message);
    block[33] = 0x80;
    block[56..].copy_from_slice(&(97u64 * 8).to_be_bytes());
    compress_bytes(&mut state, block);
    bytes(state)
}
