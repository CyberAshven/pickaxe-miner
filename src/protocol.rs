//! PHOTON mainnet protocol constants extracted from reference/miner.js (M67.38).
//! Electrum/win-tx stay in Dev Assist modules — this is search/crypto shared facts only.

use num_bigint::BigUint;
use sha2::{Digest, Sha256};

/// Mainnet PHOTON relaunch category (genesis
/// 248ef0474ac9cdf5cf1fe7abd325864f6d82891db682ec4437a77d77a24c1c30 at
/// height 971212), the author's `PHOTON_CATEGORY` in 2qx/vox
/// packages/photon/src/index.ts. Mainnet and Chipnet run the identical v3.2
/// contract (`template.v3.2.json`, v20260930); only the category differs.
/// A different contract replaces the lock, script hash, redeem script,
/// addresses and proof rule below together.
pub const MAINNET_CATEGORY_HEX: &str =
    "53bd86e3f123918d2d7040449f88f7ed1bbddc309b66f2ac67cd429278f5ea58";

/// Covenant locking bytecode (hex): P2SH32 of the v3.2 redeem script.
pub const COVENANT_LOCKING_BYTECODE_HEX: &str =
    "aa200ab476a6dab00ba11118a11c078f871c5060a66e18140368425202772d0e80c287";

/// Expected Electrum script hash for the covenant (hex, reversed-SHA256 of lock).
pub const EXPECTED_SCRIPT_HASH_HEX: &str =
    "0cea1bfb91d50a9a3fb88bdf4f9d466c15738d7feba0873a5e85313d42035261";

/// Redeem script hex (P2SH32 / covenant spend path): the compiled 273-byte
/// `lock` script of the v3.2 template.
pub const REDEEM_SCRIPT_HEX: &str = include_str!("../reference/photon_v32_redeem.hex");

pub const MAINNET_COVENANT_ADDRESS: &str =
    "bitcoincash:rv9tga4xm2cqhgg3rzs3cpu0suw9qc9xdcvpgqmggffqyaedp6qvynuy0nm8t";

/// The original mainnet PHOTON (v0), retired by its author for the
/// relaunch. Kept as the GPU kernels' base layout, the reference test
/// vectors and the legacy self-funded settlement.
pub const MAINNET_V0_CATEGORY_HEX: &str =
    "29972959d6f0dc766cdcb81bfaf8171c5605a64dd0a81fa46080f84ac87c9bef";
pub const MAINNET_V0_COVENANT_LOCKING_BYTECODE_HEX: &str =
    "aa209a2c0f31147dda170e59aaa7982e4fe3fc25928bf09f15fe1e797a2ccb05c6e087";
pub const MAINNET_V0_EXPECTED_SCRIPT_HASH_HEX: &str =
    "720bad85599cd504b114c65caedb76098cab45df5553be1c7cf260c4c2954031";
/// Redeem script hex (P2SH32 / covenant spend path) — from postcorps miner.js.
pub const MAINNET_V0_REDEEM_SCRIPT_HEX: &str = include_str!("../reference/photon_redeem.hex");

/// The author's Chipnet PHOTON (`tPHOTON_CATEGORY`) on the same v3.2 contract.
pub const CHIPNET_CATEGORY_HEX: &str =
    "a852635be88f7291bc42e427b8107546f943e73636945a2f6cc9532af71896c0";
pub const CHIPNET_COVENANT_LOCKING_BYTECODE_HEX: &str =
    "aa200ab476a6dab00ba11118a11c078f871c5060a66e18140368425202772d0e80c287";
pub const CHIPNET_EXPECTED_SCRIPT_HASH_HEX: &str =
    "0cea1bfb91d50a9a3fb88bdf4f9d466c15738d7feba0873a5e85313d42035261";
pub const CHIPNET_REDEEM_SCRIPT_HEX: &str = REDEEM_SCRIPT_HEX;
pub const CHIPNET_COVENANT_ADDRESS: &str =
    "bchtest:rv9tga4xm2cqhgg3rzs3cpu0suw9qc9xdcvpgqmggffqyaedp6qvysm43tgje";

/// How a covenant compares `HASH256(tx)` with its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofRule {
    /// v0: `ABS(BIN2NUM(hash)) < target`, so the sign bit never matters.
    Absolute,
    /// v3.2: the hash and the target must both be positive script numbers.
    Positive,
}

#[derive(Debug, Clone, Copy)]
pub struct PhotonDeployment {
    pub category_hex: &'static str,
    pub covenant_lock_hex: &'static str,
    pub script_hash_hex: &'static str,
    pub redeem_script_hex: &'static str,
    pub covenant_address: &'static str,
    pub proof_rule: ProofRule,
}

