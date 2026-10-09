//! #### PR #42
//! Template Distribution (SV2): this server hands its node's templates to
//! SV2 pools, Job Declaration clients and P2Pool over Noise, and relays the
//! blocks they find on them to the node. See docs/stratum-v2.md, "Serving
//! templates to a pool".

pub mod client;
pub mod convert;
pub mod server;
#[cfg(test)]
mod server_tests;

use std::time::Duration;

/// The largest coinbase without a client's outputs: version 4, input count
/// 1, prevout 36, script length 1, script 100, sequence 4, output count 3 and
/// locktime 4 bytes; the spec's 944 legacy weight units.
pub const COINBASE_FIXED: u64 = 153;
/// How long templates on a replaced parent still answer.
pub const STALE_GRACE: Duration = Duration::from_secs(10);
/// The templates a client can still name.
pub const MAX_RETAINED: usize = 16;
/// Template clients at once.
pub const MAX_TP_CLIENTS: usize = 8;
/// #### PR #42: the coinbase output bytes this server may add to a
/// provider's template: the miner (at most 44 bytes, P2SH32), the donation
/// (34) and a public pool's operator (44).
pub const PAYOUT_RESERVE: u32 = 122;

/// #### PR #42: the reserve a template client declares: the payouts, and
/// with merge-mined tokens the 53-byte commitment and a 46-byte ticket per
/// Case B token.
pub fn reserve(tokens: Option<&super::merge::set::TokenSet>) -> u32 {
    use super::merge::{commitment::OUTPUT_LEN, leaf::Mode, registry::TICKET_LEN};
    PAYOUT_RESERVE
        + tokens.map_or(0, |set| {
            let tickets = set
                .entries()
                .iter()
                .filter(|entry| entry.mode == Mode::BlockRequired)
                .count();
            (OUTPUT_LEN + TICKET_LEN * tickets) as u32
        })
}
