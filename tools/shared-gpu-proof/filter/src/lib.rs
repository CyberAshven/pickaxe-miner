//! Thin portable entry points for the production Rust T2 SHA/layout code.
#![cfg_attr(target_arch = "spirv", no_std)]
#![allow(clippy::too_many_arguments)]

#[path = "../../../../rust-engine/src/sha256.rs"]
pub mod sha256;
#[path = "../../../../rust-engine/src/t2_block.rs"]
pub mod t2_block;

#[cfg(target_arch = "spirv")]
use spirv_std::{arch::IndexUnchecked, glam::UVec3, memory::Scope, spirv};

// #### PR #22: buffer/index/atomic adapters only. Mining arithmetic is imported
// directly from rust-engine; keep this candidate opt-in until speed gates pass.
// Control words: base, count, windows, signature_base, baton(lo,hi), reward(lo,hi),
// target LE words[8], positive proof. Host validates range and storage sizes.
#[cfg(target_arch = "spirv")]
#[inline(always)]
unsafe fn filter<const SHIFT: usize>(
    id: UVec3,
    lane: u32,
    windows: &[u32],
    control: &[u32; 17],
    result: &mut [u32; 4],
    winners: &mut [u32],
    middle: &mut [u32; 64],
) {
    // Host allocates at least one 128-word window. The entry point has exactly
    // 64 invocations, so lane is in 0..64. Avoid compiler panic branches here:
    // every invocation must reach the barrier in uniform control flow.
    *middle.index_unchecked_mut(lane as usize) = *windows.index_unchecked(64 + lane as usize);
    spirv_std::arch::workgroup_memory_barrier_with_group_sync();
    let index = id.x + id.y * 16384 * 64;
    if index >= control[1] {
        return;
    }
    // Every active workgroup contributes once, including early-rejected hashes.
    if lane == 0 {
        spirv_std::arch::atomic_i_add::<u32, { Scope::QueueFamily as u32 }, 0>(
            &mut result[1],
            (control[1] - index).min(64),
        );
    }
    let position = control[0] + index;
    let window = (position >> 16) as usize;
    let j = position & 65535;
    let base = window * 128;
    let baton = t2_block::Amount {
        lo: control[4],
        hi: control[5],
    };
    let reward = t2_block::Amount {
        lo: control[6],
        hi: control[7],
    };
    // Host candidate bounds imply window < allocated record count. Every
    // record has 128 words; the two states fit completely inside that record.
    macro_rules! state_at {
        ($offset:expr) => {{
            let start = base + $offset;
            [
                *windows.index_unchecked(start),
                *windows.index_unchecked(start + 1),
                *windows.index_unchecked(start + 2),
                *windows.index_unchecked(start + 3),
                *windows.index_unchecked(start + 4),
                *windows.index_unchecked(start + 5),
                *windows.index_unchecked(start + 6),
                *windows.index_unchecked(start + 7),
            ]
        }};
    }
    let mut state = state_at!(0);
    let head = state_at!(56);
    let first = t2_block::block::<SHIFT, 7>(windows, base + 8, 448, true, baton, reward, j);
    sha256::compress_from::<10>(&mut state, first, head);
    sha256::compress_scheduled(&mut state, middle);
    let last = t2_block::block::<SHIFT, 9>(windows, base + 8, 448, true, baton, reward, j);
    sha256::compress(&mut state, last);
    let mut digest = [0u32; 8];
    let positive = control[16] != 0;
    if !sha256::hash_state_words_filtered(state, control[15] >> 24, positive, &mut digest) {
        return;
    }
    if positive {
        let mut nonzero = 0;
        for i in 0..8 {
            nonzero |= digest[i];
        }
        if digest[7] & 128 != 0 || nonzero == 0 {
            return;
        }
    }
    let mut valid = false;
    for k in 0..8 {
        let i = 7 - k;
        let word = if i == 7 {
            digest[i].swap_bytes() & 0x7fff_ffff
        } else {
            digest[i].swap_bytes()
        };
        let target = control[8 + i];
        if word < target {
            valid = true;
            break;
        }
        if word > target {
            break;
        }
    }
    if !valid {
        return;
    }
    let slot =
        spirv_std::arch::atomic_i_add::<u32, { Scope::QueueFamily as u32 }, 0>(&mut result[2], 1)
            as usize;
    if slot >= winners.len() / 9 {
        return;
    }
    winners[slot * 9] = control[3].wrapping_mul(65536).wrapping_add(position);
    for i in 0..8 {
        winners[slot * 9 + 1 + i] = digest[i];
    }
}

#[cfg(target_arch = "spirv")]
macro_rules! entry {
    ($name:ident, $shift:literal) => {
        #[spirv(compute(threads(64)))]
        pub fn $name(
            #[spirv(global_invocation_id)] id: UVec3,
            #[spirv(local_invocation_index)] lane: u32,
            #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] windows: &[u32],
            #[spirv(storage_buffer, descriptor_set = 0, binding = 1)] control: &[u32; 17],
            #[spirv(storage_buffer, descriptor_set = 0, binding = 2)] result: &mut [u32; 4],
            #[spirv(storage_buffer, descriptor_set = 0, binding = 3)] winners: &mut [u32],
            #[spirv(workgroup)] middle: &mut [u32; 64],
        ) {
            unsafe {
                filter::<$shift>(id, lane, windows, control, result, winners, middle);
            }
        }
    };
}

#[cfg(target_arch = "spirv")]
pub mod entries {
    use super::*;
    entry!(pickaxe_shared_t2_0, 0);
    entry!(pickaxe_shared_t2_1, 1);
    entry!(pickaxe_shared_t2_2, 2);
    entry!(pickaxe_shared_t2_3, 3);
    entry!(pickaxe_shared_t2_4, 4);
    entry!(pickaxe_shared_t2_5, 5);
    entry!(pickaxe_shared_t2_6, 6);
    entry!(pickaxe_shared_t2_7, 7);
    entry!(pickaxe_shared_t2_8, 8);
    entry!(pickaxe_shared_t2_9, 9);
    entry!(pickaxe_shared_t2_10, 10);
    entry!(pickaxe_shared_t2_11, 11);
    entry!(pickaxe_shared_t2_12, 12);
    entry!(pickaxe_shared_t2_13, 13);
    entry!(pickaxe_shared_t2_14, 14);
    entry!(pickaxe_shared_t2_15, 15);
    entry!(pickaxe_shared_t2_16, 16);
}
