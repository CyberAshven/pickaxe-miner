//! Thin portable entry points for the shared Rust BCH Schnorr signer.
#![cfg_attr(target_arch = "spirv", no_std)]

// #### PR #22: buffer and index adapters only. Signing arithmetic, RFC6979 and
// hashing are imported from rust-engine; the original WGSL stages remain the
// default until these pass the per-stage timing gates on each GPU.
// Bindings and record layouts match the original stages A, B and C1, so the
// host swaps pipelines without touching buffers:
//   0 template words (big-endian), then nonce base and layout shift
//   3 generator table, 4 private key words, 5 RFC6979 midstates
//   10 records: message[8] k[8] x[8] y[8] z[8]; 12 B dispatch (groups, count)
//   13 signatures: r[8] s[8], big-endian words
// Entry points sit at the crate root and their names do not end in a digit,
// so SPIR-V and the generated WGSL keep exactly the names the host requests.
#[cfg(target_arch = "spirv")]
#[path = "../../../../rust-engine/src/field.rs"]
pub mod field;
#[cfg(target_arch = "spirv")]
#[path = "../../../../rust-engine/src/point.rs"]
pub mod point;
#[cfg(target_arch = "spirv")]
#[path = "../../../../rust-engine/src/scalar.rs"]
pub mod scalar;
#[cfg(target_arch = "spirv")]
#[path = "../../../../rust-engine/src/sha256.rs"]
pub mod sha256;
#[cfg(target_arch = "spirv")]
#[path = "../../../../rust-engine/src/sign.rs"]
pub mod sign;
#[cfg(target_arch = "spirv")]
#[path = "../../../../rust-engine/src/wide.rs"]
pub mod wide;

#[cfg(target_arch = "spirv")]
use {
    field::Field,
    point::Point,
    scalar::Scalar,
    spirv_std::{
        arch::IndexUnchecked,
        glam::{UVec3, UVec4},
        spirv,
    },
};

// Widest template: 615 bytes plus the largest layout shift (16).
#[cfg(target_arch = "spirv")]
const TEMPLATE_WORDS: usize = 158;
#[cfg(target_arch = "spirv")]
const NONCE_BASE: usize = TEMPLATE_WORDS;
#[cfg(target_arch = "spirv")]
const LAYOUT_SHIFT: usize = TEMPLATE_WORDS + 1;
#[cfg(target_arch = "spirv")]
const RECORD_WORDS: usize = 40;
// Unshifted public key and shifted PHOTON target inside the template.
#[cfg(target_arch = "spirv")]
const PUBLIC_KEY_OFFSET: usize = 45;
#[cfg(target_arch = "spirv")]
const TARGET_OFFSET: usize = 394;

/// Four bytes at any offset of a big-endian packed byte string.
#[cfg(target_arch = "spirv")]
#[inline(always)]
unsafe fn be_word(words: &[u32], byte: usize) -> u32 {
    let first = *words.index_unchecked(byte / 4);
    let shift = (byte % 4) * 8;
    if shift == 0 {
        first
    } else {
        (first << shift) | (*words.index_unchecked(byte / 4 + 1) >> (32 - shift))
    }
}

#[cfg(target_arch = "spirv")]
#[inline(always)]
unsafe fn load8(words: &[u32], start: usize) -> [u32; 8] {
    [
        *words.index_unchecked(start),
        *words.index_unchecked(start + 1),
        *words.index_unchecked(start + 2),
        *words.index_unchecked(start + 3),
        *words.index_unchecked(start + 4),
        *words.index_unchecked(start + 5),
        *words.index_unchecked(start + 6),
        *words.index_unchecked(start + 7),
    ]
}

#[cfg(target_arch = "spirv")]
#[inline(always)]
unsafe fn store8(words: &mut [u32], start: usize, value: [u32; 8]) {
    *words.index_unchecked_mut(start) = value[0];
    *words.index_unchecked_mut(start + 1) = value[1];
    *words.index_unchecked_mut(start + 2) = value[2];
    *words.index_unchecked_mut(start + 3) = value[3];
    *words.index_unchecked_mut(start + 4) = value[4];
    *words.index_unchecked_mut(start + 5) = value[5];
    *words.index_unchecked_mut(start + 6) = value[6];
    *words.index_unchecked_mut(start + 7) = value[7];
}

