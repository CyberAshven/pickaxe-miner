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

// CUDA-only multiply path. Consumer NVIDIA GPUs issue 32-bit multiply-add
// with carry at full rate; the portable path below builds each column in
// u64 halves, which costs several more instructions per partial product.
// HIP and host builds use the portable path.
#if defined(__CUDA_ARCH__)
#define PICKAXE_FE_PTX 1

// (acc2:acc1:acc0) += x * y, using the PTX carry flag inside one statement.
__device__ __forceinline__ void fe_mac_ptx(
    uint32_t& acc0, uint32_t& acc1, uint32_t& acc2, uint32_t x, uint32_t y
) {
    asm("mad.lo.cc.u32 %0, %3, %4, %0;\n\t"
        "madc.hi.cc.u32 %1, %3, %4, %1;\n\t"
        "addc.u32 %2, %2, 0;"
        : "+r"(acc0), "+r"(acc1), "+r"(acc2)
        : "r"(x), "r"(y));
}

// t = a * b as 16 little-endian words, one product column at a time. A
// column holds at most 8 products plus the incoming carry, below 2^68, so
// three accumulator words suffice.
__device__ __forceinline__ void fe_mul_wide_ptx(uint32_t t[16], const Fe* a, const Fe* b) {
    uint32_t acc0 = 0, acc1 = 0, acc2 = 0;
#pragma unroll
    for (int k = 0; k < 15; ++k) {
#pragma unroll
        for (int i = (k < 8 ? 0 : k - 7); i <= (k < 8 ? k : 7); ++i) {
            fe_mac_ptx(acc0, acc1, acc2, a->d[i], b->d[k - i]);
        }
        t[k] = acc0;
        acc0 = acc1;
        acc1 = acc2;
        acc2 = 0;
    }
    t[15] = acc0;
}

// t = a * a: each cross product a[i]*a[j] (i < j) once, the sum doubled,
// then the diagonal squares added. The cross sum is below 2^511, so doubling
// cannot overflow 512 bits.
__device__ __forceinline__ void fe_sqr_wide_ptx(uint32_t t[16], const Fe* a) {
    uint32_t acc0 = 0, acc1 = 0, acc2 = 0;
    t[0] = 0;
#pragma unroll
    for (int k = 1; k < 15; ++k) {
#pragma unroll
        for (int i = (k < 8 ? 0 : k - 7); i < k - i; ++i) {
            fe_mac_ptx(acc0, acc1, acc2, a->d[i], a->d[k - i]);
        }
        t[k] = acc0;
        acc0 = acc1;
        acc1 = acc2;
        acc2 = 0;
    }
    t[15] = acc0;
    uint64_t w[8], s[8];
#pragma unroll
    for (int i = 0; i < 8; ++i) {
        w[i] = ((uint64_t)t[2 * i + 1] << 32) | t[2 * i];
        s[i] = (uint64_t)a->d[i] * a->d[i];
    }
    asm("add.cc.u64 %0, %0, %0;\n\t"
        "addc.cc.u64 %1, %1, %1;\n\t"
        "addc.cc.u64 %2, %2, %2;\n\t"
        "addc.cc.u64 %3, %3, %3;\n\t"
        "addc.cc.u64 %4, %4, %4;\n\t"
        "addc.cc.u64 %5, %5, %5;\n\t"
        "addc.cc.u64 %6, %6, %6;\n\t"
        "addc.u64 %7, %7, %7;"
        : "+l"(w[0]), "+l"(w[1]), "+l"(w[2]), "+l"(w[3]),
          "+l"(w[4]), "+l"(w[5]), "+l"(w[6]), "+l"(w[7]));
    asm("add.cc.u64 %0, %0, %8;\n\t"
        "addc.cc.u64 %1, %1, %9;\n\t"
        "addc.cc.u64 %2, %2, %10;\n\t"
        "addc.cc.u64 %3, %3, %11;\n\t"
        "addc.cc.u64 %4, %4, %12;\n\t"
        "addc.cc.u64 %5, %5, %13;\n\t"
        "addc.cc.u64 %6, %6, %14;\n\t"
        "addc.u64 %7, %7, %15;"
        : "+l"(w[0]), "+l"(w[1]), "+l"(w[2]), "+l"(w[3]),
          "+l"(w[4]), "+l"(w[5]), "+l"(w[6]), "+l"(w[7])
        : "l"(s[0]), "l"(s[1]), "l"(s[2]), "l"(s[3]),
          "l"(s[4]), "l"(s[5]), "l"(s[6]), "l"(s[7]));
#pragma unroll
    for (int i = 0; i < 8; ++i) {
        t[2 * i] = (uint32_t)w[i];
        t[2 * i + 1] = (uint32_t)(w[i] >> 32);
    }
}