pub const MAINNET_PHOTON: PhotonDeployment = PhotonDeployment {
    category_hex: MAINNET_CATEGORY_HEX,
    covenant_lock_hex: COVENANT_LOCKING_BYTECODE_HEX,
    script_hash_hex: EXPECTED_SCRIPT_HASH_HEX,
    redeem_script_hex: REDEEM_SCRIPT_HEX,
    covenant_address: MAINNET_COVENANT_ADDRESS,
    proof_rule: ProofRule::Positive,
};

/// Retired mainnet v0 deployment; see [`MAINNET_V0_CATEGORY_HEX`].
pub const MAINNET_V0_PHOTON: PhotonDeployment = PhotonDeployment {
    category_hex: MAINNET_V0_CATEGORY_HEX,
    covenant_lock_hex: MAINNET_V0_COVENANT_LOCKING_BYTECODE_HEX,
    script_hash_hex: MAINNET_V0_EXPECTED_SCRIPT_HASH_HEX,
    redeem_script_hex: MAINNET_V0_REDEEM_SCRIPT_HEX,
    covenant_address: "bitcoincash:rwdzcre3z37a59cwtx420xpwfl3lcfvj30cf7907reuh5txtqhrwqtx8rf5ms",
    proof_rule: ProofRule::Absolute,
};

pub const CHIPNET_PHOTON: PhotonDeployment = PhotonDeployment {
    category_hex: CHIPNET_CATEGORY_HEX,
    covenant_lock_hex: CHIPNET_COVENANT_LOCKING_BYTECODE_HEX,
    script_hash_hex: CHIPNET_EXPECTED_SCRIPT_HASH_HEX,
    redeem_script_hex: CHIPNET_REDEEM_SCRIPT_HEX,
    covenant_address: CHIPNET_COVENANT_ADDRESS,
    proof_rule: ProofRule::Positive,
};

impl PhotonDeployment {
    /// Refuses any deployment whose redeem script, P2SH32 lock, or Fulcrum hash disagree.
    pub fn verify(self) -> Result<(), String> {
        let redeem = hex::decode(self.redeem_script_hex.trim())
            .map_err(|error| format!("invalid PHOTON redeem script: {error}"))?;
        let lock = hex::decode(self.covenant_lock_hex)
            .map_err(|error| format!("invalid PHOTON covenant: {error}"))?;
        let category = hex::decode(self.category_hex)
            .map_err(|error| format!("invalid PHOTON category: {error}"))?;
        if category.len() != 32
            || lock.len() != 35
            || lock[0] != 0xaa
            || lock[1] != 0x20
            || lock[34] != 0x87
        {
            return Err("PHOTON deployment has invalid category or P2SH32 lock".into());
        }
        if lock[2..34] != hash256(&redeem) {
            return Err("PHOTON deployment redeem script does not match its lock".into());
        }
        let script_hash = Sha256::digest(&lock);
        if hex::encode(script_hash.into_iter().rev().collect::<Vec<_>>()) != self.script_hash_hex {
            return Err("PHOTON deployment Fulcrum script hash does not match its lock".into());
        }
        Ok(())
    }

    pub fn single_input_max_baton_decrease_sats(self) -> Result<u64, String> {
        self.verify()?;
        let redeem =
            hex::decode(self.redeem_script_hex.trim()).map_err(|error| error.to_string())?;
        extract_baton_decrease_rule(
            &redeem,
            &[0xc0, 0xcc, 0xc0, 0xc6],
            &[0x94, 0xa2, 0x69],
            "single-input",
        )
    }

    pub fn multi_input_min_baton_increase_sats(self) -> Result<u64, String> {
        self.verify()?;
        let redeem =
            hex::decode(self.redeem_script_hex.trim()).map_err(|error| error.to_string())?;
        extract_baton_decrease_rule(
            &redeem,
            &[0xc0, 0xcc, 0xc0, 0xc6],
            &[0x93, 0xa2, 0x69],
            "multi-input increase",
        )
    }
}

/// Computes double SHA-256 over the protocol bytes.
fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Decodes a minimally encoded positive BCH script number.
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

