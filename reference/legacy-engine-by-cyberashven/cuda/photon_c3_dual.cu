#include "stage_b_kg.cu"
#define PICKAXE_STDINT_INCLUDED 1
#include "stage_c_hash.cu"

// PHOTON Stage C2/C3 for the dual C1 output.
//
// C1 dual writes R||s for the nonce k and, separately, s for n - k. BCH
// Schnorr uses k when R.y is a square mod p and n - k otherwise, so exactly
// one of the two transactions is the real one. This kernel hashes both and
// runs the residue test only when one of them meets the target, then emits
// the candidate only if the passing variant is the real signature. The
// winner set is identical to computing the residue for every candidate.
//
// Transaction bytes 0..383 (SHA-256 blocks 0..5) are fixed for a job, so
// the host uploads the SHA-256 state after them. Block 6 (384..447) holds
// the nonce and the start of R and is shared by both variants; blocks 7..9
// and the second SHA-256 are hashed once per variant.
//
// The baton age is a minimal script-number push in the input script, so the
// transaction is 615 bytes for age 0..=16 and SHIFT = 1, 2 or 3 bytes longer
// for wider ages (the covenant requires age < 65535). Everything after the
// age push moves by SHIFT. One kernel is instantiated per SHIFT so every
// byte position stays a compile-time constant.

namespace {

constexpr uint32_t DUAL_MIDSTATE_BLOCKS = 6u;
constexpr uint32_t DUAL_TOTAL_BLOCKS = 10u;
constexpr uint32_t DUAL_POINT_WORDS = 24u;
constexpr uint32_t DUAL_MAX_SHIFT = 3u;

static_assert(DUAL_MIDSTATE_BLOCKS * 64u <= NONCE_OFFSET, "midstate must end before the nonce");
static_assert(SIGNATURE_OFFSET + 32u >= (DUAL_MIDSTATE_BLOCKS + 1u) * 64u,
              "block 6 must end before s for every layout");
static_assert(TX_BYTES + DUAL_MAX_SHIFT + 9u <= DUAL_TOTAL_BLOCKS * 64u,
              "every layout must fit ten SHA-256 blocks");

template <uint32_t SHIFT>
__device__ __forceinline__ uint8_t dual_tx_byte(
    const uint8_t* tx_template,
    const uint8_t* r_bytes,
    const uint8_t* s_bytes,
    uint32_t nonce,
    uint32_t byte_index
) {
    constexpr uint32_t tx_bytes = TX_BYTES + SHIFT;
    constexpr uint32_t nonce_offset = NONCE_OFFSET + SHIFT;
    constexpr uint32_t r_offset = SIGNATURE_OFFSET + SHIFT;
    constexpr uint32_t s_offset = r_offset + 32u;
    if (byte_index >= nonce_offset && byte_index < nonce_offset + 4u) {
#ifdef PICKAXE_INCREMENTAL_K_EXPERIMENT
        // Offline experiment: nonce_base identifies k, commitment stays fixed.
        return tx_template[byte_index];
#else
        return (uint8_t)(nonce >> ((byte_index - nonce_offset) * 8u));
#endif
    }
    if (byte_index >= r_offset && byte_index < s_offset) {
        return r_bytes[byte_index - r_offset];
    }
    if (byte_index >= s_offset && byte_index < r_offset + SIGNATURE_BYTES) {
        return s_bytes[byte_index - s_offset];
    }
    if (byte_index < tx_bytes) return tx_template[byte_index];
    if (byte_index == tx_bytes) return 0x80u;
    if (byte_index >= DUAL_TOTAL_BLOCKS * 64u - 8u) {
        const uint64_t bit_length = (uint64_t)tx_bytes * 8ull;
        const uint32_t length_offset = byte_index - (DUAL_TOTAL_BLOCKS * 64u - 8u);
        return (uint8_t)(bit_length >> ((7u - length_offset) * 8u));
    }
    return 0u;
}

template <uint32_t SHIFT>
__device__ __forceinline__ void dual_compress_block(
    uint32_t state[8],
    uint32_t block_index,
    const uint8_t* tx_template,
    const uint8_t* r_bytes,
    const uint8_t* s_bytes,
    uint32_t nonce
) {
    uint8_t block[64];
    const uint32_t block_start = block_index * 64u;
    for (uint32_t j = 0u; j < 64u; ++j) {
        block[j] = dual_tx_byte<SHIFT>(tx_template, r_bytes, s_bytes, nonce, block_start + j);
    }
    sha256_transform(state, block);
}

// Finishes HASH256 of the completed transaction from the state after block 6.
template <uint32_t SHIFT>
__device__ void dual_finish_hash(
    const uint32_t state_after_block6[8],
    const uint8_t* tx_template,
    const uint8_t* r_bytes,
    const uint8_t* s_bytes,
    uint32_t nonce,
    uint8_t final_hash[32]
) {
    uint32_t state[8];
    for (int i = 0; i < 8; ++i) state[i] = state_after_block6[i];
    for (uint32_t block_index = DUAL_MIDSTATE_BLOCKS + 1u; block_index < DUAL_TOTAL_BLOCKS; ++block_index) {
        dual_compress_block<SHIFT>(state, block_index, tx_template, r_bytes, s_bytes, nonce);
    }
    uint8_t first_hash[32];
    sha256_store(state, first_hash);
    sha256_small(first_hash, 32u, final_hash);
}

// BCH Schnorr residue test on Jacobian R: y = Y/Z^3 has the same Legendre
// symbol as Y*Z, so no inversion is needed.
__device__ bool dual_r_y_is_square(const uint32_t* points, uint32_t candidate) {
    const size_t base = (size_t)candidate * DUAL_POINT_WORDS;
    Fe y, z, yz;
    for (int limb = 0; limb < 8; ++limb) {
        y.d[limb] = points[base + 8u + (size_t)limb];
        z.d[limb] = points[base + 16u + (size_t)limb];
    }
    femul(&yz, &y, &z);
    return fe_is_square(&yz);
}

// The covenant compares ABS(BIN2NUM(HASH256(tx))) < target: the digest is
// read as a little-endian script number whose top bit is the sign, and ABS
// drops it, so bit 255 never affects the result.
__device__ __forceinline__ bool hash_meets_photon_target_le(
    const uint8_t hash[32],
    const uint8_t target[32]
) {
    const uint8_t top = (uint8_t)(hash[31] & 0x7fu);
    if (top != target[31]) return top < target[31];
    for (int i = 30; i >= 0; --i) {
        if (hash[i] < target[i]) return true;
        if (hash[i] > target[i]) return false;
    }
    return false;
}

template <uint32_t SHIFT>
__device__ __forceinline__ void stage_c_dual_filter(
    const uint8_t* __restrict__ tx_template,
    const uint32_t* __restrict__ midstate,
    const uint8_t* __restrict__ signatures,
    const uint8_t* __restrict__ negated_nonce_s,
    const uint32_t* __restrict__ points,
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
    const uint8_t* signature = signatures + (size_t)candidate * SIGNATURE_BYTES;
    const uint8_t* s_plus = signature + 32u;
    const uint8_t* s_minus = negated_nonce_s + (size_t)candidate * 32u;

    uint32_t state6[8];
    for (int i = 0; i < 8; ++i) state6[i] = midstate[i];
    // Block 6 ends before s, so either s pointer gives the same bytes.
    dual_compress_block<SHIFT>(state6, DUAL_MIDSTATE_BLOCKS, tx_template, signature, s_plus, nonce);

    uint8_t hash_plus[32];
    uint8_t hash_minus[32];
    dual_finish_hash<SHIFT>(state6, tx_template, signature, s_plus, nonce, hash_plus);
    dual_finish_hash<SHIFT>(state6, tx_template, signature, s_minus, nonce, hash_minus);
    const bool plus_meets = hash_meets_photon_target_le(hash_plus, target);
    const bool minus_meets = hash_meets_photon_target_le(hash_minus, target);
    if (!plus_meets && !minus_meets) return;

    const bool square = dual_r_y_is_square(points, candidate);
    const uint8_t* winning_hash = square ? hash_plus : hash_minus;
    if (!(square ? plus_meets : minus_meets)) return;

    const uint32_t slot = atomicAdd(winner_count, 1u);
    if (slot >= winner_cap) return;
    winner_nonces[slot] = nonce;
    copy_bytes(winner_hashes + (uint64_t)slot * 32u, winning_hash, 32u);
}

} // namespace