// r = t mod p, the same fold as fe_reduce512 below written as carry chains:
// hi * 2^256 = hi * 977 + (hi << 32) (mod p), then the remaining word above
// 2^256 is folded the same way, then one conditional subtraction of p.
__device__ __forceinline__ void fe_reduce_wide_ptx(Fe* r, const uint32_t t[16]) {
    // m = hi * 977, nine words (each hi word times 977 is below 2^42).
    uint32_t m0, m1, m2, m3, m4, m5, m6, m7, m8;
    asm("mul.lo.u32 %0, %9, 977;\n\t"
        "mul.hi.u32 %1, %9, 977;\n\t"
        "mad.lo.cc.u32 %1, %10, 977, %1;\n\t"
        "madc.hi.u32 %2, %10, 977, 0;\n\t"
        "mad.lo.cc.u32 %2, %11, 977, %2;\n\t"
        "madc.hi.u32 %3, %11, 977, 0;\n\t"
        "mad.lo.cc.u32 %3, %12, 977, %3;\n\t"
        "madc.hi.u32 %4, %12, 977, 0;\n\t"
        "mad.lo.cc.u32 %4, %13, 977, %4;\n\t"
        "madc.hi.u32 %5, %13, 977, 0;\n\t"
        "mad.lo.cc.u32 %5, %14, 977, %5;\n\t"
        "madc.hi.u32 %6, %14, 977, 0;\n\t"
        "mad.lo.cc.u32 %6, %15, 977, %6;\n\t"
        "madc.hi.u32 %7, %15, 977, 0;\n\t"
        "mad.lo.cc.u32 %7, %16, 977, %7;\n\t"
        "madc.hi.u32 %8, %16, 977, 0;"
        : "=r"(m0), "=r"(m1), "=r"(m2), "=r"(m3), "=r"(m4),
          "=r"(m5), "=r"(m6), "=r"(m7), "=r"(m8)
        : "r"(t[8]), "r"(t[9]), "r"(t[10]), "r"(t[11]),
          "r"(t[12]), "r"(t[13]), "r"(t[14]), "r"(t[15]));

    // lo + m + (hi << 32): r0..r7 and the words above 2^256 in (m8, m9).
    uint32_t r0 = t[0], r1 = t[1], r2 = t[2], r3 = t[3];
    uint32_t r4 = t[4], r5 = t[5], r6 = t[6], r7 = t[7];
    uint32_t m9;
    asm("add.cc.u32 %0, %0, %10;\n\t"
        "addc.cc.u32 %1, %1, %11;\n\t"
        "addc.cc.u32 %2, %2, %12;\n\t"
        "addc.cc.u32 %3, %3, %13;\n\t"
        "addc.cc.u32 %4, %4, %14;\n\t"
        "addc.cc.u32 %5, %5, %15;\n\t"
        "addc.cc.u32 %6, %6, %16;\n\t"
        "addc.cc.u32 %7, %7, %17;\n\t"
        "addc.cc.u32 %8, %8, 0;\n\t"
        "addc.u32 %9, 0, 0;"
        : "+r"(r0), "+r"(r1), "+r"(r2), "+r"(r3), "+r"(r4),
          "+r"(r5), "+r"(r6), "+r"(r7), "+r"(m8), "=r"(m9)
        : "r"(m0), "r"(m1), "r"(m2), "r"(m3),
          "r"(m4), "r"(m5), "r"(m6), "r"(m7));
    // hi << 32: t8..t14 land on r1..r7 and t15 on m8; the carry goes to m9.
    asm("add.cc.u32 %0, %0, %9;\n\t"
        "addc.cc.u32 %1, %1, %10;\n\t"
        "addc.cc.u32 %2, %2, %11;\n\t"
        "addc.cc.u32 %3, %3, %12;\n\t"
        "addc.cc.u32 %4, %4, %13;\n\t"
        "addc.cc.u32 %5, %5, %14;\n\t"
        "addc.cc.u32 %6, %6, %15;\n\t"
        "addc.cc.u32 %7, %7, %16;\n\t"
        "addc.u32 %8, %8, 0;"
        : "+r"(r1), "+r"(r2), "+r"(r3), "+r"(r4), "+r"(r5),
          "+r"(r6), "+r"(r7), "+r"(m8), "+r"(m9)
        : "r"(t[8]), "r"(t[9]), "r"(t[10]), "r"(t[11]),
          "r"(t[12]), "r"(t[13]), "r"(t[14]), "r"(t[15]));

    // top = m9:m8 (below 2^35). Add top * 977 + (top << 32).
    uint32_t f0, f1, f2;
    asm("mul.lo.u32 %0, %3, 977;\n\t"
        "mul.hi.u32 %1, %3, 977;\n\t"
        "mad.lo.cc.u32 %1, %4, 977, %1;\n\t"
        "madc.hi.u32 %2, %4, 977, 0;\n\t"
        "add.cc.u32 %1, %1, %3;\n\t"
        "addc.u32 %2, %2, %4;"
        : "=r"(f0), "=r"(f1), "=r"(f2)
        : "r"(m8), "r"(m9));
    uint32_t overflow;
    asm("add.cc.u32 %0, %0, %9;\n\t"
        "addc.cc.u32 %1, %1, %10;\n\t"
        "addc.cc.u32 %2, %2, %11;\n\t"
        "addc.cc.u32 %3, %3, 0;\n\t"
        "addc.cc.u32 %4, %4, 0;\n\t"
        "addc.cc.u32 %5, %5, 0;\n\t"
        "addc.cc.u32 %6, %6, 0;\n\t"
        "addc.cc.u32 %7, %7, 0;\n\t"
        "addc.u32 %8, 0, 0;"
        : "+r"(r0), "+r"(r1), "+r"(r2), "+r"(r3),
          "+r"(r4), "+r"(r5), "+r"(r6), "+r"(r7), "=r"(overflow)
        : "r"(f0), "r"(f1), "r"(f2));
    // A carry here leaves a low part below 2^67; adding 2^32 + 977 for it
    // cannot carry again.
    asm("add.cc.u32 %0, %0, %8;\n\t"
        "addc.cc.u32 %1, %1, %9;\n\t"
        "addc.cc.u32 %2, %2, 0;\n\t"
        "addc.cc.u32 %3, %3, 0;\n\t"
        "addc.cc.u32 %4, %4, 0;\n\t"
        "addc.cc.u32 %5, %5, 0;\n\t"
        "addc.cc.u32 %6, %6, 0;\n\t"
        "addc.u32 %7, %7, 0;"
        : "+r"(r0), "+r"(r1), "+r"(r2), "+r"(r3),
          "+r"(r4), "+r"(r5), "+r"(r6), "+r"(r7)
        : "r"(overflow * 977u), "r"(overflow));

    // Subtract p when the value is at least p.
    uint32_t s0, s1, s2, s3, s4, s5, s6, s7, below;
    asm("sub.cc.u32 %0, %9, 0xFFFFFC2F;\n\t"
        "subc.cc.u32 %1, %10, 0xFFFFFFFE;\n\t"
        "subc.cc.u32 %2, %11, 0xFFFFFFFF;\n\t"
        "subc.cc.u32 %3, %12, 0xFFFFFFFF;\n\t"
        "subc.cc.u32 %4, %13, 0xFFFFFFFF;\n\t"
        "subc.cc.u32 %5, %14, 0xFFFFFFFF;\n\t"
        "subc.cc.u32 %6, %15, 0xFFFFFFFF;\n\t"
        "subc.cc.u32 %7, %16, 0xFFFFFFFF;\n\t"
        "subc.u32 %8, 0, 0;"
        : "=r"(s0), "=r"(s1), "=r"(s2), "=r"(s3),
          "=r"(s4), "=r"(s5), "=r"(s6), "=r"(s7), "=r"(below)
        : "r"(r0), "r"(r1), "r"(r2), "r"(r3),
          "r"(r4), "r"(r5), "r"(r6), "r"(r7));
    // below is all ones when the value was already under p.
    r->d[0] = (r0 & below) | (s0 & ~below);
    r->d[1] = (r1 & below) | (s1 & ~below);
    r->d[2] = (r2 & below) | (s2 & ~below);
    r->d[3] = (r3 & below) | (s3 & ~below);
    r->d[4] = (r4 & below) | (s4 & ~below);
    r->d[5] = (r5 & below) | (s5 & ~below);
    r->d[6] = (r6 & below) | (s6 & ~below);
    r->d[7] = (r7 & below) | (s7 & ~below);
}
#else
#define PICKAXE_FE_PTX 0
#endif

