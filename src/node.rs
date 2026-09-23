//! Native BCH node JSON-RPC (bitcoind/BCHN-style).
//! Ban-safe: sequential endpoint tries + backoff; no parallel fan-out.
//! Native PHOTON reconstruction is available for equivalence checks, including
//! bounded unconfirmed-baton continuation. Runtime routing stays on Fulcrum
//! until the native capability is explicitly promoted after equivalence review.

use crate::electrum::{ElectrumSession, LiveJob, LiveStateSnapshot};
use crate::protocol::{derive_photon_state, COVENANT_LOCKING_BYTECODE_HEX, MAINNET_CATEGORY_HEX};
use native_tls::TlsConnector;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::thread;
use std::time::Duration;

const NODE_RPC_USER_ENV: &str = "PICKAXE_NODE_RPC_USER";
const NODE_RPC_PASSWORD_ENV: &str = "PICKAXE_NODE_RPC_PASSWORD";

/// Probe node RPC with `getblockchaininfo` (or `getblockcount` fallback).
pub fn connect_failover(endpoints: &[String]) -> Result<(String, Value), String> {
    if endpoints.is_empty() {
        return Err(
            "no native node endpoints -- set `node http://127.0.0.1:8332` (Start9 etc.)".into(),
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
            Ok(v) => return Ok((redact_url(url), v)),
            Err(e1) => match rpc_call(url, "getblockcount", json!([])) {
                Ok(v) => return Ok((redact_url(url), v)),
                Err(e2) => failures.push(format!("{}: {e1} | {e2}", redact_url(url))),
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
                    .and_then(bch_value_to_sats);
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

/// Converts a JSON BCH amount into integral satoshis.
fn bch_value_to_sats(value: &Value) -> Result<u64, String> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        _ => return Err("BCH amount must be numeric".into()),
    };
    decimal_bch_to_sats(&text)
}

/// Converts a decimal BCH amount without floating-point rounding.
fn decimal_bch_to_sats(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with('-') || text.starts_with('+') {
        return Err("BCH amount must be non-negative".into());
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
                return Err("BCH amount has more than one decimal point".into());
            }
            (whole, fractional)
        }
        None => (mantissa, ""),
    };
    if whole.is_empty() && fractional.is_empty() {
        return Err("BCH amount is empty".into());
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err("BCH amount contains non-decimal characters".into());
    }

    let digits = format!("{whole}{fractional}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let mut value = digits
        .parse::<u128>()
        .map_err(|_| "BCH amount decimal is too large")?;
    let scale = 8i32
        .checked_add(exponent)
        .and_then(|scale| scale.checked_sub(fractional.len() as i32))
        .ok_or("BCH amount scale overflow")?;
    if scale >= 0 {
        let multiplier = 10u128
            .checked_pow(scale as u32)
            .ok_or("BCH amount scale is too large")?;
        value = value
            .checked_mul(multiplier)
            .ok_or("BCH amount satoshi value overflow")?;
    } else {
        let divisor = 10u128
            .checked_pow(scale.unsigned_abs())
            .ok_or("BCH amount scale is too small")?;
        if value % divisor != 0 {
            return Err("BCH amount has precision below one satoshi".into());
        }
        value /= divisor;
    }
    u64::try_from(value).map_err(|_| "BCH amount exceeds u64 satoshis".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativePhotonBaton {
    txid: String,
    vout: u32,
    height: u32,
    value_sats: u64,
    commitment_hex: String,
    token_amount: u128,
}

const MAX_NATIVE_PHOTON_MEMPOOL_BOOTSTRAP_CANDIDATES: usize = 2_048;
const MAX_NATIVE_PHOTON_MEMPOOL_DESCENDANT_DEPTH: usize = 64;

struct NativePhotonBootstrap {
    snapshot: LiveStateSnapshot,
    baton: NativePhotonBaton,
}

pub struct NativePhotonSession {
    url: String,
    baton: NativePhotonBaton,
    snapshot: LiveStateSnapshot,
}

/// Reconstruct the PHOTON live state from BCHN's native UTXO/token RPCs.
///
/// This provider is intentionally not advertised to the runtime source router
/// yet. `scantxoutset` bootstraps the confirmed baton, then the mempool is used
/// only when that baton has already been spent by an unconfirmed successor.
/// Runtime capability remains fail-closed until the normalized state is proven
/// equivalent to the canonical Fulcrum path.
impl NativePhotonSession {
    /// Connects to a native node using the configured failover order.
    pub fn connect_failover(endpoints: &[String]) -> Result<Self, String> {
        if endpoints.is_empty() {
            return Err("no native node endpoints configured for PHOTON state".into());
        }

        let mut failures = Vec::new();
        let mut backoff_ms: u64 = 400;
        for (index, url) in endpoints.iter().enumerate() {
            if index > 0 {
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms.saturating_mul(2)).min(8_000);
            }
            match bootstrap_native_photon(url) {
                Ok(bootstrap) => {
                    return Ok(Self {
                        url: url.clone(),
                        baton: bootstrap.baton,
                        snapshot: bootstrap.snapshot,
                    });
                }
                Err(error) => failures.push(format!("{}: {error}", redact_url(url))),
            }
        }

        Err(format!(
            "All native node PHOTON-state RPCs failed (sequential, ban-safe):\n{}",
            failures.join("\n")
        ))
    }

    /// Returns the most recently verified native node state.
    pub fn snapshot(&self) -> &LiveStateSnapshot {
        &self.snapshot
    }

    /// Refresh from the cached baton without rescanning the full UTXO set.
    ///
    /// A full scan is only used for recovery after the cached baton disappears
    /// and a canonical mempool successor cannot be established.
    pub fn refresh(&mut self) -> Result<LiveStateSnapshot, String> {
        let (height, bestblock) = fetch_native_chain_tip(&self.url)?;
        let txout = rpc_call(
            &self.url,
            "gettxout",
            json!([self.baton.txid.clone(), self.baton.vout, true]),
        )?;

        let observed_baton = if txout.is_null() {
            match resolve_native_mempool_baton(&self.url, &self.baton) {
                Ok(successor) => successor,
                Err(refresh_error) => {
                    let bootstrap = bootstrap_native_photon(&self.url).map_err(|bootstrap_error| {
                        format!(
                            "cached native PHOTON baton refresh failed: {refresh_error}; rebootstrap failed: {bootstrap_error}"
                        )
                    })?;
                    self.baton = bootstrap.baton;
                    self.snapshot = bootstrap.snapshot.clone();
                    return Ok(bootstrap.snapshot);
                }
            }
        } else {
            parse_native_gettxout_baton(
                &self.baton.txid,
                self.baton.vout,
                height,
                &bestblock,
                &txout,
            )?
        };

        let snapshot =
            finalize_native_photon_snapshot(&self.url, height, &bestblock, observed_baton)?;
        self.baton = baton_from_live_job(&snapshot.job);
        self.snapshot = snapshot.clone();
        Ok(snapshot)
    }
}

/// Reconstruct the PHOTON live state from BCHN's native UTXO/token RPCs.
///
/// This compatibility wrapper performs a single bootstrap. Production callers
/// that refresh repeatedly should retain NativePhotonSession.
#[allow(dead_code)]
pub fn fetch_photon_live_job(endpoints: &[String]) -> Result<LiveJob, String> {
    let session = NativePhotonSession::connect_failover(endpoints)?;
    Ok(session.snapshot().job.clone())
}

/// Initializes native PHOTON state from canonical source evidence.
fn bootstrap_native_photon(url: &str) -> Result<NativePhotonBootstrap, String> {
    let descriptor = format!("raw({COVENANT_LOCKING_BYTECODE_HEX})");
    let scan = rpc_call(url, "scantxoutset", json!(["start", [descriptor]]))?;
    if scan.get("success").and_then(Value::as_bool) != Some(true) {
        return Err("scantxoutset did not complete successfully".into());
    }

    let height = scan
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("scantxoutset omitted a valid tip height")?;
    let bestblock = scan
        .get("bestblock")
        .and_then(Value::as_str)
        .filter(|hash| hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("scantxoutset omitted a valid bestblock")?;
    let unspents = scan
        .get("unspents")
        .and_then(Value::as_array)
        .ok_or("scantxoutset omitted unspents")?;

    let candidates = unspents
        .iter()
        .filter(|utxo| native_scan_entry_is_photon_baton(utxo))
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(format!(
            "Expected exactly one confirmed native-node PHOTON baton; found {}",
            candidates.len()
        ));
    }

    let scan_baton = parse_native_scan_baton(candidates[0])?;
    let first = rpc_call(
        url,
        "gettxout",
        json!([scan_baton.txid.clone(), scan_baton.vout, true]),
    )?;
    let observed_baton = if first.is_null() {
        resolve_native_mempool_baton(url, &scan_baton)?
    } else {
        let baton = parse_native_gettxout_baton(
            &scan_baton.txid,
            scan_baton.vout,
            height,
            bestblock,
            &first,
        )?;
        if baton != scan_baton {
            return Err("native PHOTON scantxoutset/gettxout baton mismatch".into());
        }
        baton
    };

    let snapshot = finalize_native_photon_snapshot(url, height, bestblock, observed_baton)?;
    let baton = baton_from_live_job(&snapshot.job);
    Ok(NativePhotonBootstrap { snapshot, baton })
}

/// Fetches the native node height and current tip hash.
fn fetch_native_chain_tip(url: &str) -> Result<(u32, String), String> {
    let info = rpc_call(url, "getblockchaininfo", json!([]))?;
    let height = info
        .get("blocks")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("getblockchaininfo omitted a valid blocks height")?;
    let bestblock = info
        .get("bestblockhash")
        .and_then(Value::as_str)
        .filter(|hash| hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("getblockchaininfo omitted a valid bestblockhash")?
        .to_ascii_lowercase();
    Ok((height, bestblock))
}

/// Validates and publishes the native PHOTON snapshot.
fn finalize_native_photon_snapshot(
    url: &str,
    height: u32,
    bestblock: &str,
    observed_baton: NativePhotonBaton,
) -> Result<LiveStateSnapshot, String> {
    let observed_bestblock = rpc_call(url, "getbestblockhash", json!([]))?;
    let observed_bestblock = observed_bestblock
        .as_str()
        .ok_or("getbestblockhash returned a non-string result")?;
    if !observed_bestblock.eq_ignore_ascii_case(bestblock) {
        return Err("native PHOTON snapshot became stale while reconstructing state".into());
    }

    let second = rpc_call(
        url,
        "gettxout",
        json!([observed_baton.txid.clone(), observed_baton.vout, true]),
    )?;
    if second.is_null() {
        return Err(
            "PHOTON baton changed in mempool while reconstructing native-node state".into(),
        );
    }
    let second_baton = parse_native_gettxout_baton(
        &observed_baton.txid,
        observed_baton.vout,
        height,
        bestblock,
        &second,
    )?;
    if observed_baton != second_baton {
        return Err("native PHOTON baton changed during consistency check".into());
    }

    let derived = derive_photon_state(
        &second_baton.commitment_hex,
        second_baton.token_amount,
        height,
        second_baton.height,
    )?;
    let job = LiveJob {
        url: redact_url(url),
        server_version: json!({"provider": "native-bchn", "snapshot": "stateful-gettxout"}),
        height,
        tip_hash: bestblock.to_ascii_lowercase(),
        baton_txid: second_baton.txid,
        baton_vout: second_baton.vout,
        baton_height: second_baton.height,
        baton_value_sats: second_baton.value_sats,
        commitment_hex: second_baton.commitment_hex,
        token_amount: second_baton.token_amount,
        age: derived.age,
        target_le_hex: derived.target_le_hex,
        reward_raw: derived.reward_raw,
    };
    Ok(LiveStateSnapshot {
        tip_hash: bestblock.to_ascii_lowercase(),
        job,
    })
}

/// Extracts authoritative baton information from a live job.
fn baton_from_live_job(job: &LiveJob) -> NativePhotonBaton {
    NativePhotonBaton {
        txid: job.baton_txid.clone(),
        vout: job.baton_vout,
        height: job.baton_height,
        value_sats: job.baton_value_sats,
        commitment_hex: job.commitment_hex.clone(),
        token_amount: job.token_amount,
    }
}

/// Checks a native job against the canonical PHOTON live state.
pub fn verify_live_photon_equivalence(
    native: &mut NativePhotonSession,
    canonical: &mut ElectrumSession,
) -> Result<LiveStateSnapshot, String> {
    let canonical_snapshot = canonical.fetch_live_snapshot()?;
    let native_snapshot = native.refresh()?;
    verify_photon_state_equivalence(&native_snapshot, &canonical_snapshot)?;
    Ok(native_snapshot)
}

/// Rejects native state that disagrees with the canonical source.
pub(crate) fn verify_photon_state_equivalence(
    native: &LiveStateSnapshot,
    canonical: &LiveStateSnapshot,
) -> Result<(), String> {
    if !native.tip_hash.eq_ignore_ascii_case(&canonical.tip_hash) {
        return Err(format!(
            "PHOTON provider tip mismatch: native={} canonical={}",
            native.tip_hash, canonical.tip_hash
        ));
    }

    macro_rules! require_equal {
        ($field:ident) => {
            if native.job.$field != canonical.job.$field {
                return Err(format!(
                    "PHOTON provider state mismatch for {}: native={:?} canonical={:?}",
                    stringify!($field),
                    native.job.$field,
                    canonical.job.$field
                ));
            }
        };
    }

    require_equal!(height);
    require_equal!(baton_txid);
    require_equal!(baton_vout);
    require_equal!(baton_height);
    require_equal!(baton_value_sats);
    require_equal!(commitment_hex);
    require_equal!(token_amount);
    require_equal!(age);
    require_equal!(target_le_hex);
    require_equal!(reward_raw);
    Ok(())
}

/// Finds the current baton successor in the native mempool.
fn resolve_native_mempool_baton(
    url: &str,
    confirmed_baton: &NativePhotonBaton,
) -> Result<NativePhotonBaton, String> {
    let mempool = rpc_call(url, "getrawmempool", json!([true]))?;
    let entries = mempool
        .as_object()
        .ok_or("getrawmempool verbose result is not an object")?;

    let mut roots = entries
        .iter()
        .filter_map(|(txid, entry)| {
            let depends = entry.get("depends")?.as_array()?;
            if !depends.is_empty() {
                return None;
            }
            let time = entry.get("time").and_then(Value::as_u64).unwrap_or(0);
            Some((time, txid.as_str()))
        })
        .collect::<Vec<_>>();

    if roots.len() > MAX_NATIVE_PHOTON_MEMPOOL_BOOTSTRAP_CANDIDATES {
        return Err(format!(
            "native PHOTON mempool bootstrap has {} root candidates; bounded limit is {}",
            roots.len(),
            MAX_NATIVE_PHOTON_MEMPOOL_BOOTSTRAP_CANDIDATES
        ));
    }

    roots.sort_unstable_by(|left, right| right.cmp(left));
    let mut first_successor = None;
    for (_, txid) in roots {
        let transaction = rpc_call(url, "getrawtransaction", json!([txid, 1]))?;
        if let Some(successor) =
            parse_native_mempool_successor(txid, &transaction, confirmed_baton)?
        {
            first_successor = Some(successor);
            break;
        }
    }

    let mut current = first_successor.ok_or(
        "confirmed PHOTON baton is spent in mempool, but no canonical successor could be proven",
    )?;

    for _ in 0..MAX_NATIVE_PHOTON_MEMPOOL_DESCENDANT_DEPTH {
        let entry = entries
            .get(&current.txid)
            .ok_or("native PHOTON successor disappeared from mempool snapshot")?;
        let spent_by = entry
            .get("spentby")
            .and_then(Value::as_array)
            .ok_or("native mempool entry omitted spentby")?;
        if spent_by.is_empty() {
            return Ok(current);
        }

        let mut next_successor = None;
        for child_txid in spent_by {
            let child_txid = child_txid
                .as_str()
                .ok_or("native mempool spentby entry is not a transaction id")?;
            let transaction = rpc_call(url, "getrawtransaction", json!([child_txid, 1]))?;
            if let Some(successor) =
                parse_native_mempool_successor(child_txid, &transaction, &current)?
            {
                if next_successor.replace(successor).is_some() {
                    return Err(
                        "multiple mempool transactions spend the PHOTON baton output".into(),
                    );
                }
            }
        }

        match next_successor {
            Some(successor) => current = successor,
            None => return Ok(current),
        }
    }

    Err(format!(
        "native PHOTON mempool baton chain exceeds bounded depth {}",
        MAX_NATIVE_PHOTON_MEMPOOL_DESCENDANT_DEPTH
    ))
}

/// Parses a baton successor from native mempool transaction data.
fn parse_native_mempool_successor(
    expected_txid: &str,
    transaction: &Value,
    spent_baton: &NativePhotonBaton,
) -> Result<Option<NativePhotonBaton>, String> {
    let transaction_txid = transaction
        .get("txid")
        .and_then(Value::as_str)
        .filter(|txid| txid.len() == 64 && txid.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("getrawtransaction omitted a valid txid")?;
    if !transaction_txid.eq_ignore_ascii_case(expected_txid) {
        return Err("getrawtransaction returned a different transaction id".into());
    }

    let inputs = transaction
        .get("vin")
        .and_then(Value::as_array)
        .ok_or("getrawtransaction omitted vin")?;
    let spends_baton = inputs.iter().filter(|input| {
        input
            .get("txid")
            .and_then(Value::as_str)
            .is_some_and(|txid| txid.eq_ignore_ascii_case(&spent_baton.txid))
            && input.get("vout").and_then(Value::as_u64) == Some(u64::from(spent_baton.vout))
    });
    if spends_baton.count() == 0 {
        return Ok(None);
    }
    if inputs.len() != 1 {
        return Err("PHOTON baton mempool spend does not use the one-input mining branch".into());
    }

    let outputs = transaction
        .get("vout")
        .and_then(Value::as_array)
        .ok_or("getrawtransaction omitted vout")?;
    let successors = outputs
        .iter()
        .filter(|output| native_transaction_output_is_photon_baton(output))
        .collect::<Vec<_>>();
    if successors.len() != 1 {
        return Err(format!(
            "PHOTON mempool spend must contain exactly one mutable baton output; found {}",
            successors.len()
        ));
    }

    parse_native_transaction_output_baton(transaction_txid, successors[0]).map(Some)
}

/// Checks whether an output preserves the PHOTON baton contract.
fn native_transaction_output_is_photon_baton(output: &Value) -> bool {
    let script_matches = output
        .pointer("/scriptPubKey/hex")
        .and_then(Value::as_str)
        .is_some_and(|script| script.eq_ignore_ascii_case(COVENANT_LOCKING_BYTECODE_HEX));
    let category_matches = output
        .pointer("/tokenData/category")
        .and_then(Value::as_str)
        .is_some_and(|category| category.eq_ignore_ascii_case(MAINNET_CATEGORY_HEX));
    let mutable = output
        .pointer("/tokenData/nft/capability")
        .and_then(Value::as_str)
        == Some("mutable");
    script_matches && category_matches && mutable
}

/// Parses baton details from a native transaction output.
fn parse_native_transaction_output_baton(
    txid: &str,
    output: &Value,
) -> Result<NativePhotonBaton, String> {
    if !native_transaction_output_is_photon_baton(output) {
        return Err("native transaction output is not the authoritative PHOTON baton shape".into());
    }
    let vout = output
        .get("n")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("native PHOTON successor missing valid output index")?;
    let value = output
        .get("value")
        .ok_or_else(|| "native PHOTON successor missing BCH value".to_string())?;
    let value_sats = bch_value_to_sats(value)?;
    let commitment_hex = output
        .pointer("/tokenData/nft/commitment")
        .and_then(Value::as_str)
        .ok_or("native PHOTON successor missing NFT commitment")?
        .to_ascii_lowercase();
    let token_amount_text = output
        .pointer("/tokenData/amount")
        .and_then(Value::as_str)
        .ok_or("native PHOTON successor missing token amount")?;
    let token_amount = token_amount_text
        .parse::<u128>()
        .map_err(|_| format!("bad native PHOTON successor token amount: {token_amount_text}"))?;

    Ok(NativePhotonBaton {
        txid: txid.to_ascii_lowercase(),
        vout,
        height: 0,
        value_sats,
        commitment_hex,
        token_amount,
    })
}

/// Checks whether a scan entry represents a PHOTON baton.
fn native_scan_entry_is_photon_baton(utxo: &Value) -> bool {
    let script_matches = utxo
        .get("scriptPubKey")
        .and_then(Value::as_str)
        .is_some_and(|script| script.eq_ignore_ascii_case(COVENANT_LOCKING_BYTECODE_HEX));
    let category_matches = utxo
        .pointer("/tokenData/category")
        .and_then(Value::as_str)
        .is_some_and(|category| category.eq_ignore_ascii_case(MAINNET_CATEGORY_HEX));
    let mutable = utxo
        .pointer("/tokenData/nft/capability")
        .and_then(Value::as_str)
        == Some("mutable");
    script_matches && category_matches && mutable
}

/// Parses a baton from an unspent-output scan entry.
fn parse_native_scan_baton(utxo: &Value) -> Result<NativePhotonBaton, String> {
    if !native_scan_entry_is_photon_baton(utxo) {
        return Err("native UTXO is not the authoritative PHOTON baton shape".into());
    }
    let txid = utxo
        .get("txid")
        .and_then(Value::as_str)
        .filter(|hash| hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or("native PHOTON baton missing valid txid")?;
    let vout = utxo
        .get("vout")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("native PHOTON baton missing valid vout")?;
    let height = utxo
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("native PHOTON baton missing valid height")?;
    let amount = utxo
        .get("amount")
        .ok_or_else(|| "native PHOTON baton missing BCH amount".to_string())?;
    let value_sats = bch_value_to_sats(amount)?;
    let commitment_hex = utxo
        .pointer("/tokenData/nft/commitment")
        .and_then(Value::as_str)
        .ok_or("native PHOTON baton missing NFT commitment")?
        .to_ascii_lowercase();
    let token_amount_text = utxo
        .pointer("/tokenData/amount")
        .and_then(Value::as_str)
        .ok_or("native PHOTON baton missing token amount")?;
    let token_amount = token_amount_text
        .parse::<u128>()
        .map_err(|_| format!("bad native PHOTON token amount: {token_amount_text}"))?;

    Ok(NativePhotonBaton {
        txid: txid.to_ascii_lowercase(),
        vout,
        height,
        value_sats,
        commitment_hex,
        token_amount,
    })
}

/// Parses baton information from a gettxout response.
fn parse_native_gettxout_baton(
    txid: &str,
    vout: u32,
    tip_height: u32,
    expected_bestblock: &str,
    txout: &Value,
) -> Result<NativePhotonBaton, String> {
    let bestblock = txout
        .get("bestblock")
        .and_then(Value::as_str)
        .ok_or("gettxout omitted bestblock")?;
    if !bestblock.eq_ignore_ascii_case(expected_bestblock) {
        return Err("gettxout PHOTON snapshot does not match scantxoutset bestblock".into());
    }
    let script = txout
        .pointer("/scriptPubKey/hex")
        .and_then(Value::as_str)
        .ok_or("gettxout omitted scriptPubKey.hex")?;
    if !script.eq_ignore_ascii_case(COVENANT_LOCKING_BYTECODE_HEX) {
        return Err("native PHOTON baton locking bytecode does not match the covenant".into());
    }

    let category = txout
        .pointer("/tokenData/category")
        .and_then(Value::as_str)
        .ok_or("gettxout omitted PHOTON token category")?;
    if !category.eq_ignore_ascii_case(MAINNET_CATEGORY_HEX) {
        return Err("native PHOTON baton token category mismatch".into());
    }
    if txout
        .pointer("/tokenData/nft/capability")
        .and_then(Value::as_str)
        != Some("mutable")
    {
        return Err("native PHOTON baton NFT is not mutable".into());
    }

    let commitment_hex = txout
        .pointer("/tokenData/nft/commitment")
        .and_then(Value::as_str)
        .ok_or("gettxout omitted PHOTON NFT commitment")?
        .to_ascii_lowercase();
    let token_amount_text = txout
        .pointer("/tokenData/amount")
        .and_then(Value::as_str)
        .ok_or("gettxout omitted PHOTON token amount")?;
    let token_amount = token_amount_text
        .parse::<u128>()
        .map_err(|_| format!("bad native PHOTON token amount: {token_amount_text}"))?;
    let value = txout
        .get("value")
        .ok_or_else(|| "gettxout omitted PHOTON BCH value".to_string())?;
    let value_sats = bch_value_to_sats(value)?;
    let confirmations = txout
        .get("confirmations")
        .and_then(Value::as_u64)
        .ok_or("gettxout omitted PHOTON confirmations")?;
    let confirmations = u32::try_from(confirmations)
        .map_err(|_| "PHOTON confirmation count exceeds u32".to_string())?;
    let baton_height = if confirmations == 0 {
        0
    } else {
        tip_height
            .checked_add(1)
            .and_then(|height_plus_one| height_plus_one.checked_sub(confirmations))
            .ok_or("PHOTON confirmations are inconsistent with node tip height")?
    };

    Ok(NativePhotonBaton {
        txid: txid.to_ascii_lowercase(),
        vout,
        height: baton_height,
        value_sats,
        commitment_hex,
        token_amount,
    })
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
        return Err("no node endpoints -- set `node http://user:pass@127.0.0.1:8332`".into());
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

/// Strips credentials from a node URL before reporting errors.
pub(crate) fn redact_url(url: &str) -> String {
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

/// Chooses URL or environment credentials for basic authentication.
fn select_node_rpc_basic_auth(
    url_auth: Option<&str>,
    env_user: Option<&str>,
    env_password: Option<&str>,
) -> Result<Option<String>, String> {
    if let Some(auth) = url_auth {
        return Ok(Some(auth.to_string()));
    }
    match (env_user, env_password) {
        (None, None) => Ok(None),
        (Some(user), Some(password)) => Ok(Some(format!("{user}:{password}"))),
        _ => Err(format!(
            "native node RPC environment auth requires both {NODE_RPC_USER_ENV} and {NODE_RPC_PASSWORD_ENV}"
        )),
    }
}

/// Builds the HTTP basic-auth header for a node request.
fn node_rpc_basic_auth(url_auth: Option<&str>) -> Result<Option<String>, String> {
    let env_user = std::env::var(NODE_RPC_USER_ENV).ok();
    let env_password = std::env::var(NODE_RPC_PASSWORD_ENV).ok();
    select_node_rpc_basic_auth(url_auth, env_user.as_deref(), env_password.as_deref())
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
    /// Prints a redacted summary of the connected native node.
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
/// Converts compact target bits to a little-endian target hex string.
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

/// Fetches the native node’s current block template.
pub fn fetch_block_template(endpoints: &[String]) -> Result<BlockTemplate, String> {
    if endpoints.is_empty() {
        return Err("no node endpoints -- set `node http://user:pass@127.0.0.1:8332`".into());
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeRpcScheme {
    Http,
    Https,
}

struct NodeRpcTarget {
    scheme: NodeRpcScheme,
    url_auth: Option<String>,
    host: String,
    port: u16,
    path: String,
}

/// Extracts a valid mining target from a node RPC response.
fn parse_node_rpc_target(url: &str) -> Result<NodeRpcTarget, String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("node URL must be http(s)".into());
    }
    let (scheme, rest, default_port) = if lower.starts_with("https://") {
        (NodeRpcScheme::Https, &url["https://".len()..], 443u16)
    } else {
        (NodeRpcScheme::Http, &url["http://".len()..], 8332u16)
    };

    let path_start = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..path_start];
    let path = match rest.get(path_start..) {
        Some("") | None => "/".to_string(),
        Some(suffix) if suffix.starts_with('?') => format!("/{suffix}"),
        Some(suffix) => suffix.to_string(),
    };
    let (url_auth, hostport) = if let Some((auth, hostport)) = authority.rsplit_once('@') {
        if auth.is_empty() {
            return Err("node URL has empty userinfo".into());
        }
        (Some(auth.to_string()), hostport)
    } else {
        (None, authority)
    };
    if hostport.is_empty() {
        return Err("node URL is missing host".into());
    }

    let (host, port) = if let Some(bracketed) = hostport.strip_prefix('[') {
        let Some(close) = bracketed.find(']') else {
            return Err("node URL has malformed IPv6 host".into());
        };
        let host = &bracketed[..close];
        if host.is_empty() {
            return Err("node URL is missing host".into());
        }
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else if let Some(port) = suffix.strip_prefix(':') {
            port.parse::<u16>().map_err(|_| "bad node port")?
        } else {
            return Err("node URL has malformed host/port".into());
        };
        (host.to_string(), port)
    } else {
        if hostport.matches(':').count() > 1 {
            return Err("IPv6 node hosts must use brackets".into());
        }
        match hostport.rsplit_once(':') {
            Some((host, port)) => {
                if host.is_empty() {
                    return Err("node URL is missing host".into());
                }
                (
                    host.to_string(),
                    port.parse::<u16>().map_err(|_| "bad node port")?,
                )
            }
            None => (hostport.to_string(), default_port),
        }
    };

    Ok(NodeRpcTarget {
        scheme,
        url_auth,
        host,
        port,
        path,
    })
}

trait NodeRpcStream: Read + Write {}
impl<T: Read + Write> NodeRpcStream for T {}

/// Sends a JSON-RPC request and validates its response.
fn rpc_call(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let target = parse_node_rpc_target(url)?;
    let auth = node_rpc_basic_auth(target.url_auth.as_deref())?;
    let host = &target.host;
    let port = target.port;
    let path = &target.path;

    let body = json!({"jsonrpc":"1.0","id":"pickaxe","method":method,"params":params}).to_string();
    let host_header = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_header}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(a) = auth.as_deref() {
        let b64 = simple_b64(a.as_bytes());
        req.push_str(&format!("Authorization: Basic {b64}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);

    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve: {e}"))?
        .next()
        .ok_or_else(|| "resolve: no addrs".to_string())?;
    let tcp_stream = TcpStream::connect_timeout(&addr, Duration::from_secs(8))
        .map_err(|e| format!("connect: {e}"))?;
    let _ = tcp_stream.set_read_timeout(Some(Duration::from_secs(12)));
    let _ = tcp_stream.set_write_timeout(Some(Duration::from_secs(8)));
    let mut stream: Box<dyn NodeRpcStream> = match target.scheme {
        NodeRpcScheme::Http => Box::new(tcp_stream),
        NodeRpcScheme::Https => {
            let connector = TlsConnector::new().map_err(|e| format!("tls setup: {e}"))?;
            let tls_stream = connector
                .connect(host.as_str(), tcp_stream)
                .map_err(|e| format!("tls handshake: {e}"))?;
            Box::new(tls_stream)
        }
    };
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

/// Base64-encodes credentials for the RPC authorization header.
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
    use crate::electrum::live_job_from_fulcrum_values;
    use std::net::TcpListener;
    use std::thread;

    const FIXTURE_HEADER_HEX: &str = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";

    fn serve_json_rpc_sequence(
        responses: Vec<(&'static str, Value)>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for (expected_method, response_value) in responses {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "timed out waiting for JSON-RPC method {expected_method}"
                            );
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("test JSON-RPC accept failed: {error}"),
                    }
                };
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let read = stream.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(headers_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers_end = headers_end + 4;
                        let headers = String::from_utf8_lossy(&request[..headers_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if request.len() >= headers_end + content_length {
                            break;
                        }
                    }
                }
                let body_start = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap();
                let request_json: Value = serde_json::from_slice(&request[body_start..]).unwrap();
                assert_eq!(
                    request_json.get("method").and_then(Value::as_str),
                    Some(expected_method)
                );
                let body =
                    json!({"result": response_value, "error": null, "id": "pickaxe"}).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), server)
    }

    fn assert_same_photon_state(left: &LiveJob, right: &LiveJob) {
        assert_eq!(left.height, right.height);
        assert_eq!(left.baton_txid, right.baton_txid);
        assert_eq!(left.baton_vout, right.baton_vout);
        assert_eq!(left.baton_height, right.baton_height);
        assert_eq!(left.baton_value_sats, right.baton_value_sats);
        assert_eq!(left.commitment_hex, right.commitment_hex);
        assert_eq!(left.token_amount, right.token_amount);
        assert_eq!(left.age, right.age);
        assert_eq!(left.target_le_hex, right.target_le_hex);
        assert_eq!(left.reward_raw, right.reward_raw);
    }

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
    fn node_rpc_auth_uses_complete_env_pair_and_url_auth_takes_precedence() {
        assert_eq!(
            select_node_rpc_basic_auth(None, Some("rpc-user"), Some("rpc-password"))
                .unwrap()
                .as_deref(),
            Some("rpc-user:rpc-password")
        );
        assert_eq!(
            select_node_rpc_basic_auth(
                Some("url-user:url-password"),
                Some("rpc-user"),
                Some("rpc-password")
            )
            .unwrap()
            .as_deref(),
            Some("url-user:url-password")
        );
        assert!(select_node_rpc_basic_auth(None, Some("rpc-user"), None).is_err());
        assert!(select_node_rpc_basic_auth(None, None, Some("rpc-password")).is_err());
        assert_eq!(select_node_rpc_basic_auth(None, None, None).unwrap(), None);
    }

    #[test]
    fn node_rpc_url_parsing_supports_https_defaults_auth_paths_and_ipv6() {
        let https =
            parse_node_rpc_target("https://rpc-user:rpc-password@node.example/rpc?mode=wallet")
                .unwrap();
        assert_eq!(https.scheme, NodeRpcScheme::Https);
        assert_eq!(https.url_auth.as_deref(), Some("rpc-user:rpc-password"));
        assert_eq!(https.host, "node.example");
        assert_eq!(https.port, 443);
        assert_eq!(https.path, "/rpc?mode=wallet");

        let http = parse_node_rpc_target("http://127.0.0.1").unwrap();
        assert_eq!(http.scheme, NodeRpcScheme::Http);
        assert_eq!(http.port, 8332);
        assert_eq!(http.path, "/");

        let explicit = parse_node_rpc_target("https://[::1]:18443/wallet/main").unwrap();
        assert_eq!(explicit.scheme, NodeRpcScheme::Https);
        assert_eq!(explicit.host, "::1");
        assert_eq!(explicit.port, 18443);
        assert_eq!(explicit.path, "/wallet/main");
    }

    #[test]
    fn node_rpc_request_preserves_http_path_host_and_basic_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0, "client closed before completing request headers");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("POST /wallet/main HTTP/1.1\r\n"));
            assert!(request.contains(&format!("\r\nHost: {address}\r\n")));
            assert!(request.contains("\r\nAuthorization: Basic dTpw\r\n"));
            assert!(request.contains("\"method\":\"getblockchaininfo\""));

            let body = r#"{"result":{"chain":"main"},"error":null,"id":"pickaxe"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let endpoint = format!("http://u:p@{address}/wallet/main");
        let result = rpc_call(&endpoint, "getblockchaininfo", json!([])).unwrap();
        server.join().unwrap();
        assert_eq!(result.get("chain").and_then(Value::as_str), Some("main"));
    }

    #[test]
    fn https_rpc_enters_verified_tls_path_without_leaking_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
        });

        let endpoint = format!("https://rpc-user:rpc-password@{address}/rpc");
        let error = rpc_call(&endpoint, "getblockchaininfo", json!([])).unwrap_err();
        server.join().unwrap();
        assert!(
            error.starts_with("tls handshake:"),
            "unexpected error: {error}"
        );
        assert!(!error.contains("rpc-user"));
        assert!(!error.contains("rpc-password"));
        assert_eq!(redact_url(&endpoint), format!("https://***@{address}/rpc"));
    }

    #[test]
    fn node_connection_failure_redacts_embedded_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let endpoint = format!("http://rpc-user:rpc-password@{address}");

        let error = connect_failover(&[endpoint]).unwrap_err();

        assert!(!error.contains("rpc-user"));
        assert!(!error.contains("rpc-password"));
        assert!(error.contains(&format!("http://***@{address}")));
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

    #[test]
    fn native_photon_state_matches_canonical_fulcrum_state() {
        let txid = "11".repeat(32);
        let bestblock = "22".repeat(32);
        let target = "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000";
        let commitment = format!("01000000{target}");
        let token_amount = "2100000000000000";
        let token_data = json!({
            "category": MAINNET_CATEGORY_HEX,
            "amount": token_amount,
            "nft": {
                "capability": "mutable",
                "commitment": commitment,
            }
        });
        let txout = json!({
            "bestblock": bestblock,
            "confirmations": 11,
            "value": 0.15971500,
            "scriptPubKey": {"hex": COVENANT_LOCKING_BYTECODE_HEX},
            "tokenData": token_data,
        });
        let scan = json!({
            "success": true,
            "height": 1000,
            "bestblock": bestblock,
            "unspents": [{
                "txid": txid,
                "vout": 0,
                "scriptPubKey": COVENANT_LOCKING_BYTECODE_HEX,
                "amount": 0.15971500,
                "height": 990,
                "tokenData": token_data,
            }]
        });
        let (endpoint, server) = serve_json_rpc_sequence(vec![
            ("scantxoutset", scan),
            ("gettxout", txout.clone()),
            ("getbestblockhash", json!(bestblock.clone())),
            ("gettxout", txout.clone()),
            (
                "getblockchaininfo",
                json!({"blocks": 1000, "bestblockhash": bestblock.clone()}),
            ),
            ("gettxout", txout.clone()),
            ("getbestblockhash", json!(bestblock.clone())),
            ("gettxout", txout),
        ]);
        let mut native_session =
            NativePhotonSession::connect_failover(std::slice::from_ref(&endpoint)).unwrap();
        let native = native_session.snapshot().job.clone();
        let refreshed = native_session.refresh().unwrap();
        server.join().unwrap();

        let fulcrum = live_job_from_fulcrum_values(
            "wss://fixture.invalid",
            json!(["Fulcrum", "1.5"]),
            &json!({"height": 1000, "hex": FIXTURE_HEADER_HEX}),
            &json!([{
                "tx_hash": txid,
                "tx_pos": 0,
                "height": 990,
                "value": 15_971_500,
                "token_data": {
                    "category": MAINNET_CATEGORY_HEX,
                    "amount": token_amount,
                    "nft": {
                        "capability": "mutable",
                        "commitment": commitment,
                    }
                }
            }]),
        )
        .unwrap();

        assert_same_photon_state(&native, &fulcrum);
        assert_same_photon_state(&refreshed.job, &fulcrum);
        assert_eq!(refreshed.tip_hash, bestblock);

        let native_snapshot = LiveStateSnapshot {
            tip_hash: bestblock.clone(),
            job: native.clone(),
        };
        let mut canonical_snapshot = LiveStateSnapshot {
            tip_hash: bestblock,
            job: fulcrum.clone(),
        };
        verify_photon_state_equivalence(&native_snapshot, &canonical_snapshot).unwrap();

        canonical_snapshot.tip_hash = "23".repeat(32);
        let tip_error =
            verify_photon_state_equivalence(&native_snapshot, &canonical_snapshot).unwrap_err();
        assert!(tip_error.contains("tip mismatch"), "{tip_error}");

        canonical_snapshot = native_snapshot.clone();
        canonical_snapshot.job.baton_value_sats += 1;
        let field_error =
            verify_photon_state_equivalence(&native_snapshot, &canonical_snapshot).unwrap_err();
        assert!(field_error.contains("baton_value_sats"), "{field_error}");
        assert_eq!(native.age, 10);
    }

    #[test]
    fn native_photon_state_matches_fulcrum_unconfirmed_successor() {
        let txid = "33".repeat(32);
        let successor_txid = "55".repeat(32);
        let bestblock = "44".repeat(32);
        let confirmed_commitment = format!(
            "01000000{}",
            "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
        );
        let successor_commitment = format!(
            "02000000{}",
            "ab9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
        );
        let successor_token_amount = "2099995000000001";
        let scan = json!({
            "success": true,
            "height": 1000,
            "bestblock": bestblock,
            "unspents": [{
                "txid": txid,
                "vout": 0,
                "scriptPubKey": COVENANT_LOCKING_BYTECODE_HEX,
                "amount": 0.15971500,
                "height": 990,
                "tokenData": {
                    "category": MAINNET_CATEGORY_HEX,
                    "amount": "2100000000000000",
                    "nft": {"capability": "mutable", "commitment": confirmed_commitment}
                }
            }]
        });
        let successor_token_data = json!({
            "category": MAINNET_CATEGORY_HEX,
            "amount": successor_token_amount,
            "nft": {"capability": "mutable", "commitment": successor_commitment}
        });
        let mempool = json!({
            successor_txid.clone(): {
                "size": 615,
                "time": 1234,
                "depends": [],
                "spentby": []
            }
        });
        let successor_transaction = json!({
            "txid": successor_txid,
            "vin": [{"txid": txid, "vout": 0}],
            "vout": [{
                "n": 0,
                "value": 0.15970885,
                "scriptPubKey": {"hex": COVENANT_LOCKING_BYTECODE_HEX},
                "tokenData": successor_token_data
            }, {
                "n": 1,
                "value": 0.00000546,
                "scriptPubKey": {"hex": "76a914000000000000000000000000000000000000000088ac"}
            }]
        });
        let successor_txout = json!({
            "bestblock": bestblock,
            "confirmations": 0,
            "value": 0.15970885,
            "scriptPubKey": {"hex": COVENANT_LOCKING_BYTECODE_HEX},
            "tokenData": successor_token_data,
        });
        let (endpoint, server) = serve_json_rpc_sequence(vec![
            ("scantxoutset", scan),
            ("gettxout", Value::Null),
            ("getrawmempool", mempool),
            ("getrawtransaction", successor_transaction),
            ("getbestblockhash", json!(bestblock.clone())),
            ("gettxout", successor_txout),
        ]);
        let native = fetch_photon_live_job(std::slice::from_ref(&endpoint)).unwrap();
        server.join().unwrap();

        let fulcrum = live_job_from_fulcrum_values(
            "wss://fixture.invalid",
            json!(["Fulcrum", "1.5"]),
            &json!({"height": 1000, "hex": FIXTURE_HEADER_HEX}),
            &json!([{
                "tx_hash": successor_txid,
                "tx_pos": 0,
                "height": 0,
                "value": 15_970_885,
                "token_data": {
                    "category": MAINNET_CATEGORY_HEX,
                    "amount": successor_token_amount,
                    "nft": {
                        "capability": "mutable",
                        "commitment": successor_commitment,
                    }
                }
            }]),
        )
        .unwrap();

        assert_same_photon_state(&native, &fulcrum);
        assert_eq!(native.baton_height, 0);
        assert_eq!(native.age, 0);
    }

    #[test]
    fn native_photon_state_follows_baton_descendant_past_reward_child() {
        let confirmed_txid = "81".repeat(32);
        let first_txid = "82".repeat(32);
        let reward_child_txid = "83".repeat(32);
        let final_txid = "84".repeat(32);
        let bestblock = "85".repeat(32);
        let target = "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000";
        let confirmed_commitment = format!("01000000{target}");
        let first_commitment = format!("02000000{target}");
        let final_commitment = format!("03000000{target}");
        let first_token_amount = "2099995000000001";
        let final_token_amount = "2099990000011906";

        let scan = json!({
            "success": true,
            "height": 1000,
            "bestblock": bestblock,
            "unspents": [{
                "txid": confirmed_txid,
                "vout": 0,
                "scriptPubKey": COVENANT_LOCKING_BYTECODE_HEX,
                "amount": 0.15971500,
                "height": 990,
                "tokenData": {
                    "category": MAINNET_CATEGORY_HEX,
                    "amount": "2100000000000000",
                    "nft": {"capability": "mutable", "commitment": confirmed_commitment}
                }
            }]
        });
        let first_token_data = json!({
            "category": MAINNET_CATEGORY_HEX,
            "amount": first_token_amount,
            "nft": {"capability": "mutable", "commitment": first_commitment}
        });
        let final_token_data = json!({
            "category": MAINNET_CATEGORY_HEX,
            "amount": final_token_amount,
            "nft": {"capability": "mutable", "commitment": final_commitment}
        });
        let mempool = json!({
            first_txid.clone(): {
                "time": 100,
                "depends": [],
                "spentby": [reward_child_txid.clone(), final_txid.clone()]
            },
            reward_child_txid.clone(): {
                "time": 101,
                "depends": [first_txid.clone()],
                "spentby": []
            },
            final_txid.clone(): {
                "time": 102,
                "depends": [first_txid.clone()],
                "spentby": []
            }
        });
        let first_transaction = json!({
            "txid": first_txid,
            "vin": [{"txid": confirmed_txid, "vout": 0}],
            "vout": [{
                "n": 0,
                "value": 0.15970885,
                "scriptPubKey": {"hex": COVENANT_LOCKING_BYTECODE_HEX},
                "tokenData": first_token_data
            }, {
                "n": 1,
                "value": 0.00000546,
                "scriptPubKey": {"hex": "76a914000000000000000000000000000000000000000088ac"}
            }]
        });
        let reward_child = json!({
            "txid": reward_child_txid,
            "vin": [{"txid": first_txid, "vout": 1}],
            "vout": [{
                "n": 0,
                "value": 0.00000300,
                "scriptPubKey": {"hex": "76a914111111111111111111111111111111111111111188ac"}
            }]
        });
        let final_transaction = json!({
            "txid": final_txid,
            "vin": [{"txid": first_txid, "vout": 0}],
            "vout": [{
                "n": 0,
                "value": 0.15970270,
                "scriptPubKey": {"hex": COVENANT_LOCKING_BYTECODE_HEX},
                "tokenData": final_token_data
            }, {
                "n": 1,
                "value": 0.00000546,
                "scriptPubKey": {"hex": "76a914222222222222222222222222222222222222222288ac"}
            }]
        });
        let final_txout = json!({
            "bestblock": bestblock,
            "confirmations": 0,
            "value": 0.15970270,
            "scriptPubKey": {"hex": COVENANT_LOCKING_BYTECODE_HEX},
            "tokenData": final_token_data,
        });

        let (endpoint, server) = serve_json_rpc_sequence(vec![
            ("scantxoutset", scan),
            ("gettxout", Value::Null),
            ("getrawmempool", mempool),
            ("getrawtransaction", first_transaction),
            ("getrawtransaction", reward_child),
            ("getrawtransaction", final_transaction),
            ("getbestblockhash", json!(bestblock.clone())),
            ("gettxout", final_txout),
        ]);
        let native = fetch_photon_live_job(std::slice::from_ref(&endpoint)).unwrap();
        server.join().unwrap();

        let fulcrum = live_job_from_fulcrum_values(
            "wss://fixture.invalid",
            json!(["Fulcrum", "1.5"]),
            &json!({"height": 1000, "hex": FIXTURE_HEADER_HEX}),
            &json!([{
                "tx_hash": final_txid,
                "tx_pos": 0,
                "height": 0,
                "value": 15_970_270,
                "token_data": {
                    "category": MAINNET_CATEGORY_HEX,
                    "amount": final_token_amount,
                    "nft": {
                        "capability": "mutable",
                        "commitment": final_commitment,
                    }
                }
            }]),
        )
        .unwrap();

        assert_same_photon_state(&native, &fulcrum);
        assert_eq!(native.baton_txid, final_txid);
    }

    #[test]
    fn native_photon_state_fails_closed_when_mempool_successor_cannot_be_proven() {
        let txid = "66".repeat(32);
        let bestblock = "77".repeat(32);
        let commitment = format!(
            "01000000{}",
            "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
        );
        let scan = json!({
            "success": true,
            "height": 1000,
            "bestblock": bestblock,
            "unspents": [{
                "txid": txid,
                "vout": 0,
                "scriptPubKey": COVENANT_LOCKING_BYTECODE_HEX,
                "amount": 0.15971500,
                "height": 990,
                "tokenData": {
                    "category": MAINNET_CATEGORY_HEX,
                    "amount": "2100000000000000",
                    "nft": {"capability": "mutable", "commitment": commitment}
                }
            }]
        });
        let (endpoint, server) = serve_json_rpc_sequence(vec![
            ("scantxoutset", scan),
            ("gettxout", Value::Null),
            ("getrawmempool", json!({})),
        ]);
        let error = fetch_photon_live_job(std::slice::from_ref(&endpoint)).unwrap_err();
        server.join().unwrap();

        assert!(
            error.contains("no canonical successor could be proven"),
            "{error}"
        );
    }
}
