#include "stage_b_kg.cu"

// PHOTON Stage C1: Jacobian kG -> exact BCH Schnorr R||s.
//
// Inputs are persistent device buffers produced by Stage A and Stage B. The
// fixed-private-key multiplication uses the M39 reference table:
//   T[bytePosition][digit] = d * digit * 256^bytePosition mod n
// (32 * 256 * 32 bytes = 256 KiB). No per-candidate crypto crosses to host.

namespace {

constexpr uint32_t POINT_WORDS = 24u;

struct Scalar256 {
    uint32_t d[8];
};

__device__ __forceinline__ uint32_t c1_rotr32(uint32_t x, uint32_t n) {
    return (x >> n) | (x << (32u - n));
}

__device__ __forceinline__ void c1_copy(uint8_t* dst, const uint8_t* src, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) dst[i] = src[i];
}

__device__ __forceinline__ void c1_zero(uint8_t* dst, uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) dst[i] = 0u;
}

__device__ __forceinline__ void c1_sha_init(uint32_t state[8]) {
    state[0] = 0x6a09e667u; state[1] = 0xbb67ae85u;
    state[2] = 0x3c6ef372u; state[3] = 0xa54ff53au;
    state[4] = 0x510e527fu; state[5] = 0x9b05688cu;
    state[6] = 0x1f83d9abu; state[7] = 0x5be0cd19u;
}

