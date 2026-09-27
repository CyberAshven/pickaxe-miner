//! Canonical field elements modulo 2^256 - 2^32 - 977.
//! Variable-time mining arithmetic; not suitable for general wallet signing.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Field(pub [u32; 8]);

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
    let mut borrow = 0;
    for i in 0..8 {
        let value = (a[i] as u64).wrapping_sub(b[i] as u64 + borrow);
        result[i] = value as u32;
        borrow = value >> 63;
    }
    (result, borrow as u32)
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
        let mut carry = 0u64;
        for (i, word) in words.iter_mut().enumerate() {
            carry += self.0[i] as u64 + other.0[i] as u64;
            *word = carry as u32;
            carry >>= 32;
        }
        let (reduced, borrow) = subtract(words, P);
        Self(if carry != 0 || borrow == 0 {
            reduced
        } else {
            words
        })
    }

    #[inline(always)]
    pub fn sub_mod(self, other: Self) -> Self {
        let (mut words, borrow) = subtract(self.0, other.0);
        let mask = 0u32.wrapping_sub(borrow);
        let mut carry = 0u64;
        for (i, word) in words.iter_mut().enumerate() {
            carry += *word as u64 + (P[i] & mask) as u64;
            *word = carry as u32;
            carry >>= 32;
        }
        Self(words)
    }

    #[inline(always)]
    fn reduce_wide(t: [u32; 16]) -> Self {
        let mut words = [0; 8];
        let mut carry = t[0] as u64 + t[8] as u64 * 977;
        words[0] = carry as u32;
        carry >>= 32;
        for i in 1..8 {
            carry += t[i] as u64 + t[i + 8] as u64 * 977 + t[i + 7] as u64;
            words[i] = carry as u32;
            carry >>= 32;
        }
        let top = carry + t[15] as u64;
        carry = words[0] as u64 + top * 977;
        words[0] = carry as u32;
        carry = (carry >> 32) + words[1] as u64 + top;
        words[1] = carry as u32;
        carry >>= 32;
        for word in &mut words[2..] {
            carry += *word as u64;
            *word = carry as u32;
            carry >>= 32;
        }
        let overflow = carry;
        carry = words[0] as u64 + overflow * 977;
        words[0] = carry as u32;
        carry = (carry >> 32) + words[1] as u64 + overflow;
        words[1] = carry as u32;
        carry >>= 32;
        for word in &mut words[2..] {
            carry += *word as u64;
            *word = carry as u32;
            carry >>= 32;
        }
        Self::reduced(words)
    }

    #[inline(always)]
    pub fn mul_mod(self, other: Self) -> Self {
        let mut wide = [0; 16];
        let mut carry = 0u64;
        // Explicit columns keep GPU code in registers: each index is constant.
        macro_rules! column {
            ($column:literal; $($i:literal),*) => {{
                let mut low = carry;
                let mut high = 0u64;
                $(let product = self.0[$i] as u64 * other.0[$column - $i] as u64;
                  low += product as u32 as u64;
                  high += product >> 32;)*
                wide[$column] = low as u32;
                carry = (low >> 32) + high;
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
        wide[15] = carry as u32;
        Self::reduce_wide(wide)
    }

    #[inline(always)]
    pub fn square(self) -> Self {
        let mut wide = [0; 16];
        let mut carry = 0u64;
        macro_rules! column {
            ($column:literal; $($i:literal),*) => {{
                let mut low = 0u64;
                let mut high = 0u64;
                $(let product = self.0[$i] as u64 * self.0[$column - $i] as u64;
                  low += product as u32 as u64;
                  high += product >> 32;)*
                low *= 2;
                high *= 2;
                if $column % 2 == 0 {
                    let diagonal = self.0[$column / 2] as u64 * self.0[$column / 2] as u64;
                    low += diagonal as u32 as u64;
                    high += diagonal >> 32;
                }
                low += carry;
                wide[$column] = low as u32;
                carry = (low >> 32) + high;
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
        wide[15] = carry as u32;
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

    pub fn to_be_bytes(self) -> [u8; 32] {
        let mut out = [0; 32];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.0.iter().rev()) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}