/// Returns the v0 PHOTON baton redeem script the self-funded settlement
/// budgets were proven against.
fn authoritative_redeem_script() -> Result<Vec<u8>, String> {
    let redeem_script =
        hex::decode(MAINNET_V0_REDEEM_SCRIPT_HEX.trim()).map_err(|error| error.to_string())?;
    if redeem_script.len() != 259 {
        return Err(format!(
            "PHOTON redeem script must be 259 bytes (got {})",
            redeem_script.len()
        ));
    }

    let covenant_lock =
        hex::decode(MAINNET_V0_COVENANT_LOCKING_BYTECODE_HEX).map_err(|error| error.to_string())?;
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

/// Constructs the exact PHOTON baton redeem script.
pub fn photon_authoritative_redeem_script() -> Result<Vec<u8>, String> {
    authoritative_redeem_script()
}

/// Reads the maximum baton decrease from the redeem script.
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

/// Returns the authorized single-input baton decrease in satoshis.
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

/// Returns the authorized multi-input baton decrease in satoshis.
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

/// Chipnet WSS bootstrap for PHOTON baton discovery. Public Chipnet Fulcrum
/// servers (Electron Cash's Chipnet list plus OPTN) that answered a live
/// Chipnet PHOTON baton query over WSS on 2026-10-02; the source health policy
/// picks among them. `blackie.c3-soft.com` serves mainnet on 50004 and
/// Chipnet on 64004.
pub const CHIPNET_FULCRUM_WSS_BOOTSTRAP: &[&str] = &[
    "wss://chipnet.bch.ninja:50004",
    "wss://chipnet.imaginary.cash:50004",
    "wss://blackie.c3-soft.com:64004",
    "wss://chipnet.c3-soft.com:64004",
    "wss://cbch.loping.net:62104",
    "wss://electrum-chipnet.optnlabs.com",
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

/// Decodes little-endian hexadecimal bytes into an integer.
fn le_hex_to_biguint(hex_str: &str) -> Result<BigUint, String> {
    if !hex_str.len().is_multiple_of(2) {
        return Err("Invalid little-endian hex.".into());
    }
    let bytes = hex::decode(hex_str).map_err(|error| error.to_string())?;
    Ok(BigUint::from_bytes_le(&bytes))
}

/// Encodes an integer as a 32-byte little-endian hex string.
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
    fn photon_deployments_match_their_redeem_scripts_and_fulcrum_hashes() {
        MAINNET_PHOTON.verify().unwrap();
        CHIPNET_PHOTON.verify().unwrap();
        assert_eq!(
            hex::decode(CHIPNET_REDEEM_SCRIPT_HEX.trim()).unwrap().len(),
            273
        );
        assert_eq!(
            CHIPNET_PHOTON
                .single_input_max_baton_decrease_sats()
                .unwrap(),
            1_500
        );
        assert_eq!(
            CHIPNET_PHOTON
                .multi_input_min_baton_increase_sats()
                .unwrap(),
            8_000
        );
    }

    #[test]
    fn mainnet_relaunch_runs_the_chipnet_contract() {
        MAINNET_V0_PHOTON.verify().unwrap();
        assert_eq!(
            MAINNET_PHOTON.covenant_lock_hex,
            CHIPNET_PHOTON.covenant_lock_hex
        );
        assert_eq!(
            MAINNET_PHOTON.script_hash_hex,
            CHIPNET_PHOTON.script_hash_hex
        );
        assert_eq!(
            MAINNET_PHOTON.redeem_script_hex,
            CHIPNET_PHOTON.redeem_script_hex
        );
        assert_ne!(MAINNET_PHOTON.category_hex, CHIPNET_PHOTON.category_hex);
        assert_eq!(MAINNET_PHOTON.proof_rule, ProofRule::Positive);
        assert_eq!(CHIPNET_PHOTON.proof_rule, ProofRule::Positive);
        assert_eq!(MAINNET_V0_PHOTON.proof_rule, ProofRule::Absolute);
    }

    #[test]
    fn mainnet_initial_baton_derives_positive_targets() {
        // The relaunch's initial baton commitment, published by the author.
        let commitment = "00000000ffffffffffffffffffffffffffffffffffffffffffffffffffffff0000000000";
        let unconfirmed =
            derive_photon_state(commitment, 2_100_000_000_000_000, 971_300, 0).unwrap();
        assert_eq!(unconfirmed.age, 0);
        assert_eq!(
            unconfirmed.target_le_hex,
            "e2388ee3388ee3388ee3388ee3388ee3388ee3388ee3388ee338fe0000000000"
        );
        let aged =
            derive_photon_state(commitment, 2_100_000_000_000_000, 971_400, 971_300).unwrap();
        assert_eq!(aged.age, 100);
        assert_eq!(
            aged.target_le_hex,
            "feffffffffffffffffffffffffffffffffffffffffffffffffffaf0100000000"
        );
    }

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
