//! PHOTON T2 amount grinding, ported from shrec's v0.0.2 CUDA implementation.
//! Same signatures, amount coordinate, transaction layouts and winner ABI.
//! Host validation bounds every buffer and amount before launching these kernels.
#[cfg(target_os = "cuda")]
use super::index;
#[cfg(target_os = "amdhsa")]
use super::index_in;
use super::{meets, point_at, read, write};
use crate::sha256;
use core::sync::atomic::{AtomicU32, Ordering};

// Widest transaction (615 + PhotonLayout::MAX_SHIFT); must match cuda_t2.rs.
const WINDOW_BYTES: usize = 631;
// Must match the host T2_MAX_WINDOWS: full windows plus one partial boundary
// window (cuda_t2.rs on CUDA, hip_photon.rs on AMD).
#[cfg(target_os = "cuda")]
const HEAD_OFFSET: usize = 1025 * 8;
#[cfg(not(target_os = "cuda"))]
const HEAD_OFFSET: usize = 257 * 8;
// The baton starts at byte 491 + SHIFT, so block-7 words before this one
// never depend on the amount and their rounds run once per window.
#[cfg(target_os = "cuda")]
const fn first_amount_word(shift: usize) -> usize {
    (491 + shift - 448) / 4
}

use crate::t2_block::block;

// #### CUDA T2: balance SHA-256 additions across the ALU and IMAD pipes
// What: on sm_120 the group kernel was bound by the ALU pipe alone: 95% of
// its warp instructions issued there (98.6% busy) while the FMA pipe idled
// near 1.5%. `mad.lo.u32 d, a, one, b` computes `a + b` as an IMAD on the FMA
// pipe. The schedule, the feed-forward and the three combining additions of
// each round (t, d and h) use it; `h + s1`, `ch + K + w` and `s0 + maj` stay
// on the ALU, which keeps each round's dependency chain short.
// Why: RTX 5060 Ti, interleaved built-in benchmark at intensity 100:
// 1195 -> 1246 MH/s (+4.3%). Routing every addition through IMAD measured
// slower (+3.5%) despite 26% fewer ALU instructions, and register caps
// (.maxnreg 40/48) spill and lose.
// Check: mirrors crate::sha256 round for round, including the round-60
// reject, so digests stay bit-identical. CUDA only: AMD, SPIR-V/WGSL and host
// builds keep crate::sha256, and no kernel signature changes.
#[cfg(target_os = "cuda")]
mod imad {
    /// A `1` that ptxas cannot fold, so `mad.lo.u32` stays an IMAD.
    #[no_mangle]
    #[used]
    pub static pickaxe_t2_imad_one: u32 = 1;

    /// Reads the device `1`; call once per thread and pass it down.
    /// `read_volatile` is not enough: NVPTX marks a load from an immutable
    /// global invariant and emits `ld.global.nc`, whose value ptxas may fold.
    #[inline(always)]
    pub fn one() -> u32 {
        let one;
        unsafe {
            core::arch::asm!(
                "ld.volatile.global.u32 {one}, [pickaxe_t2_imad_one];",
                one = out(reg32) one,
                options(nostack),
            );
        }
        one
    }

    /// `a + b`, issued on the FMA pipe as an IMAD.
    #[inline(always)]
    fn add(a: u32, one: u32, b: u32) -> u32 {
        let result;
        unsafe {
            core::arch::asm!(
                "mad.lo.u32 {result}, {a}, {one}, {b};",
                result = out(reg32) result,
                a = in(reg32) a, one = in(reg32) one, b = in(reg32) b,
                options(pure, nomem, nostack),
            );
        }
        result
    }

    // crate::sha256 keeps `choose` and `K` private and its file is pinned by
    // the portable shader manifests, so both are repeated here.
    #[inline(always)]
    fn choose(a: u32, b: u32, c: u32) -> u32 {
        let result;
        unsafe {
            core::arch::asm!(
                "lop3.b32 {result}, {a}, {b}, {c}, 0xca;",
                result = out(reg32) result,
                a = in(reg32) a, b = in(reg32) b, c = in(reg32) c,
                options(pure, nomem, nostack),
            );
        }
        result
    }

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    /// The padded block `crate::sha256::hash_state` hashes second.
    #[inline(always)]
    pub fn tail_block(first_hash: [u32; 8]) -> [u32; 16] {
        let mut block = [0; 16];
        block[..8].copy_from_slice(&first_hash);
        block[8] = 0x80000000;
        block[15] = 256;
        block
    }

