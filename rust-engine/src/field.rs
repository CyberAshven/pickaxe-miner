//! Canonical field elements modulo 2^256 - 2^32 - 977.
//! Variable-time mining arithmetic; not suitable for general wallet signing.
use crate::wide::{self as w, at, limbs, set};

// #### PR #22: rust-gpu cannot lower array iterators, so each iterator loop
// has a SPIR-V form beside the native form; index loops use the wide.rs
// macros, which expand to the original code natively. Native code is
// unchanged because even equivalent loops alter the verified PTX.

// rust-gpu cannot compile array equality (raw_eq) or formatting: SPIR-V
// compares words explicitly and omits Debug. Native derives are unchanged.
#[derive(Clone, Copy)]
#[cfg_attr(not(target_arch = "spirv"), derive(Debug, PartialEq, Eq))]
#[repr(transparent)]
pub struct Field(pub [u32; 8]);

#[cfg(target_arch = "spirv")]
impl PartialEq for Field {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        equal(self.0, other.0)
    }
}

/// Word-by-word equality without the array-comparison intrinsic.
#[cfg(target_arch = "spirv")]
#[inline(always)]
pub fn equal(a: [u32; 8], b: [u32; 8]) -> bool {
    let ([a0, a1, a2, a3, a4, a5, a6, a7], [b0, b1, b2, b3, b4, b5, b6, b7]) = (a, b);
    ((a0 ^ b0) | (a1 ^ b1) | (a2 ^ b2) | (a3 ^ b3) | (a4 ^ b4) | (a5 ^ b5) | (a6 ^ b6) | (a7 ^ b7))
        == 0
}

// Native keeps its verified expression; SPIR-V avoids the signed remainder,
// whose overflow check would add a panic path.
#[cfg(not(target_arch = "spirv"))]
macro_rules! even {
    ($n:literal) => {
        $n % 2 == 0
    };
}
#[cfg(target_arch = "spirv")]
macro_rules! even {
    ($n:literal) => {
        $n & 1 == 0
    };
}

pub const P: [u32; 8] = [
    0xffff_fc2f,
    0xffff_fffe,
    u32::MAX,
    u32::MAX,
    u32::MAX,
    u32::MAX,
    u32::MAX,
    u32::MAX,
];

pub fn subtract(a: [u32; 8], b: [u32; 8]) -> ([u32; 8], u32) {
    let mut result = [0; 8];
    let mut borrow = w::ZERO;
    limbs!(i in 0..8 => {
        let value = w::sub(w::extend(at!(a, i)), w::extend(at!(b, i)) + borrow);
        set!(result, i, w::low(value));
        borrow = w::sign(value);
    });
    (result, w::low(borrow))
}

impl Field {
    pub const ZERO: Self = Self([0; 8]);
    pub const ONE: Self = Self([1, 0, 0, 0, 0, 0, 0, 0]);

    #[inline(always)]
    pub fn reduced(words: [u32; 8]) -> Self {
        let (reduced, borrow) = subtract(words, P);
        Self(if borrow == 0 { reduced } else { words })
    }

    #[inline(always)]
    pub fn add_mod(self, other: Self) -> Self {
        let mut words = [0; 8];
        let mut carry = w::ZERO;
        #[cfg(not(target_arch = "spirv"))]
        for (i, word) in words.iter_mut().enumerate() {
            carry += w::extend(self.0[i]) + w::extend(other.0[i]);
            *word = w::low(carry);
            carry = w::high(carry);
        }
        #[cfg(target_arch = "spirv")]
        limbs!(i in 0..8 => {
            carry += w::extend(at!(self.0, i)) + w::extend(at!(other.0, i));
            set!(words, i, w::low(carry));
            carry = w::high(carry);
        });
        let (reduced, borrow) = subtract(words, P);
        Self(if w::nonzero(carry) || borrow == 0 {
            reduced
        } else {
            words
        })
    }

    #[inline(always)]
    pub fn sub_mod(self, other: Self) -> Self {
        let (mut words, borrow) = subtract(self.0, other.0);
        let mask = 0u32.wrapping_sub(borrow);
        let mut carry = w::ZERO;
        #[cfg(not(target_arch = "spirv"))]
        for (i, word) in words.iter_mut().enumerate() {
            carry += w::extend(*word) + w::extend(P[i] & mask);
            *word = w::low(carry);
            carry = w::high(carry);
        }
        #[cfg(target_arch = "spirv")]
        limbs!(i in 0..8 => {
            carry += w::extend(at!(words, i)) + w::extend(at!(P, i) & mask);
            set!(words, i, w::low(carry));
            carry = w::high(carry);
        });
        Self(words)
    }

    #[inline(always)]
    fn reduce_wide(t: [u32; 16]) -> Self {
        let mut words = [0; 8];
        let mut carry = w::extend(t[0]) + w::mul(t[8], 977);
        words[0] = w::low(carry);
        carry = w::high(carry);
        limbs!(i in 1..8 => {
            carry += w::extend(at!(t, i)) + w::mul(at!(t, i + 8), 977) + w::extend(at!(t, i + 7));
            set!(words, i, w::low(carry));
            carry = w::high(carry);
        });
        let top = carry + w::extend(t[15]);
        carry = w::extend(words[0]) + w::scale(top, 977);
        words[0] = w::low(carry);
        carry = w::high(carry) + w::extend(words[1]) + top;
        words[1] = w::low(carry);
        carry = w::high(carry);
        #[cfg(not(target_arch = "spirv"))]
        for word in &mut words[2..] {
            carry += w::extend(*word);
            *word = w::low(carry);
            carry = w::high(carry);
        }
        #[cfg(target_arch = "spirv")]
        limbs!(i in 2..8 => {
            carry += w::extend(at!(words, i));
            set!(words, i, w::low(carry));
            carry = w::high(carry);
        });
        let overflow = carry;
        carry = w::extend(words[0]) + w::scale(overflow, 977);
        words[0] = w::low(carry);
        carry = w::high(carry) + w::extend(words[1]) + overflow;
        words[1] = w::low(carry);
        carry = w::high(carry);
        #[cfg(not(target_arch = "spirv"))]
        for word in &mut words[2..] {
            carry += w::extend(*word);
            *word = w::low(carry);
            carry = w::high(carry);
        }
        #[cfg(target_arch = "spirv")]
        limbs!(i in 2..8 => {
            carry += w::extend(at!(words, i));
            set!(words, i, w::low(carry));
            carry = w::high(carry);
        });
        Self::reduced(words)
    }

