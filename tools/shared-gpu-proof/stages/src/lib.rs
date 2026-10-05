//! Thin portable entry points for the shared Rust mining stages.
#![cfg_attr(target_arch = "spirv", no_std)]

// #### PR #22: buffer and index adapters only. Signing, transaction words and
// hashing are imported from rust-engine. Bindings and record layouts match the
// original hand-written stages (A, B, C1, T2 preparation, C2, C3), so the host
// swaps pipelines without touching buffers; PICKAXE_WGPU_STAGES=wgsl selects
// the originals. The T2 filter is generated separately (reference/shared-t2).
//   0 template words (big-endian), then nonce base, layout shift, length, rule
//   2 counters: candidates, completed, winners; 3 generator table
//   4 private key words, 5 RFC6979 midstates, 6 state after template byte 384
//   10 records: message[8] k[8] x[8] y[8] z[8]; 12 B dispatch (groups, count)
//   13 signatures: r[8] s[8]; 14 hashes; 16 winners: nonce + digest[8]
//   17 T2 windows (128 words each); 18 T2 dispatch (offset, count, windows)
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
#[path = "../../../../rust-engine/src/window.rs"]
pub mod window;

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
const TX_LENGTH: usize = TEMPLATE_WORDS + 2;
#[cfg(target_arch = "spirv")]
const POSITIVE_RULE: usize = TEMPLATE_WORDS + 3;
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

#[cfg(target_arch = "spirv")]
#[inline(always)]
unsafe fn load16(words: &[u32], start: usize) -> [u32; 16] {
    let [a0, a1, a2, a3, a4, a5, a6, a7] = load8(words, start);
    let [b0, b1, b2, b3, b4, b5, b6, b7] = load8(words, start + 8);
    [
        a0, a1, a2, a3, a4, a5, a6, a7, b0, b1, b2, b3, b4, b5, b6, b7,
    ]
}

/// Second SHA-256 of a finished first hash: the digest as big-endian words.
#[cfg(target_arch = "spirv")]
#[inline(always)]
fn hash256_tail(first: [u32; 8]) -> [u32; 8] {
    let [s0, s1, s2, s3, s4, s5, s6, s7] = first;
    let mut state = sha256::INITIAL;
    sha256::compress(
        &mut state,
        [
            s0,
            s1,
            s2,
            s3,
            s4,
            s5,
            s6,
            s7,
            0x8000_0000,
            0,
            0,
            0,
            0,
            0,
            0,
            256,
        ],
    );
    state
}

/// T2 preparation: each signed window's transaction words from byte 448, its
/// SHA-256 state after byte 448 and the head of block 7 for the shared filter.
/// Window 0 also stores block 8's schedule, which every window shares.
#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(64)))]
pub fn pickaxe_shared_t2_prepare(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] input: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 6)] prefix: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 13)] signatures: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 17)] windows: &mut [u32],
    #[spirv(uniform, descriptor_set = 0, binding = 18)] control: &UVec4,
) {
    unsafe {
        let index = id.x as usize + id.y as usize * 1_048_576;
        if index >= control.z as usize {
            return;
        }
        let shift = *input.index_unchecked(LAYOUT_SHIFT) as usize;
        let length = *input.index_unchecked(TX_LENGTH) as usize;
        let nonce = input.index_unchecked(NONCE_BASE).wrapping_add(index as u32);
        let r = load8(signatures, index * 16);
        let s = load8(signatures, index * 16 + 8);
        let base = index * 128;
        let mut block = [0u32; 16];
        let mut w = 0;
        while w < 16 {
            *block.index_unchecked_mut(w) = window::tx_word(input, shift, length, nonce, r, s, w);
            w += 1;
        }
        while w < 64 {
            *windows.index_unchecked_mut(base + w - 8) =
                window::tx_word(input, shift, length, nonce, r, s, w);
            w += 1;
        }
        let mut state = load8(prefix, 0);
        sha256::compress(&mut state, block);
        store8(windows, base, state);
        store8(windows, base + 56, sha256::head10(state, load16(windows, base + 8)));
        if index == 0 {
            let schedule = window::schedule(load16(windows, 24));
            let mut i = 0;
            while i < 64 {
                *windows.index_unchecked_mut(64 + i) = *schedule.index_unchecked(i);
                i += 1;
            }
        }
    }
}

/// Non-T2 stage C2: HASH256 of each signed candidate transaction.
#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(64)))]
pub fn pickaxe_shared_c2_hash(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] input: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 6)] prefix: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 13)] signatures: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 14)] hashes: &mut [u32],
) {
    unsafe {
        let index = id.x as usize;
        let shift = *input.index_unchecked(LAYOUT_SHIFT) as usize;
        let length = *input.index_unchecked(TX_LENGTH) as usize;
        let nonce = input.index_unchecked(NONCE_BASE).wrapping_add(id.x);
        let r = load8(signatures, index * 16);
        let s = load8(signatures, index * 16 + 8);
        let mut state = load8(prefix, 0);
        let mut first = 0;
        while first < 64 {
            let mut block = [0u32; 16];
            let mut w = 0;
            while w < 16 {
                *block.index_unchecked_mut(w) =
                    window::tx_word(input, shift, length, nonce, r, s, first + w);
                w += 1;
            }
            sha256::compress(&mut state, block);
            first += 16;
        }
        store8(hashes, index * 8, hash256_tail(state));
    }
}

/// Non-T2 stage C3: counts completed candidates and keeps those below the
/// target, as nonce and digest records up to the winner capacity.
#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(64)))]
pub fn pickaxe_shared_c3_winners(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] input: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 2)] counters: &mut [u32; 4],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 14)] hashes: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 16)] winners: &mut [u32],
) {
    use spirv_std::memory::Scope;
    unsafe {
        let index = id.x;
        let count = *counters.index_unchecked(0);
        if index >= count {
            return;
        }
        // One completion update per workgroup of 64 candidates.
        if index % 64 == 0 {
            spirv_std::arch::atomic_i_add::<u32, { Scope::QueueFamily as u32 }, 0>(
                counters.index_unchecked_mut(1),
                (count - index).min(64),
            );
        }
        let digest = load8(hashes, index as usize * 8);
        let shift = *input.index_unchecked(LAYOUT_SHIFT) as usize;
        let positive = *input.index_unchecked(POSITIVE_RULE) != 0;
        if !window::below_target(digest, input, TARGET_OFFSET + shift, positive) {
            return;
        }
        let slot = spirv_std::arch::atomic_i_add::<u32, { Scope::QueueFamily as u32 }, 0>(
            counters.index_unchecked_mut(2),
            1,
        ) as usize;
        if slot < winners.len() / 9 {
            *winners.index_unchecked_mut(slot * 9) =
                input.index_unchecked(NONCE_BASE).wrapping_add(index);
            store8(winners, slot * 9 + 1, digest);
        }
    }
}
