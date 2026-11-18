//! 64-bit intermediates for the 32-bit limb arithmetic.
// #### PR #22
// What: native targets keep u64; SPIR-V uses the two-word value in `portable`,
// with the same operations, because WebGPU has no 64-bit integers.
// Why: field and scalar arithmetic is written once for native and portable GPUs.
// Check: native PTX identity for both Rust engines; tests compare `portable`
// with u64 arithmetic on the host.
#[cfg(not(target_arch = "spirv"))]
pub type Wide = u64;
#[cfg(target_arch = "spirv")]
pub use portable::Wide;

#[cfg(not(target_arch = "spirv"))]
pub const ZERO: Wide = 0;
#[cfg(target_arch = "spirv")]
pub const ZERO: Wide = portable::ZERO;

/// Zero-extends a word.
#[inline(always)]
pub fn extend(x: u32) -> Wide {
    #[cfg(not(target_arch = "spirv"))]
    {
        x as u64
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::extend(x)
    }
}

/// Complete 32 x 32-bit product.
#[inline(always)]
pub fn mul(a: u32, b: u32) -> Wide {
    #[cfg(not(target_arch = "spirv"))]
    {
        a as u64 * b as u64
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::mul(a, b)
    }
}

/// Low word.
#[inline(always)]
pub fn low(x: Wide) -> u32 {
    #[cfg(not(target_arch = "spirv"))]
    {
        x as u32
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::low(x)
    }
}

/// Value shifted right by one word.
#[inline(always)]
pub fn high(x: Wide) -> Wide {
    #[cfg(not(target_arch = "spirv"))]
    {
        x >> 32
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::high(x)
    }
}

/// True unless every bit is clear.
#[inline(always)]
pub fn nonzero(x: Wide) -> bool {
    #[cfg(not(target_arch = "spirv"))]
    {
        x != 0
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::nonzero(x)
    }
}

/// Top bit: the borrow out of a wrapping subtraction of extended words.
#[inline(always)]
pub fn sign(x: Wide) -> Wide {
    #[cfg(not(target_arch = "spirv"))]
    {
        x >> 63
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::sign(x)
    }
}

/// Product with a word; callers keep it below 2^64.
#[inline(always)]
pub fn scale(x: Wide, k: u32) -> Wide {
    #[cfg(not(target_arch = "spirv"))]
    {
        x * k as u64
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::scale(x, k)
    }
}

/// Wrapping difference.
#[inline(always)]
pub fn sub(a: Wide, b: Wide) -> Wide {
    #[cfg(not(target_arch = "spirv"))]
    {
        a.wrapping_sub(b)
    }
    #[cfg(target_arch = "spirv")]
    {
        portable::sub(a, b)
    }
}

// #### PR #22: indexing and limb loops that reach portable shaders without
// bounds checks or range objects, which would add panic paths and force
// rust-gpu to inline whole multiplications. Each macro expands to the original
// expression on native targets, so native code is token-for-token unchanged.

/// `array[index]`, unchecked on SPIR-V (indices there are constant or bounded).
#[cfg(not(target_arch = "spirv"))]
macro_rules! at {
    ($array:expr, $index:expr) => {
        $array[$index]
    };
}
#[cfg(target_arch = "spirv")]
macro_rules! at {
    ($array:expr, $index:expr) => {
        unsafe { *spirv_std::arch::IndexUnchecked::index_unchecked(&$array, $index) }
    };
}
pub(crate) use at;

/// `array[index] = value`, unchecked on SPIR-V.
#[cfg(not(target_arch = "spirv"))]
macro_rules! set {
    ($array:expr, $index:expr, $value:expr) => {
        $array[$index] = $value
    };
}
#[cfg(target_arch = "spirv")]
macro_rules! set {
    ($array:expr, $index:expr, $value:expr) => {{
        let value = $value;
        unsafe {
            *spirv_std::arch::IndexUnchecked::index_unchecked_mut(&mut $array, $index) = value
        }
    }};
}
pub(crate) use set;

