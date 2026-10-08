//! Electrum WSS client: live PHOTON baton / MiningJob fetch.
//! Owned by Dev Assist. Search consumes MiningJob; no keys.
//! Runtime winner settlement uses this session for ordered parent/settlement broadcast.

use crate::config::DONATION_BPS;
use crate::protocol::PhotonDeployment;
#[cfg(test)]
use crate::protocol::MAINNET_PHOTON;
use serde_json::{json, Value};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Error as WebSocketError, Message, WebSocket};

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

/// Converts a WebSocket read failure into a connection error.
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

pub(crate) use crate::live_job::*;
pub use crate::live_job::{LiveJob, LiveStateSnapshot};

impl LiveJob {
    /// Prints the connected Fulcrum source without exposing secrets.
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

/// Formats the compiled donation policy for display.
fn donation_summary_line() -> String {
    debug_assert_eq!(DONATION_BPS % 100, 0);
    format!("donation:      {}%", DONATION_BPS / 100)
}

pub struct ElectrumSession {
    pub url: String,
    pub server_version: Value,
    link: Link,
    deployment: &'static PhotonDeployment,
    pub(crate) fee_policy: Option<(std::time::Instant, u64)>,
}

/// #### PR #40
/// Where a session's answers come from: a Fulcrum server over its WebSocket,
/// or the miner's own BCH node over JSON-RPC ("mining is for nodes").
enum Link {
    Fulcrum {
        ws: Box<Ws>,
        next_id: u64,
        buf: String,
    },
    Node {
        endpoint: String,
        photon: Box<crate::node::NativePhotonSession>,
    },
}

/// #### PR #40
/// How many recent blocks a node without a transaction index searches for a
/// transaction by id; claims looked up this way are recent.
const NODE_RECENT_BLOCKS: u64 = 24;

/// #### PR #40
/// The Fulcrum methods the miner uses, answered by a BCH node with standard
/// RPCs (no transaction index needed).
fn node_rpc(endpoint: &str, method: &str, params: &Value) -> Result<Value, String> {
    use crate::node::rpc_call;
    match method {
        "server.ping" => rpc_call(endpoint, "getblockcount", json!([])).map(|_| Value::Null),
        "server.version" => rpc_call(endpoint, "getnetworkinfo", json!([])).map(|info| {
            json!([
                info.get("subversion").cloned().unwrap_or(Value::Null),
                "node"
            ])
        }),
        "mempool.get_info" => rpc_call(endpoint, "getmempoolinfo", json!([])),
        "blockchain.relayfee" => rpc_call(endpoint, "getnetworkinfo", json!([]))?
            .get("relayfee")
            .cloned()
            .ok_or_else(|| "the node's getnetworkinfo omitted relayfee".into()),
        "blockchain.transaction.broadcast" => {
            rpc_call(endpoint, "sendrawtransaction", json!([params[0].clone()]))
        }
        "blockchain.transaction.get" => {
            let txid = params[0]
                .as_str()
                .ok_or("transaction id must be a string")?;
            node_raw_transaction(endpoint, txid)?
                .map(Value::String)
                .ok_or_else(|| "no such transaction at the node".into())
        }
        _ => Err(format!("{method} is not answered by a node")),
    }
}

/// #### PR #40
/// A transaction's bytes from a node without a transaction index: from its
/// mempool; else from the block holding one of its unspent outputs; else from
/// one of the last blocks. `None` when none of them has it.
fn node_raw_transaction(endpoint: &str, txid: &str) -> Result<Option<String>, String> {
    use crate::node::rpc_call;
    match rpc_call(endpoint, "getrawtransaction", json!([txid, false])) {
        Ok(Value::String(raw)) => return Ok(Some(raw)),
        Ok(_) => return Err("the node returned non-hex transaction data".into()),
        // Without a transaction index a confirmed transaction is "no such";
        // anything else is the node failing.
        Err(error) if error.to_ascii_lowercase().contains("no such") => {}
        Err(error) => return Err(error),
    }
    let in_block = |hash: &Value| -> Result<Option<String>, String> {
        match rpc_call(endpoint, "getrawtransaction", json!([txid, false, hash])) {
            Ok(Value::String(raw)) => Ok(Some(raw)),
            Ok(_) => Err("the node returned non-hex transaction data".into()),
            Err(error) if error.to_ascii_lowercase().contains("no such") => Ok(None),
            Err(error) => Err(error),
        }
    };
    let tip = rpc_call(endpoint, "getblockcount", json!([]))?
        .as_u64()
        .ok_or("getblockcount returned a non-number")?;
    for vout in 0..4u32 {
        let output = rpc_call(endpoint, "gettxout", json!([txid, vout, false]))?;
        let Some(confirmations) = output.get("confirmations").and_then(Value::as_u64) else {
            continue;
        };
        if confirmations == 0 || confirmations > tip + 1 {
            break;
        }
        let hash = rpc_call(endpoint, "getblockhash", json!([tip + 1 - confirmations]))?;
        if let Some(raw) = in_block(&hash)? {
            return Ok(Some(raw));
        }
    }
    for depth in 0..NODE_RECENT_BLOCKS.min(tip + 1) {
        let hash = rpc_call(endpoint, "getblockhash", json!([tip - depth]))?;
        if let Some(raw) = in_block(&hash)? {
            return Ok(Some(raw));
        }
    }
    Ok(None)
}

impl ElectrumSession {
    /// Ban-safe connect: **one endpoint at a time**, exponential backoff between
    /// tries, never parallel fan-out. Custom URL should already be first in `endpoints`.
    #[cfg(test)]
    pub fn connect_failover(endpoints: &[String]) -> Result<Self, String> {
        Self::connect_failover_for_deployment(endpoints, &MAINNET_PHOTON)
    }