    // One shared copy per portable shader keeps generated WGSL small.
    #[cfg_attr(not(target_arch = "spirv"), inline(always))]
    #[cfg_attr(target_arch = "spirv", inline(never))]
    pub fn mul_mod(self, other: Self) -> Self {
        let mut wide = [0; 16];
        let mut carry = w::ZERO;
        // Explicit columns keep GPU code in registers: each index is constant.
        macro_rules! column {
            ($column:literal; $($i:literal),*) => {{
                let mut low = carry;
                let mut high = w::ZERO;
                $(let product = w::mul(self.0[$i], at!(other.0, $column - $i));
                  low += w::extend(w::low(product));
                  high += w::high(product);)*
                wide[$column] = w::low(low);
                carry = w::high(low) + high;
            }};
        }
        column!(0; 0);
        column!(1; 0, 1);
        column!(2; 0, 1, 2);
        column!(3; 0, 1, 2, 3);
        column!(4; 0, 1, 2, 3, 4);
        column!(5; 0, 1, 2, 3, 4, 5);
        column!(6; 0, 1, 2, 3, 4, 5, 6);
        column!(7; 0, 1, 2, 3, 4, 5, 6, 7);
        column!(8; 1, 2, 3, 4, 5, 6, 7);
        column!(9; 2, 3, 4, 5, 6, 7);
        column!(10; 3, 4, 5, 6, 7);
        column!(11; 4, 5, 6, 7);
        column!(12; 5, 6, 7);
        column!(13; 6, 7);
        column!(14; 7);
        wide[15] = w::low(carry);
        Self::reduce_wide(wide)
    }

    // One shared copy per portable shader keeps generated WGSL small.
    #[cfg_attr(not(target_arch = "spirv"), inline(always))]
    #[cfg_attr(target_arch = "spirv", inline(never))]
    pub fn square(self) -> Self {
        let mut wide = [0; 16];
        let mut carry = w::ZERO;
        macro_rules! column {
            ($column:literal; $($i:literal),*) => {{
                let mut low = w::ZERO;
                let mut high = w::ZERO;
                $(let product = w::mul(self.0[$i], at!(self.0, $column - $i));
                  low += w::extend(w::low(product));
                  high += w::high(product);)*
                low = w::scale(low, 2);
                high = w::scale(high, 2);
                if even!($column) {
                    let diagonal =
                        w::mul(at!(self.0, $column / 2), at!(self.0, $column / 2));
                    low += w::extend(w::low(diagonal));
                    high += w::high(diagonal);
                }
                low += carry;
                wide[$column] = w::low(low);
                carry = w::high(low) + high;
            }};
        }
        column!(0; );
        column!(1; 0);
        column!(2; 0);
        column!(3; 0, 1);
        column!(4; 0, 1);
        column!(5; 0, 1, 2);
        column!(6; 0, 1, 2);
        column!(7; 0, 1, 2, 3);
        column!(8; 1, 2, 3);
        column!(9; 2, 3, 4);
        column!(10; 3, 4);
        column!(11; 4, 5);
        column!(12; 5);
        column!(13; 6);
        column!(14; );
        wide[15] = w::low(carry);
        Self::reduce_wide(wide)
    }

    fn square_n(mut self, count: usize) -> Self {
        for _ in 0..count {
            self = self.square();
        }
        self
    }

    fn chain223(self) -> (Self, Self, Self) {
        let x2 = self.square().mul_mod(self);
        let x3 = x2.square().mul_mod(self);
        let x6 = x3.square_n(3).mul_mod(x3);
        let x9 = x6.square_n(3).mul_mod(x3);
        let x11 = x9.square_n(2).mul_mod(x2);
        let x22 = x11.square_n(11).mul_mod(x11);
        let x44 = x22.square_n(22).mul_mod(x22);
        let x88 = x44.square_n(44).mul_mod(x44);
        let x176 = x88.square_n(88).mul_mod(x88);
        let x220 = x176.square_n(44).mul_mod(x44);
        (x2, x22, x220.square_n(3).mul_mod(x3))
    }

    /// Inverse of a nonzero value; returns zero for zero.
    pub fn inverse(self) -> Self {
        let (x2, x22, x223) = self.chain223();
        x223.square_n(23)
            .mul_mod(x22)
            .square_n(5)
            .mul_mod(self)
            .square_n(3)
            .mul_mod(x2)
            .square_n(2)
            .mul_mod(self)
    }

    pub fn is_square(self) -> bool {
        let (x2, x22, x223) = self.chain223();
        x223.square_n(23)
            .mul_mod(x22)
            .square_n(6)
            .mul_mod(x2)
            .square_n(2)
            .square()
            == self
    }

    #[cfg(not(target_arch = "spirv"))]
    pub fn to_be_bytes(self) -> [u8; 32] {
        let mut out = [0; 32];
        for (chunk, word) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(self.0.iter().rev())
        {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}
