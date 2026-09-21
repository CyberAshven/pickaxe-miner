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
/// (Start9 `bitcoincashd` etc.). Job/baton fetch still prefers Fulcrum until a
/// node-indexed path exists; node list is for health/broadcast versatility.
pub const NODE_RPC_BOOTSTRAP: &[&str] = &[
    // Intentionally empty of third-party public RPC (ban risk / auth required).
    // Add only endpoints Bandar explicitly curates later.
];
