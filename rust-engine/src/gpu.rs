//! The host owns buffer sizes and launches one-dimensional grids. Kernels retain
//! the existing Pickaxe ABI so the established end-to-end oracle can test them.
use crate::sha256;
#[cfg(feature = "upstream-rust")]
use crate::upstream::{Field, Point, Scalar};
#[cfg(not(feature = "upstream-rust"))]
use crate::{field::Field, point::Point, scalar::Scalar};
#[cfg(target_os = "cuda")]
use core::arch::nvptx;
use core::sync::atomic::{AtomicU32, Ordering};
use sha2::{Digest, Sha256};

#[cfg(target_os = "cuda")]
macro_rules! gpu_index {
    ($group:literal) => {
        index()
    };
}
#[cfg(target_os = "amdhsa")]
macro_rules! gpu_index {
    ($group:literal) => {
        index_in::<$group>()
    };
}

#[path = "t2.rs"]
mod t2;

#[inline(always)]
fn field(words: [u32; 8]) -> Field {
    #[cfg(feature = "upstream-rust")]
    return Field::from_limbs_reduced(crate::upstream::limbs(words));
    #[cfg(not(feature = "upstream-rust"))]
    Field(words)
}

#[inline(always)]
fn words(field: Field) -> [u32; 8] {
    #[cfg(feature = "upstream-rust")]
    return core::array::from_fn(|i| (field.to_limbs()[i / 2] >> ((i % 2) * 32)) as u32);
    #[cfg(not(feature = "upstream-rust"))]
    field.0
}

#[cfg(target_os = "cuda")]
fn index() -> u32 {
    unsafe { nvptx::_block_idx_x() * nvptx::_block_dim_x() + nvptx::_thread_idx_x() }
}
#[cfg(target_os = "cuda")]
fn stride() -> u32 {
    unsafe { nvptx::_grid_dim_x() * nvptx::_block_dim_x() }
}

// #### PR #22: one Rust kernel source for NVIDIA and AMD
// What: AMD builds compute the thread index from each kernel's launch group
// size, which the HIP host passes unchanged; NVIDIA reads it from the GPU.
// Why: this core::arch::amdgpu exposes workgroup and work-item ids but no
// workgroup size. AMD launches keep per_thread at one, so stride is unused.
// Check: HIP launch geometry must match every gpu_index! group size.
#[cfg(target_os = "amdhsa")]
fn index_in<const GROUP: u32>() -> u32 {
    core::arch::amdgpu::workgroup_id_x() * GROUP + core::arch::amdgpu::workitem_id_x()
}
#[cfg(target_os = "amdhsa")]
fn stride() -> u32 {
    0
}

#[no_mangle]
pub unsafe extern "gpu-kernel" fn pickaxe_stage_a_rfc6979(
    base: u32,
    target: *const u8,
    secret: *const u8,
    messages: *mut u8,
    scalars: *mut u8,
    count: u32,
) {
    let candidate = gpu_index!(128) as usize;
    if candidate >= count as usize {
        return;
    }
    let target: [u8; 32] = read(target);
    let mut hash = Sha256::new();
    hash.update(base.wrapping_add(candidate as u32).to_le_bytes());
    hash.update(target);
    let message: [u8; 32] = hash.finalize().into();
    let scalar = crate::nonce::bch_rfc6979(&read(secret), &message);
    write(messages.add(candidate * 32), &message);
    write(scalars.add(candidate * 32), &scalar);
}

unsafe fn fixed_base_part<const FIRST: usize>(
    scalars: *const u8,
    table: *const u32,
    points: *mut u32,
    count: u32,
) {
    let candidate = gpu_index!(64) as usize;
    if candidate >= count as usize {
        return;
    }
    let scalar: [u8; 32] = read(scalars.add(candidate * 32));
    let mut point = if FIRST == 0 {
        Point::INFINITY
    } else {
        point_at(points, candidate)
    };
    for window in FIRST..FIRST + 4 {
        let digit = u16::from_be_bytes([scalar[30 - window * 2], scalar[31 - window * 2]]) as usize;
        if digit == 0 {
            continue;
        }
        let offset = (window * 65536 + digit) * 16;
        point = point.add_affine(
            field(read(table.add(offset))),
            field(read(table.add(offset + 8))),
        );
    }
    write(points.add(candidate * 24), &words(point.x));
    write(points.add(candidate * 24 + 8), &words(point.y));
    write(points.add(candidate * 24 + 16), &words(point.z));
}

