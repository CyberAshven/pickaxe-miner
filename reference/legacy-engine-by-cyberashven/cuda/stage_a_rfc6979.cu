#include <cstdint>

// Reference-correct PHOTON Stage A:
//   message = SHA256(nonceLE || target)
//   k       = BCH Schnorr RFC6979(message, private_key, "Schnorr+SHA256  ")
//
// This is intentionally a correctness kernel first. The production reference
// later specializes/precomputes these HMAC states for throughput.

__device__ __forceinline__ uint32_t rotr32(uint32_t x, uint32_t n) {
    return (x >> n) | (x << (32u - n));
}

__device__ __forceinline__ void copy_bytes(uint8_t* dst, const uint8_t* src, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) dst[i] = src[i];
}

__device__ __forceinline__ void zero_bytes(uint8_t* dst, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) dst[i] = 0u;
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

__device__ __forceinline__ void sha256_init(uint32_t state[8]) {
    state[0] = 0x6a09e667u; state[1] = 0xbb67ae85u;
    state[2] = 0x3c6ef372u; state[3] = 0xa54ff53au;
    state[4] = 0x510e527fu; state[5] = 0x9b05688cu;
    state[6] = 0x1f83d9abu; state[7] = 0x5be0cd19u;
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

    uint32_t offset = 0;
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
    const uint32_t blocks = (rem <= 55u) ? 1u : 2u;
    const uint64_t bit_len = (uint64_t)len * 8ull;
    const uint32_t end = blocks * 64u;
    for (uint32_t i = 0; i < 8u; ++i) {
        tail[end - 1u - i] = (uint8_t)(bit_len >> (i * 8u));
    }
    sha256_transform(state, tail);
    if (blocks == 2u) sha256_transform(state, tail + 64u);
    sha256_store(state, out);
}

// HMAC-SHA256 with a 32-byte key. The key's inner and outer pad blocks are
// absorbed once into saved SHA-256 states, so every HMAC under the same key
// skips two compressions. RFC6979 uses each key for two HMACs in a row.
struct HmacKey {
    uint32_t inner[8];
    uint32_t outer[8];
};

__device__ void hmac_key_states(const uint8_t key32[32], HmacKey* key) {
    uint8_t block[64];
    for (uint32_t i = 0; i < 64u; ++i) block[i] = (i < 32u ? key32[i] : 0u) ^ 0x36u;
    sha256_init(key->inner);
    sha256_transform(key->inner, block);
    for (uint32_t i = 0; i < 64u; ++i) block[i] = (i < 32u ? key32[i] : 0u) ^ 0x5cu;
    sha256_init(key->outer);
    sha256_transform(key->outer, block);
}

// Pad states of the all-zero key RFC6979 starts with: SHA-256 after one
// block of 0x36 bytes and after one block of 0x5c bytes.
__device__ __forceinline__ void hmac_zero_key_states(HmacKey* key) {
    const uint32_t inner[8] = {
        0xf454deadu, 0x9725214fu, 0x90daf2a0u, 0xdf1228eau,
        0x64e5750fu, 0xa3924181u, 0x824a932bu, 0xf8e04e32u
    };
    const uint32_t outer[8] = {
        0xd385480fu, 0x7abb6477u, 0x37c9c538u, 0x5dd82467u,
        0x8e043a72u, 0x753434b0u, 0xdeb82818u, 0x361d45a6u
    };
    for (int i = 0; i < 8; ++i) {
        key->inner[i] = inner[i];
        key->outer[i] = outer[i];
    }
}

// Finishes SHA-256 of (64 bytes already absorbed into state) || data.
__device__ void sha256_after_block(const uint32_t absorbed[8], const uint8_t* data, uint32_t len, uint8_t out[32]) {
    uint32_t state[8];
    for (int i = 0; i < 8; ++i) state[i] = absorbed[i];
    uint32_t offset = 0;
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
    const uint32_t blocks = (rem <= 55u) ? 1u : 2u;
    const uint64_t bit_len = (uint64_t)(64u + len) * 8ull;
    const uint32_t end = blocks * 64u;
    for (uint32_t i = 0; i < 8u; ++i) {
        tail[end - 1u - i] = (uint8_t)(bit_len >> (i * 8u));
    }
    sha256_transform(state, tail);
    if (blocks == 2u) sha256_transform(state, tail + 64u);
    sha256_store(state, out);
}

__device__ void hmac_sha256_keyed(const HmacKey* key, const uint8_t* data, uint32_t len, uint8_t out[32]) {
    uint8_t inner_hash[32];
    sha256_after_block(key->inner, data, len, inner_hash);
    sha256_after_block(key->outer, inner_hash, 32u, out);
}

__device__ __forceinline__ int cmp_be32(const uint8_t a[32], const uint8_t b[32]) {
    for (int i = 0; i < 32; ++i) {
        if (a[i] < b[i]) return -1;
        if (a[i] > b[i]) return 1;
    }
    return 0;
}

