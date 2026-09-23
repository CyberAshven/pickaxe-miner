#if defined(__HIPCC__) || defined(__HIP_PLATFORM_AMD__)
#include <stdint.h>
#elif defined(PICKAXE_STDINT_INCLUDED)
// The including file already brought in <cstdint>.
#else
typedef unsigned char uint8_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;
#endif

// PHOTON Stage C correctness kernel.
//
// The immutable job template is exactly 615 bytes. Per candidate we override:
//   nonce     bytes 390..393 (u32 little-endian)
//   signature bytes 426..489 (64-byte BCH Schnorr R||s)
// The target at bytes 394..425 remains part of the transaction being hashed.
//
// The production entry point performs completed-transaction HASH256 and a
// strict little-endian integer comparison (hash < target). Only bounded winner
// records cross the device/host boundary. Probe entry points exist for vector
// verification and are not used by the production search path.

namespace {

constexpr uint32_t TX_BYTES = 615u;
constexpr uint32_t NONCE_OFFSET = 390u;
constexpr uint32_t TARGET_OFFSET = 394u;
constexpr uint32_t SIGNATURE_OFFSET = 426u;
constexpr uint32_t SIGNATURE_BYTES = 64u;

__device__ __forceinline__ uint32_t rotr32(uint32_t x, uint32_t n) {
    return (x >> n) | (x << (32u - n));
}

__device__ __forceinline__ void copy_bytes(uint8_t* dst, const uint8_t* src, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) dst[i] = src[i];
}

__device__ __forceinline__ void zero_bytes(uint8_t* dst, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) dst[i] = 0u;
}

__device__ __forceinline__ void sha256_init(uint32_t state[8]) {
    state[0] = 0x6a09e667u; state[1] = 0xbb67ae85u;
    state[2] = 0x3c6ef372u; state[3] = 0xa54ff53au;
    state[4] = 0x510e527fu; state[5] = 0x9b05688cu;
    state[6] = 0x1f83d9abu; state[7] = 0x5be0cd19u;
}

__device__ void sha256_transform(uint32_t state[8], const uint8_t block[64]) {
    const uint32_t k[64] = {
        0x428a2f98u,0x71374491u,0xb5c0fbcfu,0xe9b5dba5u,0x3956c25bu,0x59f111f1u,0x923f82a4u,0xab1c5ed5u,
        0xd807aa98u,0x12835b01u,0x243185beu,0x550c7dc3u,0x72be5d74u,0x80deb1feu,0x9bdc06a7u,0xc19bf174u,
        0xe49b69c1u,0xefbe4786u,0x0fc19dc6u,0x240ca1ccu,0x2de92c6fu,0x4a7484aau,0x5cb0a9dcu,0x76f988dau,
        0x983e5152u,0xa831c66du,0xb00327c8u,0xbf597fc7u,0xc6e00bf3u,0xd5a79147u,0x06ca6351u,0x14292967u,
        0x27b70a85u,0x2e1b2138u,0x4d2c6dfcu,0x53380d13u,0x650a7354u,0x766a0abbu,0x81c2c92eu,0x92722c85u,
        0xa2bfe8a1u,0xa81a664bu,0xc24b8b70u,0xc76c51a3u,0xd192e819u,0xd6990624u,0xf40e3585u,0x106aa070u,
        0x19a4c116u,0x1e376c08u,0x2748774cu,0x34b0bcb5u,0x391c0cb3u,0x4ed8aa4au,0x5b9cca4fu,0x682e6ff3u,
        0x748f82eeu,0x78a5636fu,0x84c87814u,0x8cc70208u,0x90befffau,0xa4506cebu,0xbef9a3f7u,0xc67178f2u
    };

    uint32_t w[64];
    for (int i = 0; i < 16; ++i) {
        const int j = i * 4;
        w[i] = ((uint32_t)block[j] << 24) |
               ((uint32_t)block[j + 1] << 16) |
               ((uint32_t)block[j + 2] << 8) |
               (uint32_t)block[j + 3];
    }
    for (int i = 16; i < 64; ++i) {
        const uint32_t s0 = rotr32(w[i - 15], 7) ^ rotr32(w[i - 15], 18) ^ (w[i - 15] >> 3);
        const uint32_t s1 = rotr32(w[i - 2], 17) ^ rotr32(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }

    uint32_t a = state[0], b = state[1], c = state[2], d = state[3];
    uint32_t e = state[4], f = state[5], g = state[6], h = state[7];
    for (int i = 0; i < 64; ++i) {
        const uint32_t s1 = rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25);
        const uint32_t ch = (e & f) ^ ((~e) & g);
        const uint32_t t1 = h + s1 + ch + k[i] + w[i];
        const uint32_t s0 = rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22);
        const uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        const uint32_t t2 = s0 + maj;
        h = g; g = f; f = e; e = d + t1;
        d = c; c = b; b = a; a = t1 + t2;
    }
    state[0] += a; state[1] += b; state[2] += c; state[3] += d;
    state[4] += e; state[5] += f; state[6] += g; state[7] += h;
}