macro_rules! fixed_base_kernel {
    ($name:ident, $first:literal) => {
        #[no_mangle]
        pub unsafe extern "gpu-kernel" fn $name(
            scalars: *const u8,
            table: *const u32,
            points: *mut u32,
            count: u32,
        ) {
            fixed_base_part::<$first>(scalars, table, points, count);
        }
    };
}
fixed_base_kernel!(pickaxe_photon_b16_part0, 0);
fixed_base_kernel!(pickaxe_photon_b16_part1, 4);
fixed_base_kernel!(pickaxe_photon_b16_part2, 8);
fixed_base_kernel!(pickaxe_photon_b16_part3, 12);

unsafe fn read<const N: usize, T: Copy>(ptr: *const T) -> [T; N] {
    core::array::from_fn(|i| unsafe { *ptr.add(i) })
}

unsafe fn write<T: Copy>(ptr: *mut T, data: &[T]) {
    for (i, value) in data.iter().enumerate() {
        unsafe {
            *ptr.add(i) = *value;
        }
    }
}

unsafe fn point_at(ptr: *const u32, candidate: usize) -> Point {
    Point {
        x: field(read(ptr.add(candidate * 24))),
        y: field(read(ptr.add(candidate * 24 + 8))),
        z: field(read(ptr.add(candidate * 24 + 16))),
    }
}

/// Input and output contain count * 8 words and do not overlap.
#[cfg(target_os = "cuda")]
#[no_mangle]
pub unsafe extern "gpu-kernel" fn pickaxe_rust_inverse(
    inputs: *const u32,
    outputs: *mut u32,
    count: u32,
) {
    let index = index() as usize;
    if index >= count as usize {
        return;
    }
    let value = field(core::array::from_fn(|i| unsafe {
        *inputs.add(index * 8 + i)
    }));
    let inverse = value.inverse();
    for i in 0..8 {
        unsafe {
            *outputs.add(index * 8 + i) = words(inverse)[i];
        }
    }
}

/// Only public scalars for unfunded mining identities. Never use a funded key.
#[cfg(target_os = "cuda")]
#[no_mangle]
pub unsafe extern "gpu-kernel" fn pickaxe_photon_incremental_k(
    base: u32,
    count: u32,
    table: *const u32,
    step: *const u32,
    message: *const u8,
    messages: *mut u8,
    scalars: *mut u8,
    points: *mut u32,
) {
    let lane = index();
    if lane >= count {
        return;
    }
    let mut k = base as u64 + lane as u64 + 1;
    let mut point = Point::INFINITY;
    for window in 0..4usize {
        let digit = ((k >> (window * 16)) & 0xffff) as usize;
        if digit == 0 {
            continue;
        }
        let offset = (window * 65536 + digit) * 16;
        point = point.add_affine(
            field(read(table.add(offset))),
            field(read(table.add(offset + 8))),
        );
    }
    let dx = field(read(step));
    let dy = field(read(step.add(8)));
    let message: [u8; 32] = read(message);
    let stride = stride() as u64;
    let mut candidate = lane as u64;
    while candidate < count as u64 {
        let i = candidate as usize;
        write(points.add(i * 24), &words(point.x));
        write(points.add(i * 24 + 8), &words(point.y));
        write(points.add(i * 24 + 16), &words(point.z));
        write(messages.add(i * 32), &message);
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&k.to_be_bytes());
        write(scalars.add(i * 32), &bytes);
        candidate += stride;
        if candidate >= count as u64 {
            break;
        }
        point = point.add_affine(dx, dy);
        k += stride;
    }
}

