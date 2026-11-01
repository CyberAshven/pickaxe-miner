//! ELECTRIC's 32-byte SHA-256d proof, shared by CUDA and portable GPUs.
use crate::sha256;

#[inline(always)]
pub fn digest(prefix: [u32; 6], nonce_lo: u32, nonce_hi: u32) -> [u32; 8] {
    let mut block = [0u32; 16];
    block[0] = prefix[0];
    block[1] = prefix[1];
    block[2] = prefix[2];
    block[3] = prefix[3];
    block[4] = prefix[4];
    block[5] = prefix[5];
    block[6] = nonce_lo.swap_bytes();
    block[7] = nonce_hi.swap_bytes();
    block[8] = 0x8000_0000;
    block[15] = 256;
    let mut state = sha256::INITIAL;
    sha256::compress(&mut state, block);
    block[0] = state[0];
    block[1] = state[1];
    block[2] = state[2];
    block[3] = state[3];
    block[4] = state[4];
    block[5] = state[5];
    block[6] = state[6];
    block[7] = state[7];
    state = sha256::INITIAL;
    sha256::compress(&mut state, block);
    state
}

#[inline(always)]
pub fn candidate(control: &[u32; 10], index: u32) -> [u32; 8] {
    let lo = control[6].wrapping_add(index);
    let hi = control[7].wrapping_add(u32::from(lo < control[6]));
    digest(
        [
            control[0], control[1], control[2], control[3], control[4], control[5],
        ],
        lo,
        hi,
    )
}

#[inline(always)]
pub fn meets(digest: [u32; 8], difficulty: u32) -> bool {
    if difficulty > 32 {
        return false;
    }
    for i in 0..8 {
        if difficulty <= i * 4 {
            return true;
        }
        let bytes = (difficulty - i * 4).min(4);
        if bytes == 4 {
            if digest[i as usize] != 0 {
                return false;
            }
        } else if bytes != 0 && digest[i as usize] >> (32 - bytes * 8) != 0 {
            return false;
        }
    }
    true
}
