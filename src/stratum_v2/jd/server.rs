//! #### PR #42
//! The pool side of Job Declaration: the shared `Declarator` (the token book,
//! the pool's rules, the transactions clients provided and the node check)
//! and one client's JD session: SetupConnection, AllocateMiningJobToken, and
//! for Full-Template clients DeclareMiningJob with its missing transactions,
//! and PushSolution. The custom jobs themselves arrive on the client's
//! mining connection (`wire.rs`).

use super::{
    codec::serialize_outputs,
    declared::{ctor_ordered, CoinbaseShape, DeclaredJob, Solution},
    policy::PayoutRule,
    token::{PoolRates, Redeemed, TokenBook},
    AcceptJd, Refusal, ALLOCATIONS_PER_MINUTE, DECLARATIONS_PER_MINUTE, DECLARED_KEPT,
    FULL_CHECK_EVERY, MAX_DECLARED_TXS, MAX_MISSING_ROUNDS, MISSING_PER_ROUND, PENDING_EXPIRY,
    PROVIDED_BYTES, VALIDATIONS_IN_FLIGHT, VALIDATION_WAIT,
};
use crate::{
    config::MiningNetwork,
    donation::bch::BchDonation,
    stratum_v2::{
        channel::VERSION_ROLLING_MASK,
        payout::PublicPool,
        provider::NodeRpc,
        template::{checked_transaction, height_script, BchTemplate, Hash},
        wire::encoded,
    },
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex, RwLock,
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
        AllocateMiningJobToken, AllocateMiningJobTokenSuccess, DeclareMiningJob,
        DeclareMiningJobError, DeclareMiningJobSuccess, ProvideMissingTransactions,
        ProvideMissingTransactionsSuccess, PushSolution, MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN_SUCCESS, MESSAGE_TYPE_DECLARE_MINING_JOB,
        MESSAGE_TYPE_DECLARE_MINING_JOB_ERROR, MESSAGE_TYPE_DECLARE_MINING_JOB_SUCCESS,
        MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS,
        MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS_SUCCESS, MESSAGE_TYPE_PUSH_SOLUTION,
    },
};

/// Why the pool's node did not pass a declared block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The node refused it, with its reason.
    Invalid(String),
    /// The node could not be asked.
    Unavailable,
}

/// Checks a declared template's block the way the node would take it,
/// without proof of work.
pub trait Validator: Send + Sync {
    fn check(&self, block: &[u8]) -> Result<(), Verdict>;
    /// What the dashboard calls it.
    fn kind(&self) -> &'static str;
}

/// BCHN's `validateblocktemplate`: the whole block against the node's tip,
/// with the merkle root, CTOR, the coinbase's value and every transaction's
/// inputs checked. A node without it (Knuth) leaves structural checks only.
pub struct NodeValidator<R> {
    rpc: Mutex<R>,
    structural_only: AtomicBool,
}

impl<R> NodeValidator<R> {
    pub fn new(rpc: R) -> Self {
        Self {
            rpc: Mutex::new(rpc),
            structural_only: AtomicBool::new(false),
        }
    }
}

impl<R: NodeRpc + Send> Validator for NodeValidator<R> {
    fn check(&self, block: &[u8]) -> Result<(), Verdict> {
        if self.structural_only.load(Ordering::Relaxed) {
            return Ok(());
        }
        let result = self
            .rpc
            .lock()
            .map_err(|_| Verdict::Unavailable)?
            .call("validateblocktemplate", json!([hex::encode(block)]));
        match result {
            Ok(Value::Bool(true)) => Ok(()),
            Ok(_) => Err(Verdict::Invalid("the node did not accept the block".into())),
            Err(error) => match node_error(&error) {
                // Method not found: this node has no validateblocktemplate.
                Some((-32601, _)) => {
                    self.structural_only.store(true, Ordering::Relaxed);
                    Ok(())
                }
                Some((_, message)) => Err(Verdict::Invalid(message)),
                None => Err(Verdict::Unavailable),
            },
        }
    }

