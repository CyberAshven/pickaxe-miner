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

fn transaction(nonce: u32) -> Value {
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
