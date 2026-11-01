//! Separate CUDA entry points keep PHOTON's compilation unit unchanged.
#![cfg_attr(target_os = "cuda", no_std)]
#![cfg_attr(
    target_os = "cuda",
    feature(abi_gpu_kernel, stdarch_nvptx, asm_experimental_arch)
)]
#[path = "../src/electric.rs"]
pub mod electric;
#[path = "../src/sha256.rs"]
pub mod sha256;

#[cfg(target_os = "cuda")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { core::arch::nvptx::trap() }
}

#[cfg(target_os = "cuda")]
mod gpu {
    use core::{
        arch::nvptx,
        sync::atomic::{AtomicU32, Ordering},
    };
    fn index() -> u32 {
        unsafe { nvptx::_block_idx_x() * nvptx::_block_dim_x() + nvptx::_thread_idx_x() }
    }
    #[no_mangle]
    pub unsafe extern "gpu-kernel" fn pickaxe_electric_filter(
        control: *const u32,
        result: *mut u32,
    ) {
        let index = index();
        let control = &*control.cast::<[u32; 10]>();
        if index >= control[8] {
            return;
        }
        if index % 128 == 0 {
            (&*result.add(1).cast::<AtomicU32>())
                .fetch_add((control[8] - index).min(128), Ordering::Relaxed);
        }
        if crate::electric::meets(crate::electric::candidate(control, index), control[9]) {
            (&*result.cast::<AtomicU32>()).fetch_min(index, Ordering::Relaxed);
        }
    }
    #[no_mangle]
    pub unsafe extern "gpu-kernel" fn pickaxe_electric_probe(
        control: *const u32,
        output: *mut u32,
    ) {
        let index = index();
        let control = &*control.cast::<[u32; 10]>();
        if index < control[8] {
            let digest = crate::electric::candidate(control, index);
            core::ptr::copy_nonoverlapping(digest.as_ptr(), output.add(index as usize * 8), 8);
        }
    }
}