#[no_mangle]
pub unsafe extern "gpu-kernel" fn pickaxe_photon_c1_schnorr_dual_batched(
    messages: *const u8,
    scalars: *const u8,
    points: *const u32,
    public_key: *const u8,
    fixed_d: *const u32,
    signatures: *mut u8,
    negated_s: *mut u8,
    candidate_count: u32,
    per_thread: u32,
) {
    let lane = gpu_index!(64) as usize;
    let stride = stride() as usize;
    let mut prefix = [Field::ONE; 16];
    let mut product = Field::ONE;
    let mut count = 0;
    for (j, entry) in prefix
        .iter_mut()
        .enumerate()
        .take(per_thread.min(16) as usize)
    {
        let candidate = lane + j * stride;
        if candidate >= candidate_count as usize {
            break;
        }
        *entry = product;
        let z = field(read(points.add(candidate * 24 + 16)));
        if z != Field::ZERO {
            product = product.mul_mod(z);
        }
        count += 1;
    }
    if count == 0 {
        return;
    }
    let mut inverse = product.inverse();
    // Existing fixed-d table contains d*2^255 at position 31, digit 128.
    let half = crate::scalar::Scalar(read(fixed_d.add((31 * 256 + 128) * 8)));
    let d_montgomery = half.add_mod(half);
    let public: [u8; 33] = read(public_key);
    for j in (0..count).rev() {
        let candidate = lane + j * stride;
        let point = point_at(points, candidate);
        if point.z == Field::ZERO {
            continue;
        }
        let z_inverse = inverse.mul_mod(prefix[j]);
        inverse = inverse.mul_mod(point.z);
        let r = point.x.mul_mod(z_inverse.square()).to_be_bytes();
        let message: [u8; 32] = read(messages.add(candidate * 32));
        let e = Scalar::from_be_bytes(sha256::challenge(r, public, message));
        #[cfg(not(feature = "upstream-rust"))]
        let ed = e.montgomery_mul(d_montgomery);
        #[cfg(feature = "upstream-rust")]
        let ed = {
            // Pickaxe-specific fixed-key Montgomery multiplication remains
            // mining glue, as in the native upstream adapter. Generic scalar
            // arithmetic is ported separately in ufsecp-core and tested there.
            let value =
                crate::scalar::Scalar::from_be_bytes(e.to_be_bytes()).montgomery_mul(d_montgomery);
            Scalar::from_limbs_reduced(crate::upstream::limbs(value.0))
        };
        let k = Scalar::from_be_bytes(read(scalars.add(candidate * 32)));
        write(signatures.add(candidate * 64), &r);
        write(
            signatures.add(candidate * 64 + 32),
            &k.add_mod(ed).to_be_bytes(),
        );
        write(
            negated_s.add(candidate * 32),
            &k.negate().add_mod(ed).to_be_bytes(),
        );
    }
}

