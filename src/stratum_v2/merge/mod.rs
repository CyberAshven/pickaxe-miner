//! #### PR #42
//! Merge mining for BCH covenant tokens, v1 draft (no token exists yet).
//!
//! One commitment in coinbase output 0 commits to a small tree of token
//! leaves, so the same SHA-256 share can win tokens that accept a share
//! target (Case A, no BCH block needed) and tokens that need a found BCH
//! block (Case B, claimed through a coinbase ticket after maturity), both in
//! one coinbase. Each leaf binds the job's payout, so a win cannot be
//! redirected.
//!
//! The byte layouts in these files are normative until the merge-mining
//! spec in the docs repeats them: [`commitment`] (output 0, 53 bytes),
//! [`leaf`] (176 bytes), [`tree`] (slots and nodes), [`proof`] (`AuxProof`
//! v1) and [`verify`] (the Rust reference of what a covenant checks).
//!
//! Both networks' registries are empty, so no coinbase changes until a
//! token is registered; the Chipnet test token exists for tests only.

pub mod commitment;
pub mod header;
pub mod hub;
pub mod leaf;
pub mod proof;
pub mod registry;
pub mod safa;
pub mod set;
pub mod source;
pub mod tree;
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
