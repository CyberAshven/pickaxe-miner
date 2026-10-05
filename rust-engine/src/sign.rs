//! #### PR #22: per-window BCH Schnorr signing for portable GPUs
//! What: the message hash, RFC6979 nonce, fixed-base window digits and final
//! signature in big-endian words, built only on the shared field, scalar,
//! point and SHA-256 code. Portable GPUs have no byte or 64-bit integers.
//! Why: portable shaders compile this file unchanged and host tests run it.
//! Native GPU builds exclude it, so their verified kernels do not change.
//! Check: tests/arithmetic.rs compares every step with independent oracles.
use crate::{
    field::{subtract, Field, P},
    point::Point,
    scalar::{Scalar, N},
    sha256::{self, INITIAL},
    wide::{self as w, at, limbs, set},
};

/// 2^512 mod n: montgomery_mul(x, R2) converts x into Montgomery form.
pub const R2: [u32; 8] = [
    0x67d7d140, 0x896cf214, 0x0e7cf878, 0x741496c2, 0x5bcd07c6, 0xe697f5e4, 0x81c69bc5, 0x9d671cd5,
];
/// `Schnorr+SHA256  `, BCH's RFC6979 domain tag, as big-endian words.
const TAG: [u32; 4] = [0x5363686e, 0x6f72722b, 0x53484132, 0x35362020];

/// SHA-256 compression through one out-of-line copy; portable shaders would
/// otherwise inline sixteen copies into stage A.
#[inline(always)]
fn compress(state: &mut [u32; 8], block: [u32; 16]) {
    *state = compressed(*state, block);
}

#[inline(never)]
fn compressed(mut state: [u32; 8], block: [u32; 16]) -> [u32; 8] {
    sha256::compress(&mut state, block);
    state
}

/// Reverses limb order: little-endian limbs <-> big-endian SHA-256 words.
#[inline(always)]
pub fn reversed(v: [u32; 8]) -> [u32; 8] {
    [v[7], v[6], v[5], v[4], v[3], v[2], v[1], v[0]]
}

/// A scalar from big-endian words, reduced modulo n.
#[inline(always)]
pub fn scalar_from_be_words(v: [u32; 8]) -> Scalar {
    let words = reversed(v);
    let (reduced, borrow) = subtract(words, N);
    Scalar(if borrow == 0 { reduced } else { words })
}

/// PHOTON message hash: SHA-256(nonce as 4 little-endian bytes || target).
#[inline(always)]
pub fn message_words(nonce: u32, target: [u32; 8]) -> [u32; 8] {
    let t = target;
    let mut state = INITIAL;
    compress(
        &mut state,
        [
            nonce.swap_bytes(),
            t[0],
            t[1],
            t[2],
            t[3],
            t[4],
            t[5],
            t[6],
            t[7],
            0x8000_0000,
            0,
            0,
            0,
            0,
            0,
            36 * 8,
        ],
    );
    state
}

/// The message hash and RFC6979 nonce of one signature window.
#[inline(always)]
pub fn nonce_words(
    nonce: u32,
    target: [u32; 8],
    secret: [u32; 8],
    midstates: ([u32; 8], [u32; 8]),
) -> ([u32; 8], Scalar) {
    let message = message_words(nonce, target);
    let k = rfc6979_words(midstates, secret, message);
    (message, Scalar(reversed(k)))
}

/// Digit of 16-bit window `window` (0 = least significant) of a scalar.
/// Generator table entry (window * 65536 + digit) holds digit * 2^(16 window) * G.
#[inline(always)]
pub fn window_digit(k: Scalar, window: usize) -> usize {
    ((at!(k.0, window / 2) >> ((window % 2) * 16)) & 0xffff) as usize
}

