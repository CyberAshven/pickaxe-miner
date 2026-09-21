//! Native BCH node JSON-RPC (bitcoind/BCHN-style).
//! Ban-safe: sequential endpoint tries + backoff; no parallel fan-out.
//! Job/baton indexing stays on Fulcrum until a node path is wired.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::thread;
use std::time::Duration;

/// Probe node RPC with `getblockchaininfo` (or `getblockcount` fallback).
pub fn connect_failover(endpoints: &[String]) -> Result<(String, Value), String> {
    if endpoints.is_empty() {
        return Err(
            "no native node endpoints Ã¢â‚¬â€ set `node http://127.0.0.1:8332` (Start9 etc.)"
                .into(),
        );
    }
    let mut failures = Vec::new();
    let mut backoff_ms: u64 = 400;
    for (i, url) in endpoints.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
        }
        match rpc_call(url, "getblockchaininfo", json!([])) {
            Ok(v) => return Ok((url.clone(), v)),
            Err(e1) => match rpc_call(url, "getblockcount", json!([])) {
                Ok(v) => return Ok((url.clone(), v)),
                Err(e2) => failures.push(format!("{url}: {e1} | {e2}")),
            },
        }
    }
    Err(format!(
        "All native node endpoints failed (sequential, ban-safe):\n{}",
        failures.join("\n")
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayPolicy {
    /// Effective mempool admission floor reported by BCHN, in satoshis per kB.
    pub mempool_min_fee_sats_per_kb: u64,
}

/// Read the active BCHN mempool fee floor from the configured native node.
///
/// BCHN reports `mempoolminfee` in BCH/kB and defines it as the maximum of
/// `minrelaytxfee` and the current dynamic mempool minimum. Keep the same
/// sequential/backoff behavior as the other native-node operations.
pub fn fetch_relay_policy(endpoints: &[String]) -> Result<(String, RelayPolicy), String> {
    if endpoints.is_empty() {
        return Err("no native node endpoints configured for relay-policy preflight".into());
    }

    let mut failures = Vec::new();
    let mut backoff_ms: u64 = 400;
    for (i, url) in endpoints.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
        }
        match rpc_call(url, "getmempoolinfo", json!([])) {
            Ok(value) => {
                let parsed = value
                    .get("mempoolminfee")
                    .ok_or_else(|| "getmempoolinfo omitted mempoolminfee".to_string())
                    .and_then(bch_per_kb_to_sats);
                match parsed {
                    Ok(mempool_min_fee_sats_per_kb) => {
                        return Ok((
                            redact_url(url),
                            RelayPolicy {
                                mempool_min_fee_sats_per_kb,
                            },
                        ));
                    }
                    Err(error) => failures.push(format!("{}: {error}", redact_url(url))),
                }
            }
            Err(error) => failures.push(format!("{}: {error}", redact_url(url))),
        }
    }

    Err(format!(
        "All native node relay-policy RPCs failed (sequential, ban-safe):\n{}",
        failures.join("\n")
    ))
}

fn bch_per_kb_to_sats(value: &Value) -> Result<u64, String> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return Err("relay fee must be a numeric BCH/kB value".into()),
    };
    decimal_bch_to_sats(&text)
}

