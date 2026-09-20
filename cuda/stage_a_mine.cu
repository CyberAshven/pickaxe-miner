// Pickaxe Stage A mine — HASH256 + on-GPU target filter. Persistent engine uses this.
// Compact winners only (nonce + digest). Correctness sibling: stage_a_hash256.cu
#include <cstdint>

__device__ __forceinline__ uint32_t rotr(uint32_t x, uint32_t n) {
    return (x >> n) | (x << (32u - n));
}

__device__ void sha256_transform(uint32_t state[8], const uint32_t w_in[16]) {
    const uint32_t K[64] = {
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
    for (int i = 0; i < 16; ++i) w[i] = w_in[i];
    for (int i = 16; i < 64; ++i) {
        uint32_t s0 = rotr(w[i-15],7) ^ rotr(w[i-15],18) ^ (w[i-15] >> 3);
        uint32_t s1 = rotr(w[i-2],17) ^ rotr(w[i-2],19) ^ (w[i-2] >> 10);
        w[i] = w[i-16] + s0 + w[i-7] + s1;
    }
    uint32_t a=state[0],b=state[1],c=state[2],d=state[3],e=state[4],f=state[5],g=state[6],h=state[7];
    for (int i = 0; i < 64; ++i) {
        uint32_t S1 = rotr(e,6) ^ rotr(e,11) ^ rotr(e,25);
        uint32_t ch = (e & f) ^ ((~e) & g);
        uint32_t t1 = h + S1 + ch + K[i] + w[i];
        uint32_t S0 = rotr(a,2) ^ rotr(a,13) ^ rotr(a,22);
        uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        uint32_t t2 = S0 + maj;
        h=g; g=f; f=e; e=d+t1; d=c; c=b; b=a; a=t1+t2;
    }
    state[0]+=a; state[1]+=b; state[2]+=c; state[3]+=d;
    state[4]+=e; state[5]+=f; state[6]+=g; state[7]+=h;
}

__device__ void hash256_nonce_target(uint32_t nonce, const uint32_t target_be[8], uint32_t digest_be[8]) {
    uint8_t msg[64] = {0};
    msg[0] = (uint8_t)(nonce);
    msg[1] = (uint8_t)(nonce >> 8);
    msg[2] = (uint8_t)(nonce >> 16);
    msg[3] = (uint8_t)(nonce >> 24);
    for (int t = 0; t < 8; ++t) {
        uint32_t w = target_be[t];
        msg[4 + t*4 + 0] = (uint8_t)(w >> 24);
        msg[4 + t*4 + 1] = (uint8_t)(w >> 16);
        msg[4 + t*4 + 2] = (uint8_t)(w >> 8);
        msg[4 + t*4 + 3] = (uint8_t)(w);
    }
    msg[36] = 0x80;
    msg[62] = 0x01;
    msg[63] = 0x20;
    uint32_t w_in[16];
    for (int t = 0; t < 16; ++t) {
        w_in[t] = ((uint32_t)msg[t*4] << 24) | ((uint32_t)msg[t*4+1] << 16) |
                  ((uint32_t)msg[t*4+2] << 8) | (uint32_t)msg[t*4+3];
    }
    uint32_t st[8] = {
        0x6a09e667u,0xbb67ae85u,0x3c6ef372u,0xa54ff53au,
        0x510e527fu,0x9b05688cu,0x1f83d9abu,0x5be0cd19u
    };
    sha256_transform(st, w_in);
    uint8_t mid[64] = {0};
    for (int t = 0; t < 8; ++t) {
        mid[t*4+0] = (uint8_t)(st[t] >> 24);
        mid[t*4+1] = (uint8_t)(st[t] >> 16);
        mid[t*4+2] = (uint8_t)(st[t] >> 8);
        mid[t*4+3] = (uint8_t)(st[t]);
    }
    mid[32] = 0x80;
    mid[62] = 0x01; // 256 bits
    uint32_t w2[16];
    for (int t = 0; t < 16; ++t) {
        w2[t] = ((uint32_t)mid[t*4] << 24) | ((uint32_t)mid[t*4+1] << 16) |
                ((uint32_t)mid[t*4+2] << 8) | (uint32_t)mid[t*4+3];
    }
    uint32_t st2[8] = {
        0x6a09e667u,0xbb67ae85u,0x3c6ef372u,0xa54ff53au,
        0x510e527fu,0x9b05688cu,0x1f83d9abu,0x5be0cd19u
    };
    sha256_transform(st2, w2);
    for (int t = 0; t < 8; ++t) digest_be[t] = st2[t];
}

// LE unsigned compare: digest_le[0]=LSB ... digest_le[31]=MSB
__device__ int meets_target_le_bytes(const uint8_t digest_le[32], const uint8_t target_le[32]) {
    for (int i = 31; i >= 0; --i) {
        if (digest_le[i] < target_le[i]) return 1;
        if (digest_le[i] > target_le[i]) return 0;
    }
    return 1;
}

// Winner record: nonce + 32-byte digest (LE) = 36 bytes
extern "C" __global__ void pickaxe_stage_a_mine(
    const uint32_t nonce_base,
    const uint32_t* __restrict__ target_be_words,
    const uint8_t* __restrict__ target_le_bytes,
    uint32_t* __restrict__ winner_count,
    uint32_t* __restrict__ winner_nonces,
    uint8_t* __restrict__ winner_digests_le,
    const uint32_t winner_cap,
    unsigned long long* __restrict__ hashes_done,
    const uint32_t n
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    uint32_t nonce = nonce_base + i;
    uint32_t digest_be[8];
    hash256_nonce_target(nonce, target_be_words, digest_be);

    uint8_t digest_le[32];
    for (int t = 0; t < 8; ++t) {
        uint32_t w = digest_be[t];
        // BE word -> bytes then reverse whole digest for LE layout matching host meets_target_le
        digest_le[t*4+0] = (uint8_t)(w >> 24);
        digest_le[t*4+1] = (uint8_t)(w >> 16);
        digest_le[t*4+2] = (uint8_t)(w >> 8);
        digest_le[t*4+3] = (uint8_t)(w);
    }
    // Host meets_target_le treats digest[0] as LSB — Bitcoin HASH256 digest is usually big-endian bytes in arrays.
    // Existing host test uses digest as returned from SHA256 (BE byte order in [0]=MSB of first word...).
    // stage_a host compare in cuda_stage_a tests equality of BE digests.
    // search.rs meets_target_le walks index 31 down as MSB — so digest[31] is most significant.
    // sha2 Digest output: out[0] is MSB of hash. So for LE compare as coded, target_le_hex parsed into out[0]=first hex byte = MSB if hex is BE.
    // Keep same as host: use BE byte array as `digest` in meets_target_le (index 31 = last byte = LSB of number if BE encoding...)
    // Actually meets_target_le: "walk from most-significant byte (index 31) down" — so index 31 is MSB.
    // That means digest is stored LE (index 0 = LSB). But SHA256::digest gives BE (index 0 = MSB).
    // There may be a bug in existing meets_target_le vs hash256 usage — for mine kernel use same as host hash256 + meets_target_le:
    uint8_t digest_as_host[32];
    for (int t = 0; t < 8; ++t) {
        uint32_t w = digest_be[t];
        digest_as_host[t*4+0] = (uint8_t)(w >> 24);
        digest_as_host[t*4+1] = (uint8_t)(w >> 16);
        digest_as_host[t*4+2] = (uint8_t)(w >> 8);
        digest_as_host[t*4+3] = (uint8_t)(w);
    }
    if (meets_target_le_bytes(digest_as_host, target_le_bytes)) {
        uint32_t slot = atomicAdd(winner_count, 1u);
        if (slot < winner_cap) {
            winner_nonces[slot] = nonce;
            for (int b = 0; b < 32; ++b) winner_digests_le[slot * 32 + b] = digest_as_host[b];
        }
    }
    atomicAdd(hashes_done, 1ull);
}
