//! The host owns buffer sizes and launches one-dimensional grids. Kernels retain
//! the existing Pickaxe ABI so the established end-to-end oracle can test them.
use crate::{field::Field, point::Point, scalar::Scalar};
use core::arch::nvptx;
use core::sync::atomic::{AtomicU32, Ordering};
use sha2::{Digest, Sha256};

fn index() -> u32 {
    unsafe { nvptx::_block_idx_x() * nvptx::_block_dim_x() + nvptx::_thread_idx_x() }
}
fn stride() -> u32 {
    unsafe { nvptx::_grid_dim_x() * nvptx::_block_dim_x() }
}

#[no_mangle]
pub unsafe extern "ptx-kernel" fn pickaxe_stage_a_rfc6979(
    base: u32,
    target: *const u8,
    secret: *const u8,
    messages: *mut u8,
    scalars: *mut u8,
    count: u32,
) {
    let candidate = index() as usize;
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
    let candidate = index() as usize;
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
            Field(read(table.add(offset))),
            Field(read(table.add(offset + 8))),
        );
    }
    write(points.add(candidate * 24), &point.x.0);
    write(points.add(candidate * 24 + 8), &point.y.0);
    write(points.add(candidate * 24 + 16), &point.z.0);
}

macro_rules! fixed_base_kernel {
    ($name:ident, $first:literal) => {
        #[no_mangle]
        pub unsafe extern "ptx-kernel" fn $name(
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
        x: Field(read(ptr.add(candidate * 24))),
        y: Field(read(ptr.add(candidate * 24 + 8))),
        z: Field(read(ptr.add(candidate * 24 + 16))),
    }
}

/// Input and output contain count * 8 words and do not overlap.
#[no_mangle]
pub unsafe extern "ptx-kernel" fn pickaxe_rust_inverse(
    inputs: *const u32,
    outputs: *mut u32,
    count: u32,
) {
    let index = index() as usize;
    if index >= count as usize {
        return;
    }
    let value = Field(core::array::from_fn(|i| unsafe {
        *inputs.add(index * 8 + i)
    }));
    let inverse = value.inverse();
    for i in 0..8 {
        unsafe {
            *outputs.add(index * 8 + i) = inverse.0[i];
        }
    }
}

/// Only public scalars for unfunded mining identities. Never use a funded key.
#[no_mangle]
pub unsafe extern "ptx-kernel" fn pickaxe_photon_incremental_k(
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
            Field(read(table.add(offset))),
            Field(read(table.add(offset + 8))),
        );
    }
    let dx = Field(read(step));
    let dy = Field(read(step.add(8)));
    let message: [u8; 32] = read(message);
    let stride = stride() as u64;
    let mut candidate = lane as u64;
    while candidate < count as u64 {
        let i = candidate as usize;
        write(points.add(i * 24), &point.x.0);
        write(points.add(i * 24 + 8), &point.y.0);
        write(points.add(i * 24 + 16), &point.z.0);
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
pub unsafe extern "ptx-kernel" fn pickaxe_photon_c1_schnorr_dual_batched(
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
    let lane = index() as usize;
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
        let z = Field(read(points.add(candidate * 24 + 16)));
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
    let half = Scalar(read(fixed_d.add((31 * 256 + 128) * 8)));
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
        let mut challenge = Sha256::new();
        challenge.update(r);
        challenge.update(public);
        challenge.update(message);
        let e = Scalar::from_be_bytes(challenge.finalize().into());
        let ed = e.montgomery_mul(d_montgomery);
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

unsafe fn tx_block<const SHIFT: usize, const INCREMENTAL: bool>(
    template: *const u8,
    r: &[u8; 32],
    s: &[u8; 32],
    block: usize,
    nonce: u32,
) -> [u8; 64] {
    core::array::from_fn(|j| {
        let pos = block * 64 + j;
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
    })
}

unsafe fn finish_hash<const SHIFT: usize, const INCREMENTAL: bool>(
    mut state: [u32; 8],
    template: *const u8,
    r: &[u8; 32],
    s: &[u8; 32],
    nonce: u32,
) -> [u8; 32] {
    for block in 7..10 {
        sha2::compress256(
            &mut state,
            &[tx_block::<SHIFT, INCREMENTAL>(template, r, s, block, nonce).into()],
        );
    }
    let mut hash = [0; 32];
    for (bytes, word) in hash.chunks_exact_mut(4).zip(state) {
        bytes.copy_from_slice(&word.to_be_bytes());
    }
    Sha256::digest(hash).into()
}

fn meets(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        let byte = if i == 31 { hash[i] & 0x7f } else { hash[i] };
        if byte != target[i] {
            return byte < target[i];
        }
    }
    false
}

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
    let candidate = index() as usize;
    if candidate >= count as usize {
        return;
    }
    let r = read(signatures.add(candidate * 64));
    let plus = read(signatures.add(candidate * 64 + 32));
    let minus = read(negated_s.add(candidate * 32));
    let target = read(target);
    let mut state = read(midstate);
    sha2::compress256(
        &mut state,
        &[tx_block::<SHIFT, INCREMENTAL>(
            template,
            &r,
            &plus,
            6,
            base.wrapping_add(candidate as u32),
        )
        .into()],
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
    let pass_plus = meets(&hash_plus, &target);
    let pass_minus = meets(&hash_minus, &target);
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
        pub unsafe extern "ptx-kernel" fn $name(
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