// Field arithmetic mod p = 2^256 - 2^32 - 977. Results are fully reduced
// (< p). feadd/fesub expect reduced inputs; femul/fesqr accept any 256-bit
// inputs. Everything is straight-line register code: no carry loops, no
// local-memory temporaries, and the same C++ builds for CUDA and HIP.

// r = a + 2^32 + 977 (mod 2^256). Returns the carry out of bit 256, which is
// set exactly when a + 2^256 - p >= 2^256, i.e. when a >= p.
__device__ __forceinline__ uint32_t fe_add_pc(Fe* r, const Fe* a) {
    uint64_t c = (uint64_t)a->d[0] + 977u;
    r->d[0] = (uint32_t)c; c >>= 32;
    c += (uint64_t)a->d[1] + 1u;
    r->d[1] = (uint32_t)c; c >>= 32;
#pragma unroll
    for (int i = 2; i < 8; ++i) {
        c += a->d[i];
        r->d[i] = (uint32_t)c; c >>= 32;
    }
    return (uint32_t)c;
}

__device__ __forceinline__ void fe_select(Fe* r, uint32_t take_b, const Fe* a, const Fe* b) {
    const uint32_t mask = 0u - (take_b & 1u);
#pragma unroll
    for (int i = 0; i < 8; ++i) r->d[i] = (b->d[i] & mask) | (a->d[i] & ~mask);
}

