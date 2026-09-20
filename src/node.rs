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
            "no native node endpoints — set `node http://127.0.0.1:8332` (Start9 etc.)".into(),
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

/// Broadcast raw tx via `sendrawtransaction`. Explicit only; never auto.
pub fn broadcast_raw(endpoints: &[String], raw_tx_hex: &str) -> Result<(String, String), String> {
    let hex = raw_tx_hex.trim();
    if hex.is_empty() || hex.len() % 2 != 0 {
        return Err("raw tx hex empty or odd length".into());
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("raw tx must be hex".into());
    }
    if endpoints.is_empty() {
        return Err("no node endpoints — set `node http://user:pass@127.0.0.1:8332`".into());
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

pub fn fetch_block_template(endpoints: &[String]) -> Result<BlockTemplate, String> {
    if endpoints.is_empty() {
        return Err("no node endpoints — set `node http://user:pass@127.0.0.1:8332`".into());
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
                let job_id = v.get("job_id").and_then(|x| x.as_str()).map(|s| s.to_string());
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
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
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
        return Err("https node RPC not wired yet — use http:// on LAN/Tailscale for now".into());
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
