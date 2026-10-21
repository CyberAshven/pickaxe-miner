//! A bounded, in-memory index of ELECTRIC's watched scripts. Confirmed coins
//! come from one UTXO scan, then new blocks; unconfirmed coins are reconstructed
//! from the current mempool, so eviction/replacement restores the base view.

use crate::fee::bch_value_to_sats;
use crate::node::rpc_call;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

type Outpoint = (String, u32);
type Coins = BTreeMap<Outpoint, Value>;
const MAX_MEMPOOL: usize = 4_096;
const MAX_BLOCK_GAP: u32 = 24;
const VIEW_LIFETIME: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tip {
    height: u32,
    hash: String,
}

#[derive(Default)]
struct Index {
    tip: Option<Tip>,
    confirmed: Coins,
    mempool: BTreeMap<String, Value>,
    view: Coins,
}

pub(super) struct Watch {
    url: String,
    scripts: BTreeSet<String>,
    index: Index,
    refreshed: Option<Instant>,
}

impl Watch {
    pub fn connect(url: &str, scripts: &[Vec<u8>]) -> Result<Self, String> {
        let mut watch = Self {
            url: url.into(),
            scripts: scripts.iter().map(hex::encode).collect(),
            index: Index::default(),
            refreshed: None,
        };
        watch.refresh()?;
        Ok(watch)
    }

    fn refresh(&mut self) -> Result<(), String> {
        if self
            .refreshed
            .is_some_and(|at| at.elapsed() < VIEW_LIFETIME)
        {
            return Ok(());
        }
        self.index.update(&self.scripts, |method, params| {
            rpc_call(&self.url, method, params)
        })?;
        self.refreshed = Some(Instant::now());
        Ok(())
    }

    pub fn tip(&mut self) -> Result<u32, String> {
        self.refresh()?;
        Ok(self.index.tip.as_ref().ok_or("node has no tip")?.height)
    }

    pub fn unspent(&mut self, locking: &[u8]) -> Result<Vec<Value>, String> {
        let script = hex::encode(locking);
        if !self.scripts.contains(&script) {
            return Err("ELECTRIC node source was not given this watch script".into());
        }
        self.refresh()?;
        Ok(self
            .index
            .view
            .values()
            .filter(|coin| coin["script"].as_str() == Some(&script))
            .cloned()
            .collect())
    }

    pub fn relay_fee(&self) -> Result<u64, String> {
        let relay = rpc_call(&self.url, "getnetworkinfo", json!([]))?;
        let mempool = rpc_call(&self.url, "getmempoolinfo", json!([]))?;
        Ok(bch_value_to_sats(&relay["relayfee"])?
            .max(bch_value_to_sats(&mempool["mempoolminfee"])?))
    }

    pub fn transaction(&self, txid: &str) -> Result<String, String> {
        // This shared lookup locates confirmed transactions through an unspent
        // output or recent blocks when txindex is off. Unknown is an error for
        // claim recovery: never retire a journal on missing transaction data.
        crate::electrum::node_raw_transaction(&self.url, txid)?
            .ok_or_else(|| "no such mempool or blockchain transaction".into())
    }

    pub fn broadcast(&mut self, raw: &str) -> Result<String, String> {
        self.refreshed = None;
        rpc_call(&self.url, "sendrawtransaction", json!([raw]))?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "node returned no broadcast transaction id".into())
    }
}

