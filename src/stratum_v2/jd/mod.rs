//! #### PR #42
//! SV2 Job Declaration for BCH. A Pickaxe public pool accepts miners' own
//! templates (Coinbase-only in this slice): a miner's Job Declaration client
//! allocates a token on the pool's SV2 port, then sets its own job on a
//! work-selection mining channel, and the pool checks that job's coinbase
//! pays the miner, the pool's fee and the Pickaxe donation. See
//! docs/job-declaration.md.

pub mod client;
pub mod codec;
pub mod plan;
pub mod policy;
pub mod server;
pub mod token;

use super::template::Hash;
use std::time::Duration;

/// Rollable extranonce bytes a work-selection channel gets.
pub const JD_ROLLABLE: usize = 16;
/// How long an allocated token can be redeemed.
pub const TOKEN_TTL: Duration = Duration::from_secs(600);
/// Tokens the pool holds at once.
pub const MAX_TOKENS: usize = 1_024;
/// Token allocations one client connection may make per minute; more are
/// answered late.
pub const ALLOCATIONS_PER_MINUTE: usize = 20;
/// Custom job ids carry this bit, so they never meet the pool's own.
pub const CUSTOM_JOB_BIT: u32 = 0x8000_0000;

/// An accepted share on a Job Declaration job, on its way to the pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardShare {
    /// The local server's template generation the job was built from.
    pub serial: u64,
    pub version: u32,
    pub ntime: u32,
    pub nonce: u32,
    /// The bytes the pool lets the client roll: the job id, the pad, the
    /// lane and the device's 8.
    pub extranonce: Vec<u8>,
    pub hash: Hash,
}

/// The Job Declaration modes a pool accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptJd {
    /// The client sets jobs from allocated tokens; the pool never sees its
    /// transactions, and the client's node submits its blocks.
    CoinbaseOnly,
}
