//! #### PR #42
//! The aux tree: one slot per (token, mode), single SHA-256 nodes.
//!
//! - Slot: `u32le(SHA256(category ‖ mode ‖ u32le(nonce))[0..4]) mod 2^h`.
//!   The category is in the slot and in the leaf, so a commitment for one
//!   token never verifies for another; the nonce moves collisions, so no
//!   category can be ground to collide with a popular one for every nonce.
//! - Node: `SHA256(left ‖ right)`. At level i, bit i of the slot set means
//!   `SHA256(sibling ‖ current)`, otherwise `SHA256(current ‖ sibling)`.
//! - An empty slot is 32 zero bytes, and the depth is the commitment's `h`.
//!
//! The layout (height, nonce and slots) depends only on the entries'
//! categories and modes, never on a payout, so it is searched once per token
//! set; each job then hashes only its leaves and at most 2^h nodes.

use super::{leaf::Mode, sha256, Hash};

/// The tallest tree: 2^16 slots.
pub const MAX_HEIGHT: u8 = 16;
/// Nonces tried at each height: 0..65,536.
pub const NONCES: u32 = 1 << 16;

/// The slot of `(category, mode)` in a tree of height `height` (at most 16).
pub fn slot(category: &Hash, mode: Mode, nonce: u32, height: u8) -> u32 {
    let mut seed = [0; 37];
    seed[..32].copy_from_slice(category);
    seed[32] = mode.byte();
    seed[33..].copy_from_slice(&nonce.to_le_bytes());
    let digest = sha256(&seed);
    let value = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    value & (1u32 << height.min(MAX_HEIGHT)).wrapping_sub(1)
}

/// One entry to place in the tree. When the slots cannot be made distinct,
/// the entry with the lowest priority is dropped (on a tie, the later one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub category: Hash,
    pub mode: Mode,
    pub priority: u8,
}

/// Where each entry sits: the tree's height, the nonce and one slot per
/// entry, in entry order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub height: u8,
    pub nonce: u32,
    pub slots: Vec<u32>,
}

/// No height up to the limit and no nonce gives every entry its own slot;
/// `entry` is the one to drop before searching again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub entry: usize,
}

impl Layout {
    /// Tries heights from `ceil(log2 n)` up to `max_height` (at most 16),
    /// and at each height nonces 0..65,536, and keeps the first layout whose
    /// slots are all distinct: the smallest height first, then the smallest
    /// nonce, so the result is the same on every run.
    pub fn search(entries: &[Placement], max_height: u8) -> Result<Self, Dropped> {
        let lowest = |among: &mut dyn Iterator<Item = usize>| Dropped {
            entry: among
                .min_by_key(|&index| (entries[index].priority, std::cmp::Reverse(index)))
                .unwrap_or(0),
        };
        // The same (category, mode) twice collides at every height and nonce.
        for (index, entry) in entries.iter().enumerate() {
            if let Some(first) = entries[..index]
                .iter()
                .position(|other| other.category == entry.category && other.mode == entry.mode)
            {
                return Err(lowest(&mut [first, index].into_iter()));
            }
        }
        let count = entries.len();
        let min_height = count.max(1).next_power_of_two().trailing_zeros() as u8;
        let mut slots = vec![0; count];
        let mut sorted = vec![0; count];
        for height in min_height..=max_height.min(MAX_HEIGHT) {
            for nonce in 0..NONCES {
                for (slot_of, entry) in slots.iter_mut().zip(entries) {
                    *slot_of = slot(&entry.category, entry.mode, nonce, height);
                }
                sorted.copy_from_slice(&slots);
                sorted.sort_unstable();
                if sorted.windows(2).all(|pair| pair[0] != pair[1]) {
                    return Ok(Self {
                        height,
                        nonce,
                        slots,
                    });
                }
            }
        }
        Err(lowest(&mut (0..count)))
    }
}

/// The whole tree, leaves first, root last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxTree {
    levels: Vec<Vec<Hash>>,
}

fn node(left: &Hash, right: &Hash) -> Hash {
    let mut pair = [0; 64];
    pair[..32].copy_from_slice(left);
    pair[32..].copy_from_slice(right);
    sha256(&pair)
}

impl AuxTree {
    /// Puts `leaves[i]` at `layout.slots[i]`; every other slot is empty.
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

    fn entry(byte: u8, mode: Mode, priority: u8) -> Placement {
        Placement {
            category: [byte; 32],
            mode,
            priority,
        }
    }