impl Index {
    fn update(
        &mut self,
        scripts: &BTreeSet<String>,
        mut rpc: impl FnMut(&str, Value) -> Result<Value, String>,
    ) -> Result<(), String> {
        let tip = read_tip(&mut rpc)?;
        let mut base = self.confirmed.clone();
        let mut rebuild = self
            .tip
            .as_ref()
            .is_none_or(|old| tip.height < old.height || tip.height - old.height > MAX_BLOCK_GAP);
        if let Some(old) = &self.tip {
            if !rebuild {
                rebuild = rpc("getblockhash", json!([old.height]))?.as_str() != Some(&old.hash);
            }
        }
        if rebuild {
            base.clear();
            if !scripts.is_empty() {
                let descriptors: Vec<_> = scripts.iter().map(|s| format!("raw({s})")).collect();
                let scan = rpc("scantxoutset", json!(["start", descriptors]))?;
                if scan["success"] != true {
                    return Err("ELECTRIC node UTXO scan did not complete".into());
                }
                // BCHN 29.1 omits height/bestblock; pin its result with the
                // blockchain-info reads before and after the scan instead.
                if scan
                    .get("bestblock")
                    .is_some_and(|hash| hash.as_str() != Some(&tip.hash))
                {
                    return Err("node tip changed during ELECTRIC UTXO scan".into());
                }
                for item in scan["unspents"]
                    .as_array()
                    .ok_or("node scan omitted unspents")?
                {
                    let script = item["scriptPubKey"]
                        .as_str()
                        .ok_or("scan output has no script")?;
                    if !scripts.contains(&script.to_ascii_lowercase()) {
                        return Err("node scan returned an unwatched script".into());
                    }
                    let key = outpoint(&item["txid"], &item["vout"])?;
                    let coin = coin(&key, script, &item["amount"], &item["tokenData"])?;
                    if base.insert(key, coin).is_some() {
                        return Err("node scan returned a duplicate outpoint".into());
                    }
                }
            }
        } else if let Some(old) = &self.tip {
            let mut previous = old.hash.clone();
            for height in old.height.saturating_add(1)..=tip.height {
                let hash = rpc("getblockhash", json!([height]))?;
                let block = rpc("getblock", json!([hash, 2]))?;
                if block["hash"] != hash
                    || block["height"].as_u64() != Some(u64::from(height))
                    || block["previousblockhash"].as_str() != Some(&previous)
                {
                    return Err("node changed chain while reading ELECTRIC blocks".into());
                }
                let transactions = block["tx"].as_array().ok_or("block has no transactions")?;
                apply_transactions(&mut base, transactions.iter(), scripts)?;
                previous = hash.as_str().ok_or("block hash is not a string")?.into();
            }
        }
        let mut known = self.mempool.clone();
        // Ordinary mempool churn is retried without another full UTXO scan.
        // Never mix two views, or keep retrying without a bound.
        for _ in 0..3 {
            let ids = mempool_ids(rpc("getrawmempool", json!([false]))?)?;
            let mut mempool = BTreeMap::new();
            let mut vanished = false;
            for id in &ids {
                let tx = match known.get(id) {
                    Some(tx) => tx.clone(),
                    None => match rpc("getrawtransaction", json!([id, true])) {
                        Ok(tx) => tx,
                        Err(error) if error.to_ascii_lowercase().contains("no such") => {
                            vanished = true;
                            break;
                        }
                        Err(error) => return Err(error),
                    },
                };
                if tx["txid"].as_str() != Some(id) {
                    return Err("node returned another mempool transaction".into());
                }
                mempool.insert(id.clone(), tx);
            }
            let stable_pool = mempool_ids(rpc("getrawmempool", json!([false]))?)? == ids;
            if read_tip(&mut rpc)? != tip {
                return Err("node tip changed during ELECTRIC snapshot".into());
            }
            if vanished || !stable_pool {
                known.extend(mempool);
                continue;
            }
            let mut view = base.clone();
            apply_transactions(&mut view, mempool.values(), scripts)?;
            // Publish only a complete view; failed reads preserve old state.
            self.tip = Some(tip);
            self.confirmed = base;
            self.mempool = mempool;
            self.view = view;
            return Ok(());
        }
        Err("node mempool kept changing during ELECTRIC snapshot".into())
    }
}

fn read_tip(rpc: &mut impl FnMut(&str, Value) -> Result<Value, String>) -> Result<Tip, String> {
    let info = rpc("getblockchaininfo", json!([]))?;
    if info["initialblockdownload"].as_bool() != Some(false) {
        return Err("ELECTRIC node is syncing or omitted initialblockdownload".into());
    }
    Ok(Tip {
        height: info["blocks"]
            .as_u64()
            .and_then(|h| u32::try_from(h).ok())
            .ok_or("node omitted block height")?,
        hash: hash(&info["bestblockhash"])?,
    })
}

fn hash(value: &Value) -> Result<String, String> {
    let text = value.as_str().ok_or("node hash is not a string")?;
    if text.len() != 64 || !text.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("node hash is not 32-byte hex".into());
    }
    Ok(text.to_ascii_lowercase())
}

fn outpoint(txid: &Value, vout: &Value) -> Result<Outpoint, String> {
    Ok((
        hash(txid)?,
        vout.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("node output index is not u32")?,
    ))
}

fn coin(key: &Outpoint, script: &str, value: &Value, token: &Value) -> Result<Value, String> {
    hex::decode(script).map_err(|_| "node script is not hex")?;
    if !token.is_null() && !token.is_object() {
        return Err("node tokenData is not an object".into());
    }
    Ok(
        json!({"tx_hash":key.0, "tx_pos":key.1, "value":bch_value_to_sats(value)?,
        "script":script.to_ascii_lowercase(), "token_data":token}),
    )
}

fn mempool_ids(value: Value) -> Result<BTreeSet<String>, String> {
    let list = value.as_array().ok_or("node mempool is not a list")?;
    if list.len() > MAX_MEMPOOL {
        return Err(format!(
            "ELECTRIC node mempool exceeds {MAX_MEMPOOL} transactions; use Fulcrum"
        ));
    }
    let ids: BTreeSet<_> = list.iter().map(hash).collect::<Result<_, _>>()?;
    if ids.len() != list.len() {
        return Err("node mempool has duplicate transaction ids".into());
    }
    Ok(ids)
}

