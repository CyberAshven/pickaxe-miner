use crate::field::{subtract, Field};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scalar(pub [u32; 8]);

impl Scalar {
    pub const ZERO: Self = Self([0; 8]);

    pub fn from_be_bytes(bytes: [u8; 32]) -> Self {
        let words = core::array::from_fn(|i| {
            let j = (7 - i) * 4;
            u32::from_be_bytes([bytes[j], bytes[j + 1], bytes[j + 2], bytes[j + 3]])
        });
        let (reduced, borrow) = subtract(words, N);
        Self(if borrow == 0 { reduced } else { words })
    }

    pub fn to_be_bytes(self) -> [u8; 32] {
        Field(self.0).to_be_bytes()
    }

    pub fn add_mod(self, other: Self) -> Self {
        let mut result = [0; 8];
        let mut carry = 0u64;
        for (i, word) in result.iter_mut().enumerate() {
            carry += self.0[i] as u64 + other.0[i] as u64;
            *word = carry as u32;
            carry >>= 32;
        }
        let (reduced, borrow) = subtract(result, N);
        Self(if carry != 0 || borrow == 0 {
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
        for i in 0..8 {
            let mut carry = 0u64;
            for (j, word) in t[..8].iter_mut().enumerate() {
                let value = self.0[j] as u64 * rhs.0[i] as u64 + *word as u64 + carry;
                *word = value as u32;
                carry = value >> 32;
            }
            let top = t[8] as u64 + carry;
            t[8] = top as u32;
            t[9] = (top >> 32) as u32;
            let m = t[0].wrapping_mul(0x5588b13f);
            carry = 0;
            for j in 0..8 {
                let value = m as u64 * N[j] as u64 + t[j] as u64 + carry;
                if j != 0 {
                    t[j - 1] = value as u32;
                }
                carry = value >> 32;
            }
            let top = t[8] as u64 + carry;
            t[7] = top as u32;
            t[8] = t[9] + (top >> 32) as u32;
        }
        let words = core::array::from_fn(|i| t[i]);
        let (reduced, borrow) = subtract(words, N);
        Self(if t[8] != 0 || borrow == 0 {
            reduced
        } else {
            words
        })
    }
}
