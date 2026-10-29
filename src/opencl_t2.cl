// #### PR #32
// PHOTON T2 amount grinding in OpenCL C: the same transaction layout, amount
// coordinate and winner rule as the shared Rust T2 filter
// (rust-engine/src/t2.rs and t2_block.rs).
//
// The host signs each window (one signature per 65,536 candidates) and sends
// the SHA-256 state after the transaction's first 448 bytes, plus the padded
// tail (bytes 448..639) as big-endian words with both token amounts zeroed.
// Each work-item adds its amount coordinate j (baton + j, reward - j),
// finishes the first SHA-256, hashes its digest again, and applies the
// covenant's target rule to the little-endian digest.
//
// PHOTON_SHIFT (the layout's byte shift, 0..16) is a build option, so every
// amount byte position is a constant and the tail stays in registers.

#ifndef PHOTON_SHIFT
#error "build with -D PHOTON_SHIFT=<layout shift>"
#endif

#define BATON_OFFSET (491 + PHOTON_SHIFT - 448)
#define REWARD_OFFSET (578 + PHOTON_SHIFT - 448)

__constant uint K[64] = {
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u, 0x3956c25bu, 0x59f111f1u,
    0x923f82a4u, 0xab1c5ed5u, 0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u,
    0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u, 0xc19bf174u, 0xe49b69c1u, 0xefbe4786u,
    0x0fc19dc6u, 0x240ca1ccu, 0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau,
    0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u, 0xc6e00bf3u, 0xd5a79147u,
    0x06ca6351u, 0x14292967u, 0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu, 0x53380d13u,
    0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u, 0xa2bfe8a1u, 0xa81a664bu,
    0xc24b8b70u, 0xc76c51a3u, 0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u,
    0x19a4c116u, 0x1e376c08u, 0x2748774cu, 0x34b0bcb5u, 0x391c0cb3u, 0x4ed8aa4au,
    0x5b9cca4fu, 0x682e6ff3u, 0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u,
    0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u};

#define ROTR(x, n) rotate((uint)(x), (uint)(32 - (n)))
#define BIG_S0(x) (ROTR(x, 2) ^ ROTR(x, 13) ^ ROTR(x, 22))
#define BIG_S1(x) (ROTR(x, 6) ^ ROTR(x, 11) ^ ROTR(x, 25))
#define SMALL_S0(x) (ROTR(x, 7) ^ ROTR(x, 18) ^ ((x) >> 3))
#define SMALL_S1(x) (ROTR(x, 17) ^ ROTR(x, 19) ^ ((x) >> 10))
#define CH(x, y, z) (((x) & (y)) ^ (~(x) & (z)))
#define MAJ(x, y, z) (((x) & (y)) | ((z) & ((x) | (y))))

// One SHA-256 compression of a 16-word block, with a 16-word rolling schedule.
void compress(uint state[8], uint w[16]) {
    uint a = state[0], b = state[1], c = state[2], d = state[3];
    uint e = state[4], f = state[5], g = state[6], h = state[7];
#pragma unroll
    for (int i = 0; i < 64; i++) {
        uint wi;
        if (i < 16) {
            wi = w[i];
        } else {
            wi = SMALL_S1(w[(i - 2) & 15]) + w[(i - 7) & 15] + SMALL_S0(w[(i - 15) & 15]) +
                 w[i & 15];
            w[i & 15] = wi;
        }
        uint t1 = h + BIG_S1(e) + CH(e, f, g) + K[i] + wi;
        uint t2 = BIG_S0(a) + MAJ(a, b, c);
        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }
    state[0] += a;
    state[1] += b;
    state[2] += c;
    state[3] += d;
    state[4] += e;
    state[5] += f;
    state[6] += g;
    state[7] += h;
}

// Writes one amount byte into the big-endian word array.
#define PUT(words, position, value) \
    ((words)[(position) >> 2] |= ((uint)((value) & 0xffu)) << (8 * (3 - ((position) & 3))))

// The covenant's rule on a little-endian digest (proof.rs,
// meets_target_le_for_rule): the positive rule also refuses a digest or a
// target that is zero or has its top bit set.
int meets(const uchar d[32], __global const uchar *t, uint strict) {
    if (strict) {
        uint digest_any = 0, target_any = 0;
        for (int i = 0; i < 32; i++) {
            digest_any |= d[i];
            target_any |= t[i];
        }
        if ((d[31] & 0x80u) || digest_any == 0 || (t[31] & 0x80u) || target_any == 0) {
            return 0;
        }
    }
    uchar top = d[31] & 0x7fu;
    if (top != t[31]) {
        return top < t[31];
    }
    for (int i = 30; i >= 0; i--) {
        if (d[i] < t[i]) {
            return 1;
        }
        if (d[i] > t[i]) {
            return 0;
        }
    }
    return 0;
}

__kernel void photon_t2(__global const uint *states, __global const uint *tails, ulong baton,
                        ulong reward, __global const uchar *target, uint strict, uint offset,
                        uint count, uint cap, __global volatile uint *winner_count,
                        __global uint *winner_window, __global uint *winner_j,
                        __global uchar *winner_hash) {
    uint index = get_global_id(0);
    if (index >= count) {
        return;
    }
    uint position = offset + index;
    uint window = position >> 16;
    uint j = position & 0xffffu;

    uint m[48];
#pragma unroll
    for (int i = 0; i < 48; i++) {
        m[i] = tails[window * 48 + i];
    }
    ulong b = baton + (ulong)j;
    ulong r = reward - (ulong)j;
#pragma unroll
    for (int i = 0; i < 8; i++) {
        PUT(m, BATON_OFFSET + i, b >> (8 * i));
        PUT(m, REWARD_OFFSET + i, r >> (8 * i));
    }

    uint state[8];
#pragma unroll
    for (int i = 0; i < 8; i++) {
        state[i] = states[window * 8 + i];
    }
#pragma unroll
    for (int block = 0; block < 3; block++) {
        uint w[16];
#pragma unroll
        for (int i = 0; i < 16; i++) {
            w[i] = m[block * 16 + i];
        }
        compress(state, w);
    }

    // The second SHA-256 hashes the 32-byte first digest: its big-endian words
    // are the state words, followed by the padding of a 256-bit message.
    uint w[16];
    for (int i = 0; i < 8; i++) {
        w[i] = state[i];
    }
    w[8] = 0x80000000u;
    for (int i = 9; i < 15; i++) {
        w[i] = 0;
    }
    w[15] = 256;
    uint h[8] = {0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
                 0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u};
    compress(h, w);

    uchar digest[32];
    for (int i = 0; i < 8; i++) {
        digest[4 * i] = (uchar)(h[i] >> 24);
        digest[4 * i + 1] = (uchar)(h[i] >> 16);
        digest[4 * i + 2] = (uchar)(h[i] >> 8);
        digest[4 * i + 3] = (uchar)h[i];
    }
    if (!meets(digest, target, strict)) {
        return;
    }
    uint slot = atomic_inc(winner_count);
    if (slot >= cap) {
        return;
    }
    winner_window[slot] = window;
    winner_j[slot] = j;
    for (int i = 0; i < 32; i++) {
        winner_hash[slot * 32 + i] = digest[i];
    }
}
