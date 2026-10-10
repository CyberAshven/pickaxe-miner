//! #### PR #38
//! Pin full-template submission to its source node. A failed refresh revokes
//! work; a light-job payload must never be sent as a full block on failover.
//! #### PR #40: the server now moves to its next node when one fails
//! (`replace_node`). Only full templates are used and the journal saves whole
//! blocks, so a block saved from one node's template is a complete, valid
//! block for any node of the same network.

use super::channel::MAX_ACTIVE_JOBS;
use super::journal::PendingBlock;
use super::template::{double_sha256, meets_target, BchTemplate, Coinbase};
use crate::config::MiningNetwork;
use serde_json::{json, Value};
use std::collections::VecDeque;

pub trait NodeRpc {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

/// #### PR #42: what kind of place templates come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A node over JSON-RPC (getblocktemplate).
    NodeRpc,
    /// An SV2 Template Provider (Template Distribution).
    TemplateProvider,
    /// #### PR #42: an ASIC-exclusive token's thread (`merge::source`).
    Token,
}

/// #### PR #42: a place templates come from. The server's node thread works
/// with the first and fails over across the rest in order.
pub trait TemplateSource: Send {
    fn kind(&self) -> SourceKind;
    /// The template last refreshed, with this source's generation.
    fn current(&self) -> Option<(u64, &BchTemplate)>;
    /// Whether the current template still builds on the chain tip.
    fn tip_is_current(&mut self) -> Result<bool, String>;
    /// A fresh template; its generation changes when the template does.
    fn refresh(&mut self) -> Result<(u64, &BchTemplate), String>;
    /// Sends a saved whole block; `Accepted` only on an exact answer.
    fn submit_saved(&mut self, pending: &PendingBlock) -> SubmissionOutcome;
    /// Forgets its templates, as when the server moves to another source.
    fn reset(&mut self);
}

impl<R: NodeRpc + Send> TemplateSource for TemplateProvider<R> {
    fn kind(&self) -> SourceKind {
        SourceKind::NodeRpc
    }
    fn current(&self) -> Option<(u64, &BchTemplate)> {
        TemplateProvider::current(self)
    }
    fn tip_is_current(&mut self) -> Result<bool, String> {
        TemplateProvider::tip_is_current(self)
    }
    fn refresh(&mut self) -> Result<(u64, &BchTemplate), String> {
        TemplateProvider::refresh(self)
    }
    fn submit_saved(&mut self, pending: &PendingBlock) -> SubmissionOutcome {
        TemplateProvider::submit_saved(self, pending)
    }
    fn reset(&mut self) {
        self.current = None;
        self.previous.clear();
    }
}

// Deliberately no Debug: endpoints may contain locally supplied credentials.
#[derive(Clone)]
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
    /// #### PR #42: the node's fork block was checked (once per node).
    chain_proven: bool,
}

impl<R: NodeRpc> TemplateProvider<R> {
    pub fn new(rpc: R, network: MiningNetwork) -> Self {
        Self {
            rpc,
            network,
            generation: 0,
            current: None,
            previous: VecDeque::new(),
            chain_proven: false,
        }
    }

    /// #### PR #40
    /// Takes templates from another node from now on and hands the old one
    /// back in `node`. Work on the old node's templates is revoked, as after
    /// a failed refresh, so devices take a new job from the new node; the
    /// generation keeps counting, so no job identifier repeats.
    pub fn replace_node(&mut self, node: &mut R) {
        std::mem::swap(&mut self.rpc, node);
        self.current = None;
        self.previous.clear();
        self.chain_proven = false;
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
        let hash = hash.to_ascii_lowercase();
        // #### PR #42: a BTC or testnet4 node reports the same chain name;
        // its block at the fork height is another (checked once per node; a
        // node below that height cannot be told yet).
        let (fork, expected) = self.network.fork_block();
        if !self.chain_proven && height >= u64::from(fork) {
            let block = self.rpc.call("getblockhash", json!([fork]))?;
            if !block
                .as_str()
                .is_some_and(|block| block.eq_ignore_ascii_case(expected))
            {
                return Err(format!("node is on {}", self.network.foreign_chain()));
            }
            self.chain_proven = true;
        }
        Ok((height, hash))
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

    // #### PR #42
    // What: a node past the fork height whose block there is not the
    // network's fork block is refused ("testnet4, not Chipnet"; on mainnet
    // "Bitcoin (BTC), not Bitcoin Cash"); the right block is asked once per
    // node, and again after the node is replaced.
    // Look here if: the fork-block check changes.
    #[test]
    fn a_node_with_another_fork_block_is_refused() {
        let synced = json!({"chain":"chip","blocks":325_908,"headers":325_908,
            "bestblockhash":"ab".repeat(32),"initialblockdownload":false});
        let (_, chipnet) = MiningNetwork::Chipnet.fork_block();
        let calls = VecDeque::from([
            ("getblockchaininfo", Ok(synced.clone())),
            (
                "getblockhash",
                Ok(json!("00000000ae25e85d".to_owned() + &"0".repeat(48))),
            ),
        ]);
        let mut provider = TemplateProvider::new(Rpc(calls), MiningNetwork::Chipnet);
        let error = provider.refresh().unwrap_err();
        assert!(error.contains("testnet4, not Chipnet"), "{error}");
        let calls = VecDeque::from([
            ("getblockchaininfo", Ok(synced.clone())),
            ("getblockhash", Ok(json!(chipnet))),
            (
                "getblocktemplate",
                Ok(super::super::template_tests::rpc_template()),
            ),
            ("getblockchaininfo", Ok(synced.clone())),
        ]);
        let mut provider = TemplateProvider::new(Rpc(calls), MiningNetwork::Chipnet);
        provider.refresh().unwrap();
        assert!(provider.chain_proven);
        let mut other = Rpc(VecDeque::new());
        provider.replace_node(&mut other);
        assert!(!provider.chain_proven);
        assert!(MiningNetwork::Mainnet
            .foreign_chain()
            .starts_with("Bitcoin (BTC)"));
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
