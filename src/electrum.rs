//! Electrum WSS client: live PHOTON baton / MiningJob fetch.
//! Owned by Dev Assist. Search consumes MiningJob; no keys.
//! Runtime winner settlement uses this session for ordered parent/settlement broadcast.

use crate::config::DONATION_BPS;
use crate::protocol::{derive_photon_state, EXPECTED_SCRIPT_HASH_HEX, MAINNET_CATEGORY_HEX};
use crate::search::MiningJob;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Error as WebSocketError, Message, WebSocket};

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn websocket_read_error(error: WebSocketError) -> String {
    match &error {
        WebSocketError::Io(io_error)
            if matches!(
                io_error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            format!("timeout: read: {error}")
        }
        WebSocketError::ConnectionClosed
        | WebSocketError::AlreadyClosed
        | WebSocketError::Io(_)
        | WebSocketError::Tls(_)
        | WebSocketError::Protocol(_) => format!("transport: read: {error}"),
        _ => format!("read: {error}"),
    }
}

/// Rich live job for CLI / win-tx; converts to search::MiningJob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveJob {
    pub url: String,
    pub server_version: Value,
    pub height: u32,
    pub tip_hash: String,
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
        println!("{}", donation_summary_line());
    }
}

fn donation_summary_line() -> String {
    debug_assert_eq!(DONATION_BPS % 100, 0);
    format!("donation:      {}%", DONATION_BPS / 100)
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
            .map_err(|e| format!("transport: send: {e}"))?;

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if std::time::Instant::now() > deadline {
                return Err(format!("timeout: rpc waiting for id={id} ({method})"));
            }
            let msg = self.ws.read().map_err(websocket_read_error)?;
            match msg {
                Message::Text(t) => {
                    if let Some(result) = consume_rpc_text(&mut self.buf, &t, id)? {
                        return Ok(result);
                    }
                }
                Message::Ping(p) => {
                    self.ws
                        .send(Message::Pong(p))
                        .map_err(|e| format!("transport: pong send: {e}"))?;
                }
                Message::Close(_) => return Err("transport: socket closed".into()),
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
        self.fetch_live_snapshot().map(|snapshot| snapshot.job)
    }

    pub fn fetch_live_snapshot(&mut self) -> Result<LiveStateSnapshot, String> {
        let mut last_error = String::new();
        for _ in 0..4 {
            match self.read_live_snapshot() {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) if snapshot_reread(&error) => last_error = error,
                Err(error) => return Err(error),
            }
        }
        Err(last_error)
    }

    fn read_live_snapshot(&mut self) -> Result<LiveStateSnapshot, String> {
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
        if !job.tip_hash.eq_ignore_ascii_case(&tip_hash) {
            return Err("Fulcrum PHOTON snapshot tip identity is inconsistent".into());
        }
        Ok(LiveStateSnapshot { tip_hash, job })
    }
}

fn rpc_value_for_id(value: &Value, id: u64) -> Option<Result<Value, String>> {
    if !id_matches(value, id) {
        return None;
    }
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Some(Err(format!("rpc error: {error}")));
    }
    Some(Ok(value.get("result").cloned().unwrap_or(Value::Null)))
}

fn consume_rpc_text(buf: &mut String, text: &str, id: u64) -> Result<Option<Value>, String> {
    buf.push_str(text);
    while let Some(pos) = buf.find('\n') {
        let line = buf[..pos].trim().to_string();
        buf.drain(..=pos);
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|error| {
            format!("transport: json: {error}: {}", &line[..line.len().min(160)])
        })?;
        if let Some(result) = rpc_value_for_id(&value, id) {
            return result.map(Some);
        }
    }

    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        // A WebSocket message may contain a complete Electrum notification
        // without a trailing newline. Consume it so the next RPC response is
        // never concatenated onto stale notification bytes.
        let matched = rpc_value_for_id(&value, id);
        buf.clear();
        if let Some(result) = matched {
            return result.map(Some);
        }
    }
    Ok(None)
}

fn fulcrum_header_height(header: &Value) -> Result<u32, String> {
    header
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "Fulcrum header response omitted a valid height".to_string())
}

