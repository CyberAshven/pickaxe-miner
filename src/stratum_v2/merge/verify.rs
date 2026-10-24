//! #### PR #42, merge-mining v2
//! The Rust reference verifier: step for step what a token covenant checks,
//! run on every proof before it is journaled as claimable (fail closed).
//!
//! 1. Decode: the bytes are exactly one canonical `AuxProof` v2.
//! 2. Rebuild the leaf from the claim: the category, `HASH256` of the
//!    baton's locking bytecode, the token's state hash, `HASH256` of the
//!    payout output's locking bytecode, the token's `target_bits` and `ext`.
//! 3. The slot: `u32le(SHA256(0x02 ‖ salt ‖ category)[0..4]) mod 2^h`, with
//!    `h` at most the token's limit (and at most 16).
//! 4. Fold the aux branch from the leaf hash (`SHA256(0x00 ‖ leaf)`) with
//!    `SHA256(0x01 ‖ left ‖ right)`: the root.
//! 5. Rebuild output 0 from the root, `h` and the salt.
//! 6. The coinbase head's shape: `len == 47 + cb_head[41]`, `cb_head[41]`
//!    at most 100, `cb_head[4..41] == 01 ‖ 00 x 32 ‖ ffffffff`, and its last
//!    byte (the output count) 1..=0xfc; the tail at least 4 bytes.
//! 7. `txid = HASH256(cb_head ‖ output 0 ‖ cb_tail)`.
//! 8. Fold the coinbase branch at index 0 (`HASH256(current ‖ sibling)`):
//!    the header's merkle root (`header[36..68]`).
//! 9. The work: `HASH256(header)`, read as an unsigned little-endian
//!    number, is at most the token's expanded `target_bits` (canonical
//!    compact: `3 <= exp <= 32`, `0 < mant < 0x800000`). The target is the
//!    token's, never the header's nBits.

use super::{
    super::template::{double_sha256, meets_target},
    commitment::{check_head, AuxCommitment},
    leaf::Leaf,
    proof::AuxProof,
    tree::{self, MAX_HEIGHT},
    Hash,
};

pub use super::proof::ProofError;

/// What a claim transaction shows a covenant through introspection.
#[derive(Clone, Copy, Debug)]
pub struct ClaimView<'a> {
    /// The token category.
    pub category: Hash,
    /// `HASH256` of the token baton's locking bytecode.
    pub lock_hash: Hash,
    /// The token's current state hash.
    pub state_hash: Hash,
    /// The payout output's locking bytecode: who the claim pays.
    pub payout: &'a [u8],
    /// The token's current compact target.
    pub target_bits: u32,
    pub ext: Hash,
    /// The covenant's largest aux tree height (at most 16).
    pub max_aux_height: u8,
}

impl ClaimView<'_> {
    /// Step 2: the leaf this claim's covenant builds.
    pub fn leaf(&self) -> Leaf {
        Leaf::new(
            self.category,
            self.lock_hash,
            self.state_hash,
            self.payout,
            self.target_bits,
            self.ext,
        )
    }
}

/// Expands a token's compact target, little-endian. Stricter than a
/// header's nBits: `3 <= exp <= 32` and `0 < mant < 0x800000` (no sign bit,
/// no zero mantissa), so a covenant expands it with one shift.
pub fn expand_compact(bits: u32) -> Option<Hash> {
    let exp = (bits >> 24) as usize;
    let mant = bits & 0x00ff_ffff;
    if !(3..=32).contains(&exp) || mant == 0 || mant >= 0x0080_0000 {
        return None;
    }
    let mut target = [0; 32];
    target[exp - 3..exp].copy_from_slice(&mant.to_le_bytes()[..3]);
    Some(target)
}

/// Steps 1-9 on a proof's bytes; the decoded proof when it verifies.
pub fn verify(bytes: &[u8], view: &ClaimView) -> Result<AuxProof, ProofError> {
    let proof = AuxProof::from_bytes(bytes)?;
    verify_proof(&proof, view)?;
    Ok(proof)
}

