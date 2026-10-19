//! Buffer adapters only; hashing and target checks live in rust-engine.
#![cfg_attr(target_arch = "spirv", no_std)]

#[path = "../../../../rust-engine/src/electric.rs"]
pub mod electric;
#[path = "../../../../rust-engine/src/sha256.rs"]
pub mod sha256;

#[cfg(target_arch = "spirv")]
use spirv_std::{glam::UVec3, memory::Scope, spirv};

#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(128)))]
pub fn pickaxe_electric_filter(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] control: &[u32; 10],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 1)] result: &mut [u32; 2],
) {
    if id.x >= control[8] {
        return;
    }
    unsafe {
        if id.x % 128 == 0 {
            spirv_std::arch::atomic_i_add::<u32, { Scope::QueueFamily as u32 }, 0>(
                &mut result[1],
                (control[8] - id.x).min(128),
            );
        }
        if electric::meets(electric::candidate(control, id.x), control[9]) {
            spirv_std::arch::atomic_u_min::<u32, { Scope::QueueFamily as u32 }, 0>(
                &mut result[0],
                id.x,
            );
        }
    }
}

#[cfg(target_arch = "spirv")]
#[spirv(compute(threads(128)))]
pub fn pickaxe_electric_probe(
    #[spirv(global_invocation_id)] id: UVec3,
    #[spirv(storage_buffer, descriptor_set = 0, binding = 0)] control: &[u32; 10],
    #[spirv(storage_buffer, descriptor_set = 0, binding = 1)] output: &mut [u32],
) {
    if id.x >= control[8] || id.x as usize >= output.len() / 8 {
        return;
    }
    let digest = electric::candidate(control, id.x);
    for i in 0..8 {
        output[id.x as usize * 8 + i] = digest[i];
    }
}
