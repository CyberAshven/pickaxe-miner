//! Offline compilation probe. This kernel is not selected by the miner.
#![cfg_attr(target_arch = "spirv", no_std)]

use spirv_std::{glam::UVec3, spirv};

// Compile the production SHA implementation directly, without another copy.
#[path = "../../../../rust-engine/src/sha256.rs"]
pub mod sha256;

#[path = "../../../../rust-engine/src/t2_block.rs"]
pub mod t2_block;

// #### PR #22: exercise the production transaction layout through both compiler
// boundaries. The CPU fixture independently serializes amounts and padding.
#[inline(always)]
fn assembled<const SHIFT: usize, const BLOCK: usize>(inputs: &[u32], base: usize) -> [u32; 16] {
    #[cfg(not(target_arch = "spirv"))]
    {
        let mut bytes = [0u8; 640];
        for i in 0..160 {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&inputs[base + i].to_le_bytes());
        }
        let baton = u64::from(inputs[base + 160]) | (u64::from(inputs[base + 161]) << 32);
        let reward = u64::from(inputs[base + 162]) | (u64::from(inputs[base + 163]) << 32);
        unsafe {
            t2_block::block::<SHIFT, BLOCK>(bytes.as_ptr(), baton, reward, inputs[base + 164])
        }
    }
    #[cfg(target_arch = "spirv")]
    {
        let baton = t2_block::Amount {
            lo: inputs[base + 160],
            hi: inputs[base + 161],
        };
        let reward = t2_block::Amount {
            lo: inputs[base + 162],
            hi: inputs[base + 163],
        };
        unsafe {
            t2_block::block::<SHIFT, BLOCK>(
                inputs,
                base,
                0,
                false,
                baton,
                reward,
                inputs[base + 164],
            )
        }
    }
}

#[spirv(compute(threads(64)))]
pub fn pickaxe_t2_assembly_proof(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] inputs: &[u32],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 1)] outputs: &mut [u32],
) {
    let index = id.x as usize;
    if index >= inputs.len() / 176 || index >= outputs.len() / 48 {
        return;
    }
    let base = index * 176;
    macro_rules! blocks {
        ($shift:literal) => {{
            let first = assembled::<$shift, 7>(inputs, base);
            let middle = assembled::<$shift, 8>(inputs, base);
            let last = assembled::<$shift, 9>(inputs, base);
            for i in 0..16 {
                outputs[index * 48 + i] = first[i];
                outputs[index * 48 + 16 + i] = middle[i];
                outputs[index * 48 + 32 + i] = last[i];
            }
        }};
    }
    match inputs[base + 165] {
        0 => blocks!(0),
        1 => blocks!(1),
        2 => blocks!(2),
        3 => blocks!(3),
        4 => blocks!(4),
        5 => blocks!(5),
        6 => blocks!(6),
        7 => blocks!(7),
        8 => blocks!(8),
        9 => blocks!(9),
        10 => blocks!(10),
        11 => blocks!(11),
        12 => blocks!(12),
        13 => blocks!(13),
        14 => blocks!(14),
        15 => blocks!(15),
        16 => blocks!(16),
        _ => {}
    }
}

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
