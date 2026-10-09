use super::{
    provider::{NodeRpc, TemplateProvider},
    template::*,
};
use crate::{config::MiningNetwork, tx::p2pkh_hash_to_cashaddr_for_network};
use serde_json::{json, Value};
use std::collections::VecDeque;
use stratum_core::bitcoin::{consensus, Block};

pub(super) fn rpc_template() -> Value {
    json!({"version":536870912,"previousblockhash":"ab".repeat(32),
        "bits":"207fffff","target":format!("7fffff{}", "00".repeat(29)),
        "mintime":1700000000,"curtime":1700000010,"height":325909,
        "sizelimit":64000000,"coinbasevalue":312500000,"coinbaseaux":{},"transactions":[]})
}

pub(super) fn payout() -> String {
    p2pkh_hash_to_cashaddr_for_network(&[0x12; 20], MiningNetwork::Chipnet).unwrap()
}

fn tip() -> Value {
    json!({"chain":"chip","initialblockdownload":false,"blocks":325908,
        "headers":325908,"bestblockhash":"ab".repeat(32)})
}

pub(super) fn transaction(nonce: u32) -> Value {
    let mut bytes = 2u32.to_le_bytes().to_vec();
    bytes.push(1);
    bytes.extend([1; 32]);
    bytes.extend(nonce.to_le_bytes());
    bytes.push(0);
    bytes.extend(u32::MAX.to_le_bytes());
    bytes.push(1);
    bytes.extend(1000u64.to_le_bytes());
    bytes.extend([1, 0x51]);
    bytes.extend(0u32.to_le_bytes());
    let mut txid = double_sha256(&bytes);
    txid.reverse();
    json!({"data":hex::encode(bytes),"txid":hex::encode(txid)})
}

/// #### PR #42: a CTOR-ordered template with `count` transactions, and
/// their hashes in internal byte order.
fn template_with(count: u32) -> (BchTemplate, Vec<Hash>) {
    let mut raw = rpc_template();
    let mut txs: Vec<Value> = (1..=count).map(transaction).collect();
    txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
    let hashes = txs
        .iter()
        .map(|tx| {
            let mut hash: Hash = hex::decode(tx["txid"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            hash.reverse();
            hash
        })
        .collect();
    raw["transactions"] = json!(txs);
    (BchTemplate::from_rpc(&raw).unwrap(), hashes)
}

// #### PR #42
// What: folding a coinbase's hash up the branch computed once per template
// gives the whole tree's root, for every transaction count from 0 to 17
// (odd levels duplicate their last hash).
// Look here if: coinbase_path or fold changes.
#[test]
fn merkle_root_by_path_equals_the_full_tree() {
    for count in 0..=17u32 {
        let (_, txids) = template_with(count);
        let leaf = double_sha256(&count.to_le_bytes());
        let mut all = vec![leaf];
        all.extend_from_slice(&txids);
        let path = coinbase_path(&txids);
        assert_eq!(fold(leaf, &path), merkle_root(all), "{count} transactions");
        let depth = if count == 0 {
            0
        } else {
            (u32::BITS - count.leading_zeros()) as usize
        };
        assert_eq!(path.len(), depth, "{count} transactions");
    }
}

// #### PR #42
// What: a coinbase rebuilt from a job's parts (prefix, extranonce, suffix)
// is byte for byte the full build, and folding its hash up the parts'
// merkle path gives the full build's root, for 0 to 17 transactions and
// for miner, donation-work and fee-work payouts.
// Look here if: coinbase_parts_with_aux, coinbase_with_aux or the extended
// share rebuild in channel.rs changes.
#[test]
fn coinbases_rebuilt_from_parts_equal_the_full_build() {
    use crate::donation::bch::{BchPayout, FeeMode, PoolFee};
    let operator = p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], MiningNetwork::Chipnet).unwrap();
    let fee = Some(PoolFee {
        rate: "2".parse().unwrap(),
        mode: FeeMode::Both,
    });
    let policies = [
        BchPayout::default(),
        BchPayout {
            donation_work: true,
            ..BchPayout::default()
        },
        BchPayout {
            fee,
            fee_work: true,
            ..BchPayout::default()
        },
    ];
    for count in 0..=17u32 {
        let (template, txids) = template_with(count);
        for policy in policies {
            let operator = policy.fee.map(|_| operator.as_str());
            let extranonce: Vec<u8> = (0..28u8).collect();
            let full = template
                .coinbase_with_payout(
                    MiningNetwork::Chipnet,
                    &payout(),
                    operator,
                    &extranonce,
                    policy,
                )
                .unwrap();
            let parts = template
                .coinbase_parts_with_payout(
                    MiningNetwork::Chipnet,
                    &payout(),
                    operator,
                    extranonce.len(),
                    policy,
                )
                .unwrap();
            let mut rebuilt = parts.prefix.clone();
            rebuilt.extend_from_slice(&extranonce);
            rebuilt.extend_from_slice(&parts.suffix);
            assert_eq!(rebuilt, full.bytes, "{count} transactions");
            assert_eq!(
                fold(double_sha256(&rebuilt), &parts.merkle_path),
                full.merkle_root
            );
            let mut all = vec![double_sha256(&full.bytes)];
            all.extend_from_slice(&txids);
            assert_eq!(full.merkle_root, merkle_root(all));
        }
    }
}

