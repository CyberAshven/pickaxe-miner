//! PHOTON mainnet protocol constants extracted from reference/miner.js (M67.38).
//! Electrum/win-tx stay in Dev Assist modules — this is search/crypto shared facts only.

use num_bigint::BigUint;

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

/// Curated Fulcrum/Electrum **WSS** bootstrap (small, redundant).
/// Custom `fulcrum` URL is tried first. Prefer CA-signed `:50004`.
pub const FULCRUM_WSS_BOOTSTRAP: &[&str] = &[
    "wss://electrum.imaginary.cash:50004",
    "wss://electroncash.dk:50004",
    "wss://fulcrum.greyh.at:50004",
];

/// Curated native **node** JSON-RPC bootstrap (BCHN/bitcoind-style HTTP).
/// Public RPC is rare — keep this list tiny; custom `node` URL is the usual path
/// (Start9 `bitcoincashd` etc.). Runtime PHOTON routing stays on Fulcrum until
/// the native provider is explicitly promoted after canonical equivalence proof.
pub const NODE_RPC_BOOTSTRAP: &[&str] = &[
    // Intentionally empty of third-party public RPC (ban risk / auth required).
    // Add only endpoints the operator explicitly curates later.
];

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
}
