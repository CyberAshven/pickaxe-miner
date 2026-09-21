#include "stage_b_kg.cu"

// M67.38/M45-compatible fixed-base Stage B.
//
// The persistent 64 MiB table contains 16 windows x 65,536 affine points.
// Each point is X||Y as 16 little-endian u32 limbs. The RFC6979 scalar from
// Stage A stays on-device as 32 big-endian bytes. Four serial kernels process
// four 16-bit windows each, matching the reference split that bounds register
// pressure while keeping the Jacobian accumulator in a persistent buffer.

namespace {

constexpr uint32_t M29_ENTRIES = 65536u;
constexpr uint32_t POINT_WORDS = 24u;

__device__ __forceinline__ uint32_t scalar_window16(const uint8_t* k_be, uint32_t window) {
    const uint32_t lo_from_end = window * 2u;
    const uint32_t hi_index = 30u - lo_from_end;
    const uint32_t lo_index = 31u - lo_from_end;
    return ((uint32_t)k_be[hi_index] << 8u) | (uint32_t)k_be[lo_index];
}

__device__ __forceinline__ void load_table_point(
    const uint32_t* table,
    uint32_t window,
    uint32_t digit,
    Fe* x,
    Fe* y
) {
    const size_t base = ((size_t)window * M29_ENTRIES + digit) * 16u;
    for (int limb = 0; limb < 8; ++limb) {
        x->d[limb] = table[base + (size_t)limb];
        y->d[limb] = table[base + 8u + (size_t)limb];
    }
}

__device__ __forceinline__ JPoint load_point(const uint32_t* points, uint32_t candidate) {
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

__device__ __forceinline__ void store_point(uint32_t* points, uint32_t candidate, const JPoint* point) {
    const size_t base = (size_t)candidate * POINT_WORDS;
    for (int limb = 0; limb < 8; ++limb) {
        points[base + (size_t)limb] = point->x.d[limb];
        points[base + 8u + (size_t)limb] = point->y.d[limb];
        points[base + 16u + (size_t)limb] = point->z.d[limb];
    }
}

__device__ void stage_b16_part(
    const uint8_t* scalars_be,
    const uint32_t* table,
    uint32_t* points,
    uint32_t candidate_count,
    uint32_t first_window,
    bool start_from_infinity
) {
    const uint32_t candidate = blockIdx.x * blockDim.x + threadIdx.x;
    if (candidate >= candidate_count) return;

    const uint8_t* scalar = scalars_be + (size_t)candidate * 32u;
    JPoint result = start_from_infinity ? jinf() : load_point(points, candidate);
    for (uint32_t offset = 0; offset < 4u; ++offset) {
        const uint32_t window = first_window + offset;
        const uint32_t digit = scalar_window16(scalar, window);
        if (digit == 0u) continue;
        Fe qx, qy;
        load_table_point(table, window, digit, &qx, &qy);
        JPoint next;
        jadd_mixed(&next, &result, &qx, &qy);
        result = next;
    }
    store_point(points, candidate, &result);
}

} // namespace

extern "C" __global__ void pickaxe_photon_b16_part0(
    const uint8_t* scalars_be,
    const uint32_t* table,
    uint32_t* points,
    uint32_t candidate_count
) {
    stage_b16_part(scalars_be, table, points, candidate_count, 0u, true);
}

extern "C" __global__ void pickaxe_photon_b16_part1(
    const uint8_t* scalars_be,
    const uint32_t* table,
    uint32_t* points,
    uint32_t candidate_count
) {
    stage_b16_part(scalars_be, table, points, candidate_count, 4u, false);
}

extern "C" __global__ void pickaxe_photon_b16_part2(
    const uint8_t* scalars_be,
    const uint32_t* table,
    uint32_t* points,
    uint32_t candidate_count
) {
    stage_b16_part(scalars_be, table, points, candidate_count, 8u, false);
}

extern "C" __global__ void pickaxe_photon_b16_part3(
    const uint8_t* scalars_be,
    const uint32_t* table,
    uint32_t* points,
    uint32_t candidate_count
) {
    stage_b16_part(scalars_be, table, points, candidate_count, 12u, false);
}