#[test]
fn header_hash_and_target_match_genesis_vector() {
    let header = hex::decode(concat!(
        "010000000000000000000000000000000000000000000000000000000000000000000000",
        "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
        "29ab5f49ffff001d1dac2b7c"
    ))
    .unwrap();
    assert_eq!(header.len(), 80);
    let hash = double_sha256(&header);
    let mut display = hash;
    display.reverse();
    assert_eq!(
        hex::encode(display),
        "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
    );
    let target = compact_target(0x1d00ffff).unwrap();
    assert!(meets_target(&hash, &target));
    assert!(meets_target(&target, &target));
    for bits in [
        0, 0x01000001, 0x1d80ffff, 0x23000001, 0x22000100, 0x21010000,
    ] {
        assert!(compact_target(bits).is_err(), "bits={bits:08x}");
    }
}

#[test]
fn full_template_preserves_ctor_transactions_and_reference_merkle() {
    let mut raw = rpc_template();
    let mut txs = vec![transaction(1), transaction(2), transaction(3)];
    txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
    raw["transactions"] = json!(txs);
    let template = BchTemplate::from_rpc(&raw).unwrap();
    let coinbase = template
        .coinbase(MiningNetwork::Chipnet, &payout(), &[1; 16])
        .unwrap();
    assert!(coinbase.bytes.len() >= 100);
    let header = template
        .header(&coinbase, template.version, template.current_time, 1)
        .unwrap();
    let block = template.block(&coinbase, header).unwrap();
    assert_eq!(block[80], 4);
    let expected_tail = txs
        .iter()
        .flat_map(|tx| hex::decode(tx["data"].as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert!(block.ends_with(&expected_tail));
    let decoded: Block = consensus::deserialize(&block).unwrap();
    assert!(decoded.check_merkle_root());
    txs.reverse();
    raw["transactions"] = json!(txs);
    assert!(BchTemplate::from_rpc(&raw).is_err());
    raw["transactions"] = json!([transaction(1), transaction(1)]);
    assert!(BchTemplate::from_rpc(&raw).is_err());
    raw["transactions"] = json!([transaction(1)]);
    raw["transactions"][0]["txid"] = json!("00".repeat(32));
    assert!(BchTemplate::from_rpc(&raw).is_err());
}

// #### PR #40
#[test]
fn a_pool_name_is_written_into_the_coinbase_and_the_parts_still_fit() {
    let mut raw = rpc_template();
    raw["coinbaseaux"] = json!({"flags":"0454455354"});
    let mut template = BchTemplate::from_rpc(&raw).unwrap();
    template.tag(b"/MyPool/");
    let extra = [7u8; 16];
    let coinbase = template
        .coinbase(MiningNetwork::Chipnet, &payout(), &extra)
        .unwrap();
    assert!(coinbase
        .bytes
        .windows(8)
        .any(|window| window == b"/MyPool/"));
    let parts = template
        .coinbase_parts(MiningNetwork::Chipnet, &payout(), extra.len())
        .unwrap();
    let mut bytes = parts.prefix.clone();
    bytes.extend(extra);
    bytes.extend(&parts.suffix);
    assert_eq!(bytes, coinbase.bytes);
    // A name too long for the 100-byte coinbase script is refused.
    template.tag(&[b'x'; 80]);
    assert!(template
        .coinbase(MiningNetwork::Chipnet, &payout(), &extra)
        .is_err());
}

/// #### PR #42: a coinbase's hex with the Pickaxe donation's locking script
/// shown as `{donation}`, as the golden coinbases write it.
fn golden_hex(coinbase: &[u8]) -> String {
    let address = crate::donation::bch::address(MiningNetwork::Chipnet);
    let donation = hex::encode(crate::tx::cashaddr_to_p2pkh_locking(address).unwrap());
    hex::encode(coinbase).replace(&donation, "{donation}")
}

/// #### PR #42: an extended-channel coinbase head (version, the null
/// prevout, the 32-byte script with height 325909 and extranonce `[7; 28]`,
/// the sequence) before the output count.
const GOLDEN_HEAD: &str = concat!(
    "02000000",
    "01",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "ffffffff",
    "20",
    "0315f904",
    "07070707070707070707070707070707070707070707070707070707",
    "ffffffff"
);
/// The miner's 99% and the donation's 1% of 3.125 BCH.
const GOLDEN_PAYOUTS: &str = concat!(
    "18b0701200000000",
    "19",
    "76a914121212121212121212121212121212121212121288ac",
    "08af2f0000000000",
    "19",
    "{donation}"
);

// #### PR #42
#[test]
fn no_tokens_keeps_coinbase_bytes_identical() {
    use crate::donation::bch::BchPayout;
    let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
    let network = MiningNetwork::Chipnet;
    let policy = BchPayout::default();
    let today = template
        .coinbase_with_payout(network, &payout(), None, &[7; 28], policy)
        .unwrap();
    assert_eq!(today.bytes.len(), 151);
    assert_eq!(
        golden_hex(&today.bytes),
        format!("{GOLDEN_HEAD}02{GOLDEN_PAYOUTS}00000000")
    );
    let none = template
        .coinbase_with_aux(network, &payout(), None, &[7; 28], policy, None)
        .unwrap();
    assert_eq!(none.bytes, today.bytes);
    assert_eq!(none.merkle_root, today.merkle_root);
    let parts = template
        .coinbase_parts_with_aux(network, &payout(), None, 28, policy, None)
        .unwrap();
    let mut bytes = parts.prefix.clone();
    bytes.extend([7; 28]);
    bytes.extend(&parts.suffix);
    assert_eq!(bytes, today.bytes);
    // No token set, no merge-mining work.
    assert!(template.tokens().is_none());
    assert!(template
        .aux_job(network, &payout(), None, policy)
        .unwrap()
        .is_none());
}

// #### PR #42
#[test]
fn the_commitment_is_output_zero_and_the_parts_still_fit() {
    use super::merge::{
        commitment::AuxCommitment, leaf::Mode, registry::is_ticket_script, set::tests::test_set,
        tree::fold,
    };
    use crate::donation::bch::BchPayout;
    use std::sync::Arc;
    let network = MiningNetwork::Chipnet;
    let policy = BchPayout::default();
    let mut raw = rpc_template();
    let mut txs = vec![transaction(4), transaction(5)];
    txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
    raw["transactions"] = json!(txs);
    let plain = BchTemplate::from_rpc(&raw).unwrap();
    let mut template = plain.clone();
    template.commit(Arc::new(test_set(&[
        Mode::ShareTarget,
        Mode::BlockRequired,
    ])));
    assert_eq!(template.tokens().unwrap().serial(), 1);
    let job = template
        .aux_job(network, &payout(), None, policy)
        .unwrap()
        .unwrap();
    let extra = [7u8; 28];
    let coinbase = template
        .coinbase_with_aux(network, &payout(), None, &extra, policy, Some(&job.outputs))
        .unwrap();
    let parts = template
        .coinbase_parts_with_aux(
            network,
            &payout(),
            None,
            extra.len(),
            policy,
            Some(&job.outputs),
        )
        .unwrap();
    let mut bytes = parts.prefix.clone();
    bytes.extend(extra);
    bytes.extend(&parts.suffix);
    assert_eq!(bytes, coinbase.bytes);
    // The prefix (up to the extranonce) and the merkle path are unchanged.
    let plain_parts = plain
        .coinbase_parts(network, &payout(), extra.len())
        .unwrap();
    assert_eq!(parts.prefix, plain_parts.prefix);
    assert_eq!(parts.merkle_path, plain_parts.merkle_path);
    // Output 0 sits right after the one-byte output count, found from the
    // input alone, and the ticket right before the locktime.
    let script_len = usize::from(coinbase.bytes[41]);
    assert_eq!(coinbase.bytes[46 + script_len], 4);
    assert_eq!(
        coinbase.bytes[47 + script_len..100 + script_len],
        job.outputs.commitment
    );
    let end = coinbase.bytes.len() - 4;
    assert_eq!(coinbase.bytes[end - 46..end], job.outputs.tickets[0]);
    let commitment =
        AuxCommitment::parse(&coinbase.bytes[47 + script_len..100 + script_len]).unwrap();
    assert_eq!(commitment, job.commitment);
    for (index, entry) in job.entries.iter().enumerate() {
        let branch = job.branch(index).unwrap();
        assert_eq!(
            fold(entry.leaf.hash(), entry.slot, &branch),
            commitment.root
        );
    }
    // The block decodes, its merkle root checks, and the coinbase pays the
    // same amounts: the commitment and the ticket are worth 0.
    let header = template
        .header(&coinbase, template.version, template.current_time, 0)
        .unwrap();
    let block: Block = consensus::deserialize(&template.block(&coinbase, header).unwrap()).unwrap();
    assert!(block.check_merkle_root());
    let outputs = &block.txdata[0].output;
    assert_eq!(outputs.len(), 4);
    assert_eq!(outputs[0].value.to_sat(), 0);
    assert_eq!(outputs[0].script_pubkey.as_bytes(), job.commitment.script());
    let vout = job.entries[1].ticket_vout.unwrap() as usize;
    assert_eq!(vout, 3);
    assert_eq!(outputs[vout].value.to_sat(), 0);
    assert!(is_ticket_script(outputs[vout].script_pubkey.as_bytes()));
    assert_eq!(
        outputs.iter().map(|o| o.value.to_sat()).sum::<u64>(),
        template.coinbase_value
    );
    // Tickets must follow the payouts, or the B leaves would name the
    // wrong vouts.
    let mut wrong = job.outputs.clone();
    wrong.first_ticket_vout += 1;
    assert!(template
        .coinbase_with_aux(network, &payout(), None, &extra, policy, Some(&wrong))
        .is_err());
}

// #### PR #42
#[test]
fn golden_extended_coinbase_with_an_a_token_and_a_b_ticket() {
    use super::merge::{leaf::Mode, set::tests::test_set};
    use crate::donation::bch::{BchPayout, FeeMode, PoolFee};
    use std::sync::Arc;
    let network = MiningNetwork::Chipnet;
    let policy = BchPayout::default();
    let build = |modes: &[Mode], extranonce: &[u8], operator: Option<&str>, policy| {
        let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        template.commit(Arc::new(test_set(modes)));
        let job = template
            .aux_job(network, &payout(), operator, policy)
            .unwrap()
            .unwrap();
        template
            .coinbase_with_aux(
                network,
                &payout(),
                operator,
                extranonce,
                policy,
                Some(&job.outputs),
            )
            .unwrap()
            .bytes
    };
    let commitment = |root: &str, height: &str, nonce: &str| {
        format!("00000000000000002c6a2a43544d4d01{root}{height}{nonce}")
    };
    // One Case A token: the 53-byte commitment as output 0.
    let a = build(&[Mode::ShareTarget], &[7; 28], None, policy);
    assert_eq!(a.len(), 204);
    let golden_a = format!(
        "{GOLDEN_HEAD}03{}{GOLDEN_PAYOUTS}00000000",
        commitment(
            "bbcfbf16c81f6c441f43cb1eedb2eeabfa563ba0bd9765c4bb9265d2f96c3a5d",
            "00",
            "00000000"
        )
    );
    assert_eq!(golden_hex(&a), golden_a);
    // The test token in both modes: one commitment and one ticket.
    let both = build(
        &[Mode::ShareTarget, Mode::BlockRequired],
        &[7; 28],
        None,
        policy,
    );
    assert_eq!(both.len(), 250);
    let golden_both = format!(
        "{GOLDEN_HEAD}04{}{GOLDEN_PAYOUTS}{}00000000",
        commitment(
            "5b7d3f2722c00b21e28af132bda1a61f157ba2ab94941635f53ad20dc6de0909",
            "01",
            "00000000"
        ),
        concat!(
            "0000000000000000",
            "25",
            "00ce21",
            "feab4bcd324f722033f5a1d67432f45caeba0e505b5ccaaf665555769425ad93",
            "02",
            "87"
        )
    );
    assert_eq!(golden_hex(&both), golden_both);
    // A standard channel's coinbase (20-byte extranonce): 143, 196, 242.
    let plain = BchTemplate::from_rpc(&rpc_template()).unwrap();
    let standard = plain
        .coinbase_with_payout(network, &payout(), None, &[7; 20], policy)
        .unwrap();
    assert_eq!(standard.bytes.len(), 143);
    assert_eq!(
        build(&[Mode::ShareTarget], &[7; 20], None, policy).len(),
        196
    );
    assert_eq!(
        build(
            &[Mode::ShareTarget, Mode::BlockRequired],
            &[7; 20],
            None,
            policy
        )
        .len(),
        242
    );
    // A public pool with a coinbase fee, with the commitment: 238.
    let operator = crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], network).unwrap();
    let fee = BchPayout {
        fee: Some(PoolFee {
            rate: "2".parse().unwrap(),
            mode: FeeMode::Coinbase,
        }),
        ..policy
    };
    assert_eq!(
        build(&[Mode::ShareTarget], &[7; 28], Some(&operator), fee).len(),
        238
    );
}