/// `for i in start..8 { body }`; SPIR-V unrolls it with constant indices.
#[cfg(not(target_arch = "spirv"))]
macro_rules! limbs {
    ($i:ident in $start:literal..8 => $body:block) => {
        for $i in $start..8 $body
    };
}
#[cfg(target_arch = "spirv")]
macro_rules! limbs {
    ($i:ident in 0..8 => $body:block) => {
        limbs!(@ $i $body 0 1 2 3 4 5 6 7)
    };
    ($i:ident in 1..8 => $body:block) => {
        limbs!(@ $i $body 1 2 3 4 5 6 7)
    };
    ($i:ident in 2..8 => $body:block) => {
        limbs!(@ $i $body 2 3 4 5 6 7)
    };
    (@ $i:ident $body:block $($n:literal)*) => {
        $(
            // The last step's carry update is dead once unrolled.
            #[allow(unused_assignments)]
            {
                let $i: usize = $n;
                $body
            }
        )*
    };
}
pub(crate) use limbs;

/// The two-word form: SPIR-V uses it, and hosts compile it so tests can
/// compare every operation with u64. Native GPU builds leave it out.
#[cfg(not(any(target_os = "cuda", target_os = "amdhsa")))]
pub mod portable {
    #[derive(Clone, Copy)]
    #[cfg_attr(not(target_arch = "spirv"), derive(Debug, PartialEq, Eq))]
    pub struct Wide {
        pub lo: u32,
        pub hi: u32,
    }

    pub const ZERO: Wide = Wide { lo: 0, hi: 0 };

    #[inline(always)]
    pub fn extend(x: u32) -> Wide {
        Wide { lo: x, hi: 0 }
    }

    #[inline(always)]
    pub fn mul(a: u32, b: u32) -> Wide {
        // WGSL has no high-word multiply: combine four 16 x 16-bit products.
        let (a0, a1, b0, b1) = (a & 0xffff, a >> 16, b & 0xffff, b >> 16);
        let low = a0 * b0;
        let cross = a1 * b0;
        // a0 * b1 + both 16-bit carries is at most 2^32 - 2.
        let middle = a0 * b1 + (low >> 16) + (cross & 0xffff);
        Wide {
            lo: (middle << 16) | (low & 0xffff),
            hi: a1 * b1 + (middle >> 16) + (cross >> 16),
        }
    }

    #[inline(always)]
    pub fn low(x: Wide) -> u32 {
        x.lo
    }

    #[inline(always)]
    pub fn high(x: Wide) -> Wide {
        Wide { lo: x.hi, hi: 0 }
    }

    #[inline(always)]
    pub fn nonzero(x: Wide) -> bool {
        (x.lo | x.hi) != 0
    }

    #[inline(always)]
    pub fn sign(x: Wide) -> Wide {
        Wide {
            lo: x.hi >> 31,
            hi: 0,
        }
    }

    #[inline(always)]
    pub fn scale(x: Wide, k: u32) -> Wide {
        let product = mul(x.lo, k);
        Wide {
            lo: product.lo,
            hi: product.hi.wrapping_add(x.hi.wrapping_mul(k)),
        }
    }

    #[inline(always)]
    pub fn sub(a: Wide, b: Wide) -> Wide {
        Wide {
            lo: a.lo.wrapping_sub(b.lo),
            hi: a.hi.wrapping_sub(b.hi).wrapping_sub(u32::from(a.lo < b.lo)),
        }
    }

    impl core::ops::Add for Wide {
        type Output = Self;
        #[inline(always)]
        fn add(self, other: Self) -> Self {
            let lo = self.lo.wrapping_add(other.lo);
            Wide {
                lo,
                hi: self
                    .hi
                    .wrapping_add(other.hi)
                    .wrapping_add(u32::from(lo < self.lo)),
            }
        }
    }

    impl core::ops::AddAssign for Wide {
        #[inline(always)]
        fn add_assign(&mut self, other: Self) {
            *self = *self + other;
        }
    }
}
