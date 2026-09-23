//! PHOTON mainnet protocol constants extracted from reference/miner.js (M67.38).
//! Electrum/win-tx stay in Dev Assist modules — this is search/crypto shared facts only.

use num_bigint::BigUint;
use sha2::{Digest, Sha256};

/// CashToken category id (hex, 32 bytes).
pub const MAINNET_CATEGORY_HEX: &str =
    "29972959d6f0dc766cdcb81bfaf8171c5605a64dd0a81fa46080f84ac87c9bef";

/// Covenant locking bytecode (hex).
pub const COVENANT_LOCKING_BYTECODE_HEX: &str =
    "aa209a2c0f31147dda170e59aaa7982e4fe3fc25928bf09f15fe1e797a2ccb05c6e087";

/// Expected Electrum script hash for the covenant (hex, reversed-SHA256 of lock).
pub const EXPECTED_SCRIPT_HASH_HEX: &str =
    "720bad85599cd504b114c65caedb76098cab45df5553be1c7cf260c4c2954031";

/// Redeem script hex (P2SH32 / covenant spend path) — from postcorps miner.js.
pub const REDEEM_SCRIPT_HEX: &str = include_str!("../reference/photon_redeem.hex");

fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

fn decode_positive_script_number(bytes: &[u8]) -> Result<u64, String> {
    if bytes.is_empty() {
        return Ok(0);
    }
    if bytes.len() > 8 {
        return Err("PHOTON covenant value rule exceeds u64 ScriptNum range".into());
    }
    if bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
        return Err("PHOTON covenant value rule must be non-negative".into());
    }
    if bytes == [0]
        || (bytes.len() > 1 && bytes.last() == Some(&0) && bytes[bytes.len() - 2] & 0x80 == 0)
    {
        return Err("PHOTON covenant value rule uses a non-minimal ScriptNum".into());
    }

    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u64::from(*byte) << (index * 8);
    }
    Ok(value)
}

fn authoritative_redeem_script() -> Result<Vec<u8>, String> {
    let redeem_script = hex::decode(REDEEM_SCRIPT_HEX.trim()).map_err(|error| error.to_string())?;
    if redeem_script.len() != 259 {
        return Err(format!(
            "PHOTON redeem script must be 259 bytes (got {})",
            redeem_script.len()
        ));
    }

    let covenant_lock =
        hex::decode(COVENANT_LOCKING_BYTECODE_HEX).map_err(|error| error.to_string())?;
    if covenant_lock.len() != 35
        || covenant_lock[0] != 0xaa
        || covenant_lock[1] != 0x20
        || covenant_lock[34] != 0x87
    {
        return Err("PHOTON covenant locking bytecode is not canonical P2SH32".into());
    }
    if covenant_lock[2..34] != hash256(&redeem_script) {
        return Err("PHOTON redeem script does not match the covenant locking bytecode".into());
    }
    Ok(redeem_script)
}

pub fn photon_authoritative_redeem_script() -> Result<Vec<u8>, String> {
    authoritative_redeem_script()
}

fn extract_baton_decrease_rule(
    redeem_script: &[u8],
    prefix: &[u8],
    suffix: &[u8],
    label: &str,
) -> Result<u64, String> {
    let mut found = None;
    for start in 0..redeem_script.len() {
        if redeem_script.get(start..start + prefix.len()) != Some(prefix) {
            continue;
        }
        let push_len = match redeem_script.get(start + prefix.len()) {
            Some(1..=8) => usize::from(redeem_script[start + prefix.len()]),
            _ => continue,
        };
        let data_start = start + prefix.len() + 1;
        let Some(data_end) = data_start.checked_add(push_len) else {
            continue;
        };
        let Some(suffix_end) = data_end.checked_add(suffix.len()) else {
            continue;
        };
        if redeem_script.get(data_end..suffix_end) != Some(suffix) {
            continue;
        }
        let data = redeem_script
            .get(data_start..data_end)
            .ok_or("truncated PHOTON covenant value rule")?;
        let value = decode_positive_script_number(data)?;
        if found.replace(value).is_some() {
            return Err(format!(
                "PHOTON redeem script contains multiple {label} value rules"
            ));
        }
    }
    found.ok_or_else(|| format!("PHOTON redeem script {label} value rule was not found"))
}

pub fn photon_single_input_max_baton_decrease_sats() -> Result<u64, String> {
    let redeem_script = authoritative_redeem_script()?;
    // output[active].value >= input[active].value - <budget>
    extract_baton_decrease_rule(
        &redeem_script,
        &[0xc0, 0xcc, 0xc0, 0xc6],
        &[0x94, 0xa2, 0x69],
        "single-input",
    )
}

