//! Requested Payout Outputs (0x5052) on the client side. Before the local
//! server publishes a template under a plan whose pool granted the
//! extension, its preparation (the one P2Pool's coinbases use) asks the
//! uplink for that template's outputs; the uplink sends RequestPayoutOutputs
//! with a token, and the coinbase devices mine is built from the answer, as
//! the Knuth node builds it (docs/sv2.md, jd_declaration.cpp).

use super::{codec::parse_outputs, declared::CoinbaseShape};
use crate::{
    p2pool::jd::Prepared,
    stratum_v2::{
        extensions::{RequestPayoutOutputsSuccess, EXACT_OUTPUTS},
        template::Hash,
    },
};
use std::{
    fmt,
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

/// How long a preparation waits for the uplink: the pool's own answer
/// deadline, and slack.
pub const PAYOUT_WAIT: Duration = Duration::from_secs(12);
/// Restarts one request may take after PayoutOutputsStale notices before the
/// session falls back (Knuth allows eight).
pub const PAYOUT_RESTARTS: u8 = 8;

/// What a template's preparation asks the uplink for.
pub struct PayoutAsk {
    pub height: u32,
    pub bits: u32,
    pub previous_hash: Hash,
    /// The template's revenue: its subsidy and fees.
    pub value: u64,
    /// The coinbase script before the extranonce.
    pub head: Vec<u8>,
    /// The whole extranonce's length: the pool channel's prefix and the
    /// rolled bytes.
    pub extranonce: usize,
    /// The template's earliest header time: the median time past plus one.
    pub min_time: u32,
    pub reply: SyncSender<Result<Prepared, String>>,
}

/// A session's door for preparations; the uplink holds the other end.
pub struct Payouts {
    asks: SyncSender<PayoutAsk>,
    /// The pool's tokens carry its rates (a Pickaxe pool's), so a coinbase
    /// can also be built from them: a template that merge-mines tokens in
    /// Full-Template is, since exact outputs leave no room for the
    /// commitment (`BchTemplate::payouts`).
    pub local_rates: bool,
}

impl Payouts {
    /// The door, and the asks it lets through.
    pub fn new(local_rates: bool) -> (Self, Receiver<PayoutAsk>) {
        let (asks, receive) = mpsc::sync_channel(4);
        (Self { asks, local_rates }, receive)
    }

    /// The pool's coinbase for a template, within `PAYOUT_WAIT`.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &self,
        height: u32,
        bits: u32,
        previous_hash: Hash,
        value: u64,
        head: Vec<u8>,
        extranonce: usize,
        min_time: u32,
    ) -> Result<Prepared, String> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.asks
            .try_send(PayoutAsk {
                height,
                bits,
                previous_hash,
                value,
                head,
                extranonce,
                min_time,
                reply,
            })
            .map_err(|_| "the Job Declaration session takes no payout request now")?;
        answer
            .recv_timeout(PAYOUT_WAIT)
            .map_err(|_| "the Job Declaration session did not answer the payout request")?
    }
}

/// One door per session: two are equal only when they are the same.
impl PartialEq for Payouts {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for Payouts {}

impl fmt::Debug for Payouts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Payouts")
            .field("local_rates", &self.local_rates)
            .finish_non_exhaustive()
    }
}

// What: the coinbase a RequestPayoutOutputs.Success makes, checked as
// Knuth's client checks it: no flag but EXACT_OUTPUTS; with it, a version of
// 1 or 2 and a sequence and lock time that are final for the template;
// without it, those three zero (the coinbase then takes version 1, sequence
// 0xffffffff and lock time 0); outputs within the token's reservation,
// without token data, summing to the template's revenue. The coinbase's
// outputs are exactly those, its script the head asked with and the
// extranonce.
// Why: a malformed answer must not reach devices; Knuth falls back to solo
// on one, and so does this client.
// Look here if: a pool's payout answer is refused, or a block built from one
// is refused by the node.
/// The prepared coinbase of `success` for `ask`, the token's reservation
/// being `reservation` bytes.
pub fn exact_coinbase(
    success: &RequestPayoutOutputsSuccess,
    ask: &PayoutAsk,
    reservation: usize,
) -> Result<CoinbaseShape, String> {
    if success.flags & !EXACT_OUTPUTS != 0 {
        return Err("the pool's payout outputs set unknown flags".into());
    }
    let (version, sequence, locktime) = if success.flags & EXACT_OUTPUTS != 0 {
        if !matches!(success.version, 1 | 2) {
            return Err("the pool's coinbase version is not 1 or 2".into());
        }
        if !is_final(success.sequence, success.locktime, ask.height, ask.min_time) {
            return Err("the pool's coinbase sequence and lock time are not final".into());
        }
        (success.version, success.sequence, success.locktime)
    } else {
        if (success.version, success.sequence, success.locktime) != (0, 0, 0) {
            return Err("the pool's payout coinbase fields are not zero".into());
        }
        (1, u32::MAX, 0)
    };
    if success.outputs.len() > reservation {
        return Err("the pool's payout outputs exceed the token's reservation".into());
    }
    let outputs = parse_outputs(&success.outputs).map_err(|_| "malformed payout outputs")?;
    if outputs.is_empty() || outputs.len() > 256 {
        return Err("the pool's payout outputs are not 1 to 256 outputs".into());
    }
    if outputs
        .iter()
        .any(|(_, script)| script.first() == Some(&0xef))
    {
        return Err("a payout output carries CashTokens".into());
    }
    let sum = outputs
        .iter()
        .try_fold(0u64, |sum, (value, _)| sum.checked_add(*value));
    if sum != Some(ask.value) {
        return Err("the pool's payout outputs do not pay the template's revenue".into());
    }
    let script = ask.head.len() + ask.extranonce;
    let mut prefix = version.to_le_bytes().to_vec();
    prefix.push(1);
    prefix.extend([0; 32]);
    prefix.extend([0xff; 4]);
    prefix.push(u8::try_from(script).map_err(|_| "the coinbase script is too long")?);
    prefix.extend(&ask.head);
    let mut suffix = sequence.to_le_bytes().to_vec();
    suffix.extend(&success.outputs);
    suffix.extend(locktime.to_le_bytes());
    CoinbaseShape::parse(&prefix, &suffix).map_err(|refusal| refusal.details)
}

