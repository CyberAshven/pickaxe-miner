// Pickaxe Stage B â€” secp256k1 k*G (Jacobian double-and-add). 8xu32 limbs. MSVC/nvcc safe.
#include <cstdint>

struct Fe { uint32_t d[8]; }; // little-endian
struct JPoint { Fe x, y, z; int infinity; };

__device__ __forceinline__ Fe fe0() {
    Fe r; for (int i = 0; i < 8; ++i) r.d[i] = 0; return r;
}
__device__ __forceinline__ Fe fe1() {
    Fe r = fe0(); r.d[0] = 1; return r;
}
__device__ __forceinline__ Fe fep() {
    // p = FFFFFFFF FFFFFFFF FFFFFFFF FFFFFFFF FFFFFFFF FFFFFFFF FFFFFFFE FFFFFC2F
    Fe r;
    r.d[0] = 0xFFFFFC2Fu; r.d[1] = 0xFFFFFFFEu;
    r.d[2] = 0xFFFFFFFFu; r.d[3] = 0xFFFFFFFFu;
    r.d[4] = 0xFFFFFFFFu; r.d[5] = 0xFFFFFFFFu;
    r.d[6] = 0xFFFFFFFFu; r.d[7] = 0xFFFFFFFFu;
    return r;
}
__device__ __forceinline__ void fecpy(Fe* r, const Fe* a) {
    for (int i = 0; i < 8; ++i) r->d[i] = a->d[i];
}
__device__ __forceinline__ int feeqz(const Fe* a) {
    uint32_t x = 0; for (int i = 0; i < 8; ++i) x |= a->d[i]; return x == 0;
}
__device__ __forceinline__ int fecmp(const Fe* a, const Fe* b) {
    for (int i = 7; i >= 0; --i) {
        if (a->d[i] < b->d[i]) return -1;
        if (a->d[i] > b->d[i]) return 1;
    }
    return 0;
}

__device__ __noinline__ void fesubp_if_ge(Fe* r) {
    Fe p = fep();
    for (int k = 0; k < 3; ++k) {
        if (fecmp(r, &p) < 0) break;
        uint32_t br = 0;
        for (int i = 0; i < 8; ++i) {
            uint64_t ai = r->d[i];
            uint64_t sub = (uint64_t)p.d[i] + br;
            uint64_t t = ai - sub;
            r->d[i] = (uint32_t)t;
            br = (ai < sub) ? 1u : 0u;
        }
    }
}

__device__ __noinline__ void feadd(Fe* r, const Fe* a, const Fe* b) {
    uint64_t c = 0;
    for (int i = 0; i < 8; ++i) {
        c += (uint64_t)a->d[i] + b->d[i];
        r->d[i] = (uint32_t)c;
        c >>= 32;
    }
    if (c) {
        // 2^256 â‰¡ 2^32 + 977 (mod p)
        Fe corr = fe0();
        corr.d[0] = 977u;
        corr.d[1] = 1u; // 2^32
        c = 0;
        for (int i = 0; i < 8; ++i) {
            c += (uint64_t)r->d[i] + corr.d[i];
            r->d[i] = (uint32_t)c;
            c >>= 32;
        }
        if (c) {
            c = 0;
            for (int i = 0; i < 8; ++i) {
                c += (uint64_t)r->d[i] + corr.d[i];
                r->d[i] = (uint32_t)c;
                c >>= 32;
            }
        }
    }
    fesubp_if_ge(r);
}

__device__ __noinline__ void fesub(Fe* r, const Fe* a, const Fe* b) {
    uint32_t br = 0;
    for (int i = 0; i < 8; ++i) {
        uint64_t ai = a->d[i];
        uint64_t sub = (uint64_t)b->d[i] + br;
        uint64_t t = ai - sub;
        r->d[i] = (uint32_t)t;
        br = (ai < sub) ? 1u : 0u;
    }
    if (br) {
        Fe p = fep();
        uint64_t c = 0;
        for (int i = 0; i < 8; ++i) {
            c += (uint64_t)r->d[i] + p.d[i];
            r->d[i] = (uint32_t)c;
            c >>= 32;
        }
    }
}