__device__ void c1_sha_transform(uint32_t state[8], const uint8_t block[64]) {
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
        const uint32_t s0 = c1_rotr32(w[i - 15], 7) ^ c1_rotr32(w[i - 15], 18) ^ (w[i - 15] >> 3);
        const uint32_t s1 = c1_rotr32(w[i - 2], 17) ^ c1_rotr32(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = state[0], b = state[1], c = state[2], d = state[3];
    uint32_t e = state[4], f = state[5], g = state[6], h = state[7];
    for (int i = 0; i < 64; ++i) {
        const uint32_t s1 = c1_rotr32(e, 6) ^ c1_rotr32(e, 11) ^ c1_rotr32(e, 25);
        const uint32_t ch = (e & f) ^ ((~e) & g);
        const uint32_t t1 = h + s1 + ch + k[i] + w[i];
        const uint32_t s0 = c1_rotr32(a, 2) ^ c1_rotr32(a, 13) ^ c1_rotr32(a, 22);
        const uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        const uint32_t t2 = s0 + maj;
        h = g; g = f; f = e; e = d + t1;
        d = c; c = b; b = a; a = t1 + t2;
    }
    state[0] += a; state[1] += b; state[2] += c; state[3] += d;
    state[4] += e; state[5] += f; state[6] += g; state[7] += h;
}

__device__ void c1_sha256(const uint8_t* data, uint32_t len, uint8_t out[32]) {
    uint32_t state[8];
    c1_sha_init(state);
    uint32_t offset = 0u;
    while (len - offset >= 64u) {
        uint8_t block[64];
        c1_copy(block, data + offset, 64u);
        c1_sha_transform(state, block);
        offset += 64u;
    }
    const uint32_t rem = len - offset;
    uint8_t tail[128];
    c1_zero(tail, 128u);
    c1_copy(tail, data + offset, rem);
    tail[rem] = 0x80u;
    const uint32_t blocks = rem <= 55u ? 1u : 2u;
    const uint64_t bit_len = (uint64_t)len * 8ull;
    const uint32_t end = blocks * 64u;
    for (uint32_t i = 0; i < 8u; ++i) {
        tail[end - 1u - i] = (uint8_t)(bit_len >> (i * 8u));
    }
    c1_sha_transform(state, tail);
    if (blocks == 2u) c1_sha_transform(state, tail + 64u);
    for (int i = 0; i < 8; ++i) {
        out[i * 4 + 0] = (uint8_t)(state[i] >> 24);
        out[i * 4 + 1] = (uint8_t)(state[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(state[i] >> 8);
        out[i * 4 + 3] = (uint8_t)state[i];
    }
}

__device__ __forceinline__ JPoint c1_load_point(const uint32_t* points, uint32_t candidate) {
    const size_t base = (size_t)candidate * POINT_WORDS;
    JPoint point;
    for (int limb = 0; limb < 8; ++limb) {
        point.x.d[limb] = points[base + (size_t)limb];
        point.y.d[limb] = points[base + 8u + (size_t)limb];
        point.z.d[limb] = points[base + 16u + (size_t)limb];
    }
    point.infinity = feeqz(&point.z);
    return point;
}

__device__ __forceinline__ bool c1_y_is_quadratic_residue(const Fe* y) {
    return fe_is_square(y);
}

__device__ __forceinline__ Scalar256 scalar_n() {
    Scalar256 n;
    n.d[0] = 0xd0364141u; n.d[1] = 0xbfd25e8cu;
    n.d[2] = 0xaf48a03bu; n.d[3] = 0xbaaedce6u;
    n.d[4] = 0xfffffffeu; n.d[5] = 0xffffffffu;
    n.d[6] = 0xffffffffu; n.d[7] = 0xffffffffu;
    return n;
}

__device__ __forceinline__ Scalar256 scalar_zero() {
    Scalar256 value;
    for (int i = 0; i < 8; ++i) value.d[i] = 0u;
    return value;
}

__device__ __forceinline__ int scalar_cmp(const Scalar256* a, const Scalar256* b) {
    for (int i = 7; i >= 0; --i) {
        if (a->d[i] < b->d[i]) return -1;
        if (a->d[i] > b->d[i]) return 1;
    }
    return 0;
}

__device__ __forceinline__ Scalar256 scalar_sub_raw(const Scalar256* a, const Scalar256* b) {
    Scalar256 out;
    uint32_t borrow = 0u;
    for (int i = 0; i < 8; ++i) {
        const uint64_t av = a->d[i];
        const uint64_t sub = (uint64_t)b->d[i] + borrow;
        out.d[i] = (uint32_t)(av - sub);
        borrow = av < sub ? 1u : 0u;
    }
    return out;
}

__device__ __forceinline__ Scalar256 scalar_add_mod_n(const Scalar256* a, const Scalar256* b) {
    Scalar256 sum;
    uint64_t carry = 0u;
    for (int i = 0; i < 8; ++i) {
        const uint64_t value = (uint64_t)a->d[i] + b->d[i] + carry;
        sum.d[i] = (uint32_t)value;
        carry = value >> 32u;
    }
    const Scalar256 n = scalar_n();
    if (carry != 0u || scalar_cmp(&sum, &n) >= 0) {
        uint32_t borrow = 0u;
        for (int i = 0; i < 8; ++i) {
            const uint64_t av = sum.d[i];
            const uint64_t sub = (uint64_t)n.d[i] + borrow;
            sum.d[i] = (uint32_t)(av - sub);
            borrow = av < sub ? 1u : 0u;
        }
    }
    return sum;
}

__device__ __forceinline__ Scalar256 scalar_from_be(const uint8_t bytes[32]) {
    Scalar256 out;
    for (int limb = 0; limb < 8; ++limb) {
        const int offset = 28 - limb * 4;
        out.d[limb] = ((uint32_t)bytes[offset] << 24) |
                      ((uint32_t)bytes[offset + 1] << 16) |
                      ((uint32_t)bytes[offset + 2] << 8) |
                      (uint32_t)bytes[offset + 3];
    }
    return out;
}

__device__ __forceinline__ void scalar_to_be(const Scalar256* scalar, uint8_t bytes[32]) {
    for (int i = 0; i < 8; ++i) {
        const uint32_t word = scalar->d[7 - i];
        bytes[i * 4 + 0] = (uint8_t)(word >> 24);
        bytes[i * 4 + 1] = (uint8_t)(word >> 16);
        bytes[i * 4 + 2] = (uint8_t)(word >> 8);
        bytes[i * 4 + 3] = (uint8_t)word;
    }
}

__device__ __forceinline__ Scalar256 scalar_reduce_hash(const uint8_t hash[32]) {
    Scalar256 value = scalar_from_be(hash);
    const Scalar256 n = scalar_n();
    if (scalar_cmp(&value, &n) >= 0) value = scalar_sub_raw(&value, &n);
    return value;
}

__device__ __forceinline__ Scalar256 fixed_d_value(
    const uint32_t* table,
    uint32_t byte_position,
    uint32_t digit
) {
    const size_t base = ((size_t)byte_position * 256u + digit) * 8u;
    Scalar256 value;
    for (int limb = 0; limb < 8; ++limb) value.d[limb] = table[base + (size_t)limb];
    return value;
}

__device__ Scalar256 scalar_mul_fixed_d(const Scalar256* e, const uint32_t* table) {
    Scalar256 result = scalar_zero();
    for (uint32_t byte_position = 0u; byte_position < 32u; ++byte_position) {
        const uint32_t limb = byte_position >> 2u;
        const uint32_t shift = (byte_position & 3u) * 8u;
        const uint32_t digit = (e->d[limb] >> shift) & 0xffu;
        if (digit == 0u) continue;
        const Scalar256 addend = fixed_d_value(table, byte_position, digit);
        result = scalar_add_mod_n(&result, &addend);
    }
    return result;
}

__device__ __forceinline__ void fe_to_be(const Fe* value, uint8_t bytes[32]) {
    for (int i = 0; i < 8; ++i) {
        const uint32_t word = value->d[7 - i];
        bytes[i * 4 + 0] = (uint8_t)(word >> 24);
        bytes[i * 4 + 1] = (uint8_t)(word >> 16);
        bytes[i * 4 + 2] = (uint8_t)(word >> 8);
        bytes[i * 4 + 3] = (uint8_t)word;
    }
}

} // namespace

extern "C" __global__ void pickaxe_photon_c1_schnorr(
    const uint8_t* __restrict__ message_hashes,
    const uint8_t* __restrict__ rfc6979_scalars,
    const uint32_t* __restrict__ points,
    const uint8_t* __restrict__ public_key33,
    const uint32_t* __restrict__ fixed_d_table,
    uint8_t* __restrict__ signatures,
    uint32_t candidate_count
) {
    const uint32_t candidate = blockIdx.x * blockDim.x + threadIdx.x;
    if (candidate >= candidate_count) return;

    const JPoint point = c1_load_point(points, candidate);
    if (point.infinity) return;

    Fe z_inv, z2, z3, rx, ry;
    feinv(&z_inv, &point.z);
    fesqr(&z2, &z_inv);
    femul(&z3, &z2, &z_inv);
    femul(&rx, &point.x, &z2);
    femul(&ry, &point.y, &z3);

    uint8_t r_bytes[32];
    fe_to_be(&rx, r_bytes);

    const uint8_t* message = message_hashes + (size_t)candidate * 32u;
    uint8_t challenge_input[97];
    c1_copy(challenge_input, r_bytes, 32u);
    c1_copy(challenge_input + 32u, public_key33, 33u);
    c1_copy(challenge_input + 65u, message, 32u);
    uint8_t challenge_hash[32];
    c1_sha256(challenge_input, 97u, challenge_hash);
    const Scalar256 e = scalar_reduce_hash(challenge_hash);
    const Scalar256 ed = scalar_mul_fixed_d(&e, fixed_d_table);

    const uint8_t* k_bytes = rfc6979_scalars + (size_t)candidate * 32u;
    Scalar256 adjusted_k = scalar_from_be(k_bytes);
    if (!c1_y_is_quadratic_residue(&ry)) {
        const Scalar256 n = scalar_n();
        adjusted_k = scalar_sub_raw(&n, &adjusted_k);
    }
    const Scalar256 s = scalar_add_mod_n(&adjusted_k, &ed);
    uint8_t s_bytes[32];
    scalar_to_be(&s, s_bytes);

    uint8_t* signature = signatures + (size_t)candidate * 64u;
    c1_copy(signature, r_bytes, 32u);
    c1_copy(signature + 32u, s_bytes, 32u);
}

// C1 without the per-candidate quadratic-residue test. BCH Schnorr signs
// with k when R.y is a square and with n - k otherwise; R.x and e are the
// same either way. This writes R||s for s = k + e*d and, separately, the
// other candidate s = (n - k) + e*d. The dual C3 filter hashes both and
// tests the residue only for a candidate that meets the target.
__device__ __forceinline__ void c1_dual_sign(
    uint32_t candidate,
    const Fe* x,
    const Fe* z_inv,
    const uint8_t* __restrict__ message_hashes,
    const uint8_t* __restrict__ rfc6979_scalars,
    const uint8_t* __restrict__ public_key33,
    const uint32_t* __restrict__ fixed_d_table,
    uint8_t* __restrict__ signatures,
    uint8_t* __restrict__ negated_nonce_s
) {
    Fe z2, rx;
    fesqr(&z2, z_inv);
    femul(&rx, x, &z2);

    uint8_t r_bytes[32];
    fe_to_be(&rx, r_bytes);

    const uint8_t* message = message_hashes + (size_t)candidate * 32u;
    uint8_t challenge_input[97];
    c1_copy(challenge_input, r_bytes, 32u);
    c1_copy(challenge_input + 32u, public_key33, 33u);
    c1_copy(challenge_input + 65u, message, 32u);
    uint8_t challenge_hash[32];
    c1_sha256(challenge_input, 97u, challenge_hash);
    const Scalar256 e = scalar_reduce_hash(challenge_hash);
    const Scalar256 ed = scalar_mul_fixed_d(&e, fixed_d_table);

    const uint8_t* k_bytes = rfc6979_scalars + (size_t)candidate * 32u;
    const Scalar256 k = scalar_from_be(k_bytes);
    const Scalar256 n = scalar_n();
    const Scalar256 negated_k = scalar_sub_raw(&n, &k);
    const Scalar256 s_plus = scalar_add_mod_n(&k, &ed);
    const Scalar256 s_minus = scalar_add_mod_n(&negated_k, &ed);

    uint8_t* signature = signatures + (size_t)candidate * 64u;
    c1_copy(signature, r_bytes, 32u);
    scalar_to_be(&s_plus, signature + 32u);
    scalar_to_be(&s_minus, negated_nonce_s + (size_t)candidate * 32u);
}

constexpr uint32_t C1_MAX_CANDIDATES_PER_THREAD = 16u;

__device__ __forceinline__ Fe c1_load_z(const uint32_t* points, uint32_t candidate) {
    const size_t base = (size_t)candidate * POINT_WORDS + 16u;
    Fe z;
    for (int limb = 0; limb < 8; ++limb) z.d[limb] = points[base + (size_t)limb];
    return z;
}

// C1 dual with one shared inversion per thread (Montgomery's trick). Thread
// t handles candidates t, t + stride, ... (up to per_thread of them), so a
// warp still reads neighbouring candidates. Each candidate then costs three
// multiplications instead of a full inversion. A point at infinity (Z = 0)
// joins the product as 1 and is skipped, as in the single-candidate kernel.
extern "C" __global__ void pickaxe_photon_c1_schnorr_dual_batched(
    const uint8_t* __restrict__ message_hashes,
    const uint8_t* __restrict__ rfc6979_scalars,
    const uint32_t* __restrict__ points,
    const uint8_t* __restrict__ public_key33,
    const uint32_t* __restrict__ fixed_d_table,
    uint8_t* __restrict__ signatures,
    uint8_t* __restrict__ negated_nonce_s,
    uint32_t candidate_count,
    uint32_t per_thread
) {
    const uint32_t thread = blockIdx.x * blockDim.x + threadIdx.x;
    const uint32_t stride = gridDim.x * blockDim.x;
    if (per_thread > C1_MAX_CANDIDATES_PER_THREAD) per_thread = C1_MAX_CANDIDATES_PER_THREAD;

    // prefix[j] = product of the Z values before candidate j.
    Fe prefix[C1_MAX_CANDIDATES_PER_THREAD];
    Fe product = fe1();
    uint32_t count = 0u;
    for (uint32_t j = 0u; j < per_thread; ++j) {
        const uint32_t candidate = thread + j * stride;
        if (candidate >= candidate_count) break;
        prefix[j] = product;
        Fe z = c1_load_z(points, candidate);
        if (feeqz(&z)) z = fe1();
        femul(&product, &product, &z);
        count = j + 1u;
    }
    if (count == 0u) return;

    Fe inverse;
    feinv(&inverse, &product);
    for (uint32_t j = count; j-- > 0u;) {
        const uint32_t candidate = thread + j * stride;
        const JPoint point = c1_load_point(points, candidate);
        const Fe z = point.infinity ? fe1() : point.z;
        Fe z_inv;
        femul(&z_inv, &inverse, &prefix[j]);
        femul(&inverse, &inverse, &z);
        if (point.infinity) continue;
        c1_dual_sign(candidate, &point.x, &z_inv, message_hashes, rfc6979_scalars,
                     public_key33, fixed_d_table, signatures, negated_nonce_s);
    }
}
