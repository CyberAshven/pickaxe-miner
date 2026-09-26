// Unfunded PHOTON search only: public k values MUST NOT sign for a funded key.
#include "photon_stage_b16.cu"

extern "C" __global__ void pickaxe_photon_incremental_k(
    uint32_t base,
    uint32_t count,
    const uint32_t* __restrict__ table,
    const uint32_t* __restrict__ step_xy,
    const uint8_t* __restrict__ message,
    uint8_t* __restrict__ messages,
    uint8_t* __restrict__ scalars,
    uint32_t* __restrict__ points
) {
    const uint32_t lane = blockIdx.x * blockDim.x + threadIdx.x;
    const uint32_t stride = gridDim.x * blockDim.x;
    if (lane >= count) return;
    uint64_t k = (uint64_t)base + lane + 1u;
    JPoint point = jinf();
    // Only seed each lane with a multiplication; the rest are point additions.
    for (uint32_t w = 0; w < 4; ++w) {
        const uint32_t digit = (uint32_t)(k >> (16u * w)) & 0xffffu;
        if (!digit) continue;
        Fe x, y;
        load_table_point(table, w, digit, &x, &y);
        JPoint next;
        jadd_mixed(&next, &point, &x, &y);
        point = next;
    }
    Fe dx, dy;
    for (int i = 0; i < 8; ++i) {
        dx.d[i] = step_xy[i];
        dy.d[i] = step_xy[8 + i];
    }
    // Interleaved lanes advance by stride*G, covering consecutive k exactly.
    for (uint64_t candidate = lane; candidate < count; candidate += stride) {
        store_point(points, (uint32_t)candidate, &point);
        for (int j = 0; j < 32; ++j) {
            messages[candidate * 32u + j] = message[j];
            scalars[candidate * 32u + j] = j < 24 ? 0 : (uint8_t)(k >> (8 * (31 - j)));
        }
        if (candidate + stride >= count) break;
        JPoint next;
        jadd_mixed(&next, &point, &dx, &dy);
        point = next;
        k += stride;
    }
}
