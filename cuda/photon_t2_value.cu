// V coordinate: keep the signed commitment and token supply fixed while
// rolling the unsigned BCH value of the miner's P2PKH output. Each thread
// hashes up to 32 values after preparing the j-dependent seventh SHA block.
#include "photon_t2_tail.cu"

namespace {

template <uint32_t SHIFT>
__device__ void t2_value_filter_group(
    const uint8_t* window_txs, const uint32_t* window_prefixes,
    uint64_t baton, uint64_t reward, const uint8_t* target,
    uint32_t nonce_base, uint32_t candidate_base, uint32_t count,
    uint32_t group_base, uint32_t group_count, uint32_t winner_cap,
    uint32_t* winner_count, uint32_t* winner_nonces, uint32_t* winner_j,
    uint32_t* winner_sats, uint8_t* winner_hashes
) {
    constexpr uint32_t TILE = 32u;
    constexpr uint32_t FEE_OPTIONS = 211u - SHIFT;
    constexpr uint32_t TILES_PER_J = (FEE_OPTIONS + TILE - 1u) / TILE;
    constexpr uint32_t GROUPS_PER_NONCE = 65536u * TILES_PER_J;
    constexpr uint32_t CANDIDATES_PER_NONCE = 65536u * FEE_OPTIONS;
    const uint32_t index = blockIdx.x * blockDim.x + threadIdx.x;
    if (index >= group_count) return;
    const uint32_t group = group_base + index;
    const uint32_t window = group / GROUPS_PER_NONCE;
    const uint32_t within = group % GROUPS_PER_NONCE;
    const uint32_t j = within / TILES_PER_J;
    const uint32_t tile = within % TILES_PER_J;
    const uint32_t first = window * CANDIDATES_PER_NONCE + j * FEE_OPTIONS + tile * TILE;
    const uint8_t* tx = window_txs + (size_t)window * T2_WINDOW_BYTES;
    const uint32_t* prefix = window_prefixes + (size_t)window * 8u;

    uint32_t state7[8];
    uint8_t block[64], block9[64];
    for (int i = 0; i < 8; ++i) state7[i] = prefix[i];
    for (uint32_t i = 0; i < 64u; ++i)
        block[i] = t2_transaction_byte<SHIFT>(tx, baton, reward, j, 448u + i);
    sha256_transform(state7, block);
    for (uint32_t i = 0; i < 64u; ++i)
        block9[i] = t2_transaction_byte<SHIFT>(tx, baton, reward, j, 576u + i);

    for (uint32_t n = 0; n < TILE; ++n) {
        const uint32_t v = tile * TILE + n;
        if (v >= FEE_OPTIONS) break;
        const uint32_t position = first + n;
        if (position < candidate_base || position >= candidate_base + count) continue;
        const uint32_t sats = 675u + v;
        uint32_t state[8];
        for (int i = 0; i < 8; ++i) state[i] = state7[i];
        for (uint32_t i = 0; i < 64u; ++i) {
            const uint32_t offset = 512u + i;
            block[i] = (offset >= 534u + SHIFT && offset < 542u + SHIFT)
                ? (uint8_t)((uint64_t)sats >> (8u * (offset - 534u - SHIFT)))
                : t2_transaction_byte<SHIFT>(tx, baton, reward, j, offset);
        }
        sha256_transform(state, block);
        sha256_transform(state, block9);
        uint8_t first_hash[32], digest[32];
        sha256_store(state, first_hash);
        sha256_small(first_hash, 32u, digest);
        if (!t2_meets_target(digest, target)) continue;
        const uint32_t slot = atomicAdd(winner_count, 1u);
        if (slot >= winner_cap) continue;
        winner_nonces[slot] = nonce_base + window;
        winner_j[slot] = j;
        winner_sats[slot] = sats;
        copy_bytes(winner_hashes + (size_t)slot * 32u, digest, 32u);
    }
}

} // namespace

#define T2_VALUE_EXPORT(SHIFT)                                                  \
extern "C" __global__ void pickaxe_t2_value_filter_shift##SHIFT(              \
    const uint8_t* window_txs, const uint32_t* window_prefixes,                 \
    uint64_t baton, uint64_t reward, const uint8_t* target,                     \
    uint32_t nonce_base, uint32_t candidate_base, uint32_t count,              \
    uint32_t group_base, uint32_t group_count, uint32_t winner_cap,             \
    uint32_t* winner_count, uint32_t* winner_nonces, uint32_t* winner_j,        \
    uint32_t* winner_sats, uint8_t* winner_hashes                               \
) { t2_value_filter_group<SHIFT>(window_txs, window_prefixes, baton, reward,    \
    target, nonce_base, candidate_base, count, group_base, group_count,         \
    winner_cap, winner_count, winner_nonces, winner_j, winner_sats,             \
    winner_hashes); }

T2_VALUE_EXPORT(0)
T2_VALUE_EXPORT(1)
T2_VALUE_EXPORT(2)
T2_VALUE_EXPORT(3)
T2_VALUE_EXPORT(14)
T2_VALUE_EXPORT(15)
T2_VALUE_EXPORT(16)
