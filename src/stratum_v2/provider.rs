//! #### PR #38
//! Pin full-template submission to its source node. A failed refresh revokes
//! work; a light-job payload must never be sent as a full block on failover.

use super::channel::MAX_ACTIVE_JOBS;
use super::journal::PendingBlock;
use super::template::{double_sha256, meets_target, BchTemplate, Coinbase};
use crate::config::MiningNetwork;
use serde_json::{json, Value};
use std::collections::VecDeque;

pub trait NodeRpc {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

// Deliberately no Debug: endpoints may contain locally supplied credentials.
pub struct NativeNodeRpc {
    endpoint: String,
}

impl NativeNodeRpc {
    pub fn new(endpoint: String) -> Self {
        Self { endpoint }
    }

    pub fn source_identity(&self) -> Result<[u8; 32], String> {
        crate::node::rpc_source_identity(&self.endpoint)
    }

    /// #### PR #40
    /// The node's client, chain and sync state.
    pub fn info(&self) -> Result<crate::node::NodeInfo, String> {
        crate::node::node_info(&self.endpoint)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Accepted,
    Rejected(&'static str),
    Pending(&'static str),
}

impl NodeRpc for NativeNodeRpc {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        crate::node::rpc_call(&self.endpoint, method, params)
    }
}

pub struct TemplateProvider<R> {
    rpc: R,
    network: MiningNetwork,
    generation: u64,
    current: Option<BchTemplate>,
    previous: VecDeque<(u64, BchTemplate)>,
}

impl<R: NodeRpc> TemplateProvider<R> {
    pub fn new(rpc: R, network: MiningNetwork) -> Self {
        Self {
            rpc,
            network,
            generation: 0,
            current: None,
            previous: VecDeque::new(),
        }
    }

    pub fn current(&self) -> Option<(u64, &BchTemplate)> {
        self.current
            .as_ref()
            .map(|template| (self.generation, template))
    }

    pub fn refresh(&mut self) -> Result<(u64, &BchTemplate), String> {
        let previous = self.current.take();
        // Move history out while fetching; any failed refresh revokes it.
        let mut history = std::mem::take(&mut self.previous);
        let before = self.chain_tip()?;
        let raw = self.rpc.call(
            "getblocktemplate",
            json!([{
                "mode": "template", "capabilities": ["coinbasevalue"], "checkvalidity": true
            }]),
        )?;
        let template = BchTemplate::from_rpc(&raw).map_err(|_| "invalid block template")?;
        let after = self.chain_tip()?;
        let mut previous_hash = template.previous_hash;
        previous_hash.reverse();
        if before != after
            || u64::from(template.height) != before.0 + 1
            || hex::encode(previous_hash) != before.1
        {
            return Err("node tip changed while fetching the template".into());
        }
        if let Some(previous) =
            previous.filter(|previous| previous.previous_hash == template.previous_hash)
        {
            history.push_back((self.generation, previous));
            while history.len() >= MAX_ACTIVE_JOBS {
                history.pop_front();
            }
            self.previous = history;
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("template generation exhausted")?;
        self.current = Some(template);
        Ok(self.current().expect("template just installed"))
    }

    pub fn tip_is_current(&mut self) -> Result<bool, String> {
        let (height, hash) = match self.chain_tip() {
            Ok(tip) => tip,
            Err(error) => {
                self.current = None;
                self.previous.clear();
                return Err(error);
            }
        };
        Ok(self.current.as_ref().is_some_and(|template| {
            let mut previous = template.previous_hash;
            previous.reverse();
            u64::from(template.height) == height.saturating_add(1) && hex::encode(previous) == hash
        }))
    }

    pub fn submit(
        &mut self,
        generation: u64,
        coinbase: &Coinbase,
        header: [u8; 80],
    ) -> Result<(), String> {
        let current = self.current.as_ref().ok_or("no active template")?;
        // A submission can cross a mempool/time refresh in flight. Retain the
        // preceding exact tx list on the same tip so a valid block is not lost.
        let template = if generation == self.generation {
            current
        } else {
            self.previous
                .iter()
                .find(|(id, template)| {
                    *id == generation && template.previous_hash == current.previous_hash
                })
                .map(|(_, template)| template)
                .ok_or("stale block generation")?
        };
        if !meets_target(&double_sha256(&header), &template.target) {
            return Err("header does not meet the block target".into());
        }
        let block = template.block(coinbase, header)?;
        let result = self.rpc.call("submitblock", json!([hex::encode(block)]))?;
        if !result.is_null() {
            return Err("source node rejected the block".into());
        }
        self.current = None;
        self.previous.clear();
        Ok(())
    }

    /// #### PR #38
    /// Recovery submits the saved full block, never a rebuilt current job.
    /// BCHN's exact "duplicate" means accepted/known; inconclusive results do
    /// not. A lost reply may also resolve through a confirmed matching header.
    /// Unknown errors remain retryable rather than discarding acknowledged work.
    pub fn submit_saved(&mut self, pending: &PendingBlock) -> SubmissionOutcome {
        if self.chain_tip().is_err() {
            return SubmissionOutcome::Pending("node-unavailable-or-wrong-network");
        }
        let result = self.rpc.call("submitblock", json!([pending.block]));
        let outcome = match result.as_ref() {
            Ok(Value::Null) => SubmissionOutcome::Accepted,
            Ok(Value::String(reason)) if reason == "duplicate" => SubmissionOutcome::Accepted,
            Ok(Value::String(reason)) => match reason.as_str() {
                "duplicate-invalid" | "high-hash" | "bad-txnmrklroot" | "bad-txns-duplicate"
                | "bad-cb-height" | "bad-cb-amount" | "bad-cb-length" | "bad-cb-missing"
                | "bad-blk-length" | "bad-blk-sigops" | "bad-tx-ordering" | "bad-diffbits"
                | "time-too-old" => SubmissionOutcome::Rejected("invalid-block"),
                "time-too-new" => SubmissionOutcome::Pending("time-too-new"),
                "inconclusive"
                | "duplicate-inconclusive"
                | "inconclusive-not-best-prevblk"
                | "bad-prevblk"
                | "prev-blk-not-found" => SubmissionOutcome::Pending("node-inconclusive"),
                _ => SubmissionOutcome::Pending("unrecognized-node-result"),
            },
            Ok(_) => SubmissionOutcome::Pending("malformed-node-result"),
            Err(_) => SubmissionOutcome::Pending("node-response-unavailable"),
        };
        let outcome = if matches!(outcome, SubmissionOutcome::Pending(_)) {
            let known = self.rpc.call("getblockheader", json!([pending.hash, true]));
            if known.as_ref().is_ok_and(|header| {
                header.get("hash").and_then(Value::as_str) == Some(pending.hash.as_str())
                    && header
                        .get("confirmations")
                        .and_then(Value::as_i64)
                        .is_some_and(|n| n > 0)
            }) {
                SubmissionOutcome::Accepted
            } else {
                outcome
            }
        } else {
            outcome
        };
        if outcome == SubmissionOutcome::Accepted {
            self.current = None;
            self.previous.clear();
        }
        outcome
    }

    fn chain_tip(&mut self) -> Result<(u64, String), String> {
        let info = self.rpc.call("getblockchaininfo", json!([]))?;
        let expected = match self.network {
            MiningNetwork::Mainnet => "main",
            MiningNetwork::Chipnet => "chip",
        };
        if info.get("chain").and_then(Value::as_str) != Some(expected) {
            return Err("node is on the wrong network".into());
        }
        let height = info
            .get("blocks")
            .and_then(Value::as_u64)
            .ok_or("node omitted height")?;
        if info.get("initialblockdownload").and_then(Value::as_bool) != Some(false)
            || info.get("headers").and_then(Value::as_u64) != Some(height)
        {
            return Err("node is not fully synchronized".into());
        }
        let hash = info
            .get("bestblockhash")
            .and_then(Value::as_str)
            .ok_or("node omitted tip")?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("node returned an invalid tip hash".into());
        }
        Ok((height, hash.to_ascii_lowercase()))
    }
}

#[cfg(test)]
mod durable_tests {
    use super::*;
    use crate::stratum_v2::journal_tests::solved_share;

    struct Rpc(VecDeque<(&'static str, Result<Value, String>)>);
    impl NodeRpc for Rpc {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            let (expected, response) = self.0.pop_front().expect("unexpected RPC");
            assert_eq!(method, expected);
            if method == "submitblock" {
                assert_eq!(params, json!([pending().block]));
            }
            response
        }
    }
    fn pending() -> PendingBlock {
        let share = solved_share(99);
        let block = share.template.block(&share.coinbase, share.header).unwrap();
        let mut hash = super::double_sha256(&share.header);
        hash.reverse();
        PendingBlock {
            hash: hex::encode(hash),
            block: hex::encode(block),
            payout: Some(share.payout),
            miner: None,
            operator: None,
        }
    }
    fn tip() -> Value {
        json!({"chain":"chip","blocks":42,"headers":42,"bestblockhash":"ab".repeat(32),"initialblockdownload":false})
    }

    #[test]
    fn exact_node_results_distinguish_accepted_invalid_and_uncertain_blocks() {
        for (response, outcome) in [
            (Value::Null, SubmissionOutcome::Accepted),
            (json!("duplicate"), SubmissionOutcome::Accepted),
            (
                json!("duplicate-invalid"),
                SubmissionOutcome::Rejected("invalid-block"),
            ),
            (
                json!("bad-cb-amount"),
                SubmissionOutcome::Rejected("invalid-block"),
            ),
            (
                json!("duplicate-inconclusive"),
                SubmissionOutcome::Pending("node-inconclusive"),
            ),
            (
                json!("time-too-new"),
                SubmissionOutcome::Pending("time-too-new"),
            ),
            (
                json!("unrecognized rejection"),
                SubmissionOutcome::Pending("unrecognized-node-result"),
            ),
            (
                json!(true),
                SubmissionOutcome::Pending("malformed-node-result"),
            ),
        ] {
            let mut calls = VecDeque::from([
                ("getblockchaininfo", Ok(tip())),
                ("submitblock", Ok(response)),
            ]);
            if matches!(outcome, SubmissionOutcome::Pending(_)) {
                calls.push_back(("getblockheader", Err("unavailable".into())));
            }
            let mut provider = TemplateProvider::new(Rpc(calls), MiningNetwork::Chipnet);
            assert_eq!(provider.submit_saved(&pending()), outcome);
            assert!(provider.rpc.0.is_empty());
        }
    }

    #[test]
    fn lost_reply_requires_a_confirmed_matching_header_to_count_acceptance() {
        for (hash, confirmations, accepted) in [
            (pending().hash, 1, true),
            (pending().hash, 0, false),
            (pending().hash, -1, false),
            ("cd".repeat(32), 5, false),
        ] {
            let calls = VecDeque::from([
                ("getblockchaininfo", Ok(tip())),
                ("submitblock", Err("lost reply".into())),
                (
                    "getblockheader",
                    Ok(json!({"hash":hash,"confirmations":confirmations})),
                ),
            ]);
            let mut provider = TemplateProvider::new(Rpc(calls), MiningNetwork::Chipnet);
            assert_eq!(
                provider.submit_saved(&pending()),
                if accepted {
                    SubmissionOutcome::Accepted
                } else {
                    SubmissionOutcome::Pending("node-response-unavailable")
                }
            );
        }
    }

    #[test]
    fn recovery_does_not_submit_to_wrong_chain_or_unsynchronized_node() {
        for field in ["chain", "headers", "initialblockdownload"] {
            let mut info = tip();
            info[field] = match field {
                "chain" => json!("main"),
                "headers" => json!(43),
                _ => json!(true),
            };
            let mut provider = TemplateProvider::new(
                Rpc(VecDeque::from([("getblockchaininfo", Ok(info))])),
                MiningNetwork::Chipnet,
            );
            assert_eq!(
                provider.submit_saved(&pending()),
                SubmissionOutcome::Pending("node-unavailable-or-wrong-network")
            );
            assert!(provider.rpc.0.is_empty());
        }
    }
}
