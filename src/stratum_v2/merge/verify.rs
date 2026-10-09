//! #### PR #42
//! The Rust reference verifier: step for step what a token covenant checks,
//! run on every proof before it is journaled as claimable (fail closed).
//!
//! Case A:
//! 1. The proof is a canonical v1 proof of mode A.
//! 2. Rebuild the leaf from the claim: the category, input 0's outpoint,
//!    `HASH256(output 1)`, the split, the token's `target_bits` and `ext`.
//! 3. Compute the slot and fold the aux branch (`h` levels, `h` at most the
//!    token's limit): the aux root.
//! 4. Rebuild output 0 from the aux root, `h` and the nonce.
//! 5. `cb_head`: `len == 47 + cb_head[41]`, `cb_head[41] <= 100`,
//!    `cb_head[4..41] == 01 ‖ 00 x 32 ‖ ffffffff`, and its last byte (the
//!    output count) is 1..0xfc.
//! 6. `txid = HASH256(cb_head ‖ output 0 ‖ cb_tail)`, `cb_tail` at least 4
//!    bytes.
//! 7. Fold the coinbase branch at index 0: the merkle root, which must be
//!    the header's.
//! 8. `HASH256(header)`, read as a little-endian number, is at most the
//!    expanded `target_bits` (`mant << 8 * (exp - 3)`, `3 <= exp <= 32`,
//!    `0 < mant < 0x800000`), the header rule `meets_target` uses.
//!
//! Case B: steps 1-6 with mode B, anchor `(00 x 32, the ticket's vout)` and
//! `target_bits = 0`; then the coinbase's txid is the ticket's (input 1),
//! and, when the token sets one, the BIP34 height is at least its start
//! height. The null-prevout check makes B sound: only a real coinbase can be
//! the ticket's parent, and a coinbase output is spent only at depth 100.

use super::{
    super::template::{double_sha256, meets_target},
    commitment::AuxCommitment,
    leaf::{Leaf, Mode, MAX_SPLIT_BPS},
    proof::AuxProof,
    tree::{self, MAX_HEIGHT},
    Hash, OutPoint,
};

pub use super::proof::ProofError;

/// What a claim transaction shows a covenant through introspection.
#[derive(Clone, Copy, Debug)]
pub struct ClaimView<'a> {
    /// The baton's token category (input 0).
    pub category: Hash,
    /// The baton's outpoint (input 0): Case A's anchor.
    pub anchor: OutPoint,
    /// The ticket's outpoint (input 1): Case B only.
    pub ticket: Option<OutPoint>,
    /// Output 1's locking bytecode: who the claim pays.
    pub payout: &'a [u8],
    /// Output 2's share and locking bytecode: the donation's split.
    pub split: Option<(u16, &'a [u8])>,
    /// The token's current compact target (Case A).
    pub target_bits: u32,
    pub ext: Hash,
    pub max_aux_height: u8,
    /// Case B: the lowest BCH height that may claim; 0 checks none.
    pub start_height: u32,
}

/// The coinbase input every merge-mined coinbase starts with: one input,
/// the null prevout.
const NULL_INPUT: [u8; 37] = {
    let mut input = [0xff; 37];
    input[0] = 1;
    let mut index = 1;
    while index < 33 {
        input[index] = 0;
        index += 1;
    }
    input
};

/// Expands a token's compact target, little-endian. Stricter than a
/// header's nBits: `3 <= exp <= 32` and `0 < mant < 0x800000`, so a
/// covenant expands it with one shift.
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

fn split<'a>(view: &ClaimView<'a>) -> Result<Option<(u16, &'a [u8])>, ProofError> {
    match view.split {
        Some((bps, _)) if bps > MAX_SPLIT_BPS => Err(ProofError::Split),
        split => Ok(split),
    }
}

fn check_head(head: &[u8]) -> Result<(), ProofError> {
    let length = usize::from(*head.get(41).ok_or(ProofError::CoinbaseHead)?);
    if length > 100
        || head.len() != 47 + length
        || head[4..41] != NULL_INPUT
        || !(1..0xfd).contains(&head[head.len() - 1])
    {
        return Err(ProofError::CoinbaseHead);
    }
    Ok(())
}

/// Steps 2-6, shared by both cases: the coinbase's txid.
fn coinbase_txid(proof: &AuxProof, leaf: &Leaf, view: &ClaimView) -> Result<Hash, ProofError> {
    if proof.leaf != *leaf {
        return Err(ProofError::Leaf);
    }
    if proof.height > view.max_aux_height.min(MAX_HEIGHT) {
        return Err(ProofError::Height);
    }
    let slot = tree::slot(&leaf.category, leaf.mode, proof.nonce, proof.height);
    let root = tree::fold(leaf.hash(), slot, &proof.aux_branch);
    let output = AuxCommitment {
        root,
        height: proof.height,
        nonce: proof.nonce,
    }
    .output();
    check_head(&proof.cb_head)?;
    if proof.cb_tail.len() < 4 {
        return Err(ProofError::CoinbaseTail);
    }
    let mut coinbase = Vec::with_capacity(proof.cb_head.len() + output.len() + proof.cb_tail.len());
    coinbase.extend_from_slice(&proof.cb_head);
    coinbase.extend_from_slice(&output);
    coinbase.extend_from_slice(&proof.cb_tail);
    Ok(double_sha256(&coinbase))
}