__device__ void sub_be32(uint8_t a[32], const uint8_t b[32]) {
    uint32_t borrow = 0u;
    for (int i = 31; i >= 0; --i) {
        const uint32_t av = a[i];
        const uint32_t bv = (uint32_t)b[i] + borrow;
        a[i] = (uint8_t)(av - bv);
        borrow = av < bv ? 1u : 0u;
    }
}

__device__ __forceinline__ bool nonzero32(const uint8_t a[32]) {
    uint8_t accum = 0u;
    for (int i = 0; i < 32; ++i) accum |= a[i];
    return accum != 0u;
}

__device__ void reduce_mod_n(uint8_t value[32]) {
    const uint8_t n[32] = {
        0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,
        0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xfe,
        0xba,0xae,0xdc,0xe6,0xaf,0x48,0xa0,0x3b,
        0xbf,0xd2,0x5e,0x8c,0xd0,0x36,0x41,0x41
    };
    if (cmp_be32(value, n) >= 0) sub_be32(value, n);
}

__device__ bool scalar_is_valid(const uint8_t value[32]) {
    const uint8_t n[32] = {
        0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,
        0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xfe,
        0xba,0xae,0xdc,0xe6,0xaf,0x48,0xa0,0x3b,
        0xbf,0xd2,0x5e,0x8c,0xd0,0x36,0x41,0x41
    };
    return nonzero32(value) && cmp_be32(value, n) < 0;
}

__device__ void bch_rfc6979(
    const uint8_t private_key[32],
    const uint8_t message_hash[32],
    uint8_t out_nonce[32]
) {
    const uint8_t algo[16] = {
        0x53,0x63,0x68,0x6e,0x6f,0x72,0x72,0x2b,
        0x53,0x48,0x41,0x32,0x35,0x36,0x20,0x20
    };
    uint8_t reduced[32];
    uint8_t v[32];
    uint8_t k[32];
    uint8_t tmp[32];
    uint8_t data113[113];
    uint8_t data33[33];

    copy_bytes(reduced, message_hash, 32u);
    reduce_mod_n(reduced);
    for (int i = 0; i < 32; ++i) v[i] = 0x01u;

    HmacKey key;
    hmac_zero_key_states(&key);
    copy_bytes(data113, v, 32u);
    data113[32] = 0x00u;
    copy_bytes(data113 + 33u, private_key, 32u);
    copy_bytes(data113 + 65u, reduced, 32u);
    copy_bytes(data113 + 97u, algo, 16u);
    hmac_sha256_keyed(&key, data113, 113u, k);
    hmac_key_states(k, &key);
    hmac_sha256_keyed(&key, v, 32u, tmp);
    copy_bytes(v, tmp, 32u);

    copy_bytes(data113, v, 32u);
    data113[32] = 0x01u;
    hmac_sha256_keyed(&key, data113, 113u, k);
    hmac_key_states(k, &key);
    hmac_sha256_keyed(&key, v, 32u, tmp);
    copy_bytes(v, tmp, 32u);

    for (;;) {
        hmac_sha256_keyed(&key, v, 32u, tmp);
        copy_bytes(v, tmp, 32u);
        if (scalar_is_valid(v)) {
            copy_bytes(out_nonce, v, 32u);
            return;
        }
        copy_bytes(data33, v, 32u);
        data33[32] = 0x00u;
        hmac_sha256_keyed(&key, data33, 33u, k);
        hmac_key_states(k, &key);
        hmac_sha256_keyed(&key, v, 32u, tmp);
        copy_bytes(v, tmp, 32u);
    }
}

extern "C" __global__ void pickaxe_stage_a_rfc6979(
    uint32_t nonce_base,
    const uint8_t* __restrict__ target32,
    const uint8_t* __restrict__ private_key32,
    uint8_t* __restrict__ out_message_hashes,
    uint8_t* __restrict__ out_rfc6979_nonces,
    uint32_t n
) {
    const uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;

    const uint32_t nonce = nonce_base + i;
    uint8_t message[36];
    message[0] = (uint8_t)nonce;
    message[1] = (uint8_t)(nonce >> 8);
    message[2] = (uint8_t)(nonce >> 16);
    message[3] = (uint8_t)(nonce >> 24);
    copy_bytes(message + 4u, target32, 32u);

    uint8_t message_hash[32];
    uint8_t deterministic_nonce[32];
    sha256_small(message, 36u, message_hash);
    bch_rfc6979(private_key32, message_hash, deterministic_nonce);

    copy_bytes(out_message_hashes + (size_t)i * 32u, message_hash, 32u);
    copy_bytes(out_rfc6979_nonces + (size_t)i * 32u, deterministic_nonce, 32u);
}