/// BCH Schnorr signature (r, s) in big-endian words from R = k*G in Jacobian
/// form. `public` is packed as challenge_words expects.
#[inline(always)]
pub fn signature_words(
    point: Point,
    message: [u32; 8],
    k: Scalar,
    public: [u32; 9],
    secret: Scalar,
) -> ([u32; 8], [u32; 8]) {
    let r = reversed(point.x.mul_mod(inverse_binary(point.z).square()).0);
    // BCH signs with k or n - k so that y(R) is a square; y / z^3 and y * z
    // have the same quadratic character.
    let k = if is_square_binary(point.y.mul_mod(point.z)) {
        k
    } else {
        k.negate()
    };
    let e = scalar_from_be_words(challenge_words(r, public, message));
    let ed = e.montgomery_mul(secret).montgomery_mul(Scalar(R2));
    (r, reversed(k.add_mod(ed).0))
}

/// SHA-256(r || public || message) for the BCH Schnorr challenge. `public`
/// holds the 33-byte compressed key big-endian, byte 32 in the high byte of
/// word 8 and zero below it; r and the message are big-endian words.
#[inline(always)]
pub fn challenge_words(r: [u32; 8], public: [u32; 9], message: [u32; 8]) -> [u32; 8] {
    let (p, m) = (public, message);
    let mut state = INITIAL;
    compress(
        &mut state,
        [
            r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], p[0], p[1], p[2], p[3], p[4], p[5],
            p[6], p[7],
        ],
    );
    compress(
        &mut state,
        [
            p[8] | (m[0] >> 8),
            (m[0] << 24) | (m[1] >> 8),
            (m[1] << 24) | (m[2] >> 8),
            (m[2] << 24) | (m[3] >> 8),
            (m[3] << 24) | (m[4] >> 8),
            (m[4] << 24) | (m[5] >> 8),
            (m[5] << 24) | (m[6] >> 8),
            (m[6] << 24) | (m[7] >> 8),
            (m[7] << 24) | 0x0080_0000,
            0,
            0,
            0,
            0,
            0,
            0,
            97 * 8,
        ],
    );
    state
}

/// Variable-time binary extended Euclid inverse; returns zero for zero.
/// Latency-bound portable signing finishes shifts and subtractions sooner
/// than the 270 multiplications of an exponentiation chain.
pub fn inverse_binary(value: Field) -> Field {
    if is_zero(value.0) {
        return Field::ZERO;
    }
    let mut u = value.0;
    let mut v = P;
    let mut x1 = Field::ONE;
    let mut x2 = Field::ZERO;
    loop {
        if is_one(u) {
            return x1;
        }
        if is_one(v) {
            return x2;
        }
        while u[0] & 1 == 0 {
            u = shift_right(u, 0);
            x1 = half(x1);
        }
        while v[0] & 1 == 0 {
            v = shift_right(v, 0);
            x2 = half(x2);
        }
        let (difference, borrow) = subtract(u, v);
        if borrow == 0 {
            u = difference;
            x1 = x1.sub_mod(x2);
        } else {
            v = subtract(v, u).0;
            x2 = x2.sub_mod(x1);
        }
    }
}

/// Variable-time binary Jacobi symbol: true only for nonzero squares.
pub fn is_square_binary(value: Field) -> bool {
    let mut a = value.0;
    let mut n = P;
    let mut negative = false;
    while !is_zero(a) {
        while a[0] & 1 == 0 {
            a = shift_right(a, 0);
            let residue = n[0] & 7;
            negative ^= residue == 3 || residue == 5;
        }
        let (difference, borrow) = subtract(a, n);
        if borrow == 0 {
            a = difference;
        } else {
            // Quadratic reciprocity for the swapped odd pair.
            negative ^= a[0] & 3 == 3 && n[0] & 3 == 3;
            let swapped = subtract(n, a).0;
            n = a;
            a = swapped;
        }
    }
    is_one(n) && !negative
}

/// value / 2 in the field.
#[inline(always)]
fn half(value: Field) -> Field {
    if value.0[0] & 1 == 0 {
        return Field(shift_right(value.0, 0));
    }
    let mut words = [0; 8];
    let mut carry = w::ZERO;
    limbs!(i in 0..8 => {
        carry += w::extend(at!(value.0, i)) + w::extend(at!(P, i));
        set!(words, i, w::low(carry));
        carry = w::high(carry);
    });
    Field(shift_right(words, w::low(carry)))
}

