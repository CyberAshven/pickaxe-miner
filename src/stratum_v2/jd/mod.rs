//! #### PR #42
//! SV2 Job Declaration for BCH. A Pickaxe public pool accepts miners' own
//! templates (Coinbase-only in this slice): a miner's Job Declaration client
//! allocates a token on the pool's SV2 port, then sets its own job on a
//! work-selection mining channel, and the pool checks that job's coinbase
//! pays the miner, the pool's fee and the Pickaxe donation. See
//! docs/job-declaration.md.

pub mod codec;
pub mod policy;
pub mod server;
pub mod token;

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

/// The Job Declaration modes a pool accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptJd {
    /// The client sets jobs from allocated tokens; the pool never sees its
    /// transactions, and the client's node submits its blocks.
    CoinbaseOnly,
}
