//! PHOTON T2 amount grinding, ported from shrec's v0.0.2 CUDA implementation.
//! Same signatures, amount coordinate, transaction layouts and winner ABI.
//! Host validation bounds every buffer and amount before launching these kernels.
use super::{index, meets, point_at, read, write};
use crate::sha256;
use core::sync::atomic::{AtomicU32, Ordering};

// Widest transaction (615 + PhotonLayout::MAX_SHIFT); must match cuda_t2.rs.
const WINDOW_BYTES: usize = 631;
// Must match cuda_t2.rs: 256 full windows plus one partial boundary window.
const HEAD_OFFSET: usize = 257 * 8;

#[inline(always)]
unsafe fn block<const SHIFT: usize, const BLOCK: usize>(
    tx: *const u8,
    baton: u64,
    reward: u64,
    j: u32,
) -> [u32; 16] {
    let byte = |pos: usize| {
        if (491 + SHIFT..499 + SHIFT).contains(&pos) {
            ((baton + u64::from(j)) >> (8 * (pos - 491 - SHIFT))) as u8
        } else if (578 + SHIFT..586 + SHIFT).contains(&pos) {
            ((reward - u64::from(j)) >> (8 * (pos - 578 - SHIFT))) as u8
        } else if pos < 615 + SHIFT {
            *tx.add(pos)
        } else if pos == 615 + SHIFT {
            0x80
        } else if pos >= 632 {
            (((615 + SHIFT) as u64 * 8) >> (8 * (639 - pos))) as u8
        } else {
            0
        }
    };
    macro_rules! word {
        ($i:literal) => {
            u32::from_be_bytes([
                byte(BLOCK * 64 + $i * 4),
                byte(BLOCK * 64 + $i * 4 + 1),
                byte(BLOCK * 64 + $i * 4 + 2),
                byte(BLOCK * 64 + $i * 4 + 3),
            ])
        };
    }
    [
        word!(0),
        word!(1),
        word!(2),
        word!(3),
        word!(4),
        word!(5),
        word!(6),
        word!(7),
        word!(8),
        word!(9),
        word!(10),
        word!(11),
        word!(12),
        word!(13),
        word!(14),
        word!(15),
    ]
}

#[inline(always)]
unsafe fn hash<const SHIFT: usize>(
    tx: *const u8,
    prefix: *const u32,
    baton: u64,
    reward: u64,
    j: u32,
) -> [u8; 32] {
    let mut state = read(prefix);
    sha256::compress(&mut state, block::<SHIFT, 7>(tx, baton, reward, j));
    sha256::compress(&mut state, block::<SHIFT, 8>(tx, baton, reward, j));
    sha256::compress(&mut state, block::<SHIFT, 9>(tx, baton, reward, j));
    sha256::hash_state(state)
}

unsafe fn prepare<const SHIFT: usize>(
    base_tx: *const u8,
    midstate: *const u32,
    signatures: *const u8,
    negated_s: *const u8,
    points: *const u32,
    nonce_base: u32,
    count: u32,
    window_txs: *mut u8,
    prefixes: *mut u32,
) {
    let window = index() as usize;
    if window >= count as usize {
        return;
    }
    let point = point_at(points, window);
    let square = point.y.mul_mod(point.z).is_square();
    let signature = signatures.add(window * 64);
    let s = if square {
        signature.add(32)
    } else {
        negated_s.add(window * 32)
    };
    let tx = window_txs.add(window * WINDOW_BYTES);
    for i in 0..615 + SHIFT {
        *tx.add(i) = *base_tx.add(i);
    }
    write(
        tx.add(390 + SHIFT),
        &nonce_base.wrapping_add(window as u32).to_le_bytes(),
    );
    write(tx.add(426 + SHIFT), &read::<32, _>(signature));
    write(tx.add(458 + SHIFT), &read::<32, _>(s));
    let mut state = read(midstate);
    sha256::compress_bytes(&mut state, read(tx.add(384)));
    write(prefixes.add(window * 8), &state);
    // Rounds 0..9 do not touch token amounts. Compute once per signature,
    // retaining the native prefix ABI for diagnostic value-grinding kernels.
    let words = core::array::from_fn(|i| u32::from_be_bytes(read(tx.add(448 + i * 4))));
    write(
        prefixes.add(HEAD_OFFSET + window * 8),
        &sha256::head10(state, words),
    );
}

unsafe fn filter<const SHIFT: usize>(
    tx: *const u8,
    prefix: *const u32,
    baton: u64,
    reward: u64,
    target: *const u8,
    base: u32,
    count: u32,
    cap: u32,
    winner_count: *mut u32,
    winner_j: *mut u32,
    hashes: *mut u8,
) {
    let index = index();
    if index >= count {
        return;
    }
    let j = base + index;
    let digest = hash::<SHIFT>(tx, prefix, baton, reward, j);
    if !meets(&digest, &read(target), *target.add(32) != 0) {
        return;
    }
    let slot = AtomicU32::from_ptr(winner_count).fetch_add(1, Ordering::Relaxed);
    if slot >= cap {
        return;
    }
    *winner_j.add(slot as usize) = j;
    write(hashes.add(slot as usize * 32), &digest);
}

