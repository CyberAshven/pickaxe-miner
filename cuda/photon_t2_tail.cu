// Offline T2 amount-grinding probe. The signed PHOTON commitment is fixed;
// j moves tokens from the reward back to the baton, preserving total supply.
// Include the existing field and SHA-256 implementations so GPU Schnorr
// preparation and digest semantics match the production CUDA pipeline.
#include "stage_b_kg.cu"
#define PICKAXE_STDINT_INCLUDED 1
#include "stage_c_hash.cu"

namespace {

constexpr uint32_t T2_WINDOW_BYTES = TX_BYTES + 3u;
constexpr uint32_t T2_POINT_WORDS = 24u;

__device__ __forceinline__ void t2_middle_compress(
    uint32_t state[8], const uint32_t* w
) {
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


template <uint32_t SHIFT>
__device__ void t2_prepare_window(
    const uint8_t* base_tx, const uint32_t* midstate6,
    const uint8_t* signatures, const uint8_t* negated_s,
    const uint32_t* points, uint32_t nonce_base, uint32_t window_count,
    uint8_t* window_txs, uint32_t* window_prefixes
) {
    const uint32_t window = blockIdx.x * blockDim.x + threadIdx.x;
    if (window >= window_count) return;
    const uint32_t nonce = nonce_base + window;
    const size_t point_offset = (size_t)window * T2_POINT_WORDS;
    Fe y, z, yz;
    for (int i = 0; i < 8; ++i) {
        y.d[i] = points[point_offset + 8u + (size_t)i];
        z.d[i] = points[point_offset + 16u + (size_t)i];
    }
    femul(&yz, &y, &z);
    const bool square = fe_is_square(&yz);
    const uint8_t* signature = signatures + (size_t)window * 64u;
    const uint8_t* s = square ? signature + 32u : negated_s + (size_t)window * 32u;
    uint8_t* tx = window_txs + (size_t)window * T2_WINDOW_BYTES;
    constexpr uint32_t tx_bytes = TX_BYTES + SHIFT;
    for (uint32_t i = 0; i < tx_bytes; ++i) tx[i] = base_tx[i];
    for (uint32_t i = 0; i < 4u; ++i)
        tx[NONCE_OFFSET + SHIFT + i] = (uint8_t)(nonce >> (8u * i));
    for (uint32_t i = 0; i < 32u; ++i) {
        tx[SIGNATURE_OFFSET + SHIFT + i] = signature[i];
        tx[SIGNATURE_OFFSET + SHIFT + 32u + i] = s[i];
    }
    uint32_t state[8];
    for (int i = 0; i < 8; ++i) state[i] = midstate6[i];
    uint8_t block[64];
    for (uint32_t i = 0; i < 64u; ++i) block[i] = tx[384u + i];
    sha256_transform(state, block);
    uint32_t* prefix = window_prefixes + (size_t)window * 8u;
    for (int i = 0; i < 8; ++i) prefix[i] = state[i];
}

template <uint32_t SHIFT>
__device__ __forceinline__ uint8_t t2_transaction_byte(
    const uint8_t* tx, uint64_t baton, uint64_t reward, uint32_t j, uint32_t index
) {
    constexpr uint32_t baton_start = 491u + SHIFT;
    constexpr uint32_t reward_start = 578u + SHIFT;
    constexpr uint32_t tx_bytes = TX_BYTES + SHIFT;
    if (index >= baton_start && index < baton_start + 8u)
        return (uint8_t)((baton + j) >> (8u * (index - baton_start)));
    if (index >= reward_start && index < reward_start + 8u)
        return (uint8_t)((reward - j) >> (8u * (index - reward_start)));
    if (index < tx_bytes) return tx[index];
    if (index == tx_bytes) return 0x80u;
    if (index >= 632u) {
        const uint64_t bit_length = (uint64_t)tx_bytes * 8ull;
        return (uint8_t)(bit_length >> (8u * (639u - index)));
    }
    return 0u;
}

// The group kernel shares rounds 0..9 of block 7; resume each candidate at round 10.
__device__ __forceinline__ void t2_sha256_from_round10(uint32_t state[8], const uint8_t block[64], const uint32_t head[8]) {
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

    uint32_t a = head[0], b = head[1], c = head[2], d = head[3];
    uint32_t e = head[4], f = head[5], g = head[6], h = head[7];
    for (int i = 10; i < 64; ++i) {
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

// The variable baton starts at byte 491 + SHIFT, after words 0..9 of block 7.
__device__ __forceinline__ void t2_block7_head(
    const uint8_t* tx, const uint32_t* prefix, uint32_t head[8]
) {
    const uint32_t k[10] = {
        0x428a2f98u,0x71374491u,0xb5c0fbcfu,0xe9b5dba5u,0x3956c25bu,
        0x59f111f1u,0x923f82a4u,0xab1c5ed5u,0xd807aa98u,0x12835b01u
    };
    uint32_t a=prefix[0], b=prefix[1], c=prefix[2], d=prefix[3];
    uint32_t e=prefix[4], f=prefix[5], g=prefix[6], h=prefix[7];
    #pragma unroll
    for (int i = 0; i < 10; ++i) {
        const int offset = 448 + 4 * i;
        const uint32_t w = ((uint32_t)tx[offset] << 24) |
            ((uint32_t)tx[offset+1] << 16) |
            ((uint32_t)tx[offset+2] << 8) | (uint32_t)tx[offset+3];
        const uint32_t s1 = rotr32(e,6) ^ rotr32(e,11) ^ rotr32(e,25);
        const uint32_t ch = (e & f) ^ ((~e) & g);
        const uint32_t t1 = h + s1 + ch + k[i] + w;
        const uint32_t s0 = rotr32(a,2) ^ rotr32(a,13) ^ rotr32(a,22);
        const uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        const uint32_t t2 = s0 + maj;
        h=g; g=f; f=e; e=d+t1; d=c; c=b; b=a; a=t1+t2;
    }
    head[0]=a; head[1]=b; head[2]=c; head[3]=d;
    head[4]=e; head[5]=f; head[6]=g; head[7]=h;
}

template <uint32_t SHIFT>
__device__ void t2_hash(
    const uint8_t* tx, const uint32_t* prefix, uint64_t baton, uint64_t reward,
    uint32_t j, uint8_t digest[32], const uint32_t* middle = nullptr, const uint32_t* head = nullptr
) {
    uint32_t state[8];
    for (int i = 0; i < 8; ++i) state[i] = prefix[i];
    uint8_t block[64];
    for (uint32_t i = 0; i < 64u; ++i)
        block[i] = t2_transaction_byte<SHIFT>(tx, baton, reward, j, 448u + i);
    if (head) t2_sha256_from_round10(state, block, head);
    else sha256_transform(state, block);
    if (middle) {
        t2_middle_compress(state, middle);
    } else {
        for (uint32_t i = 0; i < 64u; ++i)
            block[i] = t2_transaction_byte<SHIFT>(tx, baton, reward, j, 512u + i);
        sha256_transform(state, block);
    }
    for (uint32_t i = 0; i < 64u; ++i)
        block[i] = t2_transaction_byte<SHIFT>(tx, baton, reward, j, 576u + i);
    sha256_transform(state, block);
    uint8_t first_hash[32];
    sha256_store(state, first_hash);
    sha256_small(first_hash, 32u, digest);
}


__device__ __forceinline__ bool t2_meets_target(const uint8_t digest[32], const uint8_t* target) {
    const bool strict_positive = target[32] != 0u;
    if (strict_positive && (digest[31] & 0x80u)) return false;
    if (strict_positive && digest[31] == 0u) {
        uint8_t nonzero = 0u;
        for (int i = 0; i < 31; ++i) nonzero |= digest[i];
        if (nonzero == 0u) return false;
    }
    const uint8_t top = strict_positive ? digest[31] : (digest[31] & 0x7fu);
    if (top != target[31]) return top < target[31];
    for (int i = 30; i >= 0; --i) {
        if (digest[i] != target[i]) return digest[i] < target[i];
    }
    return false;
}

template <uint32_t SHIFT>
__device__ void t2_filter(
    const uint8_t* tx, const uint32_t* prefix, uint64_t baton, uint64_t reward,
    const uint8_t* target, uint32_t base, uint32_t count, uint32_t winner_cap,
    uint32_t* winner_count, uint32_t* winner_j, uint8_t* winner_hashes
) {
    const uint32_t index = blockIdx.x * blockDim.x + threadIdx.x;
    if (index >= count) return;
    const uint32_t j = base + index;
    uint8_t digest[32];
    t2_hash<SHIFT>(tx, prefix, baton, reward, j, digest);
    if (!t2_meets_target(digest, target)) return;
    const uint32_t slot = atomicAdd(winner_count, 1u);
    if (slot >= winner_cap) return;
    winner_j[slot] = j;
    copy_bytes(winner_hashes + (size_t)slot * 32u, digest, 32u);
}

template <uint32_t SHIFT>
__device__ void t2_filter_group(
    const uint8_t* window_txs, const uint32_t* window_prefixes,
    const uint32_t* middle_schedule,
    uint64_t baton, uint64_t reward, const uint8_t* target,
    uint32_t nonce_base, uint32_t j_base, uint32_t count, uint32_t winner_cap,
    uint32_t* winner_count, uint32_t* winner_nonces,
    uint32_t* winner_j, uint8_t* winner_hashes
) {
    const uint32_t first_index = blockIdx.x * blockDim.x;
    if (first_index >= count) return;
    const uint32_t first_position = j_base + first_index;
    const uint32_t valid_threads = (count - first_index < blockDim.x)
        ? count - first_index : blockDim.x;
    const uint32_t first_window = first_position >> 16;
    const uint32_t last_window = (first_position + valid_threads - 1u) >> 16;
    // A contiguous 128-thread block can cross at most one 65,536-candidate window edge.
    __shared__ uint32_t middle[64];
    __shared__ uint32_t head[2][8];
    if (threadIdx.x < 64u) middle[threadIdx.x] = middle_schedule[threadIdx.x];
    if (threadIdx.x == 2)
        t2_block7_head(window_txs + (size_t)first_window * T2_WINDOW_BYTES, window_prefixes + (size_t)first_window * 8u, head[0]);
    if (threadIdx.x == 3 && last_window != first_window)
        t2_block7_head(window_txs + (size_t)last_window * T2_WINDOW_BYTES, window_prefixes + (size_t)last_window * 8u, head[1]);
    __syncthreads();
    const uint32_t index = first_index + threadIdx.x;
    if (index >= count) return;
    const uint32_t position = j_base + index;
    const uint32_t window = position >> 16;
    const uint32_t j = position & 0xffffu;
    const uint8_t* tx = window_txs + (size_t)window * T2_WINDOW_BYTES;
    const uint32_t* prefix = window_prefixes + (size_t)window * 8u;
    uint8_t digest[32];
    t2_hash<SHIFT>(tx, prefix, baton, reward, j, digest,
                   middle,
                   head[window == first_window ? 0 : 1]);
    if (!t2_meets_target(digest, target)) return;
    const uint32_t slot = atomicAdd(winner_count, 1u);
    if (slot >= winner_cap) return;
    winner_nonces[slot] = nonce_base + window;
    winner_j[slot] = j;
    copy_bytes(winner_hashes + (size_t)slot * 32u, digest, 32u);
}


} // namespace


#define T2_EXPORT(SHIFT)                                                          \
extern "C" __global__ void pickaxe_t2_filter_shift##SHIFT(                      \
    const uint8_t* tx, const uint32_t* prefix, uint64_t baton, uint64_t reward,  \
    const uint8_t* target, uint32_t base, uint32_t count, uint32_t cap,          \
    uint32_t* result_count, uint32_t* result_j, uint8_t* result_hashes           \
) { t2_filter<SHIFT>(tx, prefix, baton, reward, target, base, count, cap,         \
                    result_count, result_j, result_hashes); }                    \
extern "C" __global__ void pickaxe_t2_probe_shift##SHIFT(                       \
    const uint8_t* tx, const uint32_t* prefix, uint64_t baton, uint64_t reward,  \
    uint16_t j, uint8_t* output                                               \
) { if (blockIdx.x == 0 && threadIdx.x == 0) {                                   \
    uint8_t digest[32]; t2_hash<SHIFT>(tx, prefix, baton, reward, j, digest);     \
    copy_bytes(output, digest, 32u); } }

T2_EXPORT(0)
T2_EXPORT(1)
T2_EXPORT(2)
T2_EXPORT(3)

#define T2_GROUP_EXPORT(SHIFT)                                                 \
extern "C" __global__ void pickaxe_t2_prepare_shift##SHIFT(                    \
    const uint8_t* base_tx, const uint32_t* midstate6,                          \
    const uint8_t* signatures, const uint8_t* negated_s, const uint32_t* points,\
    uint32_t nonce_base, uint32_t window_count, uint8_t* window_txs,            \
    uint32_t* window_prefixes                                                   \
) { t2_prepare_window<SHIFT>(base_tx, midstate6, signatures, negated_s, points,  \
                           nonce_base, window_count, window_txs, window_prefixes); } \
extern "C" __global__ void pickaxe_t2_filter_group_shift##SHIFT(               \
    const uint8_t* window_txs, const uint32_t* window_prefixes,                 \
    const uint32_t* middle_schedule,                                            \
    uint64_t baton, uint64_t reward, const uint8_t* target,                     \
    uint32_t nonce_base, uint32_t j_base, uint32_t count, uint32_t winner_cap,  \
    uint32_t* winner_count, uint32_t* winner_nonces, uint32_t* winner_j,        \
    uint8_t* winner_hashes                                                      \
) { t2_filter_group<SHIFT>(window_txs, window_prefixes, middle_schedule,       \
                          baton, reward, target, nonce_base, j_base, count,    \
                          winner_cap, winner_count, winner_nonces, winner_j,    \
                          winner_hashes); }

T2_GROUP_EXPORT(0)
T2_GROUP_EXPORT(1)
T2_GROUP_EXPORT(2)
T2_GROUP_EXPORT(3)