    fn kind(&self) -> &'static str {
        if self.structural_only.load(Ordering::Relaxed) {
            "structural only"
        } else {
            "validateblocktemplate"
        }
    }
}

/// A node's JSON-RPC error (`rpc error: {"code":…,"message":…}`): its code
/// and its message, printable and at most 200 characters; `None` when the
/// node did not answer.
fn node_error(error: &str) -> Option<(i64, String)> {
    let value: Value = serde_json::from_str(error.strip_prefix("rpc error: ")?).ok()?;
    let message: String = value
        .get("message")?
        .as_str()?
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(200)
        .collect();
    Some((
        value.get("code").and_then(Value::as_i64).unwrap_or(0),
        message,
    ))
}

// #### PR #42: the tip-race mapping
// What: a node refusal that only means the client and the pool are on
// different tips (no longer the tip, an unknown parent, a coinbase height or
// time for another tip) is answered stale-chain-tip; any other is
// invalid-job. Either way error_details carries the node's reason verbatim.
// Why: SRI clients fall back to another pool on any other error; a race at a
// new block must only drop that one declaration (ckpool maps them the same
// way).
// Look here if: clients fall back at every new block, or a template the
// node refused is answered stale-chain-tip.
fn node_refusal(reason: &str) -> Refusal {
    const RACES: [&str; 7] = [
        "does not build on chain tip",
        "unknown parent",
        "bad-cb-height",
        "prev-blk-not-found",
        "inconclusive-not-best-prevblk",
        "time-too-old",
        "time-too-new",
    ];
    let code = if RACES.iter().any(|race| reason.contains(race)) {
        "stale-chain-tip"
    } else {
        "invalid-job"
    };
    Refusal::new(code, reason)
}

/// The transactions clients provided, newest kept, up to 128 MiB.
#[derive(Default)]
pub struct ProvidedTxs {
    by_id: HashMap<Hash, Arc<[u8]>>,
    order: VecDeque<Hash>,
    bytes: usize,
}

impl ProvidedTxs {
    pub fn insert(&mut self, txid: Hash, tx: Arc<[u8]>) {
        if self.by_id.contains_key(&txid) {
            return;
        }
        self.bytes += tx.len();
        self.order.push_back(txid);
        self.by_id.insert(txid, tx);
        while self.bytes > PROVIDED_BYTES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(tx) = self.by_id.remove(&oldest) {
                self.bytes -= tx.len();
            }
        }
    }

    pub fn get(&self, txid: &Hash) -> Option<Arc<[u8]>> {
        self.by_id.get(txid).cloned()
    }
}

/// Node checks running now, across the pool.
#[derive(Default)]
struct Slots {
    busy: Mutex<usize>,
    freed: Condvar,
}

struct Slot<'a>(&'a Slots);

impl Slots {
    /// A slot within `wait`, or `None`.
    fn acquire(&self, wait: Duration) -> Option<Slot<'_>> {
        let deadline = Instant::now() + wait;
        let mut busy = self.busy.lock().ok()?;
        while *busy >= VALIDATIONS_IN_FLIGHT {
            let left = deadline.checked_duration_since(Instant::now())?;
            busy = self.freed.wait_timeout(busy, left).ok()?.0;
        }
        *busy += 1;
        Some(Slot(self))
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if let Ok(mut busy) = self.0.busy.lock() {
            *busy = busy.saturating_sub(1);
        }
        self.0.freed.notify_one();
    }
}

/// What a pool shares among its Job Declaration sessions and mining
/// connections.
pub struct Declarator {
    pub accept: AcceptJd,
    pub network: MiningNetwork,
    pub public: PublicPool,
    pub donation: Arc<RwLock<BchDonation>>,
    pub book: Mutex<TokenBook>,
    pub provided: Mutex<ProvidedTxs>,
    validator: Option<Arc<dyn Validator>>,
    slots: Slots,
    next_owner: AtomicU64,
}