    /// Connects using the selected PHOTON contract deployment.
    pub fn connect_failover_for_deployment(
        endpoints: &[String],
        deployment: &'static PhotonDeployment,
    ) -> Result<Self, String> {
        deployment.verify()?;
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
            match Self::connect_one(url, deployment) {
                Ok(s) => return Ok(s),
                Err(e) => failures.push(format!("{url}: {e}")),
            }
        }
        Err(format!(
            "All Electrum/Fulcrum endpoints failed (sequential, ban-safe):\n{}",
            failures.join("\n")
        ))
    }

    /// #### PR #40
    /// Connects to the miner's own BCH node(s), in order. The PHOTON baton is
    /// taken from `known` (the job being mined, or a Fulcrum server's answer)
    /// once the node confirms it, and otherwise found in the node's UTXO set
    /// (a scan of a minute or two on mainnet); it is then followed through
    /// the node's mempool and blocks, and claims go out through the node.
    pub fn connect_node_failover(
        endpoints: &[String],
        deployment: &'static PhotonDeployment,
        known: Option<&LiveJob>,
    ) -> Result<Self, String> {
        deployment.verify()?;
        let photon = match known {
            Some(job) => crate::node::NativePhotonSession::resume(endpoints, deployment, job)?,
            None => crate::node::NativePhotonSession::connect_failover(endpoints, deployment)?,
        };
        let endpoint = photon.endpoint().to_owned();
        let server_version =
            node_rpc(&endpoint, "server.version", &json!([])).unwrap_or(Value::Null);
        Ok(Self {
            url: crate::node::redact_url(&endpoint),
            server_version,
            link: Link::Node {
                endpoint,
                photon: Box::new(photon),
            },
            deployment,
            fee_policy: None,
        })
    }

    /// #### PR #40: whether this session is the miner's own node.
    pub fn is_node(&self) -> bool {
        matches!(self.link, Link::Node { .. })
    }

    /// Connects to one Fulcrum endpoint and verifies its response.
    fn connect_one(url_str: &str, deployment: &'static PhotonDeployment) -> Result<Self, String> {
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
            link: Link::Fulcrum {
                ws: Box::new(ws),
                next_id: 1,
                buf: String::new(),
            },
            deployment,
            fee_policy: None,
        };