#define PICKAXE_DUAL_FILTER_ENTRY(NAME, SHIFT)                            \
    extern "C" __global__ void NAME(                                      \
        const uint8_t* __restrict__ tx_template,                          \
        const uint32_t* __restrict__ midstate,                            \
        const uint8_t* __restrict__ signatures,                           \
        const uint8_t* __restrict__ negated_nonce_s,                      \
        const uint32_t* __restrict__ points,                              \
        uint32_t nonce_base,                                              \
        const uint8_t* __restrict__ target,                               \
        uint32_t candidate_count,                                         \
        uint32_t winner_cap,                                              \
        uint32_t* __restrict__ winner_count,                              \
        uint32_t* __restrict__ winner_nonces,                             \
        uint8_t* __restrict__ winner_hashes                               \
    ) {                                                                   \
        stage_c_dual_filter<SHIFT>(tx_template, midstate, signatures,     \
            negated_nonce_s, points, nonce_base, target, candidate_count, \
            winner_cap, winner_count, winner_nonces, winner_hashes);      \
    }

// Age 0..=16 (615 bytes), 17..=127 (616), 128..=32767 (617), 32768..=65534 (618).
PICKAXE_DUAL_FILTER_ENTRY(pickaxe_stage_c_dual_filter, 0u)
PICKAXE_DUAL_FILTER_ENTRY(pickaxe_stage_c_dual_filter_shift1, 1u)
PICKAXE_DUAL_FILTER_ENTRY(pickaxe_stage_c_dual_filter_shift2, 2u)
PICKAXE_DUAL_FILTER_ENTRY(pickaxe_stage_c_dual_filter_shift3, 3u)