// Subtracts p once when r >= p. Valid for any r < 2^256.
__device__ __forceinline__ void fesubp_if_ge(Fe* r) {
    Fe t;
    const uint32_t ge = fe_add_pc(&t, r);
    fe_select(r, ge, r, &t);
}

__device__ __forceinline__ void feadd(Fe* r, const Fe* a, const Fe* b) {
    Fe s;
    uint64_t c = 0;
#pragma unroll
    for (int i = 0; i < 8; ++i) {
        c += (uint64_t)a->d[i] + b->d[i];
        s.d[i] = (uint32_t)c; c >>= 32;
    }
    // a + b < 2p, so one subtraction of p is enough. With a carry out of
    // bit 256 the true sum minus p is s + 2^256 - p, which fe_add_pc gives.
    Fe t;
    const uint32_t ge = fe_add_pc(&t, &s);
    fe_select(r, (uint32_t)c | ge, &s, &t);
}

__device__ __forceinline__ void fesub(Fe* r, const Fe* a, const Fe* b) {
    Fe s;
    uint32_t borrow = 0;
#pragma unroll
    for (int i = 0; i < 8; ++i) {
        const uint64_t t = (uint64_t)a->d[i] - b->d[i] - borrow;
        s.d[i] = (uint32_t)t;
        borrow = (uint32_t)(t >> 63);
    }
    // On borrow add p, i.e. subtract 2^32 + 977 modulo 2^256.
    Fe t;
    uint64_t w = (uint64_t)s.d[0] - 977u;
    t.d[0] = (uint32_t)w;
    w = (uint64_t)s.d[1] - 1u - (uint32_t)(w >> 63);
    t.d[1] = (uint32_t)w;
#pragma unroll
    for (int i = 2; i < 8; ++i) {
        w = (uint64_t)s.d[i] - (uint32_t)(w >> 63);
        t.d[i] = (uint32_t)w;
    }
    fe_select(r, borrow, &s, &t);
}

