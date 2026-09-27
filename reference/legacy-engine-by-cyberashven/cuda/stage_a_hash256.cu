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

extern "C" __global__ void pickaxe_stage_a_hash256(
    const uint32_t nonce_base,
    const uint32_t* __restrict__ target_be_words,
    uint32_t* __restrict__ out_digest_be,
    const uint32_t n
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    uint32_t nonce = nonce_base + i;
    uint8_t msg[64] = {0};
    msg[0] = (uint8_t)(nonce);
    msg[1] = (uint8_t)(nonce >> 8);
    msg[2] = (uint8_t)(nonce >> 16);
    msg[3] = (uint8_t)(nonce >> 24);
    for (int t = 0; t < 8; ++t) {
        uint32_t w = target_be_words[t];
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
    mid[62] = 0x01;
    for (int t = 0; t < 16; ++t) {
        w_in[t] = ((uint32_t)mid[t*4] << 24) | ((uint32_t)mid[t*4+1] << 16) |
                  ((uint32_t)mid[t*4+2] << 8) | (uint32_t)mid[t*4+3];
    }
    uint32_t st2[8] = {
        0x6a09e667u,0xbb67ae85u,0x3c6ef372u,0xa54ff53au,
        0x510e527fu,0x9b05688cu,0x1f83d9abu,0x5be0cd19u
    };
    sha256_transform(st2, w_in);
    for (int t = 0; t < 8; ++t) out_digest_be[i*8 + t] = st2[t];
}