#[test]
fn extended_coinbase_parts_preserve_nonempty_odd_and_even_merkle_trees() {
    use stratum_core::bitcoin::hashes::{sha256d, Hash as _};
    for count in [1, 2, 3, 6, 7] {
        let mut raw = rpc_template();
        let mut txs = (1..=count).map(transaction).collect::<Vec<_>>();
        txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
        raw["transactions"] = json!(txs);
        raw["coinbaseaux"] = json!({"flags":"0454455354"});
        let template = BchTemplate::from_rpc(&raw).unwrap();
        let parts = template
            .coinbase_parts(MiningNetwork::Chipnet, &payout(), 24)
            .unwrap();
        for extra in [[0; 24], [0xff; 24]] {
            let coinbase = template
                .coinbase(MiningNetwork::Chipnet, &payout(), &extra)
                .unwrap();
            let mut bytes = parts.prefix.clone();
            bytes.extend(extra);
            bytes.extend(&parts.suffix);
            assert_eq!(bytes, coinbase.bytes);
            let mut root = sha256d::Hash::hash(&bytes).to_byte_array();
            for sibling in &parts.merkle_path {
                let mut pair = root.to_vec();
                pair.extend(sibling);
                root = sha256d::Hash::hash(&pair).to_byte_array();
            }
            assert_eq!(root, coinbase.merkle_root);
            let header = template
                .header(&coinbase, template.version, template.current_time, 0)
                .unwrap();
            let block: Block =
                consensus::deserialize(&template.block(&coinbase, header).unwrap()).unwrap();
            assert!(block.check_merkle_root());
        }
    }
}

