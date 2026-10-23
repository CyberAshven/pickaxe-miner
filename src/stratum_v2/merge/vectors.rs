//! #### merge-mining v2: golden vectors
//! What: byte-for-byte vectors of the v2 format, built from synthetic data
//! only (no template, payout policy or network), and repeated in
//! docs/merge-mining.md so another implementation (BCH P2Pool's) can check
//! itself: a two-token tree, a whole `AuxProof` v2 for one leaf with its
//! verification, and the empty commitment.
//! Look here if: any v2 layout changes; the docs' vectors must change too.

use super::{
    commitment::{AuxCommitment, NULL_INPUT},
    leaf::{lock_hash, Leaf},
    proof::AuxProof,
    tree::{fold, slot, AuxTree, Layout, MAX_HEIGHT},
    verify::{expand_compact, verify, ClaimView},
    Hash,
};
use crate::stratum_v2::template::{double_sha256, meets_target};

/// P2PKH to twenty 0x12 bytes: the vectors' payout.
fn payout() -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend([0x12; 20]);
    script.extend([0x88, 0xac]);
    script
}

fn token_a() -> Leaf {
    Leaf::new(
        [0x11; 32],
        lock_hash(&[0x51]),
        [0xaa; 32],
        &payout(),
        0x207f_ffff,
        [0; 32],
    )
}

fn token_b() -> Leaf {
    Leaf::new(
        [0x22; 32],
        lock_hash(&[0x52]),
        [0xbb; 32],
        &payout(),
        0x1f00_ffff,
        [0; 32],
    )
}

const LEAF_A: &str = concat!(
    "43544d4c02111111111111111111111111111111111111111111111111111111",
    "1111111111953ccfa596a6c6d39e5980194539124fdcff116a571455a212baed",
    "811f585ee0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "aaaaaaaaaa22650758a42ebfd7306e771ad24c442cad795ed9e549392233729f",
    "647a2b1be5ffff7f200000000000000000000000000000000000000000000000",
    "000000000000000000",
);
const LEAF_A_HASH: &str = "0ab882d24f8c85b880ca7cfb6f60a7773f377222e84ac28afd14f3ce46bf307a";
const LEAF_B_HASH: &str = "9520e74e37e5dffaef19eb2f61da54e502d296c8f1baa3f5dc7db772711def59";
const TWO_TOKEN_ROOT: &str = "5b61ecfe96c714397bc35ed813890f49f2b0287a78111e19d156f4eecd1c38d0";
const TWO_TOKEN_OUTPUT: &str = concat!(
    "00000000000000002c6a2a43544d4d025b61ecfe96c714397bc35ed813890f49",
    "f2b0287a78111e19d156f4eecd1c38d00100000000",
);

#[test]
fn a_two_token_tree() {
    let (a, b) = (token_a(), token_b());
    let layout = Layout::search(&[a.category, b.category], MAX_HEIGHT).unwrap();
    let tree = AuxTree::build(&layout, &[a.hash(), b.hash()]);
    let commitment = AuxCommitment {
        root: tree.root(),
        height: layout.height,
        salt: layout.salt,
    };
    // The smallest height that holds two leaves, and the first salt that
    // parts them.
    assert_eq!(
        layout,
        Layout {
            height: 1,
            salt: 0,
            slots: vec![0, 1]
        }
    );
    for (leaf, slot_of) in [a, b].iter().zip(&layout.slots) {
        assert_eq!(slot(&leaf.category, layout.salt, layout.height), *slot_of);
        let branch = tree.branch(*slot_of);
        assert_eq!(fold(leaf.hash(), *slot_of, &branch), tree.root());
    }
    assert_eq!(hex::encode(a.preimage()), LEAF_A);
    assert_eq!(hex::encode(a.hash()), LEAF_A_HASH);
    assert_eq!(hex::encode(b.hash()), LEAF_B_HASH);
    assert_eq!(hex::encode(tree.root()), TWO_TOKEN_ROOT);
    assert_eq!(hex::encode(commitment.output()), TWO_TOKEN_OUTPUT);
}