#[inline(always)]
unsafe fn group<const SHIFT: usize>(
    txs: *const u8,
    prefixes: *const u32,
    middle: *const u32,
    baton: u64,
    reward: u64,
    target: *const u8,
    nonce_base: u32,
    j_base: u32,
    count: u32,
    cap: u32,
    winner_count: *mut u32,
    nonces: *mut u32,
    winner_j: *mut u32,
    hashes: *mut u8,
) {
    // SAFETY: one 64-word array per block. Each word has one writer;
    // all threads reach the barrier before any thread reads or returns.
    let shared: *mut u32;
    core::arch::asm!(
        "{{ .shared .align 4 .b32 schedule[64];",
        "cvta.shared.u64 {ptr}, schedule; }}",
        ptr = out(reg64) shared,
        options(nostack),
    );
    let lane = core::arch::nvptx::_thread_idx_x() as usize;
    if lane < 64 {
        *shared.add(lane) = *middle.add(lane);
    }
    core::arch::nvptx::_syncthreads();
    let index = index();
    if index >= count {
        return;
    }
    let position = j_base + index;
    let window = (position >> 16) as usize;
    let j = position & 0xffff;
    let tx = txs.add(window * WINDOW_BYTES);
    let mut state = read(prefixes.add(window * 8));
    let head = read(prefixes.add(HEAD_OFFSET + window * 8));
    sha256::compress_from::<10>(&mut state, block::<SHIFT, 7>(tx, baton, reward, j), head);
    // Broadcast the fixed schedule from block-local shared memory.
    sha256::compress_scheduled(&mut state, &*shared.cast::<[u32; 64]>());
    sha256::compress(&mut state, block::<SHIFT, 9>(tx, baton, reward, j));
    let strict_positive = *target.add(32) != 0;
    let Some(digest) =
        sha256::hash_state_filtered_with_rule(state, *target.add(31), strict_positive)
    else {
        return;
    };
    if !meets(&digest, &read(target), strict_positive) {
        return;
    }
    let slot = AtomicU32::from_ptr(winner_count).fetch_add(1, Ordering::Relaxed);
    if slot >= cap {
        return;
    }
    *nonces.add(slot as usize) = nonce_base.wrapping_add(window as u32);
    *winner_j.add(slot as usize) = j;
    write(hashes.add(slot as usize * 32), &digest);
}

macro_rules! kernels {
    ($prepare:ident, $group:ident, $filter:ident, $probe:ident, $shift:literal) => {
        #[no_mangle]
        pub unsafe extern "ptx-kernel" fn $prepare(
            tx: *const u8,
            midstate: *const u32,
            signatures: *const u8,
            negated: *const u8,
            points: *const u32,
            base: u32,
            count: u32,
            windows: *mut u8,
            prefixes: *mut u32,
        ) {
            prepare::<$shift>(
                tx, midstate, signatures, negated, points, base, count, windows, prefixes,
            );
        }
        #[no_mangle]
        pub unsafe extern "ptx-kernel" fn $group(
            txs: *const u8,
            prefixes: *const u32,
            middle: *const u32,
            baton: u64,
            reward: u64,
            target: *const u8,
            base: u32,
            j_base: u32,
            count: u32,
            cap: u32,
            winner_count: *mut u32,
            nonces: *mut u32,
            js: *mut u32,
            hashes: *mut u8,
        ) {
            group::<$shift>(
                txs,
                prefixes,
                middle,
                baton,
                reward,
                target,
                base,
                j_base,
                count,
                cap,
                winner_count,
                nonces,
                js,
                hashes,
            );
        }
        #[no_mangle]
        pub unsafe extern "ptx-kernel" fn $filter(
            tx: *const u8,
            prefix: *const u32,
            baton: u64,
            reward: u64,
            target: *const u8,
            base: u32,
            count: u32,
            cap: u32,
            winner_count: *mut u32,
            js: *mut u32,
            hashes: *mut u8,
        ) {
            filter::<$shift>(
                tx,
                prefix,
                baton,
                reward,
                target,
                base,
                count,
                cap,
                winner_count,
                js,
                hashes,
            );
        }
        #[no_mangle]
        pub unsafe extern "ptx-kernel" fn $probe(
            tx: *const u8,
            prefix: *const u32,
            baton: u64,
            reward: u64,
            j: u16,
            output: *mut u8,
        ) {
            if index() == 0 {
                write(
                    output,
                    &hash::<$shift>(tx, prefix, baton, reward, u32::from(j)),
                );
            }
        }
    };
}
kernels!(
    pickaxe_t2_prepare_shift0,
    pickaxe_t2_filter_group_shift0,
    pickaxe_t2_filter_shift0,
    pickaxe_t2_probe_shift0,
    0
);
kernels!(
    pickaxe_t2_prepare_shift1,
    pickaxe_t2_filter_group_shift1,
    pickaxe_t2_filter_shift1,
    pickaxe_t2_probe_shift1,
    1
);
kernels!(
    pickaxe_t2_prepare_shift2,
    pickaxe_t2_filter_group_shift2,
    pickaxe_t2_filter_shift2,
    pickaxe_t2_probe_shift2,
    2
);
kernels!(
    pickaxe_t2_prepare_shift3,
    pickaxe_t2_filter_group_shift3,
    pickaxe_t2_filter_shift3,
    pickaxe_t2_probe_shift3,
    3
);
kernels!(
    pickaxe_t2_prepare_shift14,
    pickaxe_t2_filter_group_shift14,
    pickaxe_t2_filter_shift14,
    pickaxe_t2_probe_shift14,
    14
);
kernels!(
    pickaxe_t2_prepare_shift15,
    pickaxe_t2_filter_group_shift15,
    pickaxe_t2_filter_shift15,
    pickaxe_t2_probe_shift15,
    15
);
kernels!(
    pickaxe_t2_prepare_shift16,
    pickaxe_t2_filter_group_shift16,
    pickaxe_t2_filter_shift16,
    pickaxe_t2_probe_shift16,
    16
);