pub fn photon_multi_input_max_baton_decrease_sats() -> Result<u64, String> {
    let redeem_script = authoritative_redeem_script()?;
    // output[active].value + <budget> >= input[active].value
    extract_baton_decrease_rule(
        &redeem_script,
        &[0xc0, 0xcc],
        &[0x93, 0xc0, 0xc6, 0xa2, 0x69],
        "multi-input",
    )
}

/// Selene's published BCH mainnet WebSocket endpoints.
pub const SELENE_WSS_BOOTSTRAP: &[&str] = &[
    "wss://cashnode.bch.ninja:50004",
    "wss://bch.imaginary.cash:50004",
    "wss://bitcoincash.network:50004",
    "wss://blackie.c3-soft.com:50004",
    "wss://bch.loping.net:50004",
    "wss://bch.soul-dev.com:50004",
    "wss://bitcoincash.stackwallet.com:50004",
    "wss://node.minisatoshi.cash:50004",
    "wss://fulcrum.criptolayer.net:50004",
];

/// Published/curated BCH mainnet **WSS** endpoints used by the current client.
/// Do not infer WSS support from Electron Cash's TLS/TCP ports.
pub const FULCRUM_WSS_BOOTSTRAP: &[&str] = &[
    "wss://cashnode.bch.ninja:50004",
    "wss://bch.imaginary.cash:50004",
    "wss://bitcoincash.network:50004",
    "wss://blackie.c3-soft.com:50004",
    "wss://bch.loping.net:50004",
    "wss://bch.soul-dev.com:50004",
    "wss://bitcoincash.stackwallet.com:50004",
    "wss://node.minisatoshi.cash:50004",
    "wss://fulcrum.criptolayer.net:50004",
    "wss://electrum.imaginary.cash:50004",
    "wss://electroncash.dk:50004",
    "wss://fulcrum.greyh.at:50004",
    "wss://electron.jochen-hoenicke.de:51004",
    "wss://fulcrum.jettscythe.xyz:50004",
    "wss://fulcrum.kronbit.com:50004",
];

/// Electron Cash mainnet servers published with the `s` (TLS) transport.
/// These are catalog metadata until Pickaxe grows a native Electrum TLS client.
pub const ELECTRON_CASH_TLS_BOOTSTRAP: &[(&str, u16)] = &[
    ("bch.crypto.mldlabs.com", 50002),
    ("bch.cyberbits.eu", 50002),
    ("bch.imaginary.cash", 50002),
    ("bch.loping.net", 50002),
    (
        "j2tjfxntnsqpojaamnndgmfrc6lh3thattnlpc2xx53h2ojoi7agccid.onion",
        50002,
    ),
    ("bch.soul-dev.com", 50002),
    ("bch0.kister.net", 50002),
    ("bch2.electroncash.dk", 50002),
    ("bitcoincash.network", 50002),
    ("blackie.c3-soft.com", 50002),
    ("cashnode.bch.ninja", 50002),
    ("electron.jochen-hoenicke.de", 51002),
    ("electroncash.dk", 50002),
    ("electrs.bitcoinunlimited.info", 50002),
    ("electrum.bitcoinverde.org", 50002),
    ("electrum.imaginary.cash", 50002),
    (
        "jh3jgcrwweh6yvmprtjnp72u2hqn34nlftlg3msrr4vmlapft4yvt2id.onion",
        50002,
    ),
    ("fulcrum.aglauck.com", 50002),
    ("fulcrum.criptolayer.net", 50002),
    ("fulcrum.jettscythe.xyz", 50002),
    ("node.minisatoshi.cash", 50002),
];

/// Electron Cash mainnet servers published with the `t` (plain TCP) transport.
/// They remain catalog-only while the runtime transport is WebSocket-only.
pub const ELECTRON_CASH_TCP_BOOTSTRAP: &[(&str, u16)] = &[
    ("bch.crypto.mldlabs.com", 50001),
    ("bch.imaginary.cash", 50001),
    ("bch0.kister.net", 50001),
    ("bch.loping.net", 50001),
    (
        "j2tjfxntnsqpojaamnndgmfrc6lh3thattnlpc2xx53h2ojoi7agccid.onion",
        50001,
    ),
    ("blackie.c3-soft.com", 50001),
    ("electron.jochen-hoenicke.de", 51001),
    ("electroncash.dk", 50001),
    ("bch2.electroncash.dk", 50001),
    ("electrum.imaginary.cash", 50001),
    (
        "kisternet5tgeekwidrj7r7yd3n2l5j7y72b74y6xu3q2b6xdjrte6id.onion",
        50001,
    ),
    (
        "jh3jgcrwweh6yvmprtjnp72u2hqn34nlftlg3msrr4vmlapft4yvt2id.onion",
        50001,
    ),
    ("electrum.bitcoinverde.org", 50001),
    ("cashnode.bch.ninja", 50001),
    ("fulcrum.criptolayer.net", 50001),
];

