//! #### PR #42, merge-mining v2
//! The aux tree: one slot per token category, domain-separated SHA-256.
//!
//! - Slot: `u32le(SHA256(0x02 ‖ salt_le4 ‖ category32)[0..4]) mod 2^h`.
//!   The category is in the slot and in the leaf, so a commitment for one
//!   token never verifies for another; the salt moves collisions.
//! - Leaf hash: `SHA256(0x00 ‖ preimage)` (see [`super::leaf`]).
//! - Node: `SHA256(0x01 ‖ left ‖ right)`. At level i, bit i of the slot set
//!   means the current node is the right child, `SHA256(0x01 ‖ sibling ‖
//!   current)`; otherwise `SHA256(0x01 ‖ current ‖ sibling)`.
//! - An empty leaf slot is 32 zero bytes. Internal nodes are always hashed
//!   from their children, never replaced by zero bytes. The depth is the
//!   commitment's `h`, at most 16.
//!
//! The builder starts at the smallest `h` that holds the leaves, tries salts
//! 0..65,536 at each `h`, then the next `h`; past `h = 16` it is an error.
//! The layout depends only on the categories, never on a payout, so it is
//! searched once per token set.

use super::{sha256, Hash};

/// The tallest tree: 2^16 slots.
pub const MAX_HEIGHT: u8 = 16;
/// At most this many leaves (tokens) in one coinbase.
pub const MAX_LEAVES: usize = 16;
/// Salts the builder tries at each height: 0..65,536. A verifier accepts
/// any salt.
pub const SALTS: u32 = 1 << 16;
/// The domain byte of a node.
pub const NODE_PREFIX: u8 = 0x01;
/// The domain byte of a slot.
pub const SLOT_PREFIX: u8 = 0x02;

/// The slot of `category` in a tree of height `height` (at most 16).
pub fn slot(category: &Hash, salt: u32, height: u8) -> u32 {
    let mut seed = [0; 37];
    seed[0] = SLOT_PREFIX;
    seed[1..5].copy_from_slice(&salt.to_le_bytes());
    seed[5..].copy_from_slice(category);
    let digest = sha256(&seed);
    let value = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    value & (1u32 << height.min(MAX_HEIGHT)).wrapping_sub(1)
}

/// `SHA256(0x01 ‖ left ‖ right)`.
pub fn node(left: &Hash, right: &Hash) -> Hash {
    let mut pair = [0; 65];
    pair[0] = NODE_PREFIX;
    pair[1..33].copy_from_slice(left);
    pair[33..].copy_from_slice(right);
    sha256(&pair)
}

/// Where each leaf sits: the tree's height, the salt and one slot per
/// category, in the order given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub height: u8,
    pub salt: u32,
    pub slots: Vec<u32>,
}

/// Why no layout holds the categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// One category twice: one leaf per token category per coinbase.
    Duplicate,
    /// More than 16 tokens.
    TooMany,
    /// No salt gives every category its own slot at any height up to the
    /// limit.
    NoSalt,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Duplicate => "a token category is listed twice",
            Self::TooMany => "at most 16 tokens per coinbase",
            Self::NoSalt => "no salt gives every token its own slot",
        })
    }
}

impl Layout {
    /// Tries heights from the smallest that holds every leaf up to
    /// `max_height` (at most 16), and at each height salts 0..65,536, and
    /// keeps the first layout whose slots are all distinct: the smallest
    /// height first, then the smallest salt, so the result is the same on
    /// every run and in every implementation.
    pub fn search(categories: &[Hash], max_height: u8) -> Result<Self, LayoutError> {
        if categories.len() > MAX_LEAVES {
            return Err(LayoutError::TooMany);
        }
        if categories
            .iter()
            .enumerate()
            .any(|(index, category)| categories[..index].contains(category))
        {
            return Err(LayoutError::Duplicate);
        }
        let count = categories.len();
        let min_height = count.max(1).next_power_of_two().trailing_zeros() as u8;
        let mut slots = vec![0; count];
        for height in min_height..=max_height.min(MAX_HEIGHT) {
            // A salt is dropped at its first collision, so a height that
            // cannot hold the leaves costs a few hashes per salt.
            'salts: for salt in 0..SALTS {
                for (index, category) in categories.iter().enumerate() {
                    let placed = slot(category, salt, height);
                    if slots[..index].contains(&placed) {
                        continue 'salts;
                    }
                    slots[index] = placed;
                }
                return Ok(Self {
                    height,
                    salt,
                    slots,
                });
            }
        }
        Err(LayoutError::NoSalt)
    }
}

/// The whole tree, leaves first, root last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxTree {
    levels: Vec<Vec<Hash>>,
}

impl AuxTree {
    /// Puts `leaves[i]` at `layout.slots[i]`; every other leaf slot is 32
    /// zero bytes, and every node above is hashed from its children.
    pub fn build(layout: &Layout, leaves: &[Hash]) -> Self {
        let mut level = vec![[0; 32]; 1usize << layout.height.min(MAX_HEIGHT)];
        for (slot, leaf) in layout.slots.iter().zip(leaves) {
            if let Some(cell) = level.get_mut(*slot as usize) {
                *cell = *leaf;
            }
        }
        let mut levels = vec![level];
        while let Some(last) = levels.last().filter(|level| level.len() > 1) {
            let next = last
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| node(&pair[0], &pair[1]))
                .collect();
            levels.push(next);
        }
        Self { levels }
    }

    pub fn height(&self) -> u8 {
        (self.levels.len() - 1) as u8
    }

    pub fn root(&self) -> Hash {
        self.levels.last().map_or([0; 32], |level| level[0])
    }

    /// The siblings from the leaf at `slot` up to the root.
    pub fn branch(&self, slot: u32) -> Vec<Hash> {
        let mut index = slot as usize;
        self.levels[..self.levels.len() - 1]
            .iter()
            .map(|level| {
                let sibling = level[(index ^ 1).min(level.len() - 1)];
                index >>= 1;
                sibling
            })
            .collect()
    }
}

