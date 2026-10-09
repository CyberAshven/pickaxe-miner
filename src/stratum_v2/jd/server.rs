//! #### PR #42
//! The pool side of Job Declaration: the shared `Declarator` (the token book
//! and the pool's rules) and one client's JD session. Coinbase-only in this
//! slice: SetupConnection and AllocateMiningJobToken; the custom jobs
//! themselves arrive on the client's mining connection (`wire.rs`).

use super::{
    codec::serialize_outputs, policy::PayoutRule, token::TokenBook, AcceptJd,
    ALLOCATIONS_PER_MINUTE,
};
use crate::{
    config::MiningNetwork,
    donation::bch::BchDonation,
    stratum_v2::{payout::PublicPool, wire::encoded},
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2,
    codec_sv2::SerializedFrame,
    common_messages_sv2::{
        self as common, Protocol, SetupConnection, SetupConnectionError, SetupConnectionSuccess,
    },
    job_declaration_sv2::{
        AllocateMiningJobToken, AllocateMiningJobTokenSuccess,
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN, MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS,
    },
};

/// What a pool shares among its Job Declaration sessions and mining
/// connections.
pub struct Declarator {
    pub accept: AcceptJd,
    pub network: MiningNetwork,
    pub public: PublicPool,
    pub donation: Arc<RwLock<BchDonation>>,
    pub book: Mutex<TokenBook>,
    next_owner: AtomicU64,
}

impl Declarator {
    pub fn new(
        accept: AcceptJd,
        network: MiningNetwork,
        public: PublicPool,
        donation: Arc<RwLock<BchDonation>>,
    ) -> Self {
        Self {
            accept,
            network,
            public,
            donation,
            book: Mutex::new(TokenBook::default()),
            next_owner: AtomicU64::new(0),
        }
    }

    /// The payout rule of a custom job whose token is `token`, on a channel
    /// paying `payout`; the token is spent.
    pub fn redeem(
        &self,
        token: &[u8],
        payout: &str,
        now: Instant,
    ) -> Result<PayoutRule, &'static str> {
        let rates = self
            .book
            .lock()
            .map_err(|_| "invalid-mining-job-token")?
            .redeem(token, payout, now)?;
        PayoutRule::from_rates(self.network, payout, &self.public, rates)
            .map_err(|_| "invalid-mining-job-token")
    }
}

/// What a session's frame asks of the connection.
pub struct JdResponses {
    pub frames: Vec<SerializedFrame>,
    /// Send `frames` no earlier than this: an allocation over the rate is
    /// answered late, never dropped.
    pub not_before: Option<Instant>,
    /// Tokens allocated by this frame.
    pub allocated: u64,
    /// The session ends once `frames` are sent (a refused setup).
    pub close: bool,
}

/// One client's Job Declaration session.
pub struct DeclaratorSession {
    owner: u64,
    set_up: bool,
    allocations: VecDeque<Instant>,
}

impl DeclaratorSession {
    pub fn new(declarator: &Declarator) -> Self {
        Self {
            owner: declarator.next_owner.fetch_add(1, Ordering::Relaxed),
            set_up: false,
            allocations: VecDeque::new(),
        }
    }