/// Stage A: message hash and RFC6979 nonce of each signature window.
#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(128)))]
pub fn pickaxe_shared_stage_a(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] input: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 4)] secret: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 5)] midstates: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 10)] records: &mut [u32],
) {
    unsafe {
        let base = id.x as usize * RECORD_WORDS;
        let target_start = TARGET_OFFSET + *input.index_unchecked(LAYOUT_SHIFT) as usize;
        let target = [
            be_word(input, target_start),
            be_word(input, target_start + 4),
            be_word(input, target_start + 8),
            be_word(input, target_start + 12),
            be_word(input, target_start + 16),
            be_word(input, target_start + 20),
            be_word(input, target_start + 24),
            be_word(input, target_start + 28),
        ];
        let (message, k) = sign::nonce_words(
            input.index_unchecked(NONCE_BASE).wrapping_add(id.x),
            target,
            load8(secret, 0),
            (load8(midstates, 0), load8(midstates, 8)),
        );
        store8(records, base, message);
        store8(records, base + 8, k.0);
    }
}

/// Stage B: k*G as the sum of the sixteen 16-bit window points in the table.
/// One dispatch replaces the original four, so the shader holds one point add.
#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(32)))]
pub fn pickaxe_shared_stage_b(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 3)] table: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 10)] records: &mut [u32],
    #[spirv(uniform, descriptor_set = 0, binding = 12)] dispatch: &UVec4,
) {
    unsafe {
        let index = id.x as usize;
        if index >= dispatch.y as usize {
            return;
        }
        let base = index * RECORD_WORDS;
        let k = Scalar(load8(records, base + 8));
        let mut point = Point::INFINITY;
        // A counted while loop: a range iterator becomes an Option per step.
        let mut window = 0;
        while window < 16 {
            let digit = sign::window_digit(k, window);
            if digit != 0 {
                let entry = (window * 65536 + digit) * 16;
                point =
                    point.add_affine(Field(load8(table, entry)), Field(load8(table, entry + 8)));
            }
            window += 1;
        }
        store8(records, base + 16, point.x.0);
        store8(records, base + 24, point.y.0);
        store8(records, base + 32, point.z.0);
    }
}

/// Stage C1: normalizes R and writes the BCH Schnorr signature r || s.
#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(64)))]
pub fn pickaxe_shared_c1_signature(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] input: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 4)] secret: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 10)] records: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 13)] signatures: &mut [u32],
) {
    unsafe {
        let index = id.x as usize;
        let base = index * RECORD_WORDS;
        let point = Point {
            x: Field(load8(records, base + 16)),
            y: Field(load8(records, base + 24)),
            z: Field(load8(records, base + 32)),
        };
        let public = [
            be_word(input, PUBLIC_KEY_OFFSET),
            be_word(input, PUBLIC_KEY_OFFSET + 4),
            be_word(input, PUBLIC_KEY_OFFSET + 8),
            be_word(input, PUBLIC_KEY_OFFSET + 12),
            be_word(input, PUBLIC_KEY_OFFSET + 16),
            be_word(input, PUBLIC_KEY_OFFSET + 20),
            be_word(input, PUBLIC_KEY_OFFSET + 24),
            be_word(input, PUBLIC_KEY_OFFSET + 28),
            be_word(input, PUBLIC_KEY_OFFSET + 32) & 0xff00_0000,
        ];
        let (r, s) = sign::signature_words(
            point,
            load8(records, base),
            Scalar(load8(records, base + 8)),
            public,
            sign::scalar_from_be_words(load8(secret, 0)),
        );
        store8(signatures, index * 16, r);
        store8(signatures, index * 16 + 8, s);
    }
}

/// Development check of field and point arithmetic on host-made vectors.
/// Case: a[8] b[8] point x,y,z[24] affine qx,qy[16] (56 words); results:
/// a*b, a^2, a+b, a-b, inverse(a) and point + q (64 words). Not shipped.
#[cfg(all(target_arch = "spirv", feature = "debug"))]
#[spirv(compute(threads(64)))]
pub fn pickaxe_debug_arithmetic(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] cases: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 1)] results: &mut [u32],
) {
    unsafe {
        let case = id.x as usize * 56;
        let out = id.x as usize * 64;
        let a = Field(load8(cases, case));
        let b = Field(load8(cases, case + 8));
        let point = Point {
            x: Field(load8(cases, case + 16)),
            y: Field(load8(cases, case + 24)),
            z: Field(load8(cases, case + 32)),
        };
        store8(results, out, a.mul_mod(b).0);
        store8(results, out + 8, a.square().0);
        store8(results, out + 16, a.add_mod(b).0);
        store8(results, out + 24, a.sub_mod(b).0);
        store8(results, out + 32, sign::inverse_binary(a).0);
        let sum = point.add_affine(Field(load8(cases, case + 40)), Field(load8(cases, case + 48)));
        store8(results, out + 40, sum.x.0);
        store8(results, out + 48, sum.y.0);
        store8(results, out + 56, sum.z.0);
    }
}