__device__ __noinline__ void femul(Fe* r, const Fe* a, const Fe* b) {
    uint64_t acc[16];
    for (int i = 0; i < 16; ++i) acc[i] = 0;
    for (int i = 0; i < 8; ++i) {
        for (int j = 0; j < 8; ++j) {
            uint64_t prod = (uint64_t)a->d[i] * b->d[j];
            uint64_t s = acc[i + j] + (prod & 0xFFFFFFFFu);
            acc[i + j] = s & 0xFFFFFFFFu;
            uint64_t carry = (s >> 32) + (prod >> 32);
            uint64_t k = (uint64_t)(i + j + 1);
            while (carry) {
                uint64_t s2 = acc[k] + (carry & 0xFFFFFFFFu);
                acc[k] = s2 & 0xFFFFFFFFu;
                carry = (s2 >> 32) + (carry >> 32);
                ++k;
            }
        }
    }
    // lo = acc[0..7], hi = acc[8..15]
    Fe lo, hi;
    for (int i = 0; i < 8; ++i) { lo.d[i] = (uint32_t)acc[i]; hi.d[i] = (uint32_t)acc[i + 8]; }

    // r = lo + hi * (2^32 + 977)
    Fe hi977 = fe0();
    {
        uint64_t c = 0;
        for (int i = 0; i < 8; ++i) {
            c += (uint64_t)hi.d[i] * 977u;
            hi977.d[i] = (uint32_t)c;
            c >>= 32;
        }
        if (c) {
            // fold c * (2^32+977)
            Fe f = fe0();
            uint64_t x = c * 977u;
            f.d[0] = (uint32_t)x;
            f.d[1] = (uint32_t)(x >> 32);
            uint64_t y = c; // * 2^32 -> limb1
            uint64_t s = (uint64_t)f.d[1] + y;
            f.d[1] = (uint32_t)s;
            if (s >> 32) f.d[2] = (uint32_t)(s >> 32);
            Fe tmp; feadd(&tmp, &hi977, &f); fecpy(&hi977, &tmp);
        }
    }
    Fe hi32 = fe0();
    {
        // shift hi left by 32 bits (= one limb)
        for (int i = 7; i >= 1; --i) hi32.d[i] = hi.d[i - 1];
        hi32.d[0] = 0;
        uint32_t top = hi.d[7]; // overflow limb beyond 256
        Fe sum; feadd(&sum, &lo, &hi977);
        feadd(r, &sum, &hi32);
        if (top) {
            Fe fold = fe0();
            uint64_t x = (uint64_t)top * 977u;
            fold.d[0] = (uint32_t)x;
            fold.d[1] = (uint32_t)(x >> 32) + top; // + top<<32 into limb1
            // if limb1 overflow:
            if (fold.d[1] < top) fold.d[2] = 1;
            Fe tmp; feadd(&tmp, r, &fold); fecpy(r, &tmp);
        }
    }
    fesubp_if_ge(r);
}

__device__ __noinline__ void fesqr(Fe* r, const Fe* a) { femul(r, a, a); }
__device__ __forceinline__ void fedbl(Fe* r, const Fe* a) { feadd(r, a, a); }

__device__ __noinline__ void feinv(Fe* r, const Fe* a) {
    Fe base; fecpy(&base, a);
    Fe res = fe1();
    // exponent p-2 LE limbs
    const uint32_t e[8] = {
        0xFFFFFC2Du, 0xFFFFFFFEu, 0xFFFFFFFFu, 0xFFFFFFFFu,
        0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu, 0xFFFFFFFFu
    };
    for (int limb = 0; limb < 8; ++limb) {
        uint32_t w = e[limb];
        for (int bit = 0; bit < 32; ++bit) {
            if (w & 1u) { Fe t; femul(&t, &res, &base); fecpy(&res, &t); }
            Fe sq; fesqr(&sq, &base); fecpy(&base, &sq);
            w >>= 1;
        }
    }
    fecpy(r, &res);
}

__device__ __forceinline__ Fe secp_gx() {
    // 79BE667E F9DCBBAC 55A06295 CE870B07 029BFCDB 2DCE28D9 59F2815B 16F81798
    Fe r;
    r.d[0] = 0x16F81798u; r.d[1] = 0x59F2815Bu;
    r.d[2] = 0x2DCE28D9u; r.d[3] = 0x029BFCDBu;
    r.d[4] = 0xCE870B07u; r.d[5] = 0x55A06295u;
    r.d[6] = 0xF9DCBBACu; r.d[7] = 0x79BE667Eu;
    return r;
}
__device__ __forceinline__ Fe secp_gy() {
    // 483ADA77 26A3C465 5DA4FBFC 0E1108A8 FD17B448 A6855419 9C47D08F FB10D4B8
    Fe r;
    r.d[0] = 0xFB10D4B8u; r.d[1] = 0x9C47D08Fu;
    r.d[2] = 0xA6855419u; r.d[3] = 0xFD17B448u;
    r.d[4] = 0x0E1108A8u; r.d[5] = 0x5DA4FBFCu;
    r.d[6] = 0x26A3C465u; r.d[7] = 0x483ADA77u;
    return r;
}