/// Verifies a Case A proof: a share whose header meets the token's target.
pub fn verify_a(proof: &AuxProof, view: &ClaimView) -> Result<(), ProofError> {
    proof.check_shape()?;
    let header = match (proof.leaf.mode, proof.header) {
        (Mode::ShareTarget, Some(header)) => header,
        _ => return Err(ProofError::Shape),
    };
    let target = expand_compact(view.target_bits).ok_or(ProofError::Target)?;
    let leaf = Leaf::share_target(
        view.category,
        view.anchor,
        view.payout,
        view.target_bits,
        split(view)?,
        view.ext,
    );
    let mut root = coinbase_txid(proof, &leaf, view)?;
    for sibling in &proof.cb_branch {
        let mut pair = [0; 64];
        pair[..32].copy_from_slice(&root);
        pair[32..].copy_from_slice(sibling);
        root = double_sha256(&pair);
    }
    if header[36..68] != root {
        return Err(ProofError::MerkleRoot);
    }
    if !meets_target(&double_sha256(&header), &target) {
        return Err(ProofError::Work);
    }
    Ok(())
}

/// Verifies a Case B proof: the coinbase of a found block whose ticket the
/// claim spends.
pub fn verify_b(proof: &AuxProof, view: &ClaimView) -> Result<(), ProofError> {
    proof.check_shape()?;
    if proof.leaf.mode != Mode::BlockRequired {
        return Err(ProofError::Shape);
    }
    let ticket = view.ticket.ok_or(ProofError::Ticket)?;
    let leaf = Leaf::block_required(
        view.category,
        ticket.vout,
        view.payout,
        split(view)?,
        view.ext,
    );
    if coinbase_txid(proof, &leaf, view)? != ticket.txid {
        return Err(ProofError::Ticket);
    }
    if view.start_height > 0
        && bip34_height(&proof.cb_head).is_none_or(|height| height < view.start_height)
    {
        return Err(ProofError::StartHeight);
    }
    Ok(())
}

/// The BIP34 height pushed first in the coinbase script: a 1-4 byte
/// positive script number inside the script.
fn bip34_height(head: &[u8]) -> Option<u32> {
    let script_len = usize::from(*head.get(41)?);
    let push = usize::from(*head.get(42)?);
    if !(1..=4).contains(&push) || push >= script_len {
        return None;
    }
    let bytes = head.get(43..43 + push)?;
    if bytes[push - 1] & 0x80 != 0 {
        return None;
    }
    let mut word = [0; 4];
    word[..push].copy_from_slice(bytes);
    Some(u32::from_le_bytes(word))
}

#[cfg(test)]
mod tests {
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
                    AuxJob, AuxOutputs,
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

    /// A template with three transactions and the test token in both modes,
    /// one extended-channel coinbase and a header that wins the A token.
    fn mined() -> Mined {
        let mut raw = rpc_template();
        let mut txs = vec![transaction(1), transaction(2), transaction(3)];
        txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
        raw["transactions"] = json!(txs);
        let mut template = BchTemplate::from_rpc(&raw).unwrap();
        template.commit(Arc::new(test_set(&[
            Mode::ShareTarget,
            Mode::BlockRequired,
        ])));
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
                Some(&job.outputs),
            )
            .unwrap()
            .bytes;
        let branch = template
            .coinbase_parts_with_aux(NETWORK, &miner(), None, 28, policy, Some(&job.outputs))
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