        let ver = session.rpc("server.version", json!(["pickaxe-miner", "1.4.1"]))?;
        session.server_version = ver;
        Ok(session)
    }

    /// Sends an Electrum JSON-RPC request over the WebSocket, or asks the
    /// node the same question in its own RPCs.
    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let (ws, next_id, buf) = match &mut self.link {
            Link::Fulcrum { ws, next_id, buf } => (ws, next_id, buf),
            Link::Node { endpoint, .. } => return node_rpc(endpoint, method, &params),
        };
        let id = *next_id;
        *next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = format!("{req}\n");
        ws.send(Message::Text(line.into()))
            .map_err(|e| format!("transport: send: {e}"))?;

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if std::time::Instant::now() > deadline {
                return Err(format!("timeout: rpc waiting for id={id} ({method})"));
            }
            let msg = ws.read().map_err(websocket_read_error)?;
            match msg {
                Message::Text(t) => {
                    if let Some(result) = consume_rpc_text(buf, &t, id)? {
                        return Ok(result);
                    }
                }
                Message::Ping(p) => {
                    ws.send(Message::Pong(p))
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

    /// Fetches a PHOTON mining job from the active Fulcrum source.
    pub fn fetch_live_job(&mut self) -> Result<LiveJob, String> {
        self.fetch_live_snapshot().map(|snapshot| snapshot.job)
    }

    /// Fetches the authoritative live PHOTON baton snapshot.
    pub fn fetch_live_snapshot(&mut self) -> Result<LiveStateSnapshot, String> {
        // #### PR #40: the node follows its baton from its own state.
        if let Link::Node { photon, .. } = &mut self.link {
            return photon.refresh();
        }
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

    /// Reads a consistent snapshot from the active Fulcrum session.
    fn read_live_snapshot(&mut self) -> Result<LiveStateSnapshot, String> {
        let header_before = self.rpc("blockchain.headers.subscribe", json!([]))?;
        let unspent = self.rpc(
            "blockchain.scripthash.listunspent",
            json!([self.deployment.script_hash_hex, "include_tokens"]),
        )?;
        let header_after = self.rpc("blockchain.headers.subscribe", json!([]))?;

        let tip_hash = stable_fulcrum_tip_hash(&header_before, &header_after)?;

        let mut job = live_job_from_fulcrum_values_for_deployment(
            &self.url,
            self.server_version.clone(),
            &header_after,
            &unspent,
            self.deployment,
        )?;
        if !job.tip_hash.eq_ignore_ascii_case(&tip_hash) {
            return Err("Fulcrum PHOTON snapshot tip identity is inconsistent".into());
        }
        job.relay_fee_sats_per_kb = self.fee_policy.map_or(1_000, |(_, fee)| fee);
        Ok(LiveStateSnapshot { tip_hash, job })
    }
}

/// Matches an RPC response to its request identifier.
fn rpc_value_for_id(value: &Value, id: u64) -> Option<Result<Value, String>> {
    if !id_matches(value, id) {
        return None;
    }
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Some(Err(format!("rpc error: {error}")));
    }
    Some(Ok(value.get("result").cloned().unwrap_or(Value::Null)))
}

/// Decodes a text frame into an Electrum RPC result.
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

/// Checks whether an Electrum response ID matches the request.
fn id_matches(v: &Value, id: u64) -> bool {
    v.get("id").and_then(|x| x.as_u64()) == Some(id)
        || v.get("id").and_then(|x| x.as_i64()).map(|x| x as u64) == Some(id)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chipnet_job_accepts_only_chipnet_baton_category() {
        let header = json!({
            "height": 10,
            "hex": "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c"
        });
        let baton = |category: &str, txid: &str| {
            json!({
                "tx_hash": txid,
                "tx_pos": 0,
                "height": 9,
                "value": 1000,
                "token_data": {
                    "category": category,
                    "amount": "840001",
                    "nft": {"capability": "mutable", "commitment": format!("00000000{}", "01".repeat(32))}
                }
            })
        };
        let mainnet_txid = "11".repeat(32);
        let chipnet_txid = "22".repeat(32);
        let unspent = json!([
            baton(crate::protocol::MAINNET_CATEGORY_HEX, &mainnet_txid),
            baton(crate::protocol::CHIPNET_CATEGORY_HEX, &chipnet_txid)
        ]);
        let chipnet_job = live_job_from_fulcrum_values_for_deployment(
            "wss://fixture.invalid",
            json!(["Fulcrum", "1.5"]),
            &header,
            &unspent,
            &crate::protocol::CHIPNET_PHOTON,
        )
        .unwrap();
        assert_eq!(chipnet_job.baton_txid, chipnet_txid);
        let mainnet_job = live_job_from_fulcrum_values(
            "wss://fixture.invalid",
            json!(["Fulcrum", "1.5"]),
            &header,
            &unspent,
        )
        .unwrap();
        assert_eq!(mainnet_job.baton_txid, mainnet_txid);

        let mut invalid_target =
            json!([baton(crate::protocol::CHIPNET_CATEGORY_HEX, &chipnet_txid)]);
        invalid_target[0]["token_data"]["nft"]["commitment"] =
            json!(format!("00000000{}80", "00".repeat(31)));
        let error = live_job_from_fulcrum_values_for_deployment(
            "wss://fixture.invalid",
            Value::Null,
            &header,
            &invalid_target,
            &crate::protocol::CHIPNET_PHOTON,
        )
        .unwrap_err();
        assert!(error.contains("positive ScriptNum"), "{error}");
        invalid_target[0]["token_data"]["nft"]["commitment"] =
            json!(format!("00000000{}", "00".repeat(32)));
        let error = live_job_from_fulcrum_values_for_deployment(
            "wss://fixture.invalid",
            Value::Null,
            &header,
            &invalid_target,
            &crate::protocol::CHIPNET_PHOTON,
        )
        .unwrap_err();
        assert!(error.contains("positive ScriptNum"), "{error}");
    }

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

    // #### PR #40
    /// A scripted BCH node: answers each JSON-RPC call in order, checking its
    /// method, with a result or an RPC error.
    fn scripted_node(
        calls: Vec<(&'static str, Result<Value, &'static str>)>,
    ) -> (String, thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for (method, answer) in calls {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                let start = loop {
                    let read = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break end + 4;
                        }
                    }
                    assert!(read > 0, "connection closed before the request");
                };
                let body: Value = serde_json::from_slice(&request[start..]).unwrap();
                assert_eq!(body["method"], method);
                let reply = match answer {
                    Ok(result) => json!({"result": result, "error": null, "id": body["id"]}),
                    Err(message) => json!({"result": null,
                        "error": {"code": -5, "message": message}, "id": body["id"]}),
                }
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.len(),
                    reply
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), server)
    }

    // #### PR #40
    #[test]
    fn a_node_answers_the_miners_fulcrum_calls() {
        let (node, server) = scripted_node(vec![
            ("getmempoolinfo", Ok(json!({"mempoolminfee": 0.00001}))),
            ("getnetworkinfo", Ok(json!({"relayfee": 0.00001}))),
            ("sendrawtransaction", Ok(json!("ab".repeat(32)))),
            (
                "getnetworkinfo",
                Ok(json!({"subversion": "/Bitcoin Cash Node:29.1.0(EB32.0)/"})),
            ),
            ("getblockcount", Ok(json!(100))),
        ]);
        assert_eq!(
            node_rpc(&node, "mempool.get_info", &json!([])).unwrap()["mempoolminfee"],
            json!(0.00001)
        );
        assert_eq!(
            node_rpc(&node, "blockchain.relayfee", &json!([])).unwrap(),
            json!(0.00001)
        );
        assert_eq!(
            node_rpc(&node, "blockchain.transaction.broadcast", &json!(["00"])).unwrap(),
            json!("ab".repeat(32))
        );
        assert_eq!(
            node_rpc(&node, "server.version", &json!([])).unwrap()[1],
            json!("node")
        );
        assert_eq!(
            node_rpc(&node, "server.ping", &json!([])).unwrap(),
            Value::Null
        );
        assert!(node_rpc(&node, "blockchain.scripthash.listunspent", &json!([])).is_err());
        server.join().unwrap();
    }

    // #### PR #40
    #[test]
    fn a_node_without_a_transaction_index_finds_recent_and_unspent_transactions() {
        let txid = "cd".repeat(32);
        let missing = "No such mempool or blockchain transaction. Use gettransaction for wallet transactions.";
        let not_in_block = "No such transaction found in the provided block.";
        // In the mempool.
        let (node, server) = scripted_node(vec![("getrawtransaction", Ok(json!("aa")))]);
        assert_eq!(
            node_raw_transaction(&node, &txid).unwrap().as_deref(),
            Some("aa")
        );
        server.join().unwrap();
        // Confirmed, with an unspent output that tells its block.
        let (node, server) = scripted_node(vec![
            ("getrawtransaction", Err(missing)),
            ("getblockcount", Ok(json!(100))),
            ("gettxout", Ok(json!({"confirmations": 3}))),
            ("getblockhash", Ok(json!("98"))),
            ("getrawtransaction", Ok(json!("bb"))),
        ]);
        assert_eq!(
            node_raw_transaction(&node, &txid).unwrap().as_deref(),
            Some("bb")
        );
        server.join().unwrap();
        // Its outputs spent, but in one of the last blocks.
        let mut calls = vec![
            ("getrawtransaction", Err(missing)),
            ("getblockcount", Ok(json!(100))),
        ];
        calls.extend(std::iter::repeat_n(("gettxout", Ok(Value::Null)), 4));
        calls.extend([
            ("getblockhash", Ok(json!("100"))),
            ("getrawtransaction", Err(not_in_block)),
            ("getblockhash", Ok(json!("99"))),
            ("getrawtransaction", Ok(json!("cc"))),
        ]);
        let (node, server) = scripted_node(calls);
        assert_eq!(
            node_raw_transaction(&node, &txid).unwrap().as_deref(),
            Some("cc")
        );
        server.join().unwrap();
        // A node that fails is an error, not "unknown".
        let (node, server) = scripted_node(vec![(
            "getrawtransaction",
            Err("Work queue depth exceeded"),
        )]);
        assert!(node_raw_transaction(&node, &txid).is_err());
        server.join().unwrap();
    }
}