#[test]
fn provider_keeps_inflight_solution_across_same_tip_refresh_only() {
    for change_tip in [false, true] {
        let mut calls = initial_calls();
        let mut info = tip();
        let mut template = rpc_template();
        if change_tip {
            info["bestblockhash"] = json!("cd".repeat(32));
            info["blocks"] = json!(325909);
            info["headers"] = json!(325909);
            template["previousblockhash"] = json!("cd".repeat(32));
            template["height"] = json!(325910);
        } else {
            // An exact old tx list is required after a mempool refresh.
            template["transactions"] = json!([transaction(8)]);
        }
        calls.extend([
            ("getblockchaininfo", Ok(info.clone())),
            ("getblocktemplate", Ok(template)),
            ("getblockchaininfo", Ok(info)),
        ]);
        if !change_tip {
            calls.push_back(("submitblock", Ok(Value::Null)));
        }
        let mut provider = TemplateProvider::new(RpcFixture(calls), MiningNetwork::Chipnet);
        let (generation, old) = provider.refresh().unwrap();
        let coinbase = old
            .coinbase(MiningNetwork::Chipnet, &payout(), &[9; 16])
            .unwrap();
        let header = (0..1000)
            .map(|nonce| {
                old.header(&coinbase, old.version, old.current_time, nonce)
                    .unwrap()
            })
            .find(|header| meets_target(&double_sha256(header), &old.target))
            .unwrap();
        provider.refresh().unwrap();
        assert_eq!(
            provider.submit(generation, &coinbase, header).is_ok(),
            !change_tip
        );
    }
}