fn snapshot_reread(error: &str) -> bool {
    error.contains("changed tip while reading token state")
        || error.contains("snapshot tip identity is inconsistent")
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
    let tip_hash = fulcrum_header_hash(header)?;
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
        tip_hash,
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
    fn live_job_summary_reports_compiled_donation_policy() {
        assert_eq!(DONATION_BPS, 200);
        let summary = donation_summary_line();
        assert_eq!(summary, "donation:      2%");
        assert!(!summary.contains("disabled"));
    }

    #[test]
    fn block_template_payload_cannot_become_a_photon_job() {
        let header = json!({
            "height": 0,
            "hex": "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c"
        });
        let gbt = json!({
            "previousblockhash": "00".repeat(32),
            "bits": "1d00ffff",
            "target": "00000000ffff0000000000000000000000000000000000000000000000000000",
            "height": 1,
            "coinbasevalue": 312500000
        });
        let error = live_job_from_fulcrum_values(
            "http://node.invalid",
            json!(["BCHN", "28.0"]),
            &header,
            &gbt,
        )
        .expect_err("getblocktemplate object is not a PHOTON baton");
        assert!(
            error.contains("UTXO") || error.contains("PHOTON baton"),
            "{error}"
        );

        let gbt_shaped_utxo = json!([{
            "tx_hash": "11".repeat(32),
            "tx_pos": 0,
            "height": 0,
            "value": 312500000,
            "bits": "1d00ffff",
            "target": "ff".repeat(32)
        }]);
        let error = live_job_from_fulcrum_values(
            "http://node.invalid",
            json!(["BCHN", "28.0"]),
            &header,
            &gbt_shaped_utxo,
        )
        .expect_err("compact bits must not become a PHOTON target");
        assert!(error.contains("exactly one live PHOTON baton"), "{error}");
    }

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
        let moved = stable_fulcrum_tip_hash(&before, &changed_header).unwrap_err();
        assert!(snapshot_reread(&moved));
    }

    #[test]
    fn fulcrum_header_hash_rejects_non_header_payloads() {
        assert!(fulcrum_header_hash(&json!({"height": 1, "hex": "00"})).is_err());
        assert!(fulcrum_header_hash(&json!({"height": 1})).is_err());
    }

    #[test]
    fn live_job_requires_a_concrete_bch_tip_identity() {
        let error = live_job_from_fulcrum_values(
            "wss://fixture.invalid",
            json!(["Fulcrum", "1.5"]),
            &json!({"height": 1}),
            &json!([]),
        )
        .unwrap_err();
        assert!(error.contains("omitted hex"), "{error}");
    }

    #[test]
    fn websocket_read_timeout_is_distinct_from_transport_loss() {
        let timeout = websocket_read_error(WebSocketError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "slow server",
        )));
        assert!(timeout.starts_with("timeout:"), "{timeout}");

        let reset = websocket_read_error(WebSocketError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        )));
        assert!(reset.starts_with("transport:"), "{reset}");
    }

    #[test]
    fn rpc_consumes_notification_without_newline_before_response() {
        let mut buf = String::new();
        let notification =
            r#"{"jsonrpc":"2.0","method":"blockchain.headers.subscribe","params":[{"height":1}]}"#;
        assert_eq!(consume_rpc_text(&mut buf, notification, 7).unwrap(), None);
        assert!(buf.is_empty());

        let response = r#"{"jsonrpc":"2.0","id":7,"result":{"height":1}}"#;
        assert_eq!(
            consume_rpc_text(&mut buf, response, 7).unwrap(),
            Some(json!({"height": 1}))
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn rpc_ignores_interleaved_notification_and_returns_matching_response() {
        let mut buf = String::new();
        let payload = concat!(
            "{\"jsonrpc\":\"2.0\",\"method\":\"blockchain.headers.subscribe\",\"params\":[{\"height\":2}]}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":[\"ok\"]}\n"
        );
        assert_eq!(
            consume_rpc_text(&mut buf, payload, 9).unwrap(),
            Some(json!(["ok"]))
        );
    }
}
