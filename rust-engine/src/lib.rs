//! Experimental Rust mining arithmetic. Not a wallet signing library.
//! GPU and CPU checks compile the same arithmetic source.
#![cfg_attr(any(target_os = "cuda", target_os = "amdhsa"), no_std)]
#![cfg_attr(
    target_os = "cuda",
    feature(abi_gpu_kernel, stdarch_nvptx, asm_experimental_arch)
)]
#![cfg_attr(
    target_os = "amdhsa",
    feature(abi_gpu_kernel, stdarch_amdgpu, core_intrinsics)
)]
// abort() lowers to s_trap so a failed AMD kernel faults its dispatch.
#![cfg_attr(target_os = "amdhsa", allow(internal_features))]

pub mod field;
#[cfg(any(target_os = "cuda", target_os = "amdhsa"))]
mod gpu;
pub mod nonce;
pub mod point;
pub mod scalar;
pub mod sha256;
// #### PR #22: portable-only signing helpers stay out of native GPU builds,
// whose verified PTX and code objects must not change.
#[cfg(not(any(target_os = "cuda", target_os = "amdhsa")))]
pub mod sign;
pub mod t2_block;
#[cfg(all(
    any(target_os = "cuda", target_os = "amdhsa"),
    feature = "upstream-rust"
))]
mod upstream;
pub mod wide;

#[cfg(target_os = "cuda")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Fail the launch on bounds/invariant violations; never spin a GPU forever.
    unsafe { core::arch::nvptx::trap() }
}

#[cfg(target_os = "amdhsa")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Fail the dispatch, as the CUDA build traps; never spin a GPU forever.
    core::intrinsics::abort()
}