#[test]
fn provider_retains_exact_block_transactions_across_multiple_refreshes() {
    let mut calls = initial_calls();
    for nonce in [8, 9, 10] {
        let mut template = rpc_template();
        template["transactions"] = json!([transaction(nonce)]);
        calls.extend([
            ("getblockchaininfo", Ok(tip())),
            ("getblocktemplate", Ok(template)),
            ("getblockchaininfo", Ok(tip())),
        ]);
    }
    calls.push_back(("submitblock", Ok(Value::Null)));
    let mut provider = TemplateProvider::new(RpcFixture(calls), MiningNetwork::Chipnet);
    let (generation, old) = provider.refresh().unwrap();
    let coinbase = old
        .coinbase(MiningNetwork::Chipnet, &payout(), &[9; 16])
        .unwrap();
    let header = (0..1000)
        .map(|nonce| {
            old.header(&coinbase, old.version, old.current_time, nonce)
                .unwrap()
        })
        .find(|header| meets_target(&double_sha256(header), &old.target))
        .unwrap();
    for _ in 0..3 {
        provider.refresh().unwrap();
    }
    // The new tx lists cannot reconstruct the original header's merkle root.
    // Successful full-block serialization therefore requires the exact old list.
    assert!(provider.submit(generation, &coinbase, header).is_ok());
}