    /// Answers one frame. An error ends the session: a protocol violation,
    /// or an identity that is not a payout address (the spec has no
    /// AllocateMiningJobToken.Error).
    pub fn receive(
        &mut self,
        frame: &mut SerializedFrame,
        declarator: &Declarator,
        now: Instant,
    ) -> Result<JdResponses, String> {
        let header = frame.header();
        if header.channel_msg() || header.ext_type_without_channel_msg() != 0 {
            return Err("unexpected Job Declaration frame".into());
        }
        let mut responses = JdResponses {
            frames: Vec::new(),
            not_before: None,
            allocated: 0,
            close: false,
        };
        match header.msg_type() {
            common::MESSAGE_TYPE_SETUP_CONNECTION if !self.set_up => {
                let setup: SetupConnection = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "malformed setup message")?;
                let error = if setup.protocol != Protocol::JobDeclarationProtocol {
                    Some((0, "unsupported-protocol"))
                } else if setup.min_version > 2 || setup.max_version < 2 {
                    Some((0, "protocol-version-mismatch"))
                } else if setup.flags != 0 {
                    // Bit 0 (DECLARE_TX_DATA) asks for Full-Template, which
                    // this pool does not offer yet; no other bit is defined.
                    Some((setup.flags, "unsupported-feature-flags"))
                } else {
                    None
                };
                if let Some((flags, code)) = error {
                    responses.frames.push(encoded(
                        SetupConnectionError {
                            flags,
                            error_code: code.try_into().map_err(|_| "error code too long")?,
                        },
                        common::MESSAGE_TYPE_SETUP_CONNECTION_ERROR,
                        false,
                    )?);
                    responses.close = true;
                    return Ok(responses);
                }
                self.set_up = true;
                responses.frames.push(encoded(
                    SetupConnectionSuccess {
                        used_version: 2,
                        flags: 0,
                    },
                    common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
                    false,
                )?);
            }
            MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN if self.set_up => {
                let request: AllocateMiningJobToken = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "malformed token request")?;
                // A minute's window of answers; one over it waits for the
                // oldest to leave the window.
                while self
                    .allocations
                    .front()
                    .is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_secs(60))
                {
                    self.allocations.pop_front();
                }
                let answer_at = if self.allocations.len() >= ALLOCATIONS_PER_MINUTE {
                    let oldest = self.allocations[self.allocations.len() - ALLOCATIONS_PER_MINUTE];
                    oldest + Duration::from_secs(60)
                } else {
                    now
                };
                self.allocations.push_back(answer_at);
                responses.not_before = (answer_at > now).then_some(answer_at);
                let identity = request.user_identifier.as_utf8_or_hex();
                let payout =
                    crate::stratum_v2::payout::identity_payout(declarator.network, &identity)
                        .map_err(|_| "unknown user")?;
                let donation = *declarator
                    .donation
                    .read()
                    .map_err(|_| "donation setting unavailable")?;
                let rule = PayoutRule::public_pool(
                    declarator.network,
                    &payout,
                    &declarator.public,
                    donation,
                )?;
                let (outputs, rates) = rule.allocated_outputs();
                let token = declarator
                    .book
                    .lock()
                    .map_err(|_| "token book unavailable")?
                    .allocate(self.owner, payout, rates, now)?;
                let token = token.encode();
                let outputs = serialize_outputs(&outputs);
                responses.frames.push(encoded(
                    AllocateMiningJobTokenSuccess {
                        request_id: request.request_id,
                        mining_job_token: token
                            .as_slice()
                            .try_into()
                            .map_err(|_| "token too long")?,
                        coinbase_outputs: outputs
                            .as_slice()
                            .try_into()
                            .map_err(|_| "outputs too long")?,
                    },
                    MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS,
                    false,
                )?);
                responses.allocated = 1;
            }
            _ => return Err("unexpected Job Declaration message".into()),
        }
        Ok(responses)
    }

    /// Drops this session's tokens once its connection closes.
    pub fn close(&self, declarator: &Declarator) {
        if let Ok(mut book) = declarator.book.lock() {
            book.drop_owner(self.owner);
        }
    }
}

#[cfg(test)]
pub(in crate::stratum_v2) mod tests {
    use super::super::{codec::parse_outputs, token::PxToken};
    use super::*;
    use crate::donation::bch::{FeeMode, PoolFee};

    pub(in crate::stratum_v2) fn declarator() -> Declarator {
        Declarator::new(
            AcceptJd::CoinbaseOnly,
            MiningNetwork::Chipnet,
            PublicPool {
                fee: Some(PoolFee {
                    rate: "1".parse().unwrap(),
                    mode: FeeMode::Coinbase,
                }),
                address: crate::tx::p2pkh_hash_to_cashaddr_for_network(
                    &[0x34; 20],
                    MiningNetwork::Chipnet,
                )
                .unwrap(),
            },
            Arc::new(RwLock::new(Default::default())),
        )
    }