__device__ __forceinline__ JPoint jinf() {
    JPoint p; p.x = fe0(); p.y = fe0(); p.z = fe0(); p.infinity = 1; return p;
}
__device__ __forceinline__ JPoint jfrom_affine(const Fe* x, const Fe* y) {
    JPoint p; fecpy(&p.x, x); fecpy(&p.y, y); p.z = fe1(); p.infinity = 0; return p;
}

__device__ __noinline__ void jdouble(JPoint* r, const JPoint* p) {
    if (p->infinity || feeqz(&p->y)) { *r = jinf(); return; }
    Fe yy, s, m, x3, y3, z3, tmp;
    fesqr(&yy, &p->y);
    femul(&tmp, &p->x, &yy); fedbl(&s, &tmp); fedbl(&s, &s);
    fesqr(&tmp, &p->x); feadd(&m, &tmp, &tmp); feadd(&m, &m, &tmp);
    fesqr(&x3, &m); Fe s2; fedbl(&s2, &s); fesub(&x3, &x3, &s2);
    femul(&tmp, &p->y, &p->z); fedbl(&z3, &tmp);
    fesub(&tmp, &s, &x3); femul(&y3, &m, &tmp);
    fesqr(&tmp, &yy); fedbl(&tmp, &tmp); fedbl(&tmp, &tmp); fedbl(&tmp, &tmp);
    fesub(&y3, &y3, &tmp);
    r->x = x3; r->y = y3; r->z = z3; r->infinity = 0;
}

__device__ __noinline__ void jadd_mixed(JPoint* r, const JPoint* p, const Fe* qx, const Fe* qy) {
    if (p->infinity) { *r = jfrom_affine(qx, qy); return; }
    Fe z2, z3, u, s, h, r0, hh, hhh, v, x3, y3, z3o, tmp;
    fesqr(&z2, &p->z);
    femul(&z3, &z2, &p->z);
    femul(&u, qx, &z2);
    femul(&s, qy, &z3);
    fesub(&h, &u, &p->x);
    fesub(&r0, &s, &p->y);
    if (feeqz(&h)) {
        if (feeqz(&r0)) { jdouble(r, p); return; }
        *r = jinf(); return;
    }
    fesqr(&hh, &h);
    femul(&hhh, &hh, &h);
    femul(&v, &p->x, &hh);
    fesqr(&x3, &r0); fesub(&x3, &x3, &hhh); Fe v2; fedbl(&v2, &v); fesub(&x3, &x3, &v2);
    fesub(&tmp, &v, &x3); femul(&y3, &r0, &tmp); femul(&tmp, &p->y, &hhh); fesub(&y3, &y3, &tmp);
    femul(&z3o, &p->z, &h);
    r->x = x3; r->y = y3; r->z = z3o; r->infinity = 0;
}

__device__ __noinline__ void scalar_mul_g(JPoint* out, const uint8_t k_be[32]) {
    JPoint r = jinf();
    Fe gx = secp_gx(); Fe gy = secp_gy();
    for (int i = 0; i < 256; ++i) {
        JPoint dbl; jdouble(&dbl, &r); r = dbl;
        int byte = k_be[i >> 3];
        int bit = (byte >> (7 - (i & 7))) & 1;
        if (bit) { JPoint add; jadd_mixed(&add, &r, &gx, &gy); r = add; }
    }
    *out = r;
}

__device__ __noinline__ void jacobian_to_compressed(uint8_t out33[33], const JPoint* p) {
    if (p->infinity) { for (int i = 0; i < 33; ++i) out33[i] = 0; return; }
    Fe zinv, z2, z3, x, y;
    feinv(&zinv, &p->z);
    fesqr(&z2, &zinv);
    femul(&z3, &z2, &zinv);
    femul(&x, &p->x, &z2);
    femul(&y, &p->y, &z3);
    for (int limb = 0; limb < 8; ++limb) {
        uint32_t w = x.d[7 - limb];
        out33[1 + limb * 4 + 0] = (uint8_t)(w >> 24);
        out33[1 + limb * 4 + 1] = (uint8_t)(w >> 16);
        out33[1 + limb * 4 + 2] = (uint8_t)(w >> 8);
        out33[1 + limb * 4 + 3] = (uint8_t)(w);
    }
    out33[0] = (y.d[0] & 1u) ? 0x03 : 0x02;
}

extern "C" __global__ void pickaxe_stage_b_kg(
    const uint8_t* __restrict__ scalars32,
    uint8_t* __restrict__ out_pub33,
    const uint32_t n
) {
    uint32_t i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i >= n) return;
    const uint8_t* k = scalars32 + (size_t)i * 32;
    JPoint P;
    scalar_mul_g(&P, k);
    jacobian_to_compressed(out_pub33 + (size_t)i * 33, &P);
}
