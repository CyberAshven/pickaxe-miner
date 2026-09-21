//! Electrum WSS client: live PHOTON baton / MiningJob fetch.
//! Owned by Dev Assist. Search consumes MiningJob; no keys.
//! Runtime winner settlement uses this session for ordered parent/settlement broadcast.

use crate::protocol::{derive_photon_state, EXPECTED_SCRIPT_HASH_HEX, MAINNET_CATEGORY_HEX};
use crate::search::MiningJob;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

/// Rich live job for CLI / win-tx; converts to search::MiningJob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveJob {
    pub url: String,
    pub server_version: Value,
    pub height: u32,
    pub baton_txid: String,
    pub baton_vout: u32,
    pub baton_height: u32,
    pub baton_value_sats: u64,
    pub commitment_hex: String,
    pub token_amount: u128,
    pub age: u32,
    pub target_le_hex: String,
    pub reward_raw: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStateSnapshot {
    pub tip_hash: String,
    pub job: LiveJob,
}

impl LiveJob {
    pub fn to_mining_job(&self, generation_id: u64, payout_address: &str) -> MiningJob {
        MiningJob {
            height: self.height,
            baton_txid: self.baton_txid.clone(),
            baton_vout: self.baton_vout,
            baton_height: self.baton_height,
            baton_value_sats: self.baton_value_sats,
            age: self.age,
            target_le_hex: self.target_le_hex.clone(),
            token_amount: self.token_amount,
            reward_raw: self.reward_raw,
            payout_address: payout_address.to_string(),
            source_identity: self.url.clone(),
            generation_id,
        }
    }

    pub fn print_summary(&self) {
        println!("electrum:      {}", self.url);
        println!("server.version: {}", self.server_version);
        println!("height:        {}", self.height);
        println!("baton:         {}:{}", self.baton_txid, self.baton_vout);
        println!("baton_height:  {}", self.baton_height);
        println!("age:           {}", self.age);
        let t = &self.target_le_hex;
        let head = &t[..t.len().min(8)];
        let tail = &t[t.len().saturating_sub(8)..];
        println!("target_le:     {head}â€¦{tail}");
        println!("token_amount:  {}", self.token_amount);
        println!("reward_raw:    {}", self.reward_raw);
        println!("commitment:    {} bytes", self.commitment_hex.len() / 2);
        println!("donation:      disabled by two-output covenant");
    }
}

pub struct ElectrumSession {
    pub url: String,
    pub server_version: Value,
    ws: Ws,
    next_id: u64,
    buf: String,
}

impl ElectrumSession {
    /// Ban-safe connect: **one endpoint at a time**, exponential backoff between
    /// tries, never parallel fan-out. Custom URL should already be first in `endpoints`.
    pub fn connect_failover(endpoints: &[String]) -> Result<Self, String> {
        if endpoints.is_empty() {
            return Err("no Electrum/Fulcrum endpoints configured".into());
        }
        let mut failures = Vec::new();
        let mut backoff_ms: u64 = 400;
        for (i, url) in endpoints.iter().enumerate() {
            if i > 0 {
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
            }
            match Self::connect_one(url) {
                Ok(s) => return Ok(s),
                Err(e) => failures.push(format!("{url}: {e}")),
            }
        }
        Err(format!(
            "All Electrum/Fulcrum endpoints failed (sequential, ban-safe):\n{}",
            failures.join("\n")
        ))
    }