fn decimal_bch_to_sats(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('-') || text.starts_with('+') {
        return Err("relay fee must be a non-negative BCH amount".into());
    }

    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => {
            let exponent = text[index + 1..]
                .parse::<i32>()
                .map_err(|_| "relay fee has an invalid decimal exponent")?;
            (&text[..index], exponent)
        }
        None => (text, 0),
    };
    let (whole, fractional) = match mantissa.split_once('.') {
        Some((whole, fractional)) => {
            if fractional.contains('.') {
                return Err("relay fee has more than one decimal point".into());
            }
            (whole, fractional)
        }
        None => (mantissa, ""),
    };
    if whole.is_empty() && fractional.is_empty() {
        return Err("relay fee is empty".into());
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err("relay fee contains non-decimal characters".into());
    }

    let digits = format!("{whole}{fractional}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let mut value = digits
        .parse::<u128>()
        .map_err(|_| "relay fee decimal is too large")?;
    let scale = 8i32
        .checked_add(exponent)
        .and_then(|scale| scale.checked_sub(fractional.len() as i32))
        .ok_or("relay fee scale overflow")?;
    if scale >= 0 {
        let multiplier = 10u128
            .checked_pow(scale as u32)
            .ok_or("relay fee scale is too large")?;
        value = value
            .checked_mul(multiplier)
            .ok_or("relay fee satoshi value overflow")?;
    } else {
        let divisor = 10u128
            .checked_pow(scale.unsigned_abs())
            .ok_or("relay fee scale is too small")?;
        if value % divisor != 0 {
            return Err("relay fee has precision below one satoshi per kB".into());
        }
        value /= divisor;
    }
    u64::try_from(value).map_err(|_| "relay fee exceeds u64 satoshis per kB".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MempoolAcceptance {
    pub txid: String,
    pub allowed: bool,
    pub size: Option<u64>,
    pub vsize: Option<u64>,
    pub reject_reason: Option<String>,
    pub reject_details: Option<String>,
}

/// Ask BCHN whether these exact signed transaction bytes satisfy the node's
/// current consensus and mempool policy. BCHN currently accepts exactly one
/// raw transaction per `testmempoolaccept` call, so parent and settlement
/// checks are deliberately sequenced by the runtime.
pub fn test_mempool_accept(
    endpoints: &[String],
    raw_tx_hex: &str,
) -> Result<(String, MempoolAcceptance), String> {
    let hex = raw_tx_hex.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return Err("raw tx hex empty or odd length".into());
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("raw tx must be hex".into());
    }
    if endpoints.is_empty() {
        return Err("no node endpoints configured for mempool acceptance preflight".into());
    }

    let mut failures = Vec::new();
    let mut backoff_ms: u64 = 400;
    for (i, url) in endpoints.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
        }
        match rpc_call(url, "testmempoolaccept", json!([[hex], false])) {
            Ok(Value::Array(results)) if results.len() == 1 => {
                let item = &results[0];
                let txid = item
                    .get("txid")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "testmempoolaccept omitted txid".to_string());
                let allowed = item
                    .get("allowed")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| "testmempoolaccept omitted allowed".to_string());
                match (txid, allowed) {
                    (Ok(txid), Ok(allowed)) => {
                        return Ok((
                            redact_url(url),
                            MempoolAcceptance {
                                txid: txid.to_string(),
                                allowed,
                                size: item.get("size").and_then(Value::as_u64),
                                vsize: item.get("vsize").and_then(Value::as_u64),
                                reject_reason: item
                                    .get("reject-reason")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                                reject_details: item
                                    .get("reject-details")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            },
                        ));
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        failures.push(format!("{}: {error}", redact_url(url)));
                    }
                }
            }
            Ok(_) => failures.push(format!(
                "{}: testmempoolaccept returned an unexpected result shape",
                redact_url(url)
            )),
            Err(error) => failures.push(format!("{}: {error}", redact_url(url))),
        }
    }

    Err(format!(
        "All native node mempool acceptance RPCs failed (sequential, ban-safe):\n{}",
        failures.join("\n")
    ))
}

/// Broadcast raw tx via `sendrawtransaction`. Explicit only; never auto.
pub fn broadcast_raw(endpoints: &[String], raw_tx_hex: &str) -> Result<(String, String), String> {
    let hex = raw_tx_hex.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return Err("raw tx hex empty or odd length".into());
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("raw tx must be hex".into());
    }
    if endpoints.is_empty() {
        return Err("no node endpoints Ã¢â‚¬â€ set `node http://user:pass@127.0.0.1:8332`".into());
    }
    let mut failures = Vec::new();
    let mut backoff_ms: u64 = 400;
    for (i, url) in endpoints.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
        }
        match rpc_call(url, "sendrawtransaction", json!([hex])) {
            Ok(Value::String(txid)) => return Ok((redact_url(url), txid)),
            Ok(other) => return Ok((redact_url(url), other.to_string())),
            Err(e) => failures.push(format!("{}: {e}", redact_url(url))),
        }
    }
    Err(format!(
        "All node sendrawtransaction failed (ban-safe):\n{}",
        failures.join("\n")
    ))
}

fn redact_url(url: &str) -> String {
    // Strip userinfo before @ so credentials never land in logs.
    if let Some(scheme_end) = url.find("://") {
        let scheme = &url[..scheme_end + 3];
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            return format!("{scheme}***@{}", &rest[at + 1..]);
        }
    }
    url.to_string()
}

/// Light block template from BCHN. Prefers `getblocktemplatelight`, falls back to `getblocktemplate`.
#[derive(Debug, Clone)]
pub struct BlockTemplate {
    pub endpoint: String,
    pub light: bool,
    pub job_id: Option<String>,
    pub previousblockhash: Option<String>,
    pub version: Option<u64>,
    pub raw: Value,
}