#[inline(always)]
fn is_zero(v: [u32; 8]) -> bool {
    (v[0] | v[1] | v[2] | v[3] | v[4] | v[5] | v[6] | v[7]) == 0
}

#[inline(always)]
fn is_one(v: [u32; 8]) -> bool {
    v[0] == 1 && (v[1] | v[2] | v[3] | v[4] | v[5] | v[6] | v[7]) == 0
}

/// Shifts right by one bit, inserting top (0 or 1) as bit 255.
#[inline(always)]
fn shift_right(v: [u32; 8], top: u32) -> [u32; 8] {
    [
        (v[0] >> 1) | (v[1] << 31),
        (v[1] >> 1) | (v[2] << 31),
        (v[2] >> 1) | (v[3] << 31),
        (v[3] >> 1) | (v[4] << 31),
        (v[4] >> 1) | (v[5] << 31),
        (v[5] >> 1) | (v[6] << 31),
        (v[6] >> 1) | (v[7] << 31),
        (v[7] >> 1) | (top << 31),
    ]
}

/// Inner-hash state after the zero key's ipad block and the first RFC6979
/// data block (V = 0x01.., separator 0, secret bytes 0..31), and the zero
/// key's opad state. These depend only on the key: the host computes them.
pub fn rfc6979_midstates(secret: [u32; 8]) -> ([u32; 8], [u32; 8]) {
    let mut inner = INITIAL;
    compress(&mut inner, key_block([0; 8], 0x3636_3636));
    compress(&mut inner, first_block([0x0101_0101; 8], 0, secret));
    let mut outer = INITIAL;
    compress(&mut outer, key_block([0; 8], 0x5c5c_5c5c));
    (inner, outer)
}

/// BCH RFC6979 nonce (as crate::nonce::bch_rfc6979) in big-endian words.
/// `midstates` comes from rfc6979_midstates(secret).
///
/// The HMAC chain runs as a table of steps around one compression, so a
/// shader holds a single copy: GPU compilers take minutes to schedule the
/// fifteen unrolled compressions of the straight-line form. Steps 0-14 are
/// K1, the key states, V1, K2, the key states, V2 and the candidate V;
/// steps 15-20 are the RFC6979 retry (K = HMAC_K(V || 0), V = HMAC_K(V)).
pub fn rfc6979_words(
    midstates: ([u32; 8], [u32; 8]),
    secret: [u32; 8],
    message: [u32; 8],
) -> [u32; 8] {
    rfc6979_words_retrying(midstates, secret, message, 0)
}

/// As rfc6979_words, but rejects the first `forced` in-range candidates as
/// RFC6979 rejects out-of-range ones, so tests reach the retry steps that
/// real keys almost never take.
pub fn rfc6979_words_retrying(
    midstates: ([u32; 8], [u32; 8]),
    secret: [u32; 8],
    message: [u32; 8],
    mut forced: u32,
) -> [u32; 8] {
    let tail = second_block(secret[7], reversed(scalar_from_be_words(message).0));
    // HMAC key states; the zero key's inner state already absorbed block one.
    let (mut inner, mut outer) = midstates;
    let mut key = [0; 8];
    let mut v = [0x0101_0101; 8];
    let mut t = [0; 8];
    let mut step = 0;
    loop {
        let (state, block) = match step {
            0 => (inner, tail),
            6 => (inner, first_block(v, 1, secret)),
            7 => (t, tail),
            15 => (inner, retry_block(v)),
            2 | 9 | 17 => (INITIAL, key_block(key, 0x3636_3636)),
            3 | 10 | 18 => (INITIAL, key_block(key, 0x5c5c_5c5c)),
            4 | 11 | 13 | 19 => (inner, padded32(v)),
            // 1, 5, 8, 12, 14, 16, 20: an outer hash.
            _ => (outer, padded32(t)),
        };
        let out = compressed(state, block);
        match step {
            1 | 8 | 16 => key = out,
            2 | 9 | 17 => inner = out,
            3 | 10 | 18 => outer = out,
            5 | 12 | 14 | 20 => v = out,
            _ => t = out,
        }
        step = match step {
            14 => {
                // Accept V only as a scalar in 1..n, exactly as bch_rfc6979.
                if forced == 0 && subtract(reversed(v), N).1 != 0 && !is_zero(v) {
                    return v;
                }
                // rust-gpu has no saturating_sub.
                #[allow(clippy::implicit_saturating_sub)]
                if forced > 0 {
                    forced -= 1;
                }
                15
            }
            20 => 13,
            step => step + 1,
        };
    }
}

