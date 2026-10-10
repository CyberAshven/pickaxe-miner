//! #### PR #42
//! The Job Declaration server session, without sockets: setup per accepted
//! mode, allocation, Full-Template declarations with missing transactions,
//! the node check's cadence and tip-race mapping, and PushSolution.

use super::super::{codec::parse_outputs, declared::tests::context, token::PxToken};
use super::*;
use crate::{
    donation::bch::{FeeMode, PoolFee},
    stratum_v2::{
        template::{double_sha256, fold, meets_target},
        template_tests::{payout, rpc_template, transaction},
    },
};
use stratum_core::bitcoin::{consensus, Block};

pub(in crate::stratum_v2) fn declarator() -> Declarator {
    declarator_accepting(AcceptJd::CoinbaseOnly)
}

pub(in crate::stratum_v2) fn declarator_accepting(accept: AcceptJd) -> Declarator {
    Declarator::new(
        accept,
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

/// A node check that records the blocks it is asked about and answers
/// `verdict` (passes without one).
#[derive(Default)]
pub(in crate::stratum_v2) struct FakeNode {
    pub blocks: Mutex<Vec<Vec<u8>>>,
    pub verdict: Mutex<Option<Verdict>>,
}

impl Validator for FakeNode {
    fn check(&self, block: &[u8]) -> Result<(), Verdict> {
        self.blocks.lock().unwrap().push(block.to_vec());
        match self.verdict.lock().unwrap().clone() {
            Some(verdict) => Err(verdict),
            None => Ok(()),
        }
    }

    fn kind(&self) -> &'static str {
        "test"
    }
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

const NO_CONTEXT: Context<'static> = Context {
    current: None,
    recent: &[],
};

/// A token for the test miner and the pool's output scripts, in its order.
fn new_token(
    session: &mut DeclaratorSession,
    declarator: &Declarator,
    now: Instant,
) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut reply = session
        .receive(&mut allocate(&payout(), 1), declarator, &NO_CONTEXT, now)
        .unwrap();
    let success: AllocateMiningJobTokenSuccess =
        binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
    let scripts = parse_outputs(success.coinbase_outputs.as_ref())
        .unwrap()
        .into_iter()
        .map(|(_, script)| script)
        .collect();
    (success.mining_job_token.as_ref().to_vec(), scripts)
}

/// A Full-Template session with one token.
fn full_session(
    declarator: &Declarator,
    now: Instant,
) -> (DeclaratorSession, Vec<u8>, Vec<Vec<u8>>) {
    let mut session = DeclaratorSession::new(declarator);
    session
        .receive(&mut setup(1), declarator, &NO_CONTEXT, now)
        .unwrap();
    assert!(session.full_template());
    let (token, scripts) = new_token(&mut session, declarator, now);
    (session, token, scripts)
}

