//! #### PR #65: ELECTRIC uses the same claim path with BCHN or Fulcrum.
//! Node discovery watches public scripts, never imports keys or a node wallet.

mod native;
#[cfg(test)]
pub(super) mod tests;

use crate::config::{JobSource, MiningToken, RuntimeConfig};
use crate::electrum::ElectrumSession;
use serde_json::{json, Value};

enum Link {
    Node(Box<native::Watch>),
    Fulcrum(ElectrumSession),
}

type Outpoint = (String, u32);

/// #### PR #65: what spent a watched output, as far as the source can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Spender {
    /// Still spendable, confirmed or not.
    Unspent,
    /// An unconfirmed transaction, which can still be evicted or replaced.
    Mempool(String),
    /// A transaction in a block on the source's best chain.
    Confirmed(String),
    /// Not found within the source's bounded search.
    Unknown,
}

/// Newest history entries Fulcrum fetches when looking for a spend.
const SPEND_SEARCH_TRANSACTIONS: usize = 64;

/// The outpoints a serialized transaction spends.
fn spent_outpoints(raw: &[u8]) -> Result<Vec<Outpoint>, String> {
    fn compact(raw: &[u8], at: &mut usize) -> Result<usize, String> {
        let first = *raw.get(*at).ok_or("truncated transaction")?;
        *at += 1;
        let width = match first {
            0xfd => 2,
            0xfe => 4,
            0xff => 8,
            n => return Ok(n.into()),
        };
        let bytes = raw.get(*at..*at + width).ok_or("truncated transaction")?;
        *at += width;
        let mut value = [0u8; 8];
        value[..width].copy_from_slice(bytes);
        usize::try_from(u64::from_le_bytes(value)).map_err(|_| "transaction size overflow".into())
    }
    let mut at = 4;
    let count = compact(raw, &mut at)?;
    if count > raw.len() / 41 {
        return Err("transaction input count exceeds its size".into());
    }
    let mut spent = Vec::with_capacity(count);
    for _ in 0..count {
        let mut txid = raw
            .get(at..at + 32)
            .ok_or("truncated transaction input")?
            .to_vec();
        txid.reverse();
        let vout = raw
            .get(at + 32..at + 36)
            .ok_or("truncated transaction input")?;
        at += 36;
        let script = compact(raw, &mut at)?;
        at = at
            .checked_add(script)
            .and_then(|end| end.checked_add(4))
            .filter(|end| *end <= raw.len())
            .ok_or("truncated transaction input")?;
        spent.push((
            hex::encode(txid),
            u32::from_le_bytes([vout[0], vout[1], vout[2], vout[3]]),
        ));
    }
    Ok(spent)
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

    /// #### PR #65: what spent `target`, an output of `locking`. Fulcrum
    /// searches the script's newest history; a node, its mempool and recent
    /// blocks. Anything else is `Unknown`, never a confirmed spend.
    pub fn spender(&mut self, locking: &[u8], target: &Outpoint) -> Result<Spender, String> {
        let session = match &mut self.link {
            Link::Node(watch) => return watch.spender(target),
            Link::Fulcrum(session) => session,
        };
        let script_hash = super::script_hash_hex(locking);
        let unspent = session.rpc(
            "blockchain.scripthash.listunspent",
            json!([script_hash, "include_tokens"]),
        )?;
        let unspent = unspent
            .as_array()
            .ok_or("Fulcrum's listunspent is not a list")?;
        if unspent.iter().any(|item| {
            item["tx_hash"]
                .as_str()
                .is_some_and(|txid| txid.eq_ignore_ascii_case(&target.0))
                && item["tx_pos"].as_u64() == Some(u64::from(target.1))
        }) {
            return Ok(Spender::Unspent);
        }
        let history = session.rpc("blockchain.scripthash.get_history", json!([script_hash]))?;
        let history = history
            .as_array()
            .ok_or("Fulcrum's history is not a list")?;
        // History lists confirmed entries by height, then the mempool.
        for entry in history.iter().rev().take(SPEND_SEARCH_TRANSACTIONS) {
            let txid = entry["tx_hash"]
                .as_str()
                .filter(|txid| txid.len() == 64 && txid.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or("Fulcrum's history has an invalid transaction id")?
                .to_ascii_lowercase();
            let height = entry["height"]
                .as_i64()
                .ok_or("Fulcrum's history has no height")?;
            if txid.eq_ignore_ascii_case(&target.0) {
                continue;
            }
            let raw = session
                .rpc("blockchain.transaction.get", json!([txid, false]))?
                .as_str()
                .map(hex::decode)
                .and_then(Result::ok)
                .ok_or("server returned invalid transaction data")?;
            if crate::reward::transaction_id(&raw) != txid {
                return Err("server returned another transaction for this id".into());
            }
            if spent_outpoints(&raw)?.contains(target) {
                return Ok(if height > 0 {
                    Spender::Confirmed(txid)
                } else {
                    Spender::Mempool(txid)
                });
            }
        }
        Ok(Spender::Unknown)
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
