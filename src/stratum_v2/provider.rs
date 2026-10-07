//! #### PR #38
//! Pin full-template submission to its source node. A failed refresh revokes
//! work; a light-job payload must never be sent as a full block on failover.

use super::channel::MAX_ACTIVE_JOBS;
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
        let template = BchTemplate::from_rpc(&raw)?;
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