    /// The first `start` rounds of a block, before any token amount byte;
    /// the group kernel resumes from this state. Runs once per window.
    #[inline(always)]
    pub fn head(mut state: [u32; 8], w: [u32; 16], start: usize) -> [u32; 8] {
        for i in 0..start {
            let [a, b, c, d, e, f, g, h] = state;
            let t = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add(choose(e, f, g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            state = [t.wrapping_add(t2), a, b, c, d.wrapping_add(t), e, f, g];
        }
        state
    }

    /// `crate::sha256::compress_from::<start>` with balanced additions. With
    /// `FILTER` it applies that module's round-60 reject and returns `false`
    /// for exactly the hashes the reject drops. `start` is a per-layout
    /// constant after inlining, so the skipped rounds fold away.
    #[inline(always)]
    pub fn compress<const FILTER: bool>(
        state: &mut [u32; 8],
        mut w: [u32; 16],
        head: [u32; 8],
        start: usize,
        one: u32,
        limit: u8,
        strict_positive: bool,
    ) -> bool {
        // Unrolled rounds rotate variable roles rather than copying eight words.
        // Map a resumed logical state back to those roles at start.
        let head: [u32; 8] = core::array::from_fn(|i| head[(i + start) & 7]);
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = head;
        macro_rules! round {
            ($i:expr, $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => {{
                if $i >= 16 {
                    let x = w[($i + 1) & 15];
                    let y = w[($i + 14) & 15];
                    let s0 = x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3);
                    let s1 = y.rotate_right(17) ^ y.rotate_right(19) ^ (y >> 10);
                    w[$i & 15] = add(
                        add(add(w[$i & 15], one, s0), one, w[($i + 9) & 15]),
                        one,
                        s1,
                    );
                }
                if $i >= start {
                    let s1 = $e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25);
                    let ch = choose($e, $f, $g);
                    let t = add(
                        $h.wrapping_add(s1),
                        one,
                        ch.wrapping_add(K[$i]).wrapping_add(w[$i & 15]),
                    );
                    let s0 = $a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22);
                    let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);
                    $d = add($d, one, t);
                    if FILTER && $i == 60 {
                        // Logical e after round 60 becomes final h after round 63.
                        // Its low byte is digest[31], PHOTON's first comparison byte.
                        let high = state[7].wrapping_add($d) as u8;
                        let comparable = if strict_positive { high } else { high & 0x7f };
                        if comparable > limit {
                            return false;
                        }
                    }
                    $h = add(t, one, s0.wrapping_add(maj));
                }
            }};
        }
        macro_rules! eight {
            ($i:literal) => {
                round!($i, a, b, c, d, e, f, g, h);
                round!($i + 1, h, a, b, c, d, e, f, g);
                round!($i + 2, g, h, a, b, c, d, e, f);
                round!($i + 3, f, g, h, a, b, c, d, e);
                round!($i + 4, e, f, g, h, a, b, c, d);
                round!($i + 5, d, e, f, g, h, a, b, c);
                round!($i + 6, c, d, e, f, g, h, a, b);
                round!($i + 7, b, c, d, e, f, g, h, a);
            };
        }
        eight!(0);
        eight!(8);
        eight!(16);
        eight!(24);
        eight!(32);
        eight!(40);
        eight!(48);
        eight!(56);
        // Keep the feed-forward in registers too; the NVPTX backend does not
        // unroll the iterator/zip form and otherwise stores each block's state locally.
        state[0] = add(state[0], one, a);
        state[1] = add(state[1], one, b);
        state[2] = add(state[2], one, c);
        state[3] = add(state[3], one, d);
        state[4] = add(state[4], one, e);
        state[5] = add(state[5], one, f);
        state[6] = add(state[6], one, g);
        state[7] = add(state[7], one, h);
        true
    }

    /// `crate::sha256::compress_scheduled` with balanced additions.
    #[inline(always)]
    pub fn compress_scheduled(state: &mut [u32; 8], w: &[u32; 64], one: u32) {
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        macro_rules! round {
            ($i:expr, $a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident) => {{
                let s1 = $e.rotate_right(6) ^ $e.rotate_right(11) ^ $e.rotate_right(25);
                let ch = choose($e, $f, $g);
                let t = add(
                    $h.wrapping_add(s1),
                    one,
                    ch.wrapping_add(K[$i]).wrapping_add(w[$i]),
                );
                let s0 = $a.rotate_right(2) ^ $a.rotate_right(13) ^ $a.rotate_right(22);
                let maj = ($a & $b) ^ ($a & $c) ^ ($b & $c);
                $d = add($d, one, t);
                $h = add(t, one, s0.wrapping_add(maj));
            }};
        }
        macro_rules! eight {
            ($i:literal) => {
                round!($i, a, b, c, d, e, f, g, h);
                round!($i + 1, h, a, b, c, d, e, f, g);
                round!($i + 2, g, h, a, b, c, d, e, f);
                round!($i + 3, f, g, h, a, b, c, d, e);
                round!($i + 4, e, f, g, h, a, b, c, d);
                round!($i + 5, d, e, f, g, h, a, b, c);
                round!($i + 6, c, d, e, f, g, h, a, b);
                round!($i + 7, b, c, d, e, f, g, h, a);
            };
        }
        eight!(0);
        eight!(8);
        eight!(16);
        eight!(24);
        eight!(32);
        eight!(40);
        eight!(48);
        eight!(56);
        state[0] = add(state[0], one, a);
        state[1] = add(state[1], one, b);
        state[2] = add(state[2], one, c);
        state[3] = add(state[3], one, d);
        state[4] = add(state[4], one, e);
        state[5] = add(state[5], one, f);
        state[6] = add(state[6], one, g);
        state[7] = add(state[7], one, h);
    }
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
    let window = gpu_index!(128) as usize;
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
    // Rounds before the baton do not touch token amounts. Compute once per
    // signature, retaining the native prefix ABI for diagnostic value-grinding
    // kernels. CUDA runs every round before the first amount word (10, 11 or
    // 14 by layout); other targets keep the shared round-10 resume.
    let words = core::array::from_fn(|i| u32::from_be_bytes(read(tx.add(448 + i * 4))));
    #[cfg(target_os = "cuda")]
    let head = imad::head(state, words, first_amount_word(SHIFT));
    #[cfg(not(target_os = "cuda"))]
    let head = sha256::head10(state, words);
    write(prefixes.add(HEAD_OFFSET + window * 8), &head);
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
    let index = gpu_index!(128);
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
    #[cfg(target_os = "cuda")]
    let shared: *mut u32;
    #[cfg(target_os = "cuda")]
    core::arch::asm!(
        "{{ .shared .align 4 .b32 schedule[64];",
        "cvta.shared.u64 {ptr}, schedule; }}",
        ptr = out(reg64) shared,
        options(nostack),
    );
    #[cfg(target_os = "cuda")]
    let lane = core::arch::nvptx::_thread_idx_x() as usize;
    #[cfg(target_os = "cuda")]
    if lane < 64 {
        *shared.add(lane) = *middle.add(lane);
    }
    #[cfg(target_os = "cuda")]
    core::arch::nvptx::_syncthreads();
    // AMD reads the job-wide schedule straight from global memory: every lane
    // loads the same address, which the scalar cache serves.
    #[cfg(target_os = "amdhsa")]
    let shared = middle;
    let index = gpu_index!(256);
    if index >= count {
        return;
    }
    let position = j_base + index;
    let window = (position >> 16) as usize;
    let j = position & 0xffff;
    let tx = txs.add(window * WINDOW_BYTES);
    let mut state = read(prefixes.add(window * 8));
    let head = read(prefixes.add(HEAD_OFFSET + window * 8));
    // CUDA runs the IMAD-balanced compressions; every other target keeps the
    // shared sha256 chain, in its original order so AMD code is unchanged.
    // Both produce the same digest for the same inputs.
    #[cfg(target_os = "cuda")]
    let (strict_positive, digest) = {
        let strict_positive = *target.add(32) != 0;
        let one = imad::one();
        imad::compress::<false>(
            &mut state,
            block::<SHIFT, 7>(tx, baton, reward, j),
            head,
            first_amount_word(SHIFT),
            one,
            0,
            false,
        );
        // Broadcast the fixed schedule from block-local shared memory.
        imad::compress_scheduled(&mut state, &*shared.cast::<[u32; 64]>(), one);
        let resumed = state;
        imad::compress::<false>(
            &mut state,
            block::<SHIFT, 9>(tx, baton, reward, j),
            resumed,
            0,
            one,
            0,
            false,
        );
        let mut second = sha256::INITIAL;
        if !imad::compress::<true>(
            &mut second,
            imad::tail_block(state),
            sha256::INITIAL,
            0,
            one,
            *target.add(31),
            strict_positive,
        ) {
            return;
        }
        (strict_positive, sha256::bytes(second))
    };
    #[cfg(not(target_os = "cuda"))]
    let (strict_positive, digest) = {
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
        (strict_positive, digest)
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
        pub unsafe extern "gpu-kernel" fn $prepare(
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
        pub unsafe extern "gpu-kernel" fn $group(
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
        pub unsafe extern "gpu-kernel" fn $filter(
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
        pub unsafe extern "gpu-kernel" fn $probe(
            tx: *const u8,
            prefix: *const u32,
            baton: u64,
            reward: u64,
            j: u16,
            output: *mut u8,
        ) {
            if gpu_index!(1) == 0 {
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
