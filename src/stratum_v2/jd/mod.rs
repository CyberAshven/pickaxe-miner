//! #### PR #42
//! SV2 Job Declaration for BCH. A Pickaxe public pool accepts miners' own
//! templates: a miner's Job Declaration client allocates tokens on the
//! pool's SV2 port, declares each template with its transactions
//! (Full-Template) or not (Coinbase-only), then sets its own job on a
//! work-selection mining channel, and the pool checks that job's coinbase
//! pays the miner, the pool's fee and the Pickaxe donation. See
//! docs/job-declaration.md.

pub mod client;
pub mod codec;
pub mod declared;
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
/// How long a declared job's token can be redeemed.
pub const DECLARED_TTL: Duration = Duration::from_secs(3_600);
/// Tokens the pool holds at once.
pub const MAX_TOKENS: usize = 1_024;
/// Token allocations one client connection may make per minute; more are
/// answered late.
pub const ALLOCATIONS_PER_MINUTE: usize = 20;
/// Declarations one client connection may make per minute; more are
/// refused.
pub const DECLARATIONS_PER_MINUTE: usize = 30;
/// Transactions one declaration may list: SV2 counts them in 16 bits.
pub const MAX_DECLARED_TXS: usize = 65_535;
/// Missing transactions the pool asks for in one round.
pub const MISSING_PER_ROUND: usize = 2_048;
/// Rounds of missing transactions one declaration may take.
pub const MAX_MISSING_ROUNDS: u8 = 16;
/// How long a declaration may wait for its missing transactions.
pub const PENDING_EXPIRY: Duration = Duration::from_secs(30);
/// How often the pool's node checks a connection's declarations on one
/// parent, at most (a provided transaction is always checked).
pub const FULL_CHECK_EVERY: Duration = Duration::from_secs(60);
/// Node checks running at once across the pool, and how long a declaration
/// waits for one.
pub const VALIDATIONS_IN_FLIGHT: usize = 4;
pub const VALIDATION_WAIT: Duration = Duration::from_secs(20);
/// Declared jobs a connection keeps for PushSolution.
pub const DECLARED_KEPT: usize = 8;
/// Bytes of transactions clients provided that the pool keeps.
pub const PROVIDED_BYTES: usize = 128 * 1024 * 1024;
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
    /// Whether it is a block, which a Full-Template client also pushes to
    /// the pool.
    pub block: bool,
}

/// The Job Declaration modes a pool accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptJd {
    /// The client sets jobs from allocated tokens; the pool never sees its
    /// transactions, and the client's node submits its blocks.
    CoinbaseOnly,
    /// The client declares each template with its transactions; the pool
    /// checks it with its node and propagates its blocks too.
    FullTemplate,
    Both,
}

impl AcceptJd {
    /// Whether a client asking for Full-Template (`full`) or Coinbase-only
    /// is accepted.
    pub fn allows(self, full: bool) -> bool {
        match self {
            Self::CoinbaseOnly => !full,
            Self::FullTemplate => full,
            Self::Both => true,
        }
    }
}

/// How a client declares its templates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JdMode {
    FullTemplate,
    CoinbaseOnly,
}

impl JdMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullTemplate => "full-template",
            Self::CoinbaseOnly => "coinbase-only",
        }
    }
}

/// Why the pool refused a declaration: the spec's error code and what to
/// send as `error_details`, which never names an address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub details: String,
}

impl Refusal {
    pub fn new(code: &'static str, details: impl Into<String>) -> Self {
        Self {
            code,
            details: details.into(),
        }
    }
}
