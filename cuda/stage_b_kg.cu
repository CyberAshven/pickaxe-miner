// Pickaxe Stage B v1 ? secp256k1 k*G via Jacobian double-and-add (correctness first).
// Later: 16-bit fixed-base table (~64MiB) + split-B for hashrate.
#include <cstdint>

struct JPoint { uint64_t x[4], y[4], z[4]; int infinity; };

__device__ __forceinline__ void fe_set0(uint64_t r[4]) { r[0]=r[1]=r[2]=r[3]=0; }
__device__ __forceinline__ void fe_set1(uint64_t r[4]) { r[0]=1; r[1]=r[2]=r[3]=0; }
__device__ __forceinline__ void fe_copy(uint64_t r[4], const uint64_t a[4]) {
    r[0]=a[0]; r[1]=a[1]; r[2]=a[2]; r[3]=a[3];
}

// Field ops for p = 2^256 - 2^32 - 977 using 4x64 limbs (simplified from libsecp style).
// For Stage B v1 we use a compact limb mul with 128-bit intermediates.

__device__ void fe_add(uint64_t r[4], const uint64_t a[4], const uint64_t b[4]);
__device__ void fe_sub(uint64_t r[4], const uint64_t a[4], const uint64_t b[4]);
__device__ void fe_mul(uint64_t r[4], const uint64_t a[4], const uint64_t b[4]);
__device__ void fe_sqr(uint64_t r[4], const uint64_t a[4]);
__device__ void fe_inv(uint64_t r[4], const uint64_t a[4]);

// NOTE: Full constant-time field arithmetic is long. Stage B v1 host path uses
// secp256k1 for k*G proof; this kernel is a stub launcher interface until the
// full fe_* + table path lands in the next commit.

extern "C" __global__ void pickaxe_stage_b_kg_stub(
    const uint8_t* __restrict__ scalars32, // n * 32 big-endian scalars
    uint8_t* __restrict__ out_pub33,       // n * 33 compressed pubs (filled host-side for now)
    const uint32_t n
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    // Stub: mark output as all-zero compressed prefix 0x02 so host can detect unimplemented GPU path.
    out_pub33[i * 33] = 0x00;
    for (int t = 1; t < 33; ++t) out_pub33[i * 33 + t] = 0;
    (void)scalars32;
}