    fn setup(flags: u32) -> SerializedFrame {
        encoded(
            SetupConnection {
                protocol: Protocol::JobDeclarationProtocol,
                min_version: 2,
                max_version: 2,
                flags,
                endpoint_host: "pool.example".try_into().unwrap(),
                endpoint_port: 3336,
                vendor: "test".try_into().unwrap(),
                hardware_version: "".try_into().unwrap(),
                firmware: "".try_into().unwrap(),
                device_id: "".try_into().unwrap(),
            },
            common::MESSAGE_TYPE_SETUP_CONNECTION,
            false,
        )
        .unwrap()
    }

    fn allocate(identity: &str, request: u32) -> SerializedFrame {
        encoded(
            AllocateMiningJobToken {
                user_identifier: identity.try_into().unwrap(),
                request_id: request,
            },
            MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
            false,
        )
        .unwrap()
    }

    // #### PR #42
    // What: Coinbase-only setup succeeds; Full-Template (bit 0) is refused
    // with unsupported-feature-flags echoing the bit; a token request names
    // a PX v1 token with the pool's rates and the miner, fee and donation
    // outputs worth 0; an identity that is not an address ends the session;
    // the 21st request in a minute is answered a minute after the first.
    // Look here if: DeclaratorSession changes.
    #[test]
    fn coinbase_only_is_offered_and_full_template_is_refused_until_enabled() {
        let declarator = declarator();
        let now = Instant::now();
        let mut session = DeclaratorSession::new(&declarator);
        let mut reply = session.receive(&mut setup(1), &declarator, now).unwrap();
        let error: SetupConnectionError =
            binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
        assert_eq!(error.error_code.as_ref(), b"unsupported-feature-flags");
        assert_eq!(error.flags, 1);
        assert!(reply.close);
        let mut session = DeclaratorSession::new(&declarator);
        let reply = session.receive(&mut setup(0), &declarator, now).unwrap();
        assert_eq!(
            reply.frames[0].header().msg_type(),
            common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS
        );
        let miner = super::super::super::template_tests::payout();
        let mut reply = session
            .receive(&mut allocate(&format!("{miner}.rig1"), 9), &declarator, now)
            .unwrap();
        assert_eq!(reply.allocated, 1);
        assert!(reply.not_before.is_none());
        let success: AllocateMiningJobTokenSuccess =
            binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
        assert_eq!(success.request_id, 9);
        let token = PxToken::decode(success.mining_job_token.as_ref()).unwrap();
        assert_eq!((token.rates.donation_bps, token.rates.fee_bps), (150, 100));
        let outputs = parse_outputs(success.coinbase_outputs.as_ref()).unwrap();
        assert_eq!(outputs.len(), 3);
        assert!(outputs.iter().all(|(value, _)| *value == 0));
        assert!(declarator
            .redeem(success.mining_job_token.as_ref(), &miner, now)
            .is_ok());
        assert!(session
            .receive(&mut allocate("not-an-address", 10), &declarator, now)
            .is_err());
        let mut busy = DeclaratorSession::new(&declarator);
        busy.receive(&mut setup(0), &declarator, now).unwrap();
        for request in 0..20 {
            let reply = busy
                .receive(&mut allocate(&miner, request), &declarator, now)
                .unwrap();
            assert!(reply.not_before.is_none());
        }
        let reply = busy
            .receive(&mut allocate(&miner, 20), &declarator, now)
            .unwrap();
        assert_eq!(reply.not_before, Some(now + Duration::from_secs(60)));
        busy.close(&declarator);
        assert!(declarator.book.lock().unwrap().is_empty());
    }
}