/// What a custom job's token sets.
pub enum Custom {
    /// A Coinbase-only job, whose coinbase must meet this rule.
    CoinbaseOnly(PayoutRule),
    /// A Full-Template job, which must match this declaration.
    Declared(Arc<DeclaredJob>),
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
            provided: Mutex::new(ProvidedTxs::default()),
            validator: None,
            slots: Slots::default(),
            next_owner: AtomicU64::new(0),
        }
    }

    /// The node check for Full-Template declarations; without one, only
    /// the structural checks run.
    pub fn with_validator(mut self, validator: Arc<dyn Validator>) -> Self {
        self.validator = Some(validator);
        self
    }

    /// What the dashboard calls the node check.
    pub fn validator_kind(&self) -> &'static str {
        self.validator
            .as_ref()
            .map_or("structural only", |validator| validator.kind())
    }

    /// What a custom job whose token is `token` sets, on a channel paying
    /// `payout`; the token is spent.
    pub fn redeem(&self, token: &[u8], payout: &str, now: Instant) -> Result<Custom, &'static str> {
        let redeemed = self
            .book
            .lock()
            .map_err(|_| "invalid-mining-job-token")?
            .redeem(token, payout, now)?;
        match redeemed {
            Redeemed::Allocated(rates) => {
                PayoutRule::from_rates(self.network, payout, &self.public, rates)
                    .map(Custom::CoinbaseOnly)
                    .map_err(|_| "invalid-mining-job-token")
            }
            Redeemed::Declared(job) => Ok(Custom::Declared(job)),
        }
    }
}

/// The pool's templates a declaration is checked against: the one published
/// now (its parent, height, bits and limits) and the latest few, whose
/// transactions a declaration may name.
pub struct Context<'a> {
    pub current: Option<&'a Arc<BchTemplate>>,
    pub recent: &'a [Arc<BchTemplate>],
}

/// What a session's frame asks of the connection.
#[derive(Default)]
pub struct JdResponses {
    pub frames: Vec<SerializedFrame>,
    /// Send `frames` no earlier than this: an allocation over the rate is
    /// answered late, never dropped.
    pub not_before: Option<Instant>,
    /// Tokens allocated by this frame.
    pub allocated: u64,
    /// The session ends once `frames` are sent (a refused setup).
    pub close: bool,
    /// Declarations accepted, and the code of one refused.
    pub declared: u64,
    pub refused: Option<&'static str>,
    /// Rounds of missing transactions asked for, and node checks made.
    pub missing_rounds: u64,
    pub validations: u64,
    /// Blocks PushSolution named, to save and submit, and solutions no
    /// declared job matched.
    pub blocks: Vec<Pushed>,
    pub unmatched: u64,
}

/// A block a PushSolution named.
pub struct Pushed {
    pub parent: Hash,
    pub hash: Hash,
    pub height: u32,
    pub block: Vec<u8>,
}

/// A declaration waiting for its missing transactions.
struct Pending {
    started: Instant,
    rounds: u8,
    payout: String,
    rates: PoolRates,
    version: u32,
    shape: CoinbaseShape,
    context: Arc<BchTemplate>,
    txids: Vec<Hash>,
    found: Vec<Option<Arc<[u8]>>>,
    /// The positions asked for in the latest round, in order.
    asked: Vec<u16>,
    provided: bool,
}

/// One client's Job Declaration session.
pub struct DeclaratorSession {
    owner: u64,
    set_up: bool,
    full_template: bool,
    allocations: VecDeque<Instant>,
    declarations: VecDeque<Instant>,
    pending: HashMap<u32, Pending>,
    /// The latest declared jobs, newest last, for PushSolution.
    declared: VecDeque<Arc<DeclaredJob>>,
    /// The parent the node last checked a declaration on, and when.
    checked: Option<(Hash, Instant)>,
}