impl BlockTemplate {
    pub fn print_summary(&self) {
        println!(
            "node template via {} ({})",
            self.endpoint,
            if self.light {
                "getblocktemplatelight"
            } else {
                "getblocktemplate"
            }
        );
        if let Some(j) = &self.job_id {
            println!("  job_id: {j}");
        }
        if let Some(p) = &self.previousblockhash {
            println!("  prev:   {p}");
        }
        if let Some(h) = self.raw.get("height").and_then(|v| v.as_u64()) {
            println!("  height: {h}");
        }
    }
}

#[cfg(test)]
fn bits_to_target_le_hex(bits: u32) -> String {
    let exp = (bits >> 24) as i32;
    let mant = bits & 0x00ff_ffff;
    let mut target = [0u8; 32];
    if exp <= 3 {
        let m = mant >> (8 * (3 - exp) as u32);
        target[29] = ((m >> 16) & 0xff) as u8;
        target[30] = ((m >> 8) & 0xff) as u8;
        target[31] = (m & 0xff) as u8;
    } else {
        let idx = (32 - exp) as usize;
        if idx < 30 {
            target[idx] = ((mant >> 16) & 0xff) as u8;
            target[idx + 1] = ((mant >> 8) & 0xff) as u8;
            target[idx + 2] = (mant & 0xff) as u8;
        }
    }
    hex::encode(target.iter().rev().copied().collect::<Vec<_>>())
}

pub fn fetch_block_template(endpoints: &[String]) -> Result<BlockTemplate, String> {
    if endpoints.is_empty() {
        return Err("no node endpoints Ã¢â‚¬â€ set `node http://user:pass@127.0.0.1:8332`".into());
    }
    let mut failures = Vec::new();
    let mut backoff_ms: u64 = 400;
    let light_params = json!([{"mode": "template", "capabilities": ["coinbasetxn", "workid"]}]);
    let gbt_params = json!([{"rules": ["segwit"], "capabilities": ["coinbasetxn", "workid"]}]);
    for (i, url) in endpoints.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
        }
        match rpc_call(url, "getblocktemplatelight", light_params.clone()) {
            Ok(v) => {
                let job_id = v
                    .get("job_id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                let prev = v
                    .get("previousblockhash")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                let version = v.get("version").and_then(|x| x.as_u64());
                return Ok(BlockTemplate {
                    endpoint: redact_url(url),
                    light: true,
                    job_id,
                    previousblockhash: prev,
                    version,
                    raw: v,
                });
            }
            Err(e_light) => match rpc_call(url, "getblocktemplate", gbt_params.clone()) {
                Ok(v) => {
                    let prev = v
                        .get("previousblockhash")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    let version = v.get("version").and_then(|x| x.as_u64());
                    return Ok(BlockTemplate {
                        endpoint: redact_url(url),
                        light: false,
                        job_id: None,
                        previousblockhash: prev,
                        version,
                        raw: v,
                    });
                }
                Err(e_gbt) => failures.push(format!(
                    "{}: light={e_light} | gbt={e_gbt}",
                    redact_url(url)
                )),
            },
        }
    }
    Err(format!(
        "All node template RPCs failed (ban-safe):\n{}",
        failures.join("\n")
    ))
}

/// Submit solved work. Prefers `submitblocklight` when `job_id` is set; else `submitblock`.
pub fn submit_block(
    endpoints: &[String],
    hexdata: &str,
    job_id: Option<&str>,
) -> Result<(String, Value), String> {
    let hex = hexdata.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(2) || !hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("block hex invalid".into());
    }
    if endpoints.is_empty() {
        return Err("no node endpoints".into());
    }
    let mut failures = Vec::new();
    let mut backoff_ms: u64 = 400;
    for (i, url) in endpoints.iter().enumerate() {
        if i > 0 {
            thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
        }
        if let Some(jid) = job_id {
            match rpc_call(url, "submitblocklight", json!([hex, jid])) {
                Ok(v) => return Ok((redact_url(url), v)),
                Err(e) => failures.push(format!("{}: submitblocklight: {e}", redact_url(url))),
            }
        }
        match rpc_call(url, "submitblock", json!([hex])) {
            Ok(v) => return Ok((redact_url(url), v)),
            Err(e) => failures.push(format!("{}: submitblock: {e}", redact_url(url))),
        }
    }
    Err(format!(
        "All node submit RPCs failed (ban-safe):\n{}",
        failures.join("\n")
    ))
}

