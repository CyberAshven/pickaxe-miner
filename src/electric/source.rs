//! #### PR #65: ELECTRIC uses the same claim path with BCHN or Fulcrum.
//! Node discovery watches public scripts, never imports keys or a node wallet.

mod native;
#[cfg(test)]
mod tests;

use crate::config::{JobSource, MiningToken, RuntimeConfig};
use crate::electrum::ElectrumSession;
use serde_json::{json, Value};

enum Link {
    Node(Box<native::Watch>),
    Fulcrum(ElectrumSession),
}

pub(super) struct Session {
    /// Always redacted, including status and reconnect messages.
    pub url: String,
    pub fallback_reason: Option<String>,
    link: Link,
}

impl Session {
    pub fn retry_snapshot(&mut self, error: &str) -> bool {
        match &mut self.link {
            Link::Node(watch) => watch.retry_snapshot(error),
            Link::Fulcrum(_) => false,
        }
    }

    pub fn connect(cfg: &RuntimeConfig, scripts: &[Vec<u8>]) -> Result<Self, String> {
        let mut failures = Vec::new();
        for url in cfg.node_endpoints() {
            let result = crate::node::verify_chain(&url, cfg.network)
                .and_then(|()| native::Watch::connect(&url, scripts));
            match result {
                Ok(watch) => {
                    return Ok(Self {
                        url: crate::node::redact_url(&url),
                        fallback_reason: None,
                        link: Link::Node(Box::new(watch)),
                    });
                }
                Err(error) => failures.push(format!("{}: {error}", crate::node::redact_url(&url))),
            }
        }
        if cfg.source == JobSource::Node {
            return Err(if failures.is_empty() {
                "ELECTRIC node source needs a BCH node RPC endpoint".into()
            } else {
                failures.join("; ")
            });
        }
        let fallback_reason = (!failures.is_empty()).then(|| failures.join("; "));
        let session = ElectrumSession::connect_failover_for_deployment(
            &cfg.electrum_endpoints(),
            MiningToken::Photon.photon_deployment(cfg.network),
        )
        .map_err(|error| match &fallback_reason {
            Some(reason) => format!("ELECTRIC node: {reason}; Fulcrum: {error}"),
            None => error,
        })?;
        Ok(Self {
            url: crate::node::redact_url(&session.url),
            fallback_reason,
            link: Link::Fulcrum(session),
        })
    }

    pub fn tip(&mut self) -> Result<u32, String> {
        match &mut self.link {
            Link::Node(watch) => watch.tip(),
            Link::Fulcrum(session) => session.rpc("blockchain.headers.subscribe", json!([]))?
                ["height"]
                .as_u64()
                .and_then(|height| u32::try_from(height).ok())
                .ok_or_else(|| "Fulcrum's tip has no height".into()),
        }
    }

    pub fn relay_fee(&mut self) -> Result<u64, String> {
        match &mut self.link {
            Link::Node(watch) => watch.relay_fee(),
            Link::Fulcrum(session) => {
                crate::fee::bch_value_to_sats(&session.rpc("blockchain.relayfee", json!([]))?)
            }
        }
    }

    pub fn unspent(&mut self, locking: &[u8]) -> Result<Vec<Value>, String> {
        match &mut self.link {
            Link::Node(watch) => watch.unspent(locking),
            Link::Fulcrum(session) => session
                .rpc(
                    "blockchain.scripthash.listunspent",
                    json!([super::script_hash_hex(locking), "include_tokens"]),
                )?
                .as_array()
                .cloned()
                .ok_or_else(|| "Fulcrum's listunspent is not a list".into()),
        }
    }

    pub fn transaction(&mut self, txid: &str) -> Result<String, String> {
        match &mut self.link {
            Link::Node(watch) => watch.transaction(txid),
            Link::Fulcrum(session) => session
                .rpc("blockchain.transaction.get", json!([txid, false]))?
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "server returned invalid transaction data".into()),
        }
    }

    pub fn broadcast_raw(&mut self, raw: &str) -> Result<String, String> {
        match &mut self.link {
            Link::Node(watch) => watch.broadcast(raw),
            Link::Fulcrum(session) => session.broadcast_raw(raw),
        }
    }
}
