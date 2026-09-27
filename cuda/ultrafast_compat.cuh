// Offline candidate: pinned UltrafastSecp256k1 device arithmetic with Pickaxe's
// existing u32 buffer layout. No signing identities or intermediates leave GPU.
#pragma once
#include "secp256k1.cuh"

namespace uf = secp256k1::cuda;
static_assert(sizeof(uf::FieldElement::limbs[0]) == 8, "use upstream 4x64 layout");
struct Fe { uint32_t d[8]; };
struct JPoint { Fe x, y, z; int infinity; };

template <typename T>
__device__ __forceinline__ T uf_read(const uint32_t words[8]) {
    T out;
#pragma unroll
    for (int i = 0; i < 4; ++i)
        out.limbs[i] = (uint64_t)words[2*i] | ((uint64_t)words[2*i+1] << 32);
    return out;
}

template <typename T>
__device__ __forceinline__ void uf_write(uint32_t words[8], const T& value) {
#pragma unroll
    for (int i = 0; i < 4; ++i) {
        words[2*i] = (uint32_t)value.limbs[i];
        words[2*i+1] = (uint32_t)(value.limbs[i] >> 32);
    }
}

__device__ __forceinline__ Fe fe0() { return {}; }
__device__ __forceinline__ Fe fe1() { Fe out = {}; out.d[0] = 1; return out; }
__device__ __forceinline__ int feeqz(const Fe* a) {
    const auto value = uf_read<uf::FieldElement>(a->d);
    return uf::field_is_zero(&value);
}
__device__ __forceinline__ void femul(Fe* r, const Fe* a, const Fe* b) {
    const auto x = uf_read<uf::FieldElement>(a->d);
    const auto y = uf_read<uf::FieldElement>(b->d);
    uf::FieldElement out;
    uf::field_mul(&x, &y, &out);
    uf_write(r->d, out);
}
__device__ __forceinline__ void fesqr(Fe* r, const Fe* a) {
    const auto x = uf_read<uf::FieldElement>(a->d);
    uf::FieldElement out;
    uf::field_sqr(&x, &out);
    uf_write(r->d, out);
}
__device__ __noinline__ void feinv(Fe* r, const Fe* a) {
    const auto x = uf_read<uf::FieldElement>(a->d);
    uf::FieldElement out;
    uf::field_inv(&x, &out);
    uf_write(r->d, out);
}
__device__ __noinline__ bool fe_is_square(const Fe* a) {
    const auto x = uf_read<uf::FieldElement>(a->d);
    uf::FieldElement root, square;
    uf::field_sqrt(&x, &root);
    uf::field_sqr(&root, &square);
    return uf::field_eq(&square, &x);
}
__device__ __forceinline__ JPoint jinf() {
    return {fe0(), fe0(), fe0(), 1};
}
__device__ __noinline__ void jadd_mixed(JPoint* r, const JPoint* p, const Fe* qx, const Fe* qy) {
    const uf::JacobianPoint x = {
        uf_read<uf::FieldElement>(p->x.d), uf_read<uf::FieldElement>(p->y.d),
        uf_read<uf::FieldElement>(p->z.d), p->infinity != 0
    };
    const uf::AffinePoint q = {uf_read<uf::FieldElement>(qx->d), uf_read<uf::FieldElement>(qy->d)};
    uf::JacobianPoint out = {};
    uf::jacobian_add_mixed(&x, &q, &out);
    if (out.infinity) { *r = jinf(); return; }
    uf_write(r->x.d, out.x);
    uf_write(r->y.d, out.y);
    uf_write(r->z.d, out.z);
    r->infinity = 0;
}