#[test]
fn template_rejects_wrong_chain_payout_witness_and_malformed_work() {
    let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
    assert!(template
        .coinbase(MiningNetwork::Mainnet, &payout(), &[])
        .is_err());
    for (key, value) in [
        ("default_witness_commitment", json!("aa")),
        ("job_id", json!("aa")),
        ("merkle", json!([])),
        ("rules", json!(["!segwit"])),
        ("bits", json!("ff")),
        ("target", json!("00".repeat(32))),
        ("curtime", json!(1)),
    ] {
        let mut raw = rpc_template();
        raw[key] = value;
        assert!(BchTemplate::from_rpc(&raw).is_err(), "field={key}");
    }
    let mut raw = rpc_template();
    raw["sizelimit"] = json!(180);
    assert!(BchTemplate::from_rpc(&raw)
        .unwrap()
        .coinbase(MiningNetwork::Chipnet, &payout(), &[])
        .is_err());
    let first = template
        .coinbase(MiningNetwork::Chipnet, &payout(), &[1; 16])
        .unwrap();
    let second = template
        .coinbase(MiningNetwork::Chipnet, &payout(), &[2; 16])
        .unwrap();
    assert_ne!(first.merkle_root, second.merkle_root);
    let header = template
        .header(&first, template.version, template.current_time, 0)
        .unwrap();
    assert!(template.block(&second, header).is_err());
}