// Reduces a 512-bit little-endian product t modulo p.
__device__ __forceinline__ void fe_reduce512(Fe* r, const uint32_t t[16]) {
    // 2^256 = 2^32 + 977 (mod p): fold the high half as hi*977 + (hi << 32).
    uint64_t c = (uint64_t)t[0] + (uint64_t)t[8] * 977u;
    r->d[0] = (uint32_t)c; c >>= 32;
#pragma unroll
    for (int i = 1; i < 8; ++i) {
        c += (uint64_t)t[i] + (uint64_t)t[8 + i] * 977u + t[7 + i];
        r->d[i] = (uint32_t)c; c >>= 32;
    }
    // Remaining weight at 2^256 is below 2^34; fold it the same way.
    const uint64_t top = c + t[15];
    c = (uint64_t)r->d[0] + top * 977u;
    r->d[0] = (uint32_t)c; c >>= 32;
    c += (uint64_t)r->d[1] + top;
    r->d[1] = (uint32_t)c; c >>= 32;
#pragma unroll
    for (int i = 2; i < 8; ++i) {
        c += r->d[i];
        r->d[i] = (uint32_t)c; c >>= 32;
    }
    // A carry here leaves a low part below 2^67, so adding 2^32 + 977 for it
    // cannot carry again. Then at most one subtraction of p remains.
    const uint32_t extra = (uint32_t)c;
    c = (uint64_t)r->d[0] + (uint64_t)extra * 977u;
    r->d[0] = (uint32_t)c; c >>= 32;
    c += (uint64_t)r->d[1] + extra;
    r->d[1] = (uint32_t)c; c >>= 32;
#pragma unroll
    for (int i = 2; i < 8; ++i) {
        c += r->d[i];
        r->d[i] = (uint32_t)c; c >>= 32;
    }
    fesubp_if_ge(r);
}

__device__ __forceinline__ void femul(Fe* r, const Fe* a, const Fe* b) {
#if PICKAXE_FE_PTX
    uint32_t product[16];
    fe_mul_wide_ptx(product, a, b);
    fe_reduce_wide_ptx(r, product);
#else
    // Column-wise schoolbook product. Each column sums the low and high
    // halves of its partial products separately, so nothing overflows u64.
    uint32_t t[16];
    uint64_t carry = 0;
#pragma unroll
    for (int k = 0; k < 15; ++k) {
        uint64_t lo = carry;
        uint64_t hi = 0;
#pragma unroll
        for (int i = (k < 8 ? 0 : k - 7); i <= (k < 8 ? k : 7); ++i) {
            const uint64_t prod = (uint64_t)a->d[i] * b->d[k - i];
            lo += (uint32_t)prod;
            hi += prod >> 32;
        }
        t[k] = (uint32_t)lo;
        carry = (lo >> 32) + hi;
    }
    t[15] = (uint32_t)carry;
    fe_reduce512(r, t);
#endif
}