    // #### PR #42
    #[test]
    fn slot_follows_the_formula_and_depends_on_nonce_and_mode() {
        let category = [0x5a; 32];
        let mut seed = category.to_vec();
        seed.push(0x41);
        seed.extend(9u32.to_le_bytes());
        let digest = sha256(&seed);
        let value = u32::from_le_bytes(digest[..4].try_into().unwrap());
        for height in 0..=MAX_HEIGHT {
            assert_eq!(
                slot(&category, Mode::ShareTarget, 9, height),
                value % (1 << height),
                "h={height}"
            );
        }
        assert_eq!(slot(&category, Mode::ShareTarget, 9, 0), 0);
        let at = |mode, nonce| slot(&category, mode, nonce, MAX_HEIGHT);
        assert_ne!(at(Mode::ShareTarget, 9), at(Mode::ShareTarget, 10));
        assert_ne!(at(Mode::ShareTarget, 9), at(Mode::BlockRequired, 9));
        assert_ne!(
            at(Mode::ShareTarget, 9),
            slot(&[0x5b; 32], Mode::ShareTarget, 9, 16)
        );
    }

    // #### PR #42
    #[test]
    fn layout_prefers_the_smallest_height_and_is_deterministic() {
        let one = [entry(1, Mode::ShareTarget, 0)];
        let layout = Layout::search(&one, MAX_HEIGHT).unwrap();
        assert_eq!(
            layout,
            Layout {
                height: 0,
                nonce: 0,
                slots: vec![0]
            }
        );
        let tree = AuxTree::build(&layout, &[[0x77; 32]]);
        assert_eq!(tree.root(), [0x77; 32]);
        assert_eq!(tree.height(), 0);
        assert!(tree.branch(0).is_empty());
        let eight: Vec<_> = (1..=8).map(|b| entry(b, Mode::ShareTarget, 0)).collect();
        let layout = Layout::search(&eight, MAX_HEIGHT).unwrap();
        assert!((3..=4).contains(&layout.height), "{layout:?}");
        assert_eq!(Layout::search(&eight, MAX_HEIGHT).unwrap(), layout);
        let mut slots = layout.slots.clone();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 8);
        // No smaller height has a nonce, and no smaller nonce at this height.
        let distinct = |height, nonce| {
            let mut slots: Vec<_> = eight
                .iter()
                .map(|e| slot(&e.category, e.mode, nonce, height))
                .collect();
            slots.sort_unstable();
            slots.windows(2).all(|pair| pair[0] != pair[1])
        };
        assert!((0..layout.nonce).all(|nonce| !distinct(layout.height, nonce)));
        if layout.height == 4 {
            assert!((0..NONCES).all(|nonce| !distinct(3, nonce)));
        }
    }

    // #### PR #42
    #[test]
    fn every_branch_folds_to_the_root() {
        for count in [1u8, 2, 3, 5, 8] {
            let entries: Vec<_> = (0..count)
                .map(|b| entry(b, Mode::BlockRequired, 0))
                .collect();
            let layout = Layout::search(&entries, MAX_HEIGHT).unwrap();
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
        // Empty slots are zero leaves: a two-slot tree with one leaf.
        let layout = Layout {
            height: 1,
            nonce: 0,
            slots: vec![1],
        };
        let tree = AuxTree::build(&layout, &[[9; 32]]);
        assert_eq!(tree.root(), node(&[0; 32], &[9; 32]));
        assert_eq!(tree.branch(1), vec![[0; 32]]);
    }

    // #### PR #42
    #[test]
    fn colliding_tokens_drop_the_lowest_priority() {
        // Three entries cannot fit a height-1 tree: the lowest priority goes.
        let entries = [
            entry(1, Mode::ShareTarget, 5),
            entry(2, Mode::ShareTarget, 1),
            entry(3, Mode::BlockRequired, 9),
        ];
        assert_eq!(Layout::search(&entries, 1), Err(Dropped { entry: 1 }));
        assert!(Layout::search(&[entries[0], entries[2]], 1).is_ok());
        // On equal priorities the later entry goes.
        let tied = [
            entry(1, Mode::ShareTarget, 3),
            entry(2, Mode::ShareTarget, 3),
        ];
        assert_eq!(Layout::search(&tied, 0), Err(Dropped { entry: 1 }));
        // The same token and mode twice always collide; the lower one goes.
        let twice = [
            entry(4, Mode::ShareTarget, 8),
            entry(1, Mode::ShareTarget, 0),
            entry(4, Mode::ShareTarget, 2),
        ];
        assert_eq!(
            Layout::search(&twice, MAX_HEIGHT),
            Err(Dropped { entry: 2 })
        );
        // One token in both modes takes two slots.
        let both = [
            entry(4, Mode::ShareTarget, 0),
            entry(4, Mode::BlockRequired, 0),
        ];
        assert_eq!(Layout::search(&both, MAX_HEIGHT).unwrap().height, 1);
    }
}