/// The synthetic coinbase around output 0: head and tail.
fn coinbase_parts() -> (Vec<u8>, Vec<u8>) {
    let mut head = 2u32.to_le_bytes().to_vec();
    head.extend(NULL_INPUT);
    // L = 12: the BIP34 height 325,909 and eight extranonce bytes.
    let script: Vec<u8> = [&[0x03, 0x15, 0xf9, 0x04][..], &[0x07; 8]].concat();
    head.push(script.len() as u8);
    head.extend(script);
    head.extend(u32::MAX.to_le_bytes());
    head.push(2);
    let mut tail = 312_500_000u64.to_le_bytes().to_vec();
    tail.push(25);
    tail.extend(payout());
    tail.extend(0u32.to_le_bytes());
    (head, tail)
}

const PROOF: &str = concat!(
    "43544d5002000000205555555555555555555555555555555555555555555555",
    "5555555555555555556c933baf900d7d82a08e5689a968f70b77802a116c671c",
    "63c12b9ed534e67a1d00f15365ffff7f200000000000000000003b0002000000",
    "0100000000000000000000000000000000000000000000000000000000000000",
    "00ffffffff0c0315f9040707070707070707ffffffff022600205fa012000000",
    "001976a914121212121212121212121212121212121212121288ac0000000001",
    "4444444444444444444444444444444444444444444444444444444444444444",
);

#[test]
fn a_whole_proof_for_one_leaf_and_its_verification() {
    let leaf = token_a();
    let layout = Layout::search(&[leaf.category], MAX_HEIGHT).unwrap();
    assert_eq!((layout.height, layout.salt), (0, 0));
    let root = AuxTree::build(&layout, &[leaf.hash()]).root();
    let output = AuxCommitment {
        root,
        height: 0,
        salt: 0,
    }
    .output();
    let (head, tail) = coinbase_parts();
    let coinbase = [&head[..], &output[..], &tail[..]].concat();
    let sibling: Hash = [0x44; 32];
    let merkle = double_sha256(&[&double_sha256(&coinbase)[..], &sibling[..]].concat());
    let mut header = [0; 80];
    header[..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
    header[4..36].copy_from_slice(&[0x55; 32]);
    header[36..68].copy_from_slice(&merkle);
    header[68..72].copy_from_slice(&1_700_000_000u32.to_le_bytes());
    header[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
    let target = expand_compact(leaf.target_bits).unwrap();
    let nonce = (0u32..)
        .find(|nonce| {
            header[76..].copy_from_slice(&nonce.to_le_bytes());
            meets_target(&double_sha256(&header), &target)
        })
        .unwrap();
    header[76..].copy_from_slice(&nonce.to_le_bytes());
    let proof = AuxProof {
        header,
        height: 0,
        salt: 0,
        aux_branch: Vec::new(),
        cb_head: head,
        cb_tail: tail,
        cb_branch: vec![sibling],
    };
    let bytes = proof.to_bytes().unwrap();
    assert_eq!(
        hex::encode(double_sha256(&coinbase)),
        "245dbc4c7d02cc87b5a41eb8af9f8aebf36ffa528864177931fb3b3d527f35e9"
    );
    assert_eq!(nonce, 0);
    let payout = payout();
    let view = ClaimView {
        category: leaf.category,
        lock_hash: leaf.lock_hash,
        state_hash: leaf.state_hash,
        payout: &payout,
        target_bits: leaf.target_bits,
        ext: leaf.ext,
        max_aux_height: MAX_HEIGHT,
    };
    assert_eq!(verify(&bytes, &view), Ok(proof));
    assert_eq!(bytes.len(), 224);
    assert_eq!(hex::encode(&bytes), PROOF);
    // Another state hash is another leaf: the same bytes fail.
    let stale = ClaimView {
        state_hash: [0xab; 32],
        ..view
    };
    assert!(verify(&bytes, &stale).is_err());
}

#[test]
fn the_empty_commitment() {
    assert_eq!(
        hex::encode(AuxCommitment::EMPTY.output()),
        concat!(
            "00000000000000002c6a2a43544d4d02",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000"
        )
    );
}
