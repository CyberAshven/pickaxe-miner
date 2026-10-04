use crate::field::subtract;
#[cfg(not(target_arch = "spirv"))]
use crate::field::Field;
use crate::wide::{self as w, at, limbs, set};

// #### PR #22: limb loops have a SPIR-V index form beside the native form, as
// in field.rs; native code is unchanged. Keep each pair identical.

pub const N: [u32; 8] = [
    0xd0364141,
    0xbfd25e8c,
    0xaf48a03b,
    0xbaaedce6,
    0xfffffffe,
    u32::MAX,
    u32::MAX,
    u32::MAX,
];

#[derive(Clone, Copy)]
#[cfg_attr(not(target_arch = "spirv"), derive(Debug, PartialEq, Eq))]
pub struct Scalar(pub [u32; 8]);

#[cfg(target_arch = "spirv")]
impl PartialEq for Scalar {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        crate::field::equal(self.0, other.0)
    }
}

impl Scalar {
    pub const ZERO: Self = Self([0; 8]);

    #[cfg(not(target_arch = "spirv"))]
    pub fn from_be_bytes(bytes: [u8; 32]) -> Self {
        let words = core::array::from_fn(|i| {
            let j = (7 - i) * 4;
            u32::from_be_bytes([bytes[j], bytes[j + 1], bytes[j + 2], bytes[j + 3]])
        });
        let (reduced, borrow) = subtract(words, N);
        Self(if borrow == 0 { reduced } else { words })
    }

    #[cfg(not(target_arch = "spirv"))]
    pub fn to_be_bytes(self) -> [u8; 32] {
        Field(self.0).to_be_bytes()
    }

    pub fn add_mod(self, other: Self) -> Self {
        let mut result = [0; 8];
        let mut carry = w::ZERO;
        #[cfg(not(target_arch = "spirv"))]
        for (i, word) in result.iter_mut().enumerate() {
            carry += w::extend(self.0[i]) + w::extend(other.0[i]);
            *word = w::low(carry);
            carry = w::high(carry);
        }
        #[cfg(target_arch = "spirv")]
        limbs!(i in 0..8 => {
            carry += w::extend(at!(self.0, i)) + w::extend(at!(other.0, i));
            set!(result, i, w::low(carry));
            carry = w::high(carry);
        });
        let (reduced, borrow) = subtract(result, N);
        Self(if w::nonzero(carry) || borrow == 0 {
            reduced
        } else {
            result
        })
    }

    pub fn negate(self) -> Self {
        if self == Self::ZERO {
            self
        } else {
            Self(subtract(N, self.0).0)
        }
    }

    /// Computes self * rhs * 2^-256 mod n. Both operands must be canonical.
    pub fn montgomery_mul(self, rhs: Self) -> Self {
        let mut t = [0u32; 10];
        limbs!(i in 0..8 => {
            let mut carry = w::ZERO;
            #[cfg(not(target_arch = "spirv"))]
            for (j, word) in t[..8].iter_mut().enumerate() {
                let value = w::mul(self.0[j], rhs.0[i]) + w::extend(*word) + carry;
                *word = w::low(value);
                carry = w::high(value);
            }
            #[cfg(target_arch = "spirv")]
            limbs!(j in 0..8 => {
                let value = w::mul(at!(self.0, j), at!(rhs.0, i)) + w::extend(at!(t, j)) + carry;
                set!(t, j, w::low(value));
                carry = w::high(value);
            });
            let top = w::extend(t[8]) + carry;
            t[8] = w::low(top);
            t[9] = w::low(w::high(top));
            let m = t[0].wrapping_mul(0x5588b13f);
            carry = w::ZERO;
            limbs!(j in 0..8 => {
                let value = w::mul(m, at!(N, j)) + w::extend(at!(t, j)) + carry;
                if j != 0 {
                    set!(t, j - 1, w::low(value));
                }
                carry = w::high(value);
            });
            let top = w::extend(t[8]) + carry;
            t[7] = w::low(top);
            t[8] = t[9] + w::low(w::high(top));
        });
        let words = [t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7]];
        let (reduced, borrow) = subtract(words, N);
        Self(if t[8] != 0 || borrow == 0 {
            reduced
        } else {
            words
        })
    }
}
