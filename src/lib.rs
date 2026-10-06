//! Shared Pickaxe mining implementation for native and browser clients.
//! Native and browser adapters use the same protocol and mining primitives.

#[cfg(not(target_arch = "wasm32"))]
pub mod backend;
pub mod backend_kind;
#[cfg(not(target_arch = "wasm32"))]
pub mod benchmark;
#[cfg(not(target_arch = "wasm32"))]
pub mod cli;
pub mod config;
pub mod crypto;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
#[allow(dead_code, clippy::needless_range_loop)]
mod cuda_miner;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
pub mod cuda_photon;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod cuda_stage_a;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod cuda_stage_a_ref;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod cuda_stage_b;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
pub mod cuda_stage_c;
pub mod donation;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
pub mod electrum;
pub mod gpu_types;
#[cfg(not(target_arch = "wasm32"))]
pub mod hip_photon;
#[allow(dead_code)]
pub mod m29_table;
#[cfg(not(target_arch = "wasm32"))]
pub mod mining_lock;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
pub mod node;
pub mod proof;
#[allow(dead_code)]
pub mod protocol;
pub mod reward;
#[cfg(not(target_arch = "wasm32"))]
pub mod runtime;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
pub mod search;
#[cfg(not(target_arch = "wasm32"))]
pub mod self_test;
#[cfg(not(target_arch = "wasm32"))]
pub mod stratum_v2;
#[cfg(not(target_arch = "wasm32"))]
#[cfg(test)]
mod stage_b;
#[cfg(not(target_arch = "wasm32"))]
pub mod telemetry;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
pub mod tui;
pub mod tx;
#[cfg(feature = "portable-wgpu")]
pub mod wgpu_photon;

#[cfg(all(target_arch = "wasm32", feature = "portable-wgpu"))]
pub mod browser;
pub mod fee;
pub mod live_job;
pub mod mining_job;

mod mining_control;