/// `template_tests::transaction(nonce)`'s id and bytes.
fn tx(nonce: u32) -> (Hash, Vec<u8>) {
    let value = transaction(nonce);
    let mut id: Hash = hex::decode(value["txid"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    id.reverse();
    (id, hex::decode(value["data"].as_str().unwrap()).unwrap())
}

/// The ids of transactions `nonces`, in canonical order.
fn ids(nonces: impl IntoIterator<Item = u32>) -> Vec<Hash> {
    let mut ids: Vec<Hash> = nonces.into_iter().map(|nonce| tx(nonce).0).collect();
    ids.sort_by(|a, b| a.iter().rev().cmp(b.iter().rev()));
    ids
}

/// The coinbase prefix and suffix of a declaration at `height` paying the
/// pool's rule: miner 304,734,375, fee 3,078,125, donation 4,687,500.
fn coinbase_parts(height: u32, scripts: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
    let mut head = height_script(height);
    head.extend(b"jd");
    let mut prefix = 2u32.to_le_bytes().to_vec();
    prefix.push(1);
    prefix.extend([0; 32]);
    prefix.extend([0xff; 4]);
    prefix.push((head.len() + 32) as u8);
    prefix.extend(&head);
    let mut suffix = u32::MAX.to_le_bytes().to_vec();
    suffix.extend(serialize_outputs(
        &[304_734_375, 3_078_125, 4_687_500]
            .into_iter()
            .zip(scripts.iter().cloned())
            .collect::<Vec<_>>(),
    ));
    suffix.extend(0u32.to_le_bytes());
    (prefix, suffix)
}

fn declare_frame(
    id: u32,
    token: &[u8],
    version: u32,
    (prefix, suffix): &(Vec<u8>, Vec<u8>),
    txids: &[Hash],
) -> SerializedFrame {
    let list: Vec<binary_sv2::U256> = txids.iter().map(Into::into).collect();
    encoded(
        DeclareMiningJob {
            request_id: id,
            mining_job_token: token.try_into().unwrap(),
            version,
            coinbase_tx_prefix: prefix.as_slice().try_into().unwrap(),
            coinbase_tx_suffix: suffix.as_slice().try_into().unwrap(),
            wtxid_list: list.try_into().unwrap(),
            excess_data: (&[][..]).try_into().unwrap(),
        },
        MESSAGE_TYPE_DECLARE_MINING_JOB,
        false,
    )
    .unwrap()
}

fn provide_frame(id: u32, txs: &[Vec<u8>]) -> SerializedFrame {
    let list: Vec<binary_sv2::B016M> = txs
        .iter()
        .map(|tx| tx.as_slice().try_into().unwrap())
        .collect();
    encoded(
        ProvideMissingTransactionsSuccess {
            request_id: id,
            transaction_list: list.try_into().unwrap(),
        },
        MESSAGE_TYPE_PROVIDE_MISSING_TRANSACTIONS_SUCCESS,
        false,
    )
    .unwrap()
}

/// The error code and details of a DeclareMiningJob.Error.
fn error(responses: &mut JdResponses) -> (String, String) {
    let frame = responses.frames.last_mut().unwrap();
    assert_eq!(
        frame.header().msg_type(),
        MESSAGE_TYPE_DECLARE_MINING_JOB_ERROR
    );
    let error: DeclareMiningJobError = binary_sv2::from_bytes(frame.payload()).unwrap();
    (
        String::from_utf8(error.error_code.as_ref().to_vec()).unwrap(),
        String::from_utf8(error.error_details.as_ref().to_vec()).unwrap(),
    )
}

/// The new token of a DeclareMiningJob.Success.
fn declared_token(responses: &mut JdResponses) -> Vec<u8> {
    let frame = responses.frames.last_mut().unwrap();
    assert_eq!(
        frame.header().msg_type(),
        MESSAGE_TYPE_DECLARE_MINING_JOB_SUCCESS,
        "the declaration was refused"
    );
    let success: DeclareMiningJobSuccess = binary_sv2::from_bytes(frame.payload()).unwrap();
    success.new_mining_job_token.as_ref().to_vec()
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
    let mut reply = session
        .receive(&mut setup(1), &declarator, &NO_CONTEXT, now)
        .unwrap();
    let error: SetupConnectionError = binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
    assert_eq!(error.error_code.as_ref(), b"unsupported-feature-flags");
    assert_eq!(error.flags, 1);
    assert!(reply.close);
    let mut session = DeclaratorSession::new(&declarator);
    let reply = session
        .receive(&mut setup(0), &declarator, &NO_CONTEXT, now)
        .unwrap();
    assert_eq!(
        reply.frames[0].header().msg_type(),
        common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS
    );
    let miner = payout();
    let mut reply = session
        .receive(
            &mut allocate(&format!("{miner}.rig1"), 9),
            &declarator,
            &NO_CONTEXT,
            now,
        )
        .unwrap();
    assert_eq!(reply.allocated, 1);
    assert!(reply.not_before.is_none());
    let success: AllocateMiningJobTokenSuccess =
        binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
    assert_eq!(success.request_id, 9);
    let token = PxToken::decode(success.mining_job_token.as_ref()).unwrap();
    assert_eq!((token.rates.donation_bps, token.rates.fee_bps), (150, 100));
    assert!(!token.full_template);
    let outputs = parse_outputs(success.coinbase_outputs.as_ref()).unwrap();
    assert_eq!(outputs.len(), 3);
    assert!(outputs.iter().all(|(value, _)| *value == 0));
    assert!(matches!(
        declarator.redeem(success.mining_job_token.as_ref(), &miner, now),
        Ok(Custom::CoinbaseOnly(_))
    ));
    assert!(session
        .receive(
            &mut allocate("not-an-address", 10),
            &declarator,
            &NO_CONTEXT,
            now
        )
        .is_err());
    let mut busy = DeclaratorSession::new(&declarator);
    busy.receive(&mut setup(0), &declarator, &NO_CONTEXT, now)
        .unwrap();
    for request in 0..20 {
        let reply = busy
            .receive(
                &mut allocate(&miner, request),
                &declarator,
                &NO_CONTEXT,
                now,
            )
            .unwrap();
        assert!(reply.not_before.is_none());
    }
    let reply = busy
        .receive(&mut allocate(&miner, 20), &declarator, &NO_CONTEXT, now)
        .unwrap();
    assert_eq!(reply.not_before, Some(now + Duration::from_secs(60)));
    busy.close(&declarator);
    assert!(declarator.book.lock().unwrap().is_empty());
}

// #### PR #42
// What: a Full-Template pool refuses a Coinbase-only client with
// missing-declare-tx-data-flag (flags 1), as SRI does; a pool accepting
// both takes either; an undefined bit is refused and echoed; tokens from a
// Full-Template connection say so.
// Look here if: AcceptJd or the JD setup changes.
#[test]
fn the_accepted_modes_decide_the_setup() {
    let now = Instant::now();
    let answer = |accept, flags| {
        let declarator = declarator_accepting(accept);
        let mut session = DeclaratorSession::new(&declarator);
        let mut reply = session
            .receive(&mut setup(flags), &declarator, &NO_CONTEXT, now)
            .unwrap();
        let frame = &mut reply.frames[0];
        if frame.header().msg_type() == common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS {
            return Ok(session.full_template());
        }
        let error: SetupConnectionError = binary_sv2::from_bytes(frame.payload()).unwrap();
        Err((
            String::from_utf8(error.error_code.as_ref().to_vec()).unwrap(),
            error.flags,
        ))
    };
    assert_eq!(
        answer(AcceptJd::FullTemplate, 0),
        Err(("missing-declare-tx-data-flag".into(), 1))
    );
    assert_eq!(answer(AcceptJd::FullTemplate, 1), Ok(true));
    assert_eq!(answer(AcceptJd::Both, 0), Ok(false));
    assert_eq!(answer(AcceptJd::Both, 1), Ok(true));
    assert_eq!(
        answer(AcceptJd::Both, 0b101),
        Err(("unsupported-feature-flags".into(), 0b100))
    );
    let declarator = declarator_accepting(AcceptJd::Both);
    let (_, token, _) = full_session(&declarator, now);
    assert!(PxToken::decode(&token).unwrap().full_template);
    assert_eq!(
        declarator.redeem(&token, &payout(), now).err(),
        Some("job-not-yet-validated")
    );
}

// #### PR #42
// What: the pool's template has 2 of the client's 3 transactions: one
// ProvideMissingTransactions round asks for the third by its position, the
// client's answer is checked, the node checks the whole block once (a
// provided transaction), and Success carries a declared PX token whose job
// holds the 3 transactions. On the same parent, the next declaration finds
// the third among the provided ones and is not checked again within the
// minute; a minute later it is, and at once on a new parent.
// Look here if: DeclareMiningJob, the missing transactions or the validator
// cadence change.
#[test]
fn session_full_template_flow() {
    let node = Arc::new(FakeNode::default());
    let declarator = declarator_accepting(AcceptJd::FullTemplate).with_validator(node.clone());
    assert_eq!(declarator.validator_kind(), "test");
    let now = Instant::now();
    let (mut session, token1, scripts) = full_session(&declarator, now);
    let pool = Arc::new(context(2));
    let current = Context {
        current: Some(&pool),
        recent: &[],
    };
    let txids = ids(1..=3);
    let parts = coinbase_parts(pool.height, &scripts);
    let mut reply = session
        .receive(
            &mut declare_frame(7, &token1, pool.version, &parts, &txids),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert_eq!((reply.missing_rounds, reply.declared), (1, 0));
    let ask: ProvideMissingTransactions =
        binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
    assert_eq!(ask.request_id, 7);
    let third = txids.iter().position(|id| *id == tx(3).0).unwrap() as u16;
    assert_eq!(
        ask.unknown_tx_position_list
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![third]
    );
    assert!(node.blocks.lock().unwrap().is_empty());
    let mut reply = session
        .receive(
            &mut provide_frame(7, &[tx(3).1]),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert_eq!((reply.declared, reply.validations), (1, 1));
    let declared = declared_token(&mut reply);
    let decoded = PxToken::decode(&declared).unwrap();
    assert!(decoded.declared && decoded.full_template);
    let checked: Block = consensus::deserialize(&node.blocks.lock().unwrap()[0]).unwrap();
    assert_eq!(checked.txdata.len(), 4);
    assert!(checked.check_merkle_root());
    match declarator.redeem(&declared, &payout(), now) {
        Ok(Custom::Declared(job)) => {
            assert_eq!(job.template.transaction_ids(), txids.as_slice());
            assert_eq!(job.shape.extranonce_len(), 32);
        }
        _ => panic!("the declared job"),
    }
    let mut again = |id, at| {
        let (token, _) = new_token(&mut session, &declarator, at);
        session
            .receive(
                &mut declare_frame(id, &token, pool.version, &parts, &txids),
                &declarator,
                &current,
                at,
            )
            .unwrap()
    };
    let reply = again(8, now + Duration::from_secs(1));
    assert_eq!(
        (reply.missing_rounds, reply.validations, reply.declared),
        (0, 0, 1),
        "the provided transaction is known now; checked within the minute"
    );
    let reply = again(9, now + FULL_CHECK_EVERY);
    assert_eq!((reply.validations, reply.declared), (1, 1));
    let mut raw = rpc_template();
    raw["previousblockhash"] = serde_json::json!("cd".repeat(32));
    let next = Arc::new(BchTemplate::from_rpc(&raw).unwrap());
    let recent = [pool.clone()];
    let moved = Context {
        current: Some(&next),
        recent: &recent,
    };
    let (token, _) = new_token(&mut session, &declarator, now);
    let reply = session
        .receive(
            &mut declare_frame(10, &token, next.version, &parts, &txids),
            &declarator,
            &moved,
            now + FULL_CHECK_EVERY,
        )
        .unwrap();
    assert_eq!(
        (reply.missing_rounds, reply.validations, reply.declared),
        (0, 1, 1),
        "a new parent is checked at once; its transactions come from recent templates"
    );
}

// #### PR #42
// What: the node's refusals map as ckpool's do: "does not build on chain
// tip" is stale-chain-tip and any other reason invalid-job, both with the
// node's reason as details; a node that cannot be asked gives
// internal-error; a coinbase for the block before or after the pool's is
// stale-chain-tip, any other height invalid-coinbase-tx.
// Look here if: node_refusal or the height check changes.
#[test]
fn tip_races_become_stale_chain_tip_with_the_reason_in_details() {
    let node = Arc::new(FakeNode::default());
    let declarator = declarator_accepting(AcceptJd::FullTemplate).with_validator(node.clone());
    let now = Instant::now();
    let pool = Arc::new(context(2));
    let current = Context {
        current: Some(&pool),
        recent: &[],
    };
    let txids = ids(1..=2);
    let outcome = |verdict: Option<Verdict>, height: u32| {
        *node.verdict.lock().unwrap() = verdict;
        let (mut session, token, scripts) = full_session(&declarator, now);
        let mut reply = session
            .receive(
                &mut declare_frame(
                    1,
                    &token,
                    pool.version,
                    &coinbase_parts(height, &scripts),
                    &txids,
                ),
                &declarator,
                &current,
                now,
            )
            .unwrap();
        error(&mut reply)
    };
    let tip = "Invalid block: does not build on chain tip";
    assert_eq!(
        outcome(Some(Verdict::Invalid(tip.into())), pool.height),
        ("stale-chain-tip".into(), tip.into())
    );
    let spent = "Invalid block: bad-txns-inputs-missingorspent";
    assert_eq!(
        outcome(Some(Verdict::Invalid(spent.into())), pool.height),
        ("invalid-job".into(), spent.into())
    );
    assert_eq!(
        outcome(Some(Verdict::Unavailable), pool.height),
        ("internal-error".into(), "validation unavailable".into())
    );
    assert_eq!(
        outcome(None, pool.height + 1),
        ("stale-chain-tip".into(), "wrong height".into())
    );
    assert_eq!(
        outcome(None, pool.height + 5),
        ("invalid-coinbase-tx".into(), "wrong height".into())
    );
    assert_eq!(
        node_error(r#"rpc error: {"code":-32601,"message":"Method not found"}"#),
        Some((-32601, "Method not found".into()))
    );
    assert_eq!(node_error("connect: refused"), None);
}

// #### PR #42
// What: the pool never asks for anything until a declaration passes its
// checks: a coinbase short of the fee, transactions out of canonical order,
// a version outside the rolling bits, another connection's token and a
// 31st declaration in a minute are each refused; an answer that is not the
// transaction asked for is invalid-job; missing transactions that never
// come are missing-txs after 30 seconds; a Coinbase-only connection cannot
// declare.
// Look here if: the declaration checks change.
#[test]
fn declarations_are_checked_before_anything_is_asked() {
    let declarator = declarator_accepting(AcceptJd::Both);
    let now = Instant::now();
    let pool = Arc::new(context(2));
    let current = Context {
        current: Some(&pool),
        recent: &[],
    };
    let (mut session, _, scripts) = full_session(&declarator, now);
    let parts = coinbase_parts(pool.height, &scripts);
    let mut refused = |version: u32, parts: &(Vec<u8>, Vec<u8>), txids: &[Hash]| {
        let (token, _) = new_token(&mut session, &declarator, now);
        let mut reply = session
            .receive(
                &mut declare_frame(1, &token, version, parts, txids),
                &declarator,
                &current,
                now,
            )
            .unwrap();
        error(&mut reply)
    };
    let mut cheap = scripts.clone();
    cheap.swap(1, 2);
    let (code, details) = refused(
        pool.version,
        &coinbase_parts(pool.height, &cheap),
        &ids(1..=2),
    );
    assert_eq!(code, "invalid-coinbase-tx");
    assert!(details.contains("needs at least"), "{details}");
    assert!(!details.contains("bchtest"));
    let mut backwards = ids(1..=2);
    backwards.reverse();
    assert_eq!(
        refused(pool.version, &parts, &backwards),
        (
            "invalid-job".into(),
            "transactions are not in canonical (CTOR) order".into()
        )
    );
    assert_eq!(
        refused(pool.version ^ 1, &parts, &ids(1..=2)).0,
        "invalid-job"
    );
    let (mut other, foreign, _) = full_session(&declarator, now);
    let mut reply = session
        .receive(
            &mut declare_frame(2, &foreign, pool.version, &parts, &ids(1..=2)),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert_eq!(error(&mut reply).0, "invalid-mining-job-token");
    let (token, _) = new_token(&mut other, &declarator, now);
    let reply = other
        .receive(
            &mut declare_frame(3, &token, pool.version, &parts, &ids(1..=3)),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert_eq!(reply.missing_rounds, 1);
    let mut reply = other
        .receive(
            &mut provide_frame(3, &[tx(4).1]),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert_eq!(
        error(&mut reply),
        (
            "invalid-job".into(),
            "a provided transaction is not the one asked for".into()
        )
    );
    let (token, _) = new_token(&mut other, &declarator, now);
    other
        .receive(
            &mut declare_frame(4, &token, pool.version, &parts, &ids(1..=5)),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert!(other
        .expire(now + Duration::from_secs(29))
        .unwrap()
        .frames
        .is_empty());
    let mut expired = other.expire(now + PENDING_EXPIRY).unwrap();
    assert_eq!(error(&mut expired).0, "missing-txs");
    let mut flood = DeclaratorSession::new(&declarator);
    flood
        .receive(&mut setup(1), &declarator, &NO_CONTEXT, now)
        .unwrap();
    for request in 0..DECLARATIONS_PER_MINUTE as u32 {
        let (token, _) = new_token(&mut flood, &declarator, now);
        let mut reply = flood
            .receive(
                &mut declare_frame(request, &token, pool.version, &parts, &ids(1..=2)),
                &declarator,
                &current,
                now,
            )
            .unwrap();
        declared_token(&mut reply);
    }
    let (token, _) = new_token(&mut flood, &declarator, now);
    let mut reply = flood
        .receive(
            &mut declare_frame(99, &token, pool.version, &parts, &ids(1..=2)),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    assert_eq!(
        error(&mut reply),
        (
            "invalid-job".into(),
            "more than 30 declarations a minute".into()
        )
    );
    let mut coinbase_only = DeclaratorSession::new(&declarator);
    coinbase_only
        .receive(&mut setup(0), &declarator, &NO_CONTEXT, now)
        .unwrap();
    let (token, _) = new_token(&mut coinbase_only, &declarator, now);
    assert!(coinbase_only
        .receive(
            &mut declare_frame(1, &token, pool.version, &parts, &ids(1..=2)),
            &declarator,
            &current,
            now,
        )
        .is_err());
}

// #### PR #42
// What: a PushSolution is matched to the declared job by proof of work: the
// coinbase rebuilt with the pushed extranonce on the job's parent and bits
// meets the target, and the whole block (the job's transactions) comes back
// for the journal; a solution on another parent is only counted.
// Look here if: PushSolution handling or DeclaredJob::solved changes.
#[test]
fn push_solution_finds_the_declared_job_by_proof_of_work() {
    let declarator = declarator_accepting(AcceptJd::FullTemplate);
    let now = Instant::now();
    let pool = Arc::new(context(2));
    let current = Context {
        current: Some(&pool),
        recent: &[],
    };
    let (mut session, token, scripts) = full_session(&declarator, now);
    let parts = coinbase_parts(pool.height, &scripts);
    let txids = ids(1..=2);
    let mut reply = session
        .receive(
            &mut declare_frame(1, &token, pool.version, &parts, &txids),
            &declarator,
            &current,
            now,
        )
        .unwrap();
    declared_token(&mut reply);
    let extranonce = [0x42u8; 32];
    let mut coinbase = parts.0.clone();
    coinbase.extend(extranonce);
    coinbase.extend(&parts.1);
    let root = fold(double_sha256(&coinbase), pool.merkle_path());
    let header = |nonce: u32| {
        let mut header = pool.version.to_le_bytes().to_vec();
        header.extend(pool.previous_hash);
        header.extend(root);
        header.extend(pool.current_time.to_le_bytes());
        header.extend(pool.bits.to_le_bytes());
        header.extend(nonce.to_le_bytes());
        header
    };
    let nonce = (0..1_000)
        .find(|nonce| meets_target(&double_sha256(&header(*nonce)), &pool.target))
        .unwrap();
    let push = |prev_hash: Hash| {
        encoded(
            PushSolution {
                extranonce: extranonce.as_slice().try_into().unwrap(),
                prev_hash: (&prev_hash).into(),
                nonce,
                ntime: pool.current_time,
                nbits: pool.bits,
                version: pool.version,
            },
            MESSAGE_TYPE_PUSH_SOLUTION,
            false,
        )
        .unwrap()
    };
    let reply = session
        .receive(&mut push(pool.previous_hash), &declarator, &current, now)
        .unwrap();
    assert_eq!(reply.blocks.len(), 1);
    let pushed = &reply.blocks[0];
    assert_eq!(pushed.parent, pool.previous_hash);
    assert_eq!(pushed.height, pool.height);
    assert_eq!(pushed.hash, double_sha256(&header(nonce)));
    let block: Block = consensus::deserialize(&pushed.block).unwrap();
    assert_eq!(block.txdata.len(), 3);
    assert!(block.check_merkle_root());
    let reply = session
        .receive(&mut push([1; 32]), &declarator, &current, now)
        .unwrap();
    assert_eq!((reply.blocks.len(), reply.unmatched), (0, 1));
}
