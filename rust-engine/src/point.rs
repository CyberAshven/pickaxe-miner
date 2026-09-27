use crate::field::Field;

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: Field,
    pub y: Field,
    pub z: Field,
}

impl Point {
    pub const INFINITY: Self = Self {
        x: Field::ZERO,
        y: Field::ZERO,
        z: Field::ZERO,
    };
    pub const GENERATOR: Self = Self {
        x: Field([
            0x16f81798, 0x59f2815b, 0x2dce28d9, 0x029bfcdb, 0xce870b07, 0x55a06295, 0xf9dcbbac,
            0x79be667e,
        ]),
        y: Field([
            0xfb10d4b8, 0x9c47d08f, 0xa6855419, 0xfd17b448, 0x0e1108a8, 0x5da4fbfc, 0x26a3c465,
            0x483ada77,
        ]),
        z: Field::ONE,
    };

    pub fn double(self) -> Self {
        if self.z == Field::ZERO || self.y == Field::ZERO {
            return Self::INFINITY;
        }
        let yy = self.y.square();
        let s = self.x.mul_mod(yy);
        let s = s.add_mod(s);
        let s = s.add_mod(s);
        let xx = self.x.square();
        let m = xx.add_mod(xx).add_mod(xx);
        let x = m.square().sub_mod(s.add_mod(s));
        let yyyy = yy.square();
        let yyyy = yyyy.add_mod(yyyy);
        let yyyy = yyyy.add_mod(yyyy);
        let yyyy = yyyy.add_mod(yyyy);
        let yz = self.y.mul_mod(self.z);
        Self {
            x,
            y: m.mul_mod(s.sub_mod(x)).sub_mod(yyyy),
            z: yz.add_mod(yz),
        }
    }

    /// Adds a validated affine point. Infinity is represented only by z = 0.
    pub fn add_affine(self, x: Field, y: Field) -> Self {
        if self.z == Field::ZERO {
            return Self {
                x,
                y,
                z: Field::ONE,
            };
        }
        let zz = self.z.square();
        let h = x.mul_mod(zz).sub_mod(self.x);
        let r = y.mul_mod(zz.mul_mod(self.z)).sub_mod(self.y);
        if h == Field::ZERO {
            return if r == Field::ZERO {
                self.double()
            } else {
                Self::INFINITY
            };
        }
        let hh = h.square();
        let hhh = hh.mul_mod(h);
        let v = self.x.mul_mod(hh);
        let x = r.square().sub_mod(hhh).sub_mod(v.add_mod(v));
        Self {
            x,
            y: r.mul_mod(v.sub_mod(x)).sub_mod(self.y.mul_mod(hhh)),
            z: self.z.mul_mod(h),
        }
    }

    /// Variable-time generator multiplication for public search scalars/tests.
    pub fn generator_mul(scalar: [u8; 32]) -> Self {
        let mut point = Self::INFINITY;
        for byte in scalar {
            for bit in (0..8).rev() {
                point = point.double();
                if byte & (1 << bit) != 0 {
                    point = point.add_affine(Self::GENERATOR.x, Self::GENERATOR.y);
                }
            }
        }
        point
    }

    pub fn affine(self) -> Option<(Field, Field)> {
        if self.z == Field::ZERO {
            return None;
        }
        let inverse = self.z.inverse();
        let squared = inverse.square();
        Some((
            self.x.mul_mod(squared),
            self.y.mul_mod(squared.mul_mod(inverse)),
        ))
    }
}