/// Steps 2-9 on a decoded proof.
pub fn verify_proof(proof: &AuxProof, view: &ClaimView) -> Result<(), ProofError> {
    // 2. The leaf, from the claim alone.
    verify_leaf(proof, &view.leaf(), view.max_aux_height)
}

/// #### merge-mining v2: steps 3-9 for a leaf a covenant has rebuilt (its
/// `payout_hash` an unlocking argument, as BCH P2Pool's SAFA covenant reads
/// it).
pub fn verify_leaf(proof: &AuxProof, leaf: &Leaf, max_aux_height: u8) -> Result<(), ProofError> {
    proof.check_shape()?;
    let target = expand_compact(leaf.target_bits).ok_or(ProofError::Target)?;
    // 3. The slot, at a height the token allows.
    if proof.height > max_aux_height.min(MAX_HEIGHT) {
        return Err(ProofError::Height);
    }
    let slot = tree::slot(&leaf.category, proof.salt, proof.height);
    // 4. The root.
    let root = tree::fold(leaf.hash(), slot, &proof.aux_branch);
    // 5. Output 0.
    let output = AuxCommitment {
        root,
        height: proof.height,
        salt: proof.salt,
    }
    .output();
    // 6. The head's shape and the tail's locktime.
    check_head(&proof.cb_head).map_err(|_| ProofError::CoinbaseHead)?;
    if proof.cb_tail.len() < 4 {
        return Err(ProofError::CoinbaseTail);
    }
    // 7. The coinbase's txid.
    let mut coinbase = Vec::with_capacity(proof.cb_head.len() + output.len() + proof.cb_tail.len());
    coinbase.extend_from_slice(&proof.cb_head);
    coinbase.extend_from_slice(&output);
    coinbase.extend_from_slice(&proof.cb_tail);
    let mut merkle = double_sha256(&coinbase);
    // 8. Up the coinbase branch at index 0, to the header's merkle root.
    for sibling in &proof.cb_branch {
        let mut pair = [0; 64];
        pair[..32].copy_from_slice(&merkle);
        pair[32..].copy_from_slice(sibling);
        merkle = double_sha256(&pair);
    }
    if proof.header[36..68] != merkle {
        return Err(ProofError::MerkleRoot);
    }
    // 9. The work, against the token's target.
    if !meets_target(&double_sha256(&proof.header), &target) {
        return Err(ProofError::Work);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        config::MiningNetwork,
        donation::bch::BchPayout,
        stratum_v2::{
            merge::{
                commitment::OUTPUT_LEN,
                proof::assemble,
                registry::TEST_TOKEN,
                set::{
                    tests::{test_set, test_state},
                    AuxJob,
                },
                tree::{AuxTree, Layout},
            },
            payout,
            template::{compact_target, BchTemplate},
            template_tests::{payout as miner, rpc_template, transaction},
        },
    };
    use serde_json::json;
    use std::sync::Arc;

    const NETWORK: MiningNetwork = MiningNetwork::Chipnet;
    const EXTRANONCE: [u8; 28] = [7; 28];

    struct Mined {
        template: BchTemplate,
        job: AuxJob,
        coinbase: Vec<u8>,
        branch: Vec<Hash>,
        header: [u8; 80],
        scripts: Vec<Vec<u8>>,
    }

    /// A header over `coinbase` and `branch` that meets the test token's
    /// target (about one in 4,096).
    fn mine(template: &BchTemplate, coinbase: &[u8], branch: &[Hash]) -> [u8; 80] {
        let mut root = double_sha256(coinbase);
        for sibling in branch {
            root = double_sha256(&[&root[..], &sibling[..]].concat());
        }
        let mut header = [0; 80];
        header[..4].copy_from_slice(&template.version.to_le_bytes());
        header[4..36].copy_from_slice(&template.previous_hash);
        header[36..68].copy_from_slice(&root);
        header[68..72].copy_from_slice(&template.current_time.to_le_bytes());
        header[72..76].copy_from_slice(&template.bits.to_le_bytes());
        let target = test_state().target;
        let nonce = (0..u32::MAX)
            .find(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                meets_target(&double_sha256(&header), &target)
            })
            .unwrap();
        header[76..].copy_from_slice(&nonce.to_le_bytes());
        header
    }

    /// A template with three transactions and the test token, one
    /// extended-channel coinbase and a header that wins the token.
    fn mined() -> Mined {
        let mut raw = rpc_template();
        let mut txs = vec![transaction(1), transaction(2), transaction(3)];
        txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
        raw["transactions"] = json!(txs);
        let mut template = BchTemplate::from_rpc(&raw).unwrap();
        template.commit(Arc::new(test_set()));
        let policy = BchPayout::default();
        let job = template
            .aux_job(NETWORK, &miner(), None, policy)
            .unwrap()
            .unwrap();
        let coinbase = template
            .coinbase_with_aux(
                NETWORK,
                &miner(),
                None,
                &EXTRANONCE,
                policy,
                Some(&job.output),
            )
            .unwrap()
            .bytes;
        let branch = template
            .coinbase_parts_with_aux(NETWORK, &miner(), None, 28, policy, Some(&job.output))
            .unwrap()
            .merkle_path;
        let header = mine(&template, &coinbase, &branch);
        Mined {
            scripts: payout::scripts(NETWORK, &miner(), None).unwrap(),
            template,
            job,
            coinbase,
            branch,
            header,
        }
    }

    pub(crate) fn view(scripts: &[Vec<u8>]) -> ClaimView<'_> {
        let state = test_state();
        ClaimView {
            category: TEST_TOKEN.category,
            lock_hash: state.lock_hash,
            state_hash: state.state_hash,
            payout: &scripts[0],
            target_bits: state.target_bits,
            // #### merge-mining v2: the miner job's split, 1% of each win to
            // the donation.
            ext: crate::stratum_v2::merge::split::Split::donation(100, &scripts[1])
                .unwrap()
                .ext(),
            max_aux_height: TEST_TOKEN.max_aux_height,
        }
    }

    fn proof(mined: &Mined) -> AuxProof {
        assemble(&mined.job, 0, &mined.coinbase, &mined.branch, mined.header).unwrap()
    }

    // #### merge-mining v2
    #[test]
    fn a_built_proof_verifies() {
        let mined = mined();
        assert_eq!(mined.branch.len(), 2);
        let proof = proof(&mined);
        let view = view(&mined.scripts);
        verify_proof(&proof, &view).unwrap();
        // Through the journal's bytes and back.
        let bytes = proof.to_bytes().unwrap();
        let head = 47 + usize::from(mined.coinbase[41]);
        assert_eq!(
            bytes.len(),
            95 + head + (mined.coinbase.len() - head - OUTPUT_LEN) + 64
        );
        assert_eq!(verify(&bytes, &view).unwrap(), proof);
        // The block itself is whole: the template accepts the header.
        let coinbase = mined
            .template
            .coinbase_with_aux(
                NETWORK,
                &miner(),
                None,
                &EXTRANONCE,
                BchPayout::default(),
                Some(&mined.job.output),
            )
            .unwrap();
        assert!(mined.template.block(&coinbase, mined.header).is_ok());
        // A coinbase without this job's commitment gives no proof.
        let plain = mined
            .template
            .coinbase_with_aux(
                NETWORK,
                &miner(),
                None,
                &EXTRANONCE,
                BchPayout::default(),
                None,
            )
            .unwrap();
        assert_eq!(
            assemble(&mined.job, 0, &plain.bytes, &mined.branch, mined.header),
            Err(ProofError::NotInJob)
        );
        assert_eq!(
            assemble(&mined.job, 1, &mined.coinbase, &mined.branch, mined.header),
            Err(ProofError::NotInJob)
        );
    }

    // #### merge-mining v2
    #[test]
    fn every_mutation_fails() {
        let mined = mined();
        let a = proof(&mined);
        let view = view(&mined.scripts);
        let check = |proof: &AuxProof| verify_proof(proof, &view);
        let with = |change: &dyn Fn(&mut AuxProof)| {
            let mut proof = a.clone();
            change(&mut proof);
            proof
        };
        // Flipped bytes: cb_head (in the script), cb_tail, the coinbase
        // branch, the header (in its version and in its merkle root), the
        // aux branch, the salt.
        assert_eq!(
            check(&with(&|p| p.cb_head[50] ^= 1)),
            Err(ProofError::MerkleRoot)
        );
        assert_eq!(
            check(&with(&|p| p.cb_tail[0] ^= 1)),
            Err(ProofError::MerkleRoot)
        );
        assert_eq!(
            check(&with(&|p| p.cb_branch[1][3] ^= 1)),
            Err(ProofError::MerkleRoot)
        );
        assert_eq!(check(&with(&|p| p.header[0] ^= 1)), Err(ProofError::Work));
        assert_eq!(
            check(&with(&|p| p.header[40] ^= 1)),
            Err(ProofError::MerkleRoot)
        );
        assert_eq!(check(&with(&|p| p.salt ^= 1)), Err(ProofError::MerkleRoot));
        // The coinbase head's fixed fields: the script length (L > 100, and
        // L that disagrees with the length), a non-null prevout, the output
        // count (0 and 0xfd and above).
        let mut long = a.clone();
        long.cb_head.splice(42..42, [0; 69]);
        long.cb_head[41] = 101;
        assert_eq!(long.cb_head.len(), 47 + 101);
        assert_eq!(check(&long), Err(ProofError::CoinbaseHead));
        assert_eq!(
            check(&with(&|p| p.cb_head[41] += 1)),
            Err(ProofError::CoinbaseHead)
        );
        assert_eq!(
            check(&with(&|p| p.cb_head[10] ^= 1)),
            Err(ProofError::CoinbaseHead)
        );
        assert_eq!(
            check(&with(&|p| p.cb_head[38] = 0)),
            Err(ProofError::CoinbaseHead)
        );
        for count in [0, 0xfd, 0xff] {
            assert_eq!(
                check(&with(&|p| *p.cb_head.last_mut().unwrap() = count)),
                Err(ProofError::CoinbaseHead)
            );
        }
        assert_eq!(
            check(&with(&|p| p.cb_tail.truncate(3))),
            Err(ProofError::CoinbaseTail)
        );
        // Output 0 moved to output 1, with a header mined over that coinbase.
        let head = a.cb_head.len();
        let miner_output = mined.coinbase[head + OUTPUT_LEN..head + OUTPUT_LEN + 34].to_vec();
        let mut moved_cb = mined.coinbase[..head].to_vec();
        moved_cb.extend(&miner_output);
        moved_cb.extend(&mined.coinbase[head..head + OUTPUT_LEN]);
        moved_cb.extend(&mined.coinbase[head + OUTPUT_LEN + 34..]);
        let moved = with(&|p| {
            p.cb_head.extend(&miner_output);
            p.cb_tail.drain(..34);
            p.header = mine(&mined.template, &moved_cb, &mined.branch);
        });
        assert_eq!(check(&moved), Err(ProofError::CoinbaseHead));
        // The leaf at the wrong slot of a tree the coinbase does commit to.
        let entry = &mined.job.entries[0];
        let wrong = Layout {
            height: 1,
            salt: mined.job.commitment.salt,
            slots: vec![tree::slot(&TEST_TOKEN.category, mined.job.commitment.salt, 1) ^ 1],
        };
        let tree = AuxTree::build(&wrong, &[entry.leaf.hash()]);
        let output = AuxCommitment {
            root: tree.root(),
            height: 1,
            salt: wrong.salt,
        }
        .output();
        let wrong_cb = mined
            .template
            .coinbase_with_aux(
                NETWORK,
                &miner(),
                None,
                &EXTRANONCE,
                BchPayout::default(),
                Some(&output),
            )
            .unwrap()
            .bytes;
        let wrong_slot = with(&|p| {
            p.height = 1;
            p.aux_branch = tree.branch(wrong.slots[0]);
            p.cb_tail = wrong_cb[head + OUTPUT_LEN..].to_vec();
            p.header = mine(&mined.template, &wrong_cb, &mined.branch);
        });
        assert_eq!(check(&wrong_slot), Err(ProofError::MerkleRoot));
        // Another category, lock, state, payout, target or ext.
        let mut other = view;
        other.category[0] ^= 1;
        assert_eq!(verify_proof(&a, &other), Err(ProofError::MerkleRoot));
        let mut other = view;
        other.lock_hash[0] ^= 1;
        assert_eq!(verify_proof(&a, &other), Err(ProofError::MerkleRoot));
        let mut stale = view;
        stale.state_hash[0] ^= 1;
        assert_eq!(verify_proof(&a, &stale), Err(ProofError::MerkleRoot));
        let other = ClaimView {
            payout: &mined.scripts[1],
            ..view
        };
        assert_eq!(verify_proof(&a, &other), Err(ProofError::MerkleRoot));
        let mut other = view;
        other.ext[31] = 1;
        assert_eq!(verify_proof(&a, &other), Err(ProofError::MerkleRoot));
        let easier = ClaimView {
            target_bits: 0x207f_ffff,
            ..view
        };
        assert_eq!(verify_proof(&a, &easier), Err(ProofError::MerkleRoot));
        // A tree taller than the token allows.
        let low = ClaimView {
            max_aux_height: 0,
            ..view
        };
        if a.height > 0 {
            assert_eq!(verify_proof(&a, &low), Err(ProofError::Height));
        }
        assert_eq!(check(&with(&|p| p.height = 2)), Err(ProofError::Shape));
        // Targets a covenant cannot expand: exp < 3, exp > 32, the sign bit,
        // a zero mantissa.
        for target_bits in [0x0200_8000, 0x2100_0001, 0x1f80_0000, 0x1f00_0000] {
            let bad = ClaimView {
                target_bits,
                ..view
            };
            assert_eq!(
                verify_proof(&a, &bad),
                Err(ProofError::Target),
                "{target_bits:08x}"
            );
        }
        // A header that meets its own nBits but not the token's target is no
        // win: the target is the token's.
        let mut header = a.header;
        let token_target = test_state().target;
        (0u32..)
            .find(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                let hash = double_sha256(&header);
                meets_target(&hash, &mined.template.target) && !meets_target(&hash, &token_target)
            })
            .unwrap();
        assert_eq!(check(&with(&|p| p.header = header)), Err(ProofError::Work));
        // Bytes: a branch that is not a multiple of 32, another h.
        let bytes = a.to_bytes().unwrap();
        let mut odd = bytes.clone();
        odd.insert(90, 0);
        assert_eq!(verify(&odd, &view), Err(ProofError::Decode));
        let mut taller = bytes.clone();
        taller[85] += 1;
        assert_eq!(verify(&taller, &view), Err(ProofError::Decode));
    }

    // #### merge-mining v2
    #[test]
    fn token_targets_expand_like_header_targets_within_the_covenant_range() {
        for bits in [
            0x0300_0001,
            0x1d00_ffff,
            0x1f10_0000,
            0x207f_ffff,
            0x2000_8000,
            0x0400_0080,
        ] {
            assert_eq!(
                expand_compact(bits),
                compact_target(bits).ok(),
                "{bits:08x}"
            );
        }
        assert_eq!(expand_compact(0x0300_0001).unwrap()[0], 1);
        let mut top = [0; 32];
        top[29..].copy_from_slice(&[0xff, 0xff, 0x7f]);
        assert_eq!(expand_compact(0x207f_ffff), Some(top));
        for bits in [
            0x0200_0001,
            0x2100_0001,
            0x1d80_0000,
            0x1d00_0000,
            0,
            u32::MAX,
        ] {
            assert_eq!(expand_compact(bits), None, "{bits:08x}");
        }
    }
}