fn rpc_call(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("node URL must be http(s)".into());
    }
    if lower.starts_with("https://") {
        return Err(
            "https node RPC not wired yet Ã¢â‚¬â€ use http:// on LAN/Tailscale for now".into(),
        );
    }
    let rest = &url["http://".len()..];
    let (auth, hostport_path) = if let Some(at) = rest.find('@') {
        (Some(&rest[..at]), &rest[at + 1..])
    } else {
        (None, rest)
    };
    let (hostport, path) = match hostport_path.split_once('/') {
        Some((hp, p)) => (hp, format!("/{p}")),
        None => (hostport_path, "/".to_string()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad node port")?),
        None => (hostport, 8332u16),
    };

    let body = json!({"jsonrpc":"1.0","id":"pickaxe","method":method,"params":params}).to_string();
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(a) = auth {
        let b64 = simple_b64(a.as_bytes());
        req.push_str(&format!("Authorization: Basic {b64}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);

    let addr = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| format!("resolve: {e}"))?
        .next()
        .ok_or_else(|| "resolve: no addrs".to_string())?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
        .map_err(|e| format!("connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(12)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(8)));
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let mut resp = Vec::new();
    stream
        .read_to_end(&mut resp)
        .map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&resp);
    let Some(idx) = text.find("\r\n\r\n") else {
        return Err("malformed HTTP response".into());
    };
    let body = text[idx + 4..].trim();
    let v: Value = serde_json::from_str(body).map_err(|e| format!("json: {e}"))?;
    if let Some(err) = v.get("error") {
        if !err.is_null() {
            return Err(format!("rpc error: {err}"));
        }
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

fn simple_b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i] as u32;
        let b1 = if i + 1 < data.len() {
            data[i + 1] as u32
        } else {
            0
        };
        let b2 = if i + 2 < data.len() {
            data[i + 2] as u32
        } else {
            0
        };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if i + 1 < data.len() {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if i + 2 < data.len() {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
        i += 3;
    }
    out
}

#[cfg(test)]
mod gbt_tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn bits_genesis_style_nonzero() {
        let h = bits_to_target_le_hex(0x1d00ffff);
        assert_eq!(h.len(), 64);
        assert_ne!(h, "00".repeat(32));
    }

    #[test]
    fn relay_fee_decimal_conversion_is_exact() {
        assert_eq!(decimal_bch_to_sats("0.00001000").unwrap(), 1_000);
        assert_eq!(decimal_bch_to_sats("0.00001234").unwrap(), 1_234);
        assert_eq!(decimal_bch_to_sats("1e-5").unwrap(), 1_000);
        assert!(decimal_bch_to_sats("0.000000001").is_err());
        assert!(decimal_bch_to_sats("-0.00001").is_err());
    }

    #[test]
    fn relay_policy_uses_live_mempool_minimum() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("\"method\":\"getmempoolinfo\""));
            let body = r#"{"result":{"mempoolminfee":0.00001234,"minrelaytxfee":0.00001000},"error":null,"id":"pickaxe"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let endpoint = format!("http://{address}");
        let (reported_endpoint, policy) =
            fetch_relay_policy(std::slice::from_ref(&endpoint)).unwrap();
        server.join().unwrap();
        assert_eq!(reported_endpoint, endpoint);
        assert_eq!(policy.mempool_min_fee_sats_per_kb, 1_234);
    }

    #[test]
    fn mempool_acceptance_preserves_policy_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let txid = "11".repeat(32);
        let expected_txid = txid.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("\"method\":\"testmempoolaccept\""));
            assert!(request.contains("[[\"0200\"],false]"));
            let body = format!(
                "{{\"result\":[{{\"txid\":\"{txid}\",\"allowed\":false,\"reject-reason\":\"dust\",\"reject-details\":\"policy floor\"}}],\"error\":null,\"id\":\"pickaxe\"}}"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let endpoint = format!("http://{address}");
        let (reported_endpoint, acceptance) =
            test_mempool_accept(std::slice::from_ref(&endpoint), "0200").unwrap();
        server.join().unwrap();
        assert_eq!(reported_endpoint, endpoint);
        assert_eq!(acceptance.txid, expected_txid);
        assert!(!acceptance.allowed);
        assert_eq!(acceptance.reject_reason.as_deref(), Some("dust"));
        assert_eq!(acceptance.reject_details.as_deref(), Some("policy floor"));
    }
}
