//! Offline compilation probe. This kernel is not selected by the miner.
#![cfg_attr(target_arch = "spirv", no_std)]

use spirv_std::{glam::UVec3, spirv};

// Compile the production SHA implementation directly, without another copy.
#[path = "../../../../rust-engine/src/sha256.rs"]
pub mod sha256;

// Rust-GPU cannot lower the fixed-array to runtime-slice cast required by copy_from_slice.
#[allow(clippy::manual_memcpy)]
#[inline(always)]
fn load<const N: usize>(inputs: &[u32], base: usize) -> [u32; N] {
    let mut words = [0; N];
    for i in 0..N {
        words[i] = inputs[base + i];
    }
    words
}

/// Reproduce the compression chain in the native T2 group filter.
/// Input records contain prefix, head10, first block, middle schedule,
/// final block, high-byte limit and positive-hash rule, padded to 128 words.
#[spirv(compute(threads(64)))]
pub fn pickaxe_t2_hash_proof(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] inputs: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 1)] outputs: &mut [u32],
) {
    let index = id.x as usize;
    if index >= inputs.len() / 128 || index >= outputs.len() / 9 {
        return;
    }
    let base = index * 128;
    let mut state = load(inputs, base);
    let head = load(inputs, base + 8);
    let first = load(inputs, base + 16);
    let middle = load(inputs, base + 32);
    let last = load(inputs, base + 96);
    sha256::compress_from::<10>(&mut state, first, head);
    sha256::compress_scheduled(&mut state, &middle);
    sha256::compress(&mut state, last);
    let mut digest = [0u32; 8];
    let valid = sha256::hash_state_words_filtered(
        state,
        inputs[base + 112],
        inputs[base + 113] != 0,
        &mut digest,
    );
    outputs[index * 9 + 8] = u32::from(valid);
    if valid {
        for i in 0..8 {
            outputs[index * 9 + i] = digest[i];
        }
    }
}