struct RpcFixture(VecDeque<(&'static str, Result<Value, String>)>);
impl NodeRpc for RpcFixture {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let (expected, result) = self.0.pop_front().expect("unexpected RPC");
        assert_eq!(method, expected);
        if method == "getblocktemplate" {
            assert!(params[0].get("rules").is_none());
        }
        if method == "submitblock" {
            let bytes = hex::decode(params[0].as_str().unwrap()).unwrap();
            let block: Block = consensus::deserialize(&bytes).unwrap();
            assert!(block.check_merkle_root());
        }
        result
    }
}
fn initial_calls() -> VecDeque<(&'static str, Result<Value, String>)> {
    VecDeque::from([
        ("getblockchaininfo", Ok(tip())),
        ("getblocktemplate", Ok(rpc_template())),
        ("getblockchaininfo", Ok(tip())),
    ])
}

#[test]
fn full_provider_binds_generation_and_requires_node_acceptance() {
    for result in [Value::Null, json!("rejected")] {
        let mut calls = initial_calls();
        calls.push_back(("submitblock", Ok(result.clone())));
        let mut provider = TemplateProvider::new(RpcFixture(calls), MiningNetwork::Chipnet);
        let (generation, template) = provider.refresh().unwrap();
        let coinbase = template
            .coinbase(MiningNetwork::Chipnet, &payout(), &[3; 16])
            .unwrap();
        let header = (0..1000)
            .map(|nonce| {
                template
                    .header(&coinbase, template.version, template.current_time, nonce)
                    .unwrap()
            })
            .find(|header| meets_target(&double_sha256(header), &template.target))
            .unwrap();
        assert!(provider.submit(generation + 1, &coinbase, header).is_err());
        assert_eq!(
            provider.submit(generation, &coinbase, header).is_ok(),
            result.is_null()
        );
        assert_eq!(provider.current().is_none(), result.is_null());
    }
}

#[test]
fn provider_revokes_work_on_failure_wrong_network_sync_or_tip_race() {
    let mut calls = initial_calls();
    calls.push_back(("getblockchaininfo", Err("offline".into())));
    let mut provider = TemplateProvider::new(RpcFixture(calls), MiningNetwork::Chipnet);
    provider.refresh().unwrap();
    assert!(provider.refresh().is_err());
    assert!(provider.current().is_none());
    for (key, value) in [
        ("chain", json!("main")),
        ("initialblockdownload", json!(true)),
        ("headers", json!(325910)),
    ] {
        let mut info = tip();
        info[key] = value;
        let calls = VecDeque::from([("getblockchaininfo", Ok(info))]);
        let mut provider = TemplateProvider::new(RpcFixture(calls), MiningNetwork::Chipnet);
        assert!(provider.refresh().is_err());
    }
    let mut calls = initial_calls();
    let mut changed = tip();
    changed["bestblockhash"] = json!("cd".repeat(32));
    *calls.back_mut().unwrap() = ("getblockchaininfo", Ok(changed));
    let mut provider = TemplateProvider::new(RpcFixture(calls), MiningNetwork::Chipnet);
    assert!(provider.refresh().is_err());
}