/// An HMAC key block: the 32-byte key XOR the pad, then the pad.
#[inline(always)]
fn key_block(key: [u32; 8], pad: u32) -> [u32; 16] {
    let [k0, k1, k2, k3, k4, k5, k6, k7] = key;
    [
        k0 ^ pad,
        k1 ^ pad,
        k2 ^ pad,
        k3 ^ pad,
        k4 ^ pad,
        k5 ^ pad,
        k6 ^ pad,
        k7 ^ pad,
        pad,
        pad,
        pad,
        pad,
        pad,
        pad,
        pad,
        pad,
    ]
}

/// RFC6979 retry message V || 0x00 after one key block, padded.
#[inline(always)]
fn retry_block(v: [u32; 8]) -> [u32; 16] {
    [
        v[0],
        v[1],
        v[2],
        v[3],
        v[4],
        v[5],
        v[6],
        v[7],
        0x0080_0000,
        0,
        0,
        0,
        0,
        0,
        0,
        (64 + 33) * 8,
    ]
}

/// A 32-byte message after one key block, padded as the final block.
#[inline(always)]
fn padded32(m: [u32; 8]) -> [u32; 16] {
    [
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        m[6],
        m[7],
        0x8000_0000,
        0,
        0,
        0,
        0,
        0,
        0,
        (64 + 32) * 8,
    ]
}

/// RFC6979 data bytes 0..64: V, the separator byte and secret bytes 0..31.
#[inline(always)]
fn first_block(v: [u32; 8], separator: u32, x: [u32; 8]) -> [u32; 16] {
    [
        v[0],
        v[1],
        v[2],
        v[3],
        v[4],
        v[5],
        v[6],
        v[7],
        (separator << 24) | (x[0] >> 8),
        (x[0] << 24) | (x[1] >> 8),
        (x[1] << 24) | (x[2] >> 8),
        (x[2] << 24) | (x[3] >> 8),
        (x[3] << 24) | (x[4] >> 8),
        (x[4] << 24) | (x[5] >> 8),
        (x[5] << 24) | (x[6] >> 8),
        (x[6] << 24) | (x[7] >> 8),
    ]
}

/// RFC6979 data bytes 64..113 (secret byte 31, message, tag) and padding.
#[inline(always)]
fn second_block(x7: u32, h: [u32; 8]) -> [u32; 16] {
    [
        (x7 << 24) | (h[0] >> 8),
        (h[0] << 24) | (h[1] >> 8),
        (h[1] << 24) | (h[2] >> 8),
        (h[2] << 24) | (h[3] >> 8),
        (h[3] << 24) | (h[4] >> 8),
        (h[4] << 24) | (h[5] >> 8),
        (h[5] << 24) | (h[6] >> 8),
        (h[6] << 24) | (h[7] >> 8),
        (h[7] << 24) | (TAG[0] >> 8),
        (TAG[0] << 24) | (TAG[1] >> 8),
        (TAG[1] << 24) | (TAG[2] >> 8),
        (TAG[2] << 24) | (TAG[3] >> 8),
        (TAG[3] << 24) | 0x0080_0000,
        0,
        0,
        (64 + 113) * 8,
    ]
}