impl DeclaratorSession {
    pub fn new(declarator: &Declarator) -> Self {
        Self {
            owner: declarator.next_owner.fetch_add(1, Ordering::Relaxed),
            set_up: false,
            full_template: false,
            allocations: VecDeque::new(),
            declarations: VecDeque::new(),
            pending: HashMap::new(),
            declared: VecDeque::new(),
            checked: None,
        }
    }

    /// Whether the client declares its templates (its frames may then take
    /// 16 MiB).
    pub fn full_template(&self) -> bool {
        self.full_template
    }

    /// Answers one frame. An error ends the session: a protocol violation,
    /// or an identity that is not a payout address (the spec has no
    /// AllocateMiningJobToken.Error).
    pub fn receive(
        &mut self,
        frame: &mut SerializedFrame,
        declarator: &Declarator,
        context: &Context<'_>,
        now: Instant,
    ) -> Result<JdResponses, String> {
        let header = frame.header();
        if header.channel_msg() || header.ext_type_without_channel_msg() != 0 {
            return Err("unexpected Job Declaration frame".into());
        }
        let mut responses = JdResponses::default();
        match header.msg_type() {
            common::MESSAGE_TYPE_SETUP_CONNECTION if !self.set_up => {
                let setup: SetupConnection = binary_sv2::from_bytes(frame.payload())
                    .map_err(|_| "malformed setup message")?;
                // #### PR #42: bit 0 (DECLARE_TX_DATA) asks for
                // Full-Template, bit 0 clear for Coinbase-only; a mode the
                // pool does not accept is refused as SRI refuses it.
                let full = setup.flags & 1 != 0;
                let error = if setup.protocol != Protocol::JobDeclarationProtocol {
                    Some((0, "unsupported-protocol"))
                } else if setup.min_version > 2 || setup.max_version < 2 {
                    Some((0, "protocol-version-mismatch"))
                } else if setup.flags & !1 != 0 {
                    Some((setup.flags & !1, "unsupported-feature-flags"))
                } else if !declarator.accept.allows(full) {
                    Some(if full {
                        (1, "unsupported-feature-flags")
                    } else {
                        (1, "missing-declare-tx-data-flag")
                    })
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
                self.full_template = full;
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
                    .allocate(self.owner, payout, rates, self.full_template, now)?;
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
            MESSAGE_TYPE_DECLARE_MINING_JOB if self.set_up && self.full_template => {
                let request: DeclareMiningJob =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed declaration")?;
                let id = request.request_id;
                if let Err(refusal) =
                    self.declare(&request, declarator, context, now, &mut responses)
                {
                    refuse(id, refusal, &mut responses)?;
                }
            }
            MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS_SUCCESS
                if self.set_up && self.full_template =>
            {
                let reply: ProvideMissingTransactionsSuccess =
                    binary_sv2::from_bytes(frame.payload())
                        .map_err(|_| "malformed missing transactions")?;
                let id = reply.request_id;
                // A reply after its declaration expired finds nothing.
                if let Some(pending) = self.pending.remove(&id) {
                    if let Err(refusal) =
                        self.provide(pending, &reply, declarator)
                            .and_then(|pending| {
                                self.advance(id, pending, declarator, now, &mut responses)
                            })
                    {
                        refuse(id, refusal, &mut responses)?;
                    }
                }
            }
            MESSAGE_TYPE_PUSH_SOLUTION if self.set_up && self.full_template => {
                let push: PushSolution =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed solution")?;
                let solution = Solution {
                    extranonce: push.extranonce.as_ref().to_vec(),
                    prev_hash: push
                        .prev_hash
                        .as_ref()
                        .try_into()
                        .map_err(|_| "malformed solution")?,
                    nonce: push.nonce,
                    ntime: push.ntime,
                    nbits: push.nbits,
                    version: push.version,
                };
                // #### PR #42: PushSolution
                // What: the newest of the connection's last 8 declared jobs
                // whose coinbase, rebuilt with the pushed extranonce, gives
                // a header on its parent and bits that meets the block
                // target is the block; the pool saves and submits it. No
                // match is only counted.
                // Why: Full-Template lets the pool propagate a client's
                // block too; the message names no job, so the proof of work
                // finds it.
                // Look here if: a client's block reaches only its own node,
                // or unmatched solutions are counted.
                match self.declared.iter().rev().find_map(|job| {
                    job.solved(&solution).map(|(hash, block)| Pushed {
                        parent: job.template.previous_hash,
                        hash,
                        height: job.template.height,
                        block,
                    })
                }) {
                    Some(found) => responses.blocks.push(found),
                    None => responses.unmatched += 1,
                }
            }
            _ => return Err("unexpected Job Declaration message".into()),
        }
        Ok(responses)
    }

    /// Refuses declarations whose missing transactions did not arrive
    /// within 30 seconds (`missing-txs`).
    pub fn expire(&mut self, now: Instant) -> Result<JdResponses, String> {
        let mut responses = JdResponses::default();
        let expired: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, pending)| now.saturating_duration_since(pending.started) >= PENDING_EXPIRY)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.pending.remove(&id);
            refuse(
                id,
                Refusal::new(
                    "missing-txs",
                    "the missing transactions did not arrive within 30 seconds",
                ),
                &mut responses,
            )?;
        }
        Ok(responses)
    }

    // #### PR #42: DeclareMiningJob (Full-Template)
    // What: at most 30 declarations a minute; the token must be one this
    // connection allocated and not yet declared; the coinbase must parse
    // (one null input, a script of at most 100 bytes beginning with the
    // pool's height, a 1 to 32 byte extranonce, no segwit) and pay the
    // token's rule; the transaction ids must be in canonical order; the
    // version may differ from the pool's only in the rolling bits. Missing
    // transactions are looked up in the pool's latest templates and in what
    // clients provided, then asked for.
    // Why: a declared job is what the pool pays shares on and propagates, so
    // it must be a block the pool would build itself.
    // Look here if: declarations are refused with invalid-coinbase-tx,
    // invalid-job or stale-chain-tip.
    fn declare(
        &mut self,
        request: &DeclareMiningJob<'_>,
        declarator: &Declarator,
        context: &Context<'_>,
        now: Instant,
        responses: &mut JdResponses,
    ) -> Result<(), Refusal> {
        while self
            .declarations
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_secs(60))
        {
            self.declarations.pop_front();
        }
        if self.declarations.len() >= DECLARATIONS_PER_MINUTE {
            return Err(Refusal::new(
                "invalid-job",
                "more than 30 declarations a minute",
            ));
        }
        self.declarations.push_back(now);
        let (payout, rates) = declarator
            .book
            .lock()
            .map_err(|_| Refusal::new("internal-error", "token book unavailable"))?
            .declare(request.mining_job_token.as_ref(), self.owner, now)
            .map_err(|code| Refusal::new(code, "unknown, spent or expired token"))?;
        let current = context
            .current
            .cloned()
            .ok_or_else(|| Refusal::new("stale-chain-tip", "the pool has no template"))?;
        let shape = CoinbaseShape::parse(
            request.coinbase_tx_prefix.as_ref(),
            request.coinbase_tx_suffix.as_ref(),
        )?;
        if !shape.head.starts_with(&current.height_push()) {
            // The height of the block before or after the pool's is a race
            // at a new block, not a broken client.
            let race = [current.height.saturating_sub(1), current.height + 1]
                .iter()
                .any(|height| shape.head.starts_with(&height_script(*height)));
            return Err(Refusal::new(
                if race {
                    "stale-chain-tip"
                } else {
                    "invalid-coinbase-tx"
                },
                "wrong height",
            ));
        }
        PayoutRule::from_rates(declarator.network, &payout, &declarator.public, rates)
            .map_err(|_| Refusal::new("invalid-mining-job-token", "unknown payout"))?
            .check(&shape.outputs)
            .map_err(|why| Refusal::new("invalid-coinbase-tx", why))?;
        if request.wtxid_list.len() > MAX_DECLARED_TXS {
            return Err(Refusal::new("invalid-job", "more than 65,535 transactions"));
        }
        let txids = request
            .wtxid_list
            .iter()
            .map(|txid| Hash::try_from(txid.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Refusal::new("invalid-job", "malformed transaction id"))?;
        if !ctor_ordered(&txids) {
            return Err(Refusal::new(
                "invalid-job",
                "transactions are not in canonical (CTOR) order",
            ));
        }
        if (request.version ^ current.version) & !VERSION_ROLLING_MASK != 0 {
            return Err(Refusal::new(
                "invalid-job",
                "the version differs from the pool's outside the rolling bits",
            ));
        }
        let found = {
            let provided = declarator
                .provided
                .lock()
                .map_err(|_| Refusal::new("internal-error", "transactions unavailable"))?;
            txids
                .iter()
                .map(|txid| {
                    std::iter::once(&current)
                        .chain(context.recent)
                        .find_map(|template| template.transaction(txid).cloned())
                        .or_else(|| provided.get(txid))
                })
                .collect()
        };
        let pending = Pending {
            started: now,
            rounds: 0,
            payout,
            rates,
            version: request.version,
            shape,
            context: current,
            txids,
            found,
            asked: Vec::new(),
            provided: false,
        };
        self.advance(request.request_id, pending, declarator, now, responses)
    }

    /// The transactions a client provided for `pending`: each must be the
    /// one asked for and a plain transaction.
    fn provide(
        &self,
        mut pending: Pending,
        reply: &ProvideMissingTransactionsSuccess<'_>,
        declarator: &Declarator,
    ) -> Result<Pending, Refusal> {
        if reply.transaction_list.len() != pending.asked.len() {
            return Err(Refusal::new(
                "invalid-job",
                "the transactions do not match the request",
            ));
        }
        let mut provided = declarator
            .provided
            .lock()
            .map_err(|_| Refusal::new("internal-error", "transactions unavailable"))?;
        for (position, tx) in pending.asked.iter().zip(reply.transaction_list.iter()) {
            let bytes = tx.as_ref();
            let txid = checked_transaction(bytes).map_err(|_| {
                Refusal::new(
                    "invalid-job",
                    "a provided transaction is malformed, a coinbase or segwit",
                )
            })?;
            let position = usize::from(*position);
            if pending.txids.get(position) != Some(&txid) {
                return Err(Refusal::new(
                    "invalid-job",
                    "a provided transaction is not the one asked for",
                ));
            }
            let tx: Arc<[u8]> = bytes.into();
            provided.insert(txid, tx.clone());
            pending.found[position] = Some(tx);
        }
        pending.provided = true;
        Ok(pending)
    }

    /// Asks for what `pending` still misses, or finishes it.
    fn advance(
        &mut self,
        id: u32,
        mut pending: Pending,
        declarator: &Declarator,
        now: Instant,
        responses: &mut JdResponses,
    ) -> Result<(), Refusal> {
        let missing: Vec<u16> = pending
            .found
            .iter()
            .enumerate()
            .filter(|(_, tx)| tx.is_none())
            .map(|(position, _)| position as u16)
            .take(MISSING_PER_ROUND)
            .collect();
        if missing.is_empty() {
            return self.finish(id, pending, declarator, now, responses);
        }
        if pending.rounds >= MAX_MISSING_ROUNDS {
            return Err(Refusal::new(
                "missing-txs",
                "transactions still missing after 16 rounds",
            ));
        }
        pending.rounds += 1;
        responses.missing_rounds += 1;
        responses.frames.push(
            encoded(
                ProvideMissingTransactions {
                    request_id: id,
                    unknown_tx_position_list: missing
                        .clone()
                        .try_into()
                        .map_err(|_| Refusal::new("internal-error", "too many positions"))?,
                },
                MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS,
                false,
            )
            .map_err(|_| Refusal::new("internal-error", "cannot ask for transactions"))?,
        );
        pending.asked = missing;
        self.pending.insert(id, pending);
        Ok(())
    }

    // #### PR #42: the validator cadence
    // What: a declaration with every transaction is checked by the pool's
    // node (validateblocktemplate, the whole block without proof of work):
    // the connection's first declaration on each parent, then at most once
    // a minute, and always when the client provided a transaction. At most
    // 4 checks run at once across the pool; a declaration waits up to 20 s
    // for one ("validation busy"). The others pass on the structural checks.
    // Why: a node check of a large BCH block takes time and memory; a
    // provided transaction is the one the pool's node has never seen, so it
    // is never taken unchecked.
    // Look here if: the node is busy with validateblocktemplate, or a
    // declaration with a bad transaction is accepted.
    fn finish(
        &mut self,
        id: u32,
        pending: Pending,
        declarator: &Declarator,
        now: Instant,
        responses: &mut JdResponses,
    ) -> Result<(), Refusal> {
        let parent = pending.context.previous_hash;
        let transactions = pending.found.into_iter().flatten().collect();
        let job = DeclaredJob {
            shape: pending.shape,
            template: Arc::new(BchTemplate::declared(
                &pending.context,
                pending.version,
                transactions,
                pending.txids,
            )),
        };
        let due = pending.provided
            || self.checked.is_none_or(|(checked, at)| {
                checked != parent || now.saturating_duration_since(at) >= FULL_CHECK_EVERY
            });
        if let Some(validator) = declarator.validator.as_ref().filter(|_| due) {
            let candidate = job.candidate()?;
            let _slot = declarator
                .slots
                .acquire(VALIDATION_WAIT)
                .ok_or_else(|| Refusal::new("internal-error", "validation busy"))?;
            responses.validations += 1;
            match validator.check(&candidate) {
                Ok(()) => self.checked = Some((parent, now)),
                Err(Verdict::Invalid(reason)) => return Err(node_refusal(&reason)),
                Err(Verdict::Unavailable) => {
                    return Err(Refusal::new("internal-error", "validation unavailable"))
                }
            }
        } else if !job.fits() {
            return Err(Refusal::new(
                "invalid-job",
                "too large for this pool's block size",
            ));
        }
        let job = Arc::new(job);
        let token = declarator
            .book
            .lock()
            .map_err(|_| Refusal::new("internal-error", "token book unavailable"))?
            .declared(self.owner, pending.payout, pending.rates, &job, now)
            .map_err(|_| Refusal::new("internal-error", "the pool holds too many tokens"))?
            .encode();
        self.declared.push_back(job);
        while self.declared.len() > DECLARED_KEPT {
            self.declared.pop_front();
        }
        responses.declared += 1;
        responses.frames.push(
            encoded(
                DeclareMiningJobSuccess {
                    request_id: id,
                    new_mining_job_token: token
                        .as_slice()
                        .try_into()
                        .map_err(|_| Refusal::new("internal-error", "token too long"))?,
                },
                MESSAGE_TYPE_DECLARE_MINING_JOB_SUCCESS,
                false,
            )
            .map_err(|_| Refusal::new("internal-error", "cannot answer"))?,
        );
        Ok(())
    }

    /// Drops this session's tokens once its connection closes.
    pub fn close(&self, declarator: &Declarator) {
        if let Ok(mut book) = declarator.book.lock() {
            book.drop_owner(self.owner);
        }
    }
}

/// DeclareMiningJob.Error for declaration `id`.
fn refuse(id: u32, refusal: Refusal, responses: &mut JdResponses) -> Result<(), String> {
    responses.refused = Some(refusal.code);
    responses.frames.push(encoded(
        DeclareMiningJobError {
            request_id: id,
            error_code: refusal.code.try_into().map_err(|_| "error code too long")?,
            error_details: refusal
                .details
                .as_bytes()
                .try_into()
                .map_err(|_| "error details too long")?,
        },
        MESSAGE_TYPE_DECLARE_MINING_JOB_ERROR,
        false,
    )?);
    Ok(())
}

#[cfg(test)]
#[path = "server_tests.rs"]
pub(in crate::stratum_v2) mod tests;
