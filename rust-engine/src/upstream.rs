//! Pickaxe's buffer/point adapter. Arithmetic lives in the upstream MIT crate.
use ufsecp_core::{AffinePoint, JacobianPoint};
pub use ufsecp_core::{FieldElement as Field, Scalar};

#[derive(Clone, Copy)]
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
    pub fn add_affine(self, x: Field, y: Field) -> Self {
        let point = JacobianPoint {
            x: self.x,
            y: self.y,
            z: self.z,
            infinity: self.z.is_zero(),
        }
        .add_mixed(AffinePoint { x, y });
        if point.infinity {
            Self::INFINITY
        } else {
            Self {
                x: point.x,
                y: point.y,
                z: point.z,
            }
        }
    }
}

pub fn limbs(words: [u32; 8]) -> [u64; 4] {
    core::array::from_fn(|i| words[i * 2] as u64 | ((words[i * 2 + 1] as u64) << 32))
}