fn apply_transactions<'a>(
    coins: &mut Coins,
    transactions: impl Iterator<Item = &'a Value>,
    scripts: &BTreeSet<String>,
) -> Result<(), String> {
    let mut spent = BTreeSet::new();
    let mut added = Coins::new();
    for tx in transactions {
        let txid = hash(&tx["txid"])?;
        for input in tx["vin"]
            .as_array()
            .ok_or("node transaction has no inputs")?
        {
            if input.get("coinbase").is_none() {
                let key = outpoint(&input["txid"], &input["vout"])?;
                if !spent.insert(key) {
                    return Err("node snapshot contains a double spend".into());
                }
            }
        }
        for output in tx["vout"]
            .as_array()
            .ok_or("node transaction has no outputs")?
        {
            let script = output["scriptPubKey"]["hex"]
                .as_str()
                .ok_or("node output has no script")?;
            if scripts.contains(&script.to_ascii_lowercase()) {
                let key = outpoint(&Value::String(txid.clone()), &output["n"])?;
                let value = coin(&key, script, &output["value"], &output["tokenData"])?;
                if added.insert(key, value).is_some() {
                    return Err("node transaction repeats an output".into());
                }
            }
        }
    }
    coins.extend(added);
    coins.retain(|key, _| !spent.contains(key));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "51";
    fn id(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }
    fn tip(height: u32, byte: u8) -> Value {
        json!({"blocks":height,"bestblockhash":id(byte),"initialblockdownload":false})
    }
    fn output(n: u32, script: &str, value: &str, token: Value) -> Value {
        json!({"n":n,"scriptPubKey":{"hex":script},"value":value,"tokenData":token})
    }
    fn tx(byte: u8, parent: u8, vout: u32, outputs: Vec<Value>) -> Value {
        json!({"txid":id(byte),"vin":[{"txid":id(parent),"vout":vout}],"vout":outputs})
    }
    fn scan() -> Value {
        json!({"success":true,"unspents":[{"txid":id(1),"vout":0,
            "scriptPubKey":SCRIPT,"amount":"0.00010000"}]})
    }
    fn scripts() -> BTreeSet<String> {
        [SCRIPT.into()].into()
    }
    fn update(
        index: &mut Index,
        chain_tip: Value,
        scan: Value,
        pool: Vec<Value>,
        block: Option<Value>,
    ) -> Result<usize, String> {
        let mut scans = 0;
        index.update(&scripts(), |method, params| match method {
            "getblockchaininfo" => Ok(chain_tip.clone()),
            "getblockhash" if params[0] == 100 => Ok(json!(id(10))),
            "getblockhash" => Ok(chain_tip["bestblockhash"].clone()),
            "scantxoutset" => {
                scans += 1;
                Ok(scan.clone())
            }
            "getrawmempool" => Ok(Value::Array(
                pool.iter().map(|t| t["txid"].clone()).collect(),
            )),
            "getrawtransaction" => pool
                .iter()
                .find(|t| t["txid"] == params[0])
                .cloned()
                .ok_or("no such transaction".into()),
            "getblock" => block.clone().ok_or("block unavailable".into()),
            other => Err(format!("unexpected RPC {other}")),
        })?;
        Ok(scans)
    }

    #[test]
    fn bootstrap_overlays_unconfirmed_successors_and_pot_funding_in_any_order() {
        let mut index = Index::default();
        // Child sorts before parent: discovery must not depend on txid order.
        let parent = tx(
            9,
            1,
            0,
            vec![output(
                1,
                SCRIPT,
                "0.00009000",
                json!({"category":id(42),"amount":"99","nft":{"capability":"mutable","commitment":"010000000000000000"}}),
            )],
        );
        let child = tx(2, 9, 1, vec![output(1, SCRIPT, "0.00008000", Value::Null)]);
        let funding = tx(8, 7, 0, vec![output(0, SCRIPT, "0.00001000", Value::Null)]);
        assert_eq!(
            update(
                &mut index,
                tip(100, 10),
                scan(),
                vec![child, parent, funding],
                None
            )
            .unwrap(),
            1
        );
        assert_eq!(index.view.len(), 2);
        assert_eq!(index.view[&(id(2), 1)]["value"], 8000);
        assert_eq!(index.view[&(id(8), 0)]["value"], 1000);
        assert!(!index.view.contains_key(&(id(1), 0)));
    }

    #[test]
    fn eviction_and_replacement_restore_confirmed_coins_without_rescanning() {
        let mut index = Index::default();
        let spend = tx(3, 1, 0, vec![output(0, "52", "0.00009000", Value::Null)]);
        update(&mut index, tip(100, 10), scan(), vec![spend], None).unwrap();
        assert!(index.view.is_empty());
        assert_eq!(
            update(&mut index, tip(100, 10), scan(), vec![], None).unwrap(),
            0
        );
        assert_eq!(index.view[&(id(1), 0)]["value"], 10_000);
        let replacement = tx(4, 1, 0, vec![output(0, SCRIPT, "0.00008500", Value::Null)]);
        update(&mut index, tip(100, 10), scan(), vec![replacement], None).unwrap();
        assert_eq!(index.view.len(), 1);
        assert_eq!(index.view[&(id(4), 0)]["value"], 8500);
    }

    #[test]
    fn confirmation_updates_base_from_blocks_without_txindex_or_rescan() {
        let mut index = Index::default();
        let claim = tx(3, 1, 0, vec![output(1, SCRIPT, "0.00009000", Value::Null)]);
        update(&mut index, tip(100, 10), scan(), vec![claim.clone()], None).unwrap();
        let block = json!({"hash":id(11),"height":101,"previousblockhash":id(10),"tx":[claim]});
        assert_eq!(
            update(&mut index, tip(101, 11), scan(), vec![], Some(block)).unwrap(),
            0
        );
        assert_eq!(index.confirmed[&(id(3), 1)]["value"], 9000);
        assert!(!index.confirmed.contains_key(&(id(1), 0)));
        assert!(index.mempool.is_empty());
    }

    #[test]
    fn reorg_discards_old_outputs_and_rebuilds_from_utxo_scan() {
        let mut index = Index::default();
        update(&mut index, tip(100, 10), scan(), vec![], None).unwrap();
        let mut scans = 0;
        index
            .update(&scripts(), |method, _| match method {
                "getblockchaininfo" => Ok(tip(100, 11)),
                "getblockhash" => Ok(json!(id(11))),
                "scantxoutset" => {
                    scans += 1;
                    Ok(json!({"success":true,"unspents":[]}))
                }
                "getrawmempool" => Ok(json!([])),
                _ => panic!("unexpected {method}"),
            })
            .unwrap();
        assert_eq!(scans, 1);
        assert!(index.view.is_empty());
    }

    #[test]
    fn moving_tip_or_mempool_never_publishes_partial_snapshot() {
        for change_tip in [false, true] {
            let mut index = Index::default();
            update(&mut index, tip(100, 10), scan(), vec![], None).unwrap();
            let saved = index.view.clone();
            let (mut tips, mut pools) = (0, 0);
            assert!(index
                .update(&scripts(), |method, _| match method {
                    "getblockchaininfo" => {
                        tips += 1;
                        Ok(tip(100, if change_tip && tips > 1 { 11 } else { 10 }))
                    }
                    "getblockhash" => Ok(json!(id(10))),
                    "getrawmempool" => {
                        pools += 1;
                        Ok(if !change_tip && pools % 2 == 0 {
                            json!([id(8)])
                        } else {
                            json!([])
                        })
                    }
                    _ => panic!("unexpected {method}"),
                })
                .is_err());
            assert_eq!(index.view, saved);
            assert_eq!(index.tip.as_ref().unwrap().hash, id(10));
        }
    }

    #[test]
    fn malformed_scan_syncing_node_and_excessive_mempool_fail_closed() {
        for bad in [
            json!({"success":false}),
            json!({"success":true}),
            json!({"success":true,"unspents":[{"txid":id(1),"vout":0,"scriptPubKey":"52","amount":"0.1"}]}),
            json!({"success":true,"unspents":[{"txid":id(1),"vout":0,"scriptPubKey":SCRIPT,"amount":"0.000000001"}]}),
        ] {
            let mut index = Index::default();
            assert!(update(&mut index, tip(100, 10), bad, vec![], None).is_err());
            assert!(index.tip.is_none());
        }
        assert!(read_tip(&mut |_, _| Ok(json!({"initialblockdownload":true}))).is_err());
        assert!(mempool_ids(json!(vec![id(1); MAX_MEMPOOL + 1])).is_err());
        assert!(mempool_ids(json!([id(1), id(1)])).is_err());
    }

    #[test]
    fn token_outputs_remain_distinguishable_from_plain_bch_and_double_spends_rejected() {
        let token = json!({"category":id(42),"amount":"9007199254740993"});
        let mut index = Index::default();
        let tx = tx(
            3,
            1,
            0,
            vec![output(0, SCRIPT, "0.00001000", token.clone())],
        );
        update(&mut index, tip(100, 10), scan(), vec![tx.clone()], None).unwrap();
        assert_eq!(index.view[&(id(3), 0)]["token_data"], token);
        let double = self::tx(4, 1, 0, vec![]);
        assert!(update(&mut index, tip(100, 10), scan(), vec![tx, double], None).is_err());
        assert_eq!(index.view.len(), 1);
    }
}