    fn view_a(scripts: &[Vec<u8>]) -> ClaimView<'_> {
        ClaimView {
            category: TEST_TOKEN.category,
            anchor: test_state().anchor,
            ticket: None,
            payout: &scripts[0],
            split: Some((100, &scripts[1])),
            target_bits: test_state().target_bits,
            ext: [0; 32],
            max_aux_height: TEST_TOKEN.max_aux_height,
            start_height: 0,
        }
    }

    fn proofs(mined: &Mined) -> (AuxProof, AuxProof) {
        let a = assemble(
            &mined.job,
            0,
            &mined.coinbase,
            &mined.branch,
            Some(mined.header),
        )
        .unwrap();
        let b = assemble(&mined.job, 1, &mined.coinbase, &mined.branch, None).unwrap();
        (a, b)
    }

    fn ticket(mined: &Mined) -> OutPoint {
        OutPoint {
            txid: double_sha256(&mined.coinbase),
            vout: mined.job.entries[1].ticket_vout.unwrap(),
        }
    }

    // #### PR #42
    #[test]
    fn a_built_proof_verifies_case_a_and_case_b() {
        let mined = mined();
        assert_eq!(mined.job.entries[0].mode, Mode::ShareTarget);
        assert_eq!(mined.job.entries[1].mode, Mode::BlockRequired);
        assert_eq!(mined.branch.len(), 2);
        let (a, b) = proofs(&mined);
        let view = view_a(&mined.scripts);
        verify_a(&a, &view).unwrap();
        // Through the journal's bytes and back.
        let bytes = a.to_bytes().unwrap();
        assert_eq!(bytes.len(), 181 + 32 + 2 + 79 + 2 + 72 + 46 + 1 + 64 + 80);
        verify_a(&AuxProof::from_bytes(&bytes).unwrap(), &view).unwrap();
        // The block itself is whole: the template accepts the header.
        let coinbase = mined
            .template
            .coinbase_with_aux(
                NETWORK,
                &miner(),
                None,
                &EXTRANONCE,
                BchPayout::default(),
                Some(&mined.job.outputs),
            )
            .unwrap();
        assert!(mined.template.block(&coinbase, mined.header).is_ok());
        // Case B: no header or branch; the ticket names the coinbase.
        assert!(b.header.is_none() && b.cb_branch.is_empty());
        let view_b = ClaimView {
            ticket: Some(ticket(&mined)),
            start_height: 325_909,
            ..view
        };
        verify_b(&b, &view_b).unwrap();
        verify_b(
            &AuxProof::from_bytes(&b.to_bytes().unwrap()).unwrap(),
            &view_b,
        )
        .unwrap();
        // Each proof checks only in its own case.
        assert_eq!(verify_b(&a, &view_b), Err(ProofError::Shape));
        assert_eq!(verify_a(&b, &view_b), Err(ProofError::Shape));
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
            assemble(
                &mined.job,
                0,
                &plain.bytes,
                &mined.branch,
                Some(mined.header)
            ),
            Err(ProofError::NotInJob)
        );
        assert_eq!(
            assemble(
                &mined.job,
                2,
                &mined.coinbase,
                &mined.branch,
                Some(mined.header)
            ),
            Err(ProofError::NotInJob)
        );
        assert_eq!(
            assemble(&mined.job, 0, &mined.coinbase, &mined.branch, None),
            Err(ProofError::Shape)
        );
    }

    // #### PR #42
    #[test]
    fn every_mutation_fails() {
        let mined = mined();
        let (a, b) = proofs(&mined);
        let view = view_a(&mined.scripts);
        let check = |proof: &AuxProof| verify_a(proof, &view);
        let with = |change: &dyn Fn(&mut AuxProof)| {
            let mut proof = a.clone();
            change(&mut proof);
            proof
        };
        // Flipped bytes: cb_head (in the script), cb_tail, the header (in
        // its version and in its merkle root), the aux branch, the nonce.
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
        assert_eq!(
            check(&with(&|p| p.header.as_mut().unwrap()[0] ^= 1)),
            Err(ProofError::Work)
        );
        assert_eq!(
            check(&with(&|p| p.header.as_mut().unwrap()[40] ^= 1)),
            Err(ProofError::MerkleRoot)
        );
        assert_eq!(
            check(&with(&|p| p.aux_branch[0][7] ^= 1)),
            Err(ProofError::MerkleRoot)
        );
        assert_eq!(check(&with(&|p| p.nonce ^= 1)), Err(ProofError::MerkleRoot));
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
            p.header = Some(mine(&mined.template, &moved_cb, &mined.branch));
        });
        assert_eq!(check(&moved), Err(ProofError::CoinbaseHead));
        // The leaf at the wrong slot of a tree the coinbase does commit to.
        let entries = &mined.job.entries;
        let swapped = Layout {
            height: mined.job.commitment.height,
            nonce: mined.job.commitment.nonce,
            slots: vec![entries[1].slot, entries[0].slot],
        };
        let tree = AuxTree::build(&swapped, &[entries[0].leaf.hash(), entries[1].leaf.hash()]);
        let outputs = AuxOutputs {
            commitment: AuxCommitment {
                root: tree.root(),
                ..mined.job.commitment
            }
            .output(),
            ..mined.job.outputs.clone()
        };
        let wrong_cb = mined
            .template
            .coinbase_with_aux(
                NETWORK,
                &miner(),
                None,
                &EXTRANONCE,
                BchPayout::default(),
                Some(&outputs),
            )
            .unwrap()
            .bytes;
        let wrong_slot = with(&|p| {
            p.aux_branch = tree.branch(entries[1].slot);
            p.header = Some(mine(&mined.template, &wrong_cb, &mined.branch));
        });
        assert_eq!(check(&wrong_slot), Err(ProofError::MerkleRoot));
        // Another category, payout, split or anchor (a stale baton).
        let mut other = view;
        other.category[0] ^= 1;
        assert_eq!(verify_a(&a, &other), Err(ProofError::Leaf));
        let other = ClaimView {
            payout: &mined.scripts[1],
            ..view
        };
        assert_eq!(verify_a(&a, &other), Err(ProofError::Leaf));
        for split in [
            None,
            Some((99, &mined.scripts[1][..])),
            Some((100, &mined.scripts[0][..])),
        ] {
            assert_eq!(
                verify_a(&a, &ClaimView { split, ..view }),
                Err(ProofError::Leaf)
            );
        }
        let over = ClaimView {
            split: Some((10_001, &mined.scripts[1])),
            ..view
        };
        assert_eq!(verify_a(&a, &over), Err(ProofError::Split));
        let mut stale = view;
        stale.anchor.txid[0] ^= 1;
        assert_eq!(verify_a(&a, &stale), Err(ProofError::Leaf));
        let mut stale = view;
        stale.anchor.vout = 1;
        assert_eq!(verify_a(&a, &stale), Err(ProofError::Leaf));
        // A tree taller than the token allows; h unlike the branch.
        let low = ClaimView {
            max_aux_height: 0,
            ..view
        };
        assert_eq!(verify_a(&a, &low), Err(ProofError::Height));
        assert_eq!(check(&with(&|p| p.height = 2)), Err(ProofError::Shape));
        // Targets a covenant cannot expand: exp < 3, exp > 32, mant >= 0x800000, mant 0.
        for target_bits in [0x0200_8000, 0x2100_0001, 0x1f80_0000, 0x1f00_0000] {
            let bad = ClaimView {
                target_bits,
                ..view
            };
            assert_eq!(
                verify_a(&a, &bad),
                Err(ProofError::Target),
                "{target_bits:08x}"
            );
        }
        // Bytes: a branch that is not a multiple of 32, another h.
        let bytes = a.to_bytes().unwrap();
        let mut odd = bytes.clone();
        odd.insert(181 + 32, 0);
        assert_eq!(AuxProof::from_bytes(&odd), Err(ProofError::Decode));
        let mut taller = bytes.clone();
        taller[176] = 2;
        assert_eq!(AuxProof::from_bytes(&taller), Err(ProofError::Decode));
        let mut shorter = bytes;
        shorter[176] = 0;
        assert_eq!(AuxProof::from_bytes(&shorter), Err(ProofError::Decode));

        // Case B: another txid, the wrong vout, below the start height.
        let ticket = ticket(&mined);
        let view_b = ClaimView {
            ticket: Some(ticket),
            ..view
        };
        verify_b(&b, &view_b).unwrap();
        let mut other = ticket;
        other.txid[0] ^= 1;
        assert_eq!(
            verify_b(
                &b,
                &ClaimView {
                    ticket: Some(other),
                    ..view_b
                }
            ),
            Err(ProofError::Ticket)
        );
        let mut other = ticket;
        other.vout += 1;
        assert_eq!(
            verify_b(
                &b,
                &ClaimView {
                    ticket: Some(other),
                    ..view_b
                }
            ),
            Err(ProofError::Leaf)
        );
        assert_eq!(verify_b(&b, &view), Err(ProofError::Ticket));
        let early = ClaimView {
            start_height: 325_910,
            ..view_b
        };
        assert_eq!(verify_b(&b, &early), Err(ProofError::StartHeight));
        let mut flipped = b.clone();
        flipped.cb_tail[5] ^= 1;
        assert_eq!(verify_b(&flipped, &view_b), Err(ProofError::Ticket));
        let mut flipped = b.clone();
        flipped.cb_head[20] ^= 1;
        assert_eq!(verify_b(&flipped, &view_b), Err(ProofError::CoinbaseHead));
    }

    // #### PR #42
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
        // BIP34 heights as Pickaxe writes them.
        let head = |script: &[u8]| {
            let mut head = vec![0; 41];
            head.push(script.len() as u8);
            head.extend(script);
            head
        };
        assert_eq!(
            bip34_height(&head(&[3, 0x15, 0xf9, 0x04, 0, 0])),
            Some(325_909)
        );
        assert_eq!(bip34_height(&head(&[4, 0, 0, 0x80, 0, 0])), Some(0x80_0000));
        assert_eq!(bip34_height(&head(&[3, 0, 0, 0x80, 0])), None);
        assert_eq!(bip34_height(&head(&[0x51, 0])), None);
        assert_eq!(bip34_height(&head(&[3, 1, 2])), None);
    }
}