/// Curated native **node** JSON-RPC bootstrap (BCHN/bitcoind-style HTTP).
/// Unauthenticated `getblockchaininfo` did not answer on public host:8332
/// (timeout or connection refused) or on common HTTPS RPC hostnames (404 or
/// DNS failure). This list stays empty instead of shipping dead RPC URLs.
/// A configured node is used when it is healthy and its PHOTON proof is current.
pub const NODE_RPC_BOOTSTRAP: &[&str] = &[];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotonDerivedState {
    pub age: u32,
    pub target_le_hex: String,
    pub reward_raw: u128,
}

/// Derive provider-independent PHOTON mining state from the authoritative
/// mutable baton fields.
pub fn derive_photon_state(
    commitment_hex: &str,
    token_amount: u128,
    height: u32,
    baton_height: u32,
) -> Result<PhotonDerivedState, String> {
    if commitment_hex.len() < 72 || !commitment_hex.len().is_multiple_of(2) {
        return Err("Live PHOTON baton commitment is missing or too short.".into());
    }
    if !commitment_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Live PHOTON baton commitment is not hexadecimal.".into());
    }

    let age = if baton_height == 0 {
        0
    } else {
        height.saturating_sub(baton_height)
    };
    let previous_target = le_hex_to_biguint(&commitment_hex[8..72])?;
    let next_target = previous_target * (BigUint::from(age as u64) + BigUint::from(143u64))
        / BigUint::from(144u64);
    let target_le_hex = biguint_to_le_hex32(&next_target)?;
    let reward_raw = token_amount
        .checked_div(420_000)
        .and_then(|value| value.checked_sub(1))
        .ok_or("reward_raw underflow")?;

    Ok(PhotonDerivedState {
        age,
        target_le_hex,
        reward_raw,
    })
}

fn le_hex_to_biguint(hex_str: &str) -> Result<BigUint, String> {
    if !hex_str.len().is_multiple_of(2) {
        return Err("Invalid little-endian hex.".into());
    }
    let bytes = hex::decode(hex_str).map_err(|error| error.to_string())?;
    Ok(BigUint::from_bytes_le(&bytes))
}

fn biguint_to_le_hex32(value: &BigUint) -> Result<String, String> {
    let mut bytes = value.to_bytes_le();
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
    fn photon_target_derivation_preserves_reference_little_endian_math() {
        let previous_target = "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000";
        let commitment = format!("00000000{previous_target}");
        let state = derive_photon_state(&commitment, 2_100_000_000_000_000, 1_000, 1_000)
            .expect("reference-shaped PHOTON state");

        assert_eq!(state.age, 0);
        let expected = le_hex_to_biguint(previous_target).unwrap() * BigUint::from(143u32)
            / BigUint::from(144u32);
        assert_eq!(state.target_le_hex, biguint_to_le_hex32(&expected).unwrap());
        assert_eq!(state.reward_raw, 4_999_999_999);
    }

    #[test]
    fn covenant_baton_decrease_budgets_come_from_authoritative_redeem_script() {
        assert_eq!(
            photon_single_input_max_baton_decrease_sats().unwrap(),
            1_500
        );
        assert_eq!(photon_multi_input_max_baton_decrease_sats().unwrap(), 8_000);
    }

    #[test]
    fn covenant_value_rule_extractors_fail_closed_on_script_drift() {
        let mut redeem_script = authoritative_redeem_script().unwrap();

        let single_rule = [0xc0, 0xcc, 0xc0, 0xc6, 0x02, 0xdc, 0x05, 0x94, 0xa2, 0x69];
        let single = redeem_script
            .windows(single_rule.len())
            .position(|window| window == single_rule)
            .expect("authoritative single-input value rule");
        redeem_script[single + 7] = 0x93;
        assert!(extract_baton_decrease_rule(
            &redeem_script,
            &[0xc0, 0xcc, 0xc0, 0xc6],
            &[0x94, 0xa2, 0x69],
            "single-input",
        )
        .is_err());

        let mut redeem_script = authoritative_redeem_script().unwrap();
        let multi_rule = [0xc0, 0xcc, 0x02, 0x40, 0x1f, 0x93, 0xc0, 0xc6, 0xa2, 0x69];
        let multi = redeem_script
            .windows(multi_rule.len())
            .position(|window| window == multi_rule)
            .expect("authoritative multi-input value rule");
        redeem_script[multi + 5] = 0x94;
        assert!(extract_baton_decrease_rule(
            &redeem_script,
            &[0xc0, 0xcc],
            &[0x93, 0xc0, 0xc6, 0xa2, 0x69],
            "multi-input",
        )
        .is_err());
    }
}
