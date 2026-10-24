//! #### PR #42, merge-mining v2
//! Merge mining for BCH covenant tokens, the v2 common format (no token
//! exists yet).
//!
//! One commitment in coinbase output 0 commits to a small tree of token
//! leaves, so the same SHA-256 share can win every token whose target it
//! meets, with no BCH block needed. Each leaf binds the job's payout, so a
//! win cannot be redirected.
//!
//! The byte layouts in these files are normative, repeated with golden
//! vectors in docs/merge-mining.md: [`commitment`] (output 0, 53 bytes),
//! [`leaf`] (169 bytes), [`tree`] (slots and nodes), [`proof`] (`AuxProof`
//! v2) and [`verify`] (the Rust reference of what a covenant checks).
//! v1's block-required mode (Case B) is gone; version 03 is reserved for it.
//!
//! Both networks' registries are empty, so no coinbase changes until a
//! token is registered; the Chipnet test token exists for tests only.
//! [`safa`], [`header`] and [`source`] are the separate ASIC-exclusive mode
//! (docs/asic-tokens.md).

pub mod commitment;
pub mod disclosure;
pub mod header;
pub mod hub;
pub mod leaf;
pub mod proof;
pub mod registry;
pub mod safa;
pub mod safa_auxpow;
pub mod set;
pub mod source;
pub mod split;
pub mod tree;
#[cfg(test)]
mod vectors;
pub mod verify;

pub use super::template::Hash;
use sha2::{Digest, Sha256};

/// A transaction output's identity: its transaction's hash in internal byte
/// order (as `OP_OUTPOINTTXHASH` gives it) and its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutPoint {
    pub txid: Hash,
    pub vout: u32,
}

/// Single SHA-256, the hash of leaves, nodes and slots.
pub(crate) fn sha256(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}