/// Whether a coinbase with `sequence` and `locktime` is final in a block at
/// `height` whose median time past is `min_time - 1`.
fn is_final(sequence: u32, locktime: u32, height: u32, min_time: u32) -> bool {
    const THRESHOLD: u32 = 500_000_000;
    let limit = if locktime < THRESHOLD {
        height
    } else {
        min_time.saturating_sub(1)
    };
    locktime == 0 || locktime < limit || sequence == u32::MAX
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stratum_v2::jd::codec::serialize_outputs;

    fn ask() -> (PayoutAsk, Receiver<Result<Prepared, String>>) {
        let (reply, answer) = mpsc::sync_channel(1);
        (
            PayoutAsk {
                height: 101,
                bits: 0x207f_ffff,
                previous_hash: [1; 32],
                value: 1_000,
                head: vec![0x01, 0x65, b'j', b'd'],
                extranonce: 16,
                min_time: 1_700_000_000,
                reply,
            },
            answer,
        )
    }

    fn success(outputs: &[(u64, Vec<u8>)]) -> RequestPayoutOutputsSuccess {
        RequestPayoutOutputsSuccess {
            request_id: 1,
            flags: EXACT_OUTPUTS,
            version: 2,
            sequence: u32::MAX,
            locktime: 0,
            outputs: serialize_outputs(outputs),
        }
    }

    // What: an exact answer makes a coinbase of exactly its outputs, with
    // its version, sequence and lock time around the head and extranonce;
    // unknown flags, a version outside 1..2, a lock time that is not final,
    // non-zero fields without EXACT_OUTPUTS, outputs over the reservation,
    // with token data, or not paying the revenue are refused; without the
    // flag the coinbase takes version 1, 0xffffffff and 0.
    // Look here if: `exact_coinbase` changes.
    #[test]
    fn exact_outputs_make_the_coinbase_or_are_refused() {
        let (ask, _answer) = ask();
        let outputs = vec![(900, vec![0x51]), (100, vec![0x52])];
        let exact = success(&outputs);
        let shape = exact_coinbase(&exact, &ask, exact.outputs.len()).unwrap();
        assert_eq!(shape.outputs, outputs);
        assert_eq!(
            (shape.tx_version, shape.sequence, shape.locktime),
            (2, u32::MAX, 0)
        );
        assert_eq!(shape.head, ask.head);
        assert_eq!(shape.extranonce_len(), 16);
        let bytes = shape.coinbase(&[7; 16]).unwrap();
        assert_eq!(bytes[..4], 2u32.to_le_bytes());
        assert_eq!(bytes[41], 4 + 16);
        // A lock time below the height with another sequence is final.
        let timed = RequestPayoutOutputsSuccess {
            sequence: 0xffff_fffe,
            locktime: 100,
            ..exact.clone()
        };
        assert!(exact_coinbase(&timed, &ask, exact.outputs.len()).is_ok());
        let refused = |success: RequestPayoutOutputsSuccess, reservation: usize| {
            exact_coinbase(&success, &ask, reservation).unwrap_err()
        };
        let size = exact.outputs.len();
        assert!(refused(
            RequestPayoutOutputsSuccess {
                flags: 3,
                ..exact.clone()
            },
            size
        )
        .contains("flags"));
        assert!(refused(
            RequestPayoutOutputsSuccess {
                version: 3,
                ..exact.clone()
            },
            size
        )
        .contains("version"));
        assert!(refused(
            RequestPayoutOutputsSuccess {
                sequence: 0,
                locktime: 101,
                ..exact.clone()
            },
            size
        )
        .contains("final"));
        assert!(refused(
            RequestPayoutOutputsSuccess {
                flags: 0,
                ..exact.clone()
            },
            size
        )
        .contains("zero"));
        assert!(refused(exact.clone(), size - 1).contains("reservation"));
        assert!(refused(success(&[(1_000, vec![0xef, 0x01])]), 64).contains("CashTokens"));
        assert!(refused(success(&[(999, vec![0x51])]), 64).contains("revenue"));
        let plain = RequestPayoutOutputsSuccess {
            flags: 0,
            version: 0,
            sequence: 0,
            locktime: 0,
            ..exact
        };
        let shape = exact_coinbase(&plain, &ask, size).unwrap();
        assert_eq!(
            (shape.tx_version, shape.sequence, shape.locktime),
            (1, u32::MAX, 0)
        );
    }
}