#[inline(always)]
unsafe fn tx_block<const SHIFT: usize, const INCREMENTAL: bool, const BLOCK: usize>(
    template: *const u8,
    r: &[u8; 32],
    s: &[u8; 32],
    nonce: u32,
) -> [u32; 16] {
    let byte = |pos: usize| {
        let signature = 426 + SHIFT;
        let length = 615 + SHIFT;
        if !INCREMENTAL && (390 + SHIFT..394 + SHIFT).contains(&pos) {
            nonce.to_le_bytes()[pos - 390 - SHIFT]
        } else if pos >= signature && pos < signature + 32 {
            r[pos - signature]
        } else if pos >= signature + 32 && pos < signature + 64 {
            s[pos - signature - 32]
        } else if pos < length {
            unsafe { *template.add(pos) }
        } else if pos == length {
            0x80
        } else if pos >= 632 {
            ((length as u64 * 8) >> ((639 - pos) * 8)) as u8
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
unsafe fn finish_hash<const SHIFT: usize, const INCREMENTAL: bool>(
    mut state: [u32; 8],
    template: *const u8,
    r: &[u8; 32],
    s: &[u8; 32],
    nonce: u32,
) -> [u8; 32] {
    macro_rules! block {
        ($block:literal) => {
            sha256::compress(
                &mut state,
                tx_block::<SHIFT, INCREMENTAL, $block>(template, r, s, nonce),
            );
        };
    }
    block!(7);
    block!(8);
    block!(9);
    sha256::hash_state(state)
}

fn meets(hash: &[u8; 32], target: &[u8; 32], strict_positive: bool) -> bool {
    if strict_positive && hash[31] & 0x80 != 0 {
        return false;
    }
    for i in (0..32).rev() {
        let byte = if i == 31 && !strict_positive {
            hash[i] & 0x7f
        } else {
            hash[i]
        };
        if byte != target[i] {
            return byte < target[i] && (!strict_positive || hash.iter().any(|byte| *byte != 0));
        }
    }
    false
}

#[inline(always)]
unsafe fn filter<const SHIFT: usize, const INCREMENTAL: bool>(
    template: *const u8,
    midstate: *const u32,
    signatures: *const u8,
    negated_s: *const u8,
    points: *const u32,
    base: u32,
    target: *const u8,
    count: u32,
    cap: u32,
    winner_count: *mut u32,
    winners: *mut u32,
    hashes: *mut u8,
) {
    let candidate = gpu_index!(128) as usize;
    if candidate >= count as usize {
        return;
    }
    let r = read(signatures.add(candidate * 64));
    let plus = read(signatures.add(candidate * 64 + 32));
    let minus = read(negated_s.add(candidate * 32));
    let strict_positive = *target.add(32) != 0;
    let target = read(target);
    let mut state = read(midstate);
    sha256::compress(
        &mut state,
        tx_block::<SHIFT, INCREMENTAL, 6>(template, &r, &plus, base.wrapping_add(candidate as u32)),
    );
    let hash_plus = finish_hash::<SHIFT, INCREMENTAL>(
        state,
        template,
        &r,
        &plus,
        base.wrapping_add(candidate as u32),
    );
    let hash_minus = finish_hash::<SHIFT, INCREMENTAL>(
        state,
        template,
        &r,
        &minus,
        base.wrapping_add(candidate as u32),
    );
    let pass_plus = meets(&hash_plus, &target, strict_positive);
    let pass_minus = meets(&hash_minus, &target, strict_positive);
    if !pass_plus && !pass_minus {
        return;
    }
    let point = point_at(points, candidate);
    if point.z == Field::ZERO {
        return;
    }
    let square = point.y.mul_mod(point.z).is_square();
    if !(if square { pass_plus } else { pass_minus }) {
        return;
    }
    let hash = if square { hash_plus } else { hash_minus };
    // CUDA device-scope atomic, followed by host stream synchronization before readback.
    let slot = AtomicU32::from_ptr(winner_count).fetch_add(1, Ordering::Relaxed);
    if slot >= cap {
        return;
    }
    *winners.add(slot as usize) = base.wrapping_add(candidate as u32);
    write(hashes.add(slot as usize * 32), &hash);
}

macro_rules! filter_kernel {
    ($name:ident, $shift:literal, $incremental:literal) => {
        #[no_mangle]
        pub unsafe extern "gpu-kernel" fn $name(
            template: *const u8,
            midstate: *const u32,
            signatures: *const u8,
            negated_s: *const u8,
            points: *const u32,
            base: u32,
            target: *const u8,
            count: u32,
            cap: u32,
            winner_count: *mut u32,
            winners: *mut u32,
            hashes: *mut u8,
        ) {
            filter::<$shift, $incremental>(
                template,
                midstate,
                signatures,
                negated_s,
                points,
                base,
                target,
                count,
                cap,
                winner_count,
                winners,
                hashes,
            );
        }
    };
}

filter_kernel!(pickaxe_stage_c_dual_filter, 0, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc, 0, false);
filter_kernel!(pickaxe_stage_c_dual_filter_shift1, 1, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc_shift1, 1, false);
filter_kernel!(pickaxe_stage_c_dual_filter_shift2, 2, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc_shift2, 2, false);
filter_kernel!(pickaxe_stage_c_dual_filter_shift3, 3, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc_shift3, 3, false);
filter_kernel!(pickaxe_stage_c_dual_filter_shift14, 14, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc_shift14, 14, false);
filter_kernel!(pickaxe_stage_c_dual_filter_shift15, 15, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc_shift15, 15, false);
filter_kernel!(pickaxe_stage_c_dual_filter_shift16, 16, true);
filter_kernel!(pickaxe_stage_c_dual_filter_rfc_shift16, 16, false);
