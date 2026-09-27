//! Experimental Rust mining arithmetic. Not a wallet signing library.
//! GPU and CPU checks compile the same arithmetic source.
#![cfg_attr(target_os = "cuda", no_std)]
#![cfg_attr(target_os = "cuda", feature(abi_ptx, stdarch_nvptx))]

pub mod field;
#[cfg(target_os = "cuda")]
mod gpu;
pub mod nonce;
pub mod point;
pub mod scalar;
pub mod sha256;
#[cfg(all(target_os = "cuda", feature = "upstream-rust"))]
mod upstream;

#[cfg(target_os = "cuda")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Fail the launch on bounds/invariant violations; never spin a GPU forever.
    unsafe { core::arch::nvptx::trap() }
}