__device__ __forceinline__ void sha256_store(const uint32_t state[8], uint8_t out[32]) {
    for (int i = 0; i < 8; ++i) {
        out[i * 4 + 0] = (uint8_t)(state[i] >> 24);
        out[i * 4 + 1] = (uint8_t)(state[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(state[i] >> 8);
        out[i * 4 + 3] = (uint8_t)state[i];
    }
}

__device__ void sha256_small(const uint8_t* data, uint32_t len, uint8_t out[32]) {
    uint32_t state[8];
    sha256_init(state);
    uint32_t offset = 0u;
    while (len - offset >= 64u) {
        uint8_t block[64];
        copy_bytes(block, data + offset, 64u);
        sha256_transform(state, block);
        offset += 64u;
    }

    const uint32_t rem = len - offset;
    uint8_t tail[128];
    zero_bytes(tail, 128u);
    copy_bytes(tail, data + offset, rem);
    tail[rem] = 0x80u;
    const uint32_t blocks = rem <= 55u ? 1u : 2u;
    const uint64_t bit_len = (uint64_t)len * 8ull;
    const uint32_t end = blocks * 64u;
    for (uint32_t i = 0; i < 8u; ++i) {
        tail[end - 1u - i] = (uint8_t)(bit_len >> (i * 8u));
    }
    sha256_transform(state, tail);
    if (blocks == 2u) sha256_transform(state, tail + 64u);
    sha256_store(state, out);
}

__device__ __forceinline__ uint8_t completed_tx_byte(
    const uint8_t* tx_template,
    const uint8_t* signatures,
    uint32_t candidate,
    uint32_t nonce,
    uint32_t byte_index
) {
    if (byte_index >= NONCE_OFFSET && byte_index < NONCE_OFFSET + 4u) {
        return (uint8_t)(nonce >> ((byte_index - NONCE_OFFSET) * 8u));
    }
    if (byte_index >= SIGNATURE_OFFSET && byte_index < SIGNATURE_OFFSET + SIGNATURE_BYTES) {
        return signatures[(uint64_t)candidate * SIGNATURE_BYTES + (byte_index - SIGNATURE_OFFSET)];
    }
    return tx_template[byte_index];
}

__device__ void photon_transaction_hashes(
    const uint8_t* tx_template,
    const uint8_t* signatures,
    uint32_t candidate,
    uint32_t nonce,
    uint8_t first_hash[32],
    uint8_t final_hash[32]
) {
    uint32_t state[8];
    sha256_init(state);
    constexpr uint32_t TOTAL_BLOCKS = 10u;
    constexpr uint64_t BIT_LENGTH = (uint64_t)TX_BYTES * 8ull;

    for (uint32_t block_index = 0u; block_index < TOTAL_BLOCKS; ++block_index) {
        uint8_t block[64];
        const uint32_t block_start = block_index * 64u;
        for (uint32_t j = 0u; j < 64u; ++j) {
            const uint32_t byte_index = block_start + j;
            uint8_t value = 0u;
            if (byte_index < TX_BYTES) {
                value = completed_tx_byte(tx_template, signatures, candidate, nonce, byte_index);
            } else if (byte_index == TX_BYTES) {
                value = 0x80u;
            } else if (byte_index >= TOTAL_BLOCKS * 64u - 8u) {
                const uint32_t length_offset = byte_index - (TOTAL_BLOCKS * 64u - 8u);
                value = (uint8_t)(BIT_LENGTH >> ((7u - length_offset) * 8u));
            }
            block[j] = value;
        }
        sha256_transform(state, block);
    }
    sha256_store(state, first_hash);
    sha256_small(first_hash, 32u, final_hash);
}

__device__ __forceinline__ bool hash_below_target_le(
    const uint8_t hash[32],
    const uint8_t target[32]
) {
    for (int i = 31; i >= 0; --i) {
        if (hash[i] < target[i]) return true;
        if (hash[i] > target[i]) return false;
    }
    return false;
}

} // namespace

extern "C" __global__ void pickaxe_stage_c_hash_filter(
    const uint8_t* __restrict__ tx_template,
    const uint8_t* __restrict__ signatures,
    uint32_t nonce_base,
    const uint8_t* __restrict__ target,
    uint32_t candidate_count,
    uint32_t winner_cap,
    uint32_t* __restrict__ winner_count,
    uint32_t* __restrict__ winner_nonces,
    uint8_t* __restrict__ winner_hashes
) {
    const uint32_t candidate = blockIdx.x * blockDim.x + threadIdx.x;
    if (candidate >= candidate_count) return;

    const uint32_t nonce = nonce_base + candidate;
    uint8_t first_hash[32];
    uint8_t final_hash[32];
    photon_transaction_hashes(tx_template, signatures, candidate, nonce, first_hash, final_hash);

    if (!hash_below_target_le(final_hash, target)) return;

    const uint32_t slot = atomicAdd(winner_count, 1u);
    if (slot >= winner_cap) return;
    winner_nonces[slot] = nonce;
    copy_bytes(winner_hashes + (uint64_t)slot * 32u, final_hash, 32u);
}

extern "C" __global__ void pickaxe_stage_c_probe(
    const uint8_t* __restrict__ tx_template,
    const uint8_t* __restrict__ signature,
    uint32_t nonce,
    uint8_t* __restrict__ first_hash_out,
    uint8_t* __restrict__ final_hash_out
) {
    if (blockIdx.x != 0 || threadIdx.x != 0) return;
    uint8_t first_hash[32];
    uint8_t final_hash[32];
    photon_transaction_hashes(tx_template, signature, 0u, nonce, first_hash, final_hash);
    copy_bytes(first_hash_out, first_hash, 32u);
    copy_bytes(final_hash_out, final_hash, 32u);
}

extern "C" __global__ void pickaxe_stage_c_compare_probe(
    const uint8_t* __restrict__ hash,
    const uint8_t* __restrict__ target,
    uint32_t* __restrict__ out
) {
    if (blockIdx.x != 0 || threadIdx.x != 0) return;
    out[0] = hash_below_target_le(hash, target) ? 1u : 0u;
}
