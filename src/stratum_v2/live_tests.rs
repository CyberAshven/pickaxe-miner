//! #### PR #38
//! Opt-in Chipnet proposal validation against a real BCHN node. This checks
//! production coinbase/header/full-block assembly without mining or publishing
//! a block. It must never be described as PoW or propagation evidence.

use super::{
    channel::{Channel, ChannelKind},
    provider::{NativeNodeRpc, NodeRpc, TemplateProvider},
    template::{double_sha256, Coinbase},
    template_tests::payout,
};
use crate::config::{MiningNetwork, SavedConfig};
use crate::donation::bch::BchPayout;
use serde_json::json;
use std::{path::Path, sync::Arc};
use stratum_core::bitcoin::{consensus, Block};

#[test]
#[ignore = "requires PICKAXE_CHIPNET_CONFIG pointing to a private Chipnet RPC config"]
fn live_chipnet_node_validates_standard_and_extended_block_proposals() {
    let path = std::env::var("PICKAXE_CHIPNET_CONFIG").expect("private config path required");
    let config = SavedConfig::load(Path::new(&path)).expect("cannot load private config");
    assert_eq!(config.network.as_deref(), Some("chipnet"));
    let endpoint = config.node_rpc.expect("RPC endpoint required");
    let mut provider =
        TemplateProvider::new(NativeNodeRpc::new(endpoint.clone()), MiningNetwork::Chipnet);
    let mut rpc = NativeNodeRpc::new(endpoint);
    // A public synthetic test script is intentional: proposals do not publish,
    // and this test neither reads a wallet nor requests a real payout address.
    for (index, (kind, donation_work)) in [
        (ChannelKind::Standard, false),
        (ChannelKind::Extended, false),
        (ChannelKind::Standard, true),
        (ChannelKind::Extended, true),
    ]
    .into_iter()
    .enumerate()
    {
        let (generation, template) = provider.refresh().expect("live template preflight failed");
        let template = Arc::new(template.clone());
        let mut channel = Channel::new(
            index as u32 + 1,
            kind,
            template.target,
            [0x37; 12],
            MiningNetwork::Chipnet,
            &payout(),
        )
        .unwrap();
        let job = channel
            .install_with_payout(
                1,
                generation,
                template.clone(),
                BchPayout {
                    donation_work,
                    ..Default::default()
                },
            )
            .unwrap()
            .clone();
        let coinbase = if kind == ChannelKind::Standard {
            job.standard_coinbase.clone()
        } else {
            let mut bytes = job.parts.prefix.clone();
            bytes.extend(&channel.extranonce_prefix);
            bytes.extend([0x59; 8]);
            bytes.extend(&job.parts.suffix);
            let mut root = double_sha256(&bytes);
            for sibling in &job.parts.merkle_path {
                let mut pair = [0; 64];
                pair[..32].copy_from_slice(&root);
                pair[32..].copy_from_slice(sibling);
                root = double_sha256(&pair);
            }
            Coinbase {
                bytes,
                merkle_root: root,
            }
        };
        let header = template
            .header(&coinbase, template.version, template.current_time, 0)
            .unwrap();
        let block = template.block(&coinbase, header).unwrap();
        let decoded: Block = consensus::deserialize(&block).unwrap();
        assert!(decoded.check_merkle_root());
        assert_eq!(decoded.txdata.len(), template.transaction_count());
        let outputs = &decoded.txdata[0].output;
        assert_eq!(
            outputs.iter().map(|o| o.value.to_sat()).sum::<u64>(),
            template.coinbase_value
        );
        let donor = crate::tx::cashaddr_to_p2pkh_locking(crate::donation::bch::address(
            MiningNetwork::Chipnet,
        ))
        .unwrap();
        if donation_work {
            assert_eq!(outputs.len(), 1);
            assert!(outputs[0].script_pubkey.as_bytes() == donor);
        } else {
            assert_eq!(outputs.len(), 2);
            assert_eq!(outputs[1].value.to_sat(), template.coinbase_value * 2 / 199);
            assert!(outputs[1].script_pubkey.as_bytes() == donor);
            assert!(
                outputs[0].script_pubkey.as_bytes()
                    == crate::tx::cashaddr_to_p2pkh_locking(&payout()).unwrap()
            );
        }
        let result = rpc
            .call("validateblocktemplate", json!([hex::encode(&block)]))
            .expect("BCHN proposal validation failed (a moved tip requires a new run)");
        assert_eq!(result, json!(true), "BCHN did not validate the proposal");
        // A negative control proves that this RPC actually checks the assembled
        // block rather than merely reporting connectivity or template readiness.
        let mut corrupt = block;
        corrupt[36] ^= 1;
        let rejected = rpc
            .call(
                "getblocktemplate",
                json!([{"mode":"proposal","data":hex::encode(corrupt)}]),
            )
            .expect("negative-control proposal RPC failed");
        assert_eq!(rejected, json!("bad-txnmrklroot"));
        println!(
            "Chipnet {kind:?}: BCHN validated height {}, {} transactions; corrupted merkle root rejected; no block submitted",
            template.height, template.transaction_count()
        );
    }
}