__device__ __forceinline__ void fesqr(Fe* r, const Fe* a) {
#if PICKAXE_FE_PTX
    uint32_t product[16];
    fe_sqr_wide_ptx(product, a);
    fe_reduce_wide_ptx(r, product);
#else
    // As femul, but each cross product a[i]*a[j] (i < j) is computed once
    // and doubled.
    uint32_t t[16];
    uint64_t carry = 0;
#pragma unroll
    for (int k = 0; k < 15; ++k) {
        uint64_t lo = 0;
        uint64_t hi = 0;
#pragma unroll
        for (int i = (k < 8 ? 0 : k - 7); i < k - i; ++i) {
            const uint64_t prod = (uint64_t)a->d[i] * a->d[k - i];
            lo += (uint32_t)prod;
            hi += prod >> 32;
        }
        lo <<= 1;
        hi <<= 1;
        if ((k & 1) == 0) {
            const uint64_t prod = (uint64_t)a->d[k >> 1] * a->d[k >> 1];
            lo += (uint32_t)prod;
            hi += prod >> 32;
        }
        lo += carry;
        t[k] = (uint32_t)lo;
        carry = (lo >> 32) + hi;
    }
    t[15] = (uint32_t)carry;
    fe_reduce512(r, t);
#endif
}

__device__ __forceinline__ void fesqr_n(Fe* r, int n) {
    for (int i = 0; i < n; ++i) fesqr(r, r);
}

// Shared prefix of the inversion and square-root addition chains
// (the libsecp256k1 chain): x2 = a^(2^2-1), x22 = a^(2^22-1),
// x223 = a^(2^223-1).
__device__ __noinline__ void fe_pow_chain223(Fe* x2, Fe* x22, Fe* x223, const Fe* a) {
    Fe x3, x6, x9, x11, x44, x88, x176, x220;
    fesqr(x2, a);          femul(x2, x2, a);
    fesqr(&x3, x2);        femul(&x3, &x3, a);
    x6 = x3;     fesqr_n(&x6, 3);    femul(&x6, &x6, &x3);
    x9 = x6;     fesqr_n(&x9, 3);    femul(&x9, &x9, &x3);
    x11 = x9;    fesqr_n(&x11, 2);   femul(&x11, &x11, x2);
    *x22 = x11;  fesqr_n(x22, 11);   femul(x22, x22, &x11);
    x44 = *x22;  fesqr_n(&x44, 22);  femul(&x44, &x44, x22);
    x88 = x44;   fesqr_n(&x88, 44);  femul(&x88, &x88, &x44);
    x176 = x88;  fesqr_n(&x176, 88); femul(&x176, &x176, &x88);
    x220 = x176; fesqr_n(&x220, 44); femul(&x220, &x220, &x44);
    *x223 = x220; fesqr_n(x223, 3);  femul(x223, x223, &x3);
}

// r = a^(p-2) = a^-1 (and 0 for a = 0): 255 squarings, 15 multiplications.
__device__ __noinline__ void feinv(Fe* r, const Fe* a) {
    Fe x2, x22, t;
    fe_pow_chain223(&x2, &x22, &t, a);
    fesqr_n(&t, 23); femul(&t, &t, &x22);
    fesqr_n(&t, 5);  femul(&t, &t, a);
    fesqr_n(&t, 3);  femul(&t, &t, &x2);
    fesqr_n(&t, 2);  femul(r, &t, a);
}

// r = a^((p+1)/4). When a is a square mod p, r is a square root of a.
__device__ __noinline__ void fe_pow_sqrt(Fe* r, const Fe* a) {
    Fe x2, x22, t;
    fe_pow_chain223(&x2, &x22, &t, a);
    fesqr_n(&t, 23); femul(&t, &t, &x22);
    fesqr_n(&t, 6);  femul(&t, &t, &x2);
    fesqr_n(&t, 2);
    *r = t;
}

// Quadratic-residue test: a is a square mod p iff (a^((p+1)/4))^2 == a.
// Zero counts as a square, matching the previous Euler-criterion check.
__device__ __forceinline__ bool fe_is_square(const Fe* a) {
    Fe root, check;
    fe_pow_sqrt(&root, a);
    fesqr(&check, &root);
    uint32_t diff = 0;
#pragma unroll
    for (int i = 0; i < 8; ++i) diff |= check.d[i] ^ a->d[i];
    return diff == 0u;
}

__device__ __forceinline__ void fedbl(Fe* r, const Fe* a) { feadd(r, a, a); }

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