/// Folds `leaf` at `slot` up `branch`, as a covenant does: the root.
pub fn fold(leaf: Hash, slot: u32, branch: &[Hash]) -> Hash {
    let mut current = leaf;
    let mut index = slot;
    for sibling in branch {
        current = if index & 1 == 1 {
            node(sibling, &current)
        } else {
            node(&current, sibling)
        };
        index >>= 1;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    // #### merge-mining v2
    #[test]
    fn slot_and_node_follow_the_formulas() {
        let category = [0x5a; 32];
        let mut seed = vec![0x02];
        seed.extend(9u32.to_le_bytes());
        seed.extend(category);
        let digest = sha256(&seed);
        let value = u32::from_le_bytes(digest[..4].try_into().unwrap());
        for height in 0..=MAX_HEIGHT {
            assert_eq!(
                slot(&category, 9, height),
                value % (1 << height),
                "h={height}"
            );
        }
        assert_eq!(slot(&category, 9, 0), 0);
        assert_ne!(slot(&category, 9, 16), slot(&category, 10, 16));
        assert_ne!(slot(&category, 9, 16), slot(&[0x5b; 32], 9, 16));
        let mut pair = vec![0x01];
        pair.extend([1; 32]);
        pair.extend([2; 32]);
        assert_eq!(node(&[1; 32], &[2; 32]), sha256(&pair));
    }

    // #### merge-mining v2
    #[test]
    fn layout_prefers_the_smallest_height_then_salt_and_is_deterministic() {
        let one = [[1; 32]];
        let layout = Layout::search(&one, MAX_HEIGHT).unwrap();
        assert_eq!(
            layout,
            Layout {
                height: 0,
                salt: 0,
                slots: vec![0]
            }
        );
        let tree = AuxTree::build(&layout, &[[0x77; 32]]);
        assert_eq!(tree.root(), [0x77; 32]);
        assert_eq!(tree.height(), 0);
        assert!(tree.branch(0).is_empty());
        let eight: Vec<Hash> = (1..=8).map(|b| [b; 32]).collect();
        let layout = Layout::search(&eight, MAX_HEIGHT).unwrap();
        assert!((3..=4).contains(&layout.height), "{layout:?}");
        assert_eq!(Layout::search(&eight, MAX_HEIGHT).unwrap(), layout);
        let distinct = |height, salt| {
            let mut slots: Vec<_> = eight.iter().map(|c| slot(c, salt, height)).collect();
            slots.sort_unstable();
            slots.windows(2).all(|pair| pair[0] != pair[1])
        };
        assert!(distinct(layout.height, layout.salt));
        assert!((0..layout.salt).all(|salt| !distinct(layout.height, salt)));
        if layout.height == 4 {
            assert!((0..SALTS).all(|salt| !distinct(3, salt)));
        }
        // No category: the empty layout (its tree's root is zero).
        let empty = Layout::search(&[], MAX_HEIGHT).unwrap();
        assert_eq!((empty.height, empty.salt), (0, 0));
        assert_eq!(AuxTree::build(&empty, &[]).root(), [0; 32]);
    }

    // #### merge-mining v2
    #[test]
    fn layouts_refuse_duplicates_too_many_and_no_room() {
        assert_eq!(
            Layout::search(&[[1; 32], [2; 32], [1; 32]], MAX_HEIGHT),
            Err(LayoutError::Duplicate)
        );
        let many: Vec<Hash> = (0..17).map(|b| [b; 32]).collect();
        assert_eq!(Layout::search(&many, MAX_HEIGHT), Err(LayoutError::TooMany));
        assert!(Layout::search(&many[..16], MAX_HEIGHT).is_ok());
        // Three leaves never fit a tree of height 1.
        assert_eq!(Layout::search(&many[..3], 1), Err(LayoutError::NoSalt));
    }

    // #### merge-mining v2
    #[test]
    fn every_branch_folds_to_the_root() {
        for count in [1u8, 2, 3, 5, 8] {
            let categories: Vec<Hash> = (0..count).map(|b| [b; 32]).collect();
            let layout = Layout::search(&categories, MAX_HEIGHT).unwrap();
            let leaves: Vec<Hash> = (0..count).map(|b| [b.wrapping_add(100); 32]).collect();
            let tree = AuxTree::build(&layout, &leaves);
            assert_eq!(tree.height(), layout.height);
            for (slot, leaf) in layout.slots.iter().zip(&leaves) {
                let branch = tree.branch(*slot);
                assert_eq!(branch.len(), usize::from(layout.height));
                assert_eq!(fold(*leaf, *slot, &branch), tree.root());
                if layout.height > 0 {
                    // The wrong slot or a changed sibling misses the root.
                    assert_ne!(fold(*leaf, *slot ^ 1, &branch), tree.root());
                    let mut bad = branch.clone();
                    bad[0][0] ^= 1;
                    assert_ne!(fold(*leaf, *slot, &bad), tree.root());
                }
            }
        }
        // Empty leaf slots are zero bytes, but nodes above them are hashed:
        // a four-slot tree with one leaf at slot 2.
        let layout = Layout {
            height: 2,
            salt: 0,
            slots: vec![2],
        };
        let tree = AuxTree::build(&layout, &[[9; 32]]);
        let empty_pair = node(&[0; 32], &[0; 32]);
        assert_eq!(tree.root(), node(&empty_pair, &node(&[9; 32], &[0; 32])));
        assert_eq!(tree.branch(2), vec![[0; 32], empty_pair]);
    }
}
