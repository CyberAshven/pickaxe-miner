//! Electrum WSS client: live PHOTON baton / MiningJob fetch.
//! Owned by Dev Assist. Search consumes MiningJob; no keys.
//! Runtime winner settlement uses this session for ordered parent/settlement broadcast.

use crate::protocol::{EXPECTED_SCRIPT_HASH_HEX, MAINNET_CATEGORY_HEX};
use crate::search::MiningJob;
use num_bigint::BigUint;
use serde_json::{json, Value};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

/// Rich live job for CLI / win-tx; converts to search::MiningJob.
#[derive(Debug, Clone)]
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
        let height = header
            .get("height")
            .and_then(|h| h.as_u64())
            .ok_or("Invalid live BCH header response")? as u32;

        let unspent = self.rpc(
            "blockchain.scripthash.listunspent",
            json!([EXPECTED_SCRIPT_HASH_HEX, "include_tokens"]),
        )?;
        let arr = unspent
            .as_array()
            .ok_or("Invalid live PHOTON UTXO response")?;

        let batons: Vec<&Value> = arr
            .iter()
            .filter(|u| {
                let cat = u
                    .pointer("/token_data/category")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let cap = u
                    .pointer("/token_data/nft/capability")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                cat == MAINNET_CATEGORY_HEX && cap == "mutable"
            })
            .collect();

        if batons.len() != 1 {
            return Err(format!(
                "Expected exactly one live PHOTON baton; found {}",
                batons.len()
            ));
        }
        let baton = batons[0];

        let baton_txid = baton
            .get("tx_hash")
            .and_then(|t| t.as_str())
            .ok_or("baton missing tx_hash")?
            .to_string();
        let baton_vout = baton
            .get("tx_pos")
            .and_then(|t| t.as_u64())
            .ok_or("baton missing tx_pos")? as u32;
        let baton_height = baton.get("height").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
        let baton_value_sats = baton.get("value").and_then(|t| t.as_u64()).unwrap_or(0);
        let commitment_hex = baton
            .pointer("/token_data/nft/commitment")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if commitment_hex.len() < 72 {
            return Err("Live PHOTON baton commitment is missing or too short.".into());
        }

        let token_amount_str = baton
            .pointer("/token_data/amount")
            .and_then(|a| a.as_str())
            .unwrap_or("0");
        let token_amount: u128 = token_amount_str
            .parse()
            .map_err(|_| format!("bad token amount: {token_amount_str}"))?;

        let age = if baton_height == 0 {
            0
        } else {
            height.saturating_sub(baton_height)
        };

        let previous_target_hex = &commitment_hex[8..72];
        let previous_target = le_hex_to_biguint(previous_target_hex)?;
        let next_target = &previous_target * (BigUint::from(age as u64) + BigUint::from(143u64))
            / BigUint::from(144u64);
        let target_le_hex = biguint_to_le_hex32(&next_target)?;

        let reward_raw = token_amount
            .checked_div(420_000)
            .and_then(|v| v.checked_sub(1))
            .ok_or("reward_raw underflow")?;

        Ok(LiveJob {
            url: self.url.clone(),
            server_version: self.server_version.clone(),
            height,
            baton_txid,
            baton_vout,
            baton_height,
            baton_value_sats,
            commitment_hex,
            token_amount,
            age,
            target_le_hex,
            reward_raw,
        })
    }
}

fn id_matches(v: &Value, id: u64) -> bool {
    v.get("id").and_then(|x| x.as_u64()) == Some(id)
        || v.get("id").and_then(|x| x.as_i64()).map(|x| x as u64) == Some(id)
}

fn le_hex_to_biguint(hex_str: &str) -> Result<BigUint, String> {
    if !hex_str.len().is_multiple_of(2) {
        return Err("Invalid little-endian hex.".into());
    }
    let bytes = hex::decode(hex_str).map_err(|e| e.to_string())?;
    Ok(BigUint::from_bytes_le(&bytes))
}

fn biguint_to_le_hex32(v: &BigUint) -> Result<String, String> {
    let mut bytes = v.to_bytes_le();
    if bytes.len() > 32 {
        return Err("Target does not fit in 256 bits.".into());
    }
    bytes.resize(32, 0);
    Ok(hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_roundtrip_small() {
        let h = "0100000000000000000000000000000000000000000000000000000000000000";
        let n = le_hex_to_biguint(h).unwrap();
        assert_eq!(n, BigUint::from(1u32));
        assert_eq!(biguint_to_le_hex32(&n).unwrap(), h);
    }
}