    fn connect_one(url_str: &str) -> Result<Self, String> {
        let (ws, _resp) = connect(url_str).map_err(|e| format!("connect: {e}"))?;

        match ws.get_ref() {
            MaybeTlsStream::NativeTls(t) => {
                let _ = t.get_ref().set_read_timeout(Some(Duration::from_secs(15)));
                let _ = t.get_ref().set_write_timeout(Some(Duration::from_secs(15)));
            }
            MaybeTlsStream::Plain(t) => {
                let _ = t.set_read_timeout(Some(Duration::from_secs(15)));
                let _ = t.set_write_timeout(Some(Duration::from_secs(15)));
            }
            _ => {}
        }

        let mut session = Self {
            url: url_str.to_string(),
            server_version: Value::Null,
            ws,
            next_id: 1,
            buf: String::new(),
        };

        let ver = session.rpc("server.version", json!(["pickaxe-miner", "1.4.1"]))?;
        session.server_version = ver;
        Ok(session)
    }

    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = format!("{req}\n");
        self.ws
            .send(Message::Text(line))
            .map_err(|e| format!("send: {e}"))?;

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if std::time::Instant::now() > deadline {
                return Err(format!("rpc timeout waiting for id={id} ({method})"));
            }
            let msg = self.ws.read().map_err(|e| format!("read: {e}"))?;
            match msg {
                Message::Text(t) => {
                    self.buf.push_str(&t);
                    while let Some(pos) = self.buf.find('\n') {
                        let line = self.buf[..pos].trim().to_string();
                        self.buf = self.buf[pos + 1..].to_string();
                        if line.is_empty() {
                            continue;
                        }
                        let v: Value = serde_json::from_str(&line)
                            .map_err(|e| format!("json: {e}: {}", &line[..line.len().min(160)]))?;
                        if id_matches(&v, id) {
                            if let Some(err) = v.get("error") {
                                if !err.is_null() {
                                    return Err(format!("rpc error: {err}"));
                                }
                            }
                            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                        }
                    }
                    let trimmed = self.buf.trim().to_string();
                    if trimmed.starts_with('{') && trimmed.ends_with('}') {
                        if let Ok(v) = serde_json::from_str::<Value>(&trimmed) {
                            if id_matches(&v, id) {
                                self.buf.clear();
                                if let Some(err) = v.get("error") {
                                    if !err.is_null() {
                                        return Err(format!("rpc error: {err}"));
                                    }
                                }
                                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                            }
                        }
                    }
                }
                Message::Ping(p) => {
                    let _ = self.ws.send(Message::Pong(p));
                }
                Message::Close(_) => return Err("socket closed".into()),
                _ => {}
            }
        }
    }

    /// Submit a raw tx hex via lockchain.transaction.broadcast. Explicit only.
    pub fn broadcast_raw(&mut self, raw_tx_hex: &str) -> Result<String, String> {
        let hex = raw_tx_hex.trim();
        if hex.is_empty() || !hex.len().is_multiple_of(2) {
            return Err("raw tx hex empty or odd length".into());
        }
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("raw tx must be hex".into());
        }
        let v = self.rpc("blockchain.transaction.broadcast", json!([hex]))?;
        match v {
            Value::String(txid) => Ok(txid),
            other => Ok(other.to_string()),
        }
    }

    /// Return true when this server can retrieve the transaction by txid.
    /// Used only to make retrying a previously journaled submission idempotent.
    pub fn transaction_known(&mut self, txid: &str) -> Result<bool, String> {
        if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("transaction id must be exactly 64 hexadecimal characters".into());
        }
        match self.rpc("blockchain.transaction.get", json!([txid, false])) {
            Ok(Value::String(_)) => Ok(true),
            Ok(Value::Null) => Ok(false),
            Ok(_) => Ok(true),
            Err(error) => {
                let lower = error.to_ascii_lowercase();
                if lower.contains("no such") || lower.contains("not found") {
                    Ok(false)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub fn fetch_live_job(&mut self) -> Result<LiveJob, String> {
        let header = self.rpc("blockchain.headers.subscribe", json!([]))?;
        let unspent = self.rpc(
            "blockchain.scripthash.listunspent",
            json!([EXPECTED_SCRIPT_HASH_HEX, "include_tokens"]),
        )?;
        live_job_from_fulcrum_values(&self.url, self.server_version.clone(), &header, &unspent)
    }

    pub fn fetch_live_snapshot(&mut self) -> Result<LiveStateSnapshot, String> {
        let header_before = self.rpc("blockchain.headers.subscribe", json!([]))?;
        let unspent = self.rpc(
            "blockchain.scripthash.listunspent",
            json!([EXPECTED_SCRIPT_HASH_HEX, "include_tokens"]),
        )?;
        let header_after = self.rpc("blockchain.headers.subscribe", json!([]))?;

        let tip_hash = stable_fulcrum_tip_hash(&header_before, &header_after)?;

        let job = live_job_from_fulcrum_values(
            &self.url,
            self.server_version.clone(),
            &header_after,
            &unspent,
        )?;
        Ok(LiveStateSnapshot { tip_hash, job })
    }
}

fn fulcrum_header_height(header: &Value) -> Result<u32, String> {
    header
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "Fulcrum header response omitted a valid height".to_string())
}

fn stable_fulcrum_tip_hash(before: &Value, after: &Value) -> Result<String, String> {
    let before_hash = fulcrum_header_hash(before)?;
    let after_hash = fulcrum_header_hash(after)?;
    let before_height = fulcrum_header_height(before)?;
    let after_height = fulcrum_header_height(after)?;
    if before_height != after_height || !before_hash.eq_ignore_ascii_case(&after_hash) {
        return Err("Fulcrum PHOTON snapshot changed tip while reading token state".into());
    }
    Ok(after_hash)
}

pub(crate) fn fulcrum_header_hash(header: &Value) -> Result<String, String> {
    let header_hex = header
        .get("hex")
        .and_then(Value::as_str)
        .ok_or("Fulcrum header response omitted hex")?;
    let header_bytes =
        hex::decode(header_hex).map_err(|error| format!("invalid Fulcrum header hex: {error}"))?;
    if header_bytes.len() != 80 {
        return Err(format!(
            "Fulcrum header must be exactly 80 bytes; got {}",
            header_bytes.len()
        ));
    }
    let first = Sha256::digest(&header_bytes);
    let second = Sha256::digest(first);
    Ok(second
        .iter()
        .rev()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(crate) fn live_job_from_fulcrum_values(
    url: &str,
    server_version: Value,
    header: &Value,
    unspent: &Value,
) -> Result<LiveJob, String> {
    let height = header
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("Invalid live BCH header response")?;
    let arr = unspent
        .as_array()
        .ok_or("Invalid live PHOTON UTXO response")?;

    let batons = arr
        .iter()
        .filter(|utxo| {
            let category = utxo
                .pointer("/token_data/category")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let capability = utxo
                .pointer("/token_data/nft/capability")
                .and_then(Value::as_str)
                .unwrap_or("");
            category == MAINNET_CATEGORY_HEX && capability == "mutable"
        })
        .collect::<Vec<_>>();

    if batons.len() != 1 {
        return Err(format!(
            "Expected exactly one live PHOTON baton; found {}",
            batons.len()
        ));
    }
    let baton = batons[0];
    let baton_txid = baton
        .get("tx_hash")
        .and_then(Value::as_str)
        .ok_or("baton missing tx_hash")?
        .to_ascii_lowercase();
    let baton_vout = baton
        .get("tx_pos")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("baton missing tx_pos")?;
    let baton_height = baton
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let baton_value_sats = baton.get("value").and_then(Value::as_u64).unwrap_or(0);
    let commitment_hex = baton
        .pointer("/token_data/nft/commitment")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let token_amount_str = baton
        .pointer("/token_data/amount")
        .and_then(Value::as_str)
        .unwrap_or("0");
    let token_amount = token_amount_str
        .parse::<u128>()
        .map_err(|_| format!("bad token amount: {token_amount_str}"))?;
    let derived = derive_photon_state(&commitment_hex, token_amount, height, baton_height)?;

    Ok(LiveJob {
        url: url.to_string(),
        server_version,
        height,
        baton_txid,
        baton_vout,
        baton_height,
        baton_value_sats,
        commitment_hex,
        token_amount,
        age: derived.age,
        target_le_hex: derived.target_le_hex,
        reward_raw: derived.reward_raw,
    })
}

fn id_matches(v: &Value, id: u64) -> bool {
    v.get("id").and_then(|x| x.as_u64()) == Some(id)
        || v.get("id").and_then(|x| x.as_i64()).map(|x| x as u64) == Some(id)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fulcrum_header_hash_matches_genesis_header() {
        let header = json!({
            "height": 0,
            "hex": "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c"
        });
        assert_eq!(
            fulcrum_header_hash(&header).unwrap(),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
    }

    #[test]
    fn fulcrum_snapshot_requires_stable_height_and_tip_hash() {
        let before = json!({
            "height": 0,
            "hex": "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c"
        });
        assert!(stable_fulcrum_tip_hash(&before, &before).is_ok());

        let mut changed_height = before.clone();
        changed_height["height"] = json!(1);
        assert!(stable_fulcrum_tip_hash(&before, &changed_height).is_err());

        let mut changed_header = before.clone();
        let mut header_hex = before["hex"].as_str().unwrap().to_string();
        header_hex.replace_range(0..2, "02");
        changed_header["hex"] = json!(header_hex);
        assert!(stable_fulcrum_tip_hash(&before, &changed_header).is_err());
    }

    #[test]
    fn fulcrum_header_hash_rejects_non_header_payloads() {
        assert!(fulcrum_header_hash(&json!({"height": 1, "hex": "00"})).is_err());
        assert!(fulcrum_header_hash(&json!({"height": 1})).is_err());
    }
}
