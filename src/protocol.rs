//! PHOTON mainnet protocol constants extracted from reference/miner.js (M67.38).
//! Electrum/win-tx stay in Dev Assist modules — this is search/crypto shared facts only.

/// CashToken category id (hex, 32 bytes).
pub const MAINNET_CATEGORY_HEX: &str =
    "29972959d6f0dc766cdcb81bfaf8171c5605a64dd0a81fa46080f84ac87c9bef";

/// Covenant locking bytecode (hex).
pub const COVENANT_LOCKING_BYTECODE_HEX: &str =
    "aa209a2c0f31147dda170e59aaa7982e4fe3fc25928bf09f15fe1e797a2ccb05c6e087";

/// Expected Electrum script hash for the covenant (hex, reversed-SHA256 of lock).
pub const EXPECTED_SCRIPT_HASH_HEX: &str =
    "720bad85599cd504b114c65caedb76098cab45df5553be1c7cf260c4c2954031";

/// Proven live mining template size required by the WebGPU kernel.
pub const TEMPLATE_BYTES: usize = 615;

/// Redeem script hex (P2SH32 / covenant spend path) — from postcorps miner.js.
pub const REDEEM_SCRIPT_HEX: &str = include_str!("../reference/photon_redeem.hex");

/// Public Fulcrum/Electrum WSS bootstrap (failover + redundancy).
/// Custom user URL (Start9 Fulcrum etc.) is tried first when set in RuntimeConfig.
/// Prefer WSS :50004 hosts with CA-signed certs.
pub const ELECTRUM_WSS_BOOTSTRAP: &[&str] = &[
    "wss://electrum.imaginary.cash:50004",
    "wss://bch.imaginary.cash:50004",
    "wss://electroncash.dk:50004",
    "wss://btc.electroncash.dk:50004",
    "wss://fulcrum.greyh.at:50004",
];

/// Back-compat alias.
pub const ELECTRUM_WSS: &[&str] = ELECTRUM_WSS_BOOTSTRAP;
