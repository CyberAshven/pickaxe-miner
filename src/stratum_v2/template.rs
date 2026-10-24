//! #### PR #38
//! Validated BCH full templates. Preserve the node's complete CTOR transaction
//! list and target; do not inherit Bitcoin witness or fixed block-size rules.

use super::jd::plan::JdPlan;
use super::merge::{
    commitment::OUTPUT_LEN,
    header::{forwarder_body, MAX_P2S, TAG},
    set::{AuxJob, LeafTerms, TokenSet},
    split::{Slot, Split},
    OutPoint,
};
use crate::config::MiningNetwork;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};
use stratum_core::bitcoin::{consensus, Transaction};

/// Hash bytes in serialized/internal order, not RPC display order.
pub type Hash = [u8; 32];

/// #### PR #48: Chipnet's proof-of-work limit (00000000ffff…) as bits.
pub const MIN_DIFFICULTY_BITS: u32 = 0x1d00ffff;
/// #### PR #48: the gap after the parent's time from which a Chipnet block
/// must carry `MIN_DIFFICULTY_BITS`: twice the 10-minute target spacing.
pub const MIN_DIFFICULTY_GAP: u32 = 1200;

#[derive(Clone, Debug)]
pub struct BchTemplate {
    pub previous_hash: Hash,
    pub version: u32,
    pub bits: u32,
    pub target: Hash,
    pub min_time: u32,
    pub current_time: u32,
    pub height: u32,
    pub size_limit: u64,
    pub coinbase_value: u64,
    /// #### PR #48: the parent block's time, on a network that allows
    /// minimum-difficulty blocks (Chipnet); see `stale_bits`.
    pub parent_time: Option<u32>,
    coinbase_flags: Vec<u8>,
    /// #### PR #42: shared, so a template's copies (with tokens, a plan or a
    /// pool's name) and the jobs a pool checks for Job Declaration clients
    /// hold the same bytes.
    transactions: Arc<[Arc<[u8]>]>,
    /// #### PR #42: the transactions' ids (internal byte order), and an
    /// index of them built on first use.
    txids: Arc<[Hash]>,
    index: Arc<OnceLock<HashMap<Hash, u32>>>,
    /// #### PR #42: the coinbase's merkle branch (index 0), computed once
    /// from the transactions' hashes, which nothing else needs.
    merkle_path: Arc<[Hash]>,
    /// #### PR #42: the merge-mined tokens of jobs built from this template.
    tokens: Option<Arc<TokenSet>>,
    /// #### PR #42: where the template's jobs come from.
    origin: Origin,
    /// #### PR #42: the Job Declaration plan of a client's local server:
    /// jobs built from this template pay the pool's outputs and nest their
    /// extranonce inside the pool channel's.
    jd: Option<Arc<JdPlan>>,
    p2pool: Option<Arc<crate::p2pool::jd::Prepared>>,
    p2pool_donation: Option<Arc<BchTemplate>>,
    declaration_id: Option<u64>,
    /// #### PR #42: a BCH block, or an ASIC-exclusive token's header.
    work: Work,
}

// #### PR #42: Work::Token
// What: a template is BCH block work (the default; merge-mined tokens may
// ride along) or an ASIC-exclusive token's job, mined instead of BCH: a
// header-shaped token (SAFA layout v1) whose header is the token's
// commitment and whose coinbase is the keyless forwarder, or a Case A
// pure-token job (a coinbase-only "block" whose commitment names the token).
// Token work has no transactions, is never a block, and carries its own
// version rule.
// Why: one job pipeline: vardiff, the SV1 adapter, SV2 standard and
// extended channels, telemetry and donation rotation run unchanged on token
// jobs; with BCH selected nothing changes.
// Look here if: a token job reaches the block journal, or a BCH job's
// version rule changes.
/// What a template's jobs are worth.
#[derive(Clone, Debug, Default)]
pub enum Work {
    #[default]
    Block,
    Token(Arc<TokenJob>),
}

/// How a token's job is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// SAFA v1: the header is the token's commitment, and the coinbase is
    /// the forwarder that receives the claim.
    Safa,
    /// Case A pure-token: a coinbase-only "block" whose output 0 commits to
    /// the token.
    CaseA,
}

/// An ASIC-exclusive token's job, mined instead of BCH.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenJob {
    /// The token's name, for the dashboard.
    pub token: &'static str,
    pub layout: Layout,
    /// The version bits a device may roll; 0 keeps the version fixed.
    pub version_mask: u32,
    /// What the work answers: HASH256 of the thread's commitment (SAFA), or
    /// the Case A anchor.
    pub anchor: Hash,
    /// The thread (or baton) the job was issued for.
    pub thread: OutPoint,
    pub age: u16,
    /// The chain's median time past when the job was built.
    pub mtp: u32,
}

/// #### PR #42: where a template's jobs come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// This server's own source: a node or a template provider.
    Own,
    /// A Coinbase-only custom job (Job Declaration): it checks headers but
    /// has no transactions to build a block with.
    HeaderOnly,
    /// A Full-Template declaration a pool checked: the client's
    /// transactions, so its blocks are whole.
    Declared,
}

#[derive(Clone, Debug)]
pub struct Coinbase {
    pub bytes: Vec<u8>,
    pub merkle_root: Hash,
}

/// Payout and transaction order stay server-owned. Only the fixed-length
/// extranonce hole is delegated to an extended channel.
#[derive(Clone, Debug)]
pub struct CoinbaseParts {
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub merkle_path: Vec<Hash>,
}

impl BchTemplate {
    /// Accept a full BCHN getblocktemplate response. Light templates require a
    /// distinct source-bound job and must not enter the full-block serializer.
    pub fn from_rpc(value: &Value) -> Result<Self, String> {
        if value.get("default_witness_commitment").is_some()
            || value.get("job_id").is_some()
            || value.get("merkle").is_some()
        {
            return Err("expected a full BCH template without witness commitment".into());
        }
        if let Some(rules) = value.get("rules") {
            let rules = rules.as_array().ok_or("invalid template rules")?;
            if rules.iter().any(|rule| {
                rule.as_str()
                    .is_none_or(|rule| rule.trim_start_matches('!') == "segwit")
            }) {
                return Err("BCH templates must not require segwit".into());
            }
        }
        let bits_text = text(value, "bits")?;
        if bits_text.len() != 8 {
            return Err("bits must contain four bytes".into());
        }
        let bits = u32::from_str_radix(bits_text, 16).map_err(|_| "invalid bits")?;
        let target = compact_target(bits)?;
        if display_hash(text(value, "target")?)? != target {
            return Err("template target disagrees with bits".into());
        }
        let size_limit = number(value, "sizelimit")?;
        if size_limit < 180 {
            return Err("template block size limit is too small".into());
        }
        let mut transactions = Vec::new();
        let mut transaction_hashes = Vec::new();
        let mut previous_display = None;
        let mut total_size = 80u64;
        let entries = value
            .get("transactions")
            .and_then(Value::as_array)
            .ok_or("full template omitted transactions")?;
        for entry in entries {
            let encoded = text(entry, "data")?;
            if encoded.len() as u64 > size_limit.saturating_mul(2) {
                return Err("template transaction exceeds block size limit".into());
            }
            let bytes = hex::decode(encoded).map_err(|_| "invalid transaction hex")?;
            total_size = total_size
                .checked_add(bytes.len() as u64)
                .ok_or("template size overflow")?;
            if total_size > size_limit {
                return Err("template transactions exceed block size limit".into());
            }
            let hash = checked_transaction(&bytes)?;
            if display_hash(text(entry, "txid")?)? != hash {
                return Err("transaction bytes disagree with template txid".into());
            }
            let mut display = hash;
            display.reverse();
            if previous_display.is_some_and(|previous| previous >= display) {
                return Err("template transactions are duplicated or not in CTOR order".into());
            }
            previous_display = Some(display);
            transactions.push(Arc::<[u8]>::from(bytes));
            transaction_hashes.push(hash);
        }
        let min_time = word(value, "mintime")?;
        let current_time = word(value, "curtime")?;
        if current_time < min_time {
            return Err("template time is below mintime".into());
        }
        let coinbase_value = number(value, "coinbasevalue")?;
        if coinbase_value > 21_000_000 * 100_000_000 {
            return Err("coinbase value exceeds BCH monetary range".into());
        }
        let mut coinbase_flags = Vec::new();
        if let Some(aux) = value.get("coinbaseaux") {
            for flag in aux.as_object().ok_or("invalid coinbaseaux")?.values() {
                let encoded = flag.as_str().ok_or("invalid coinbase flag")?;
                if encoded.len() > 200 {
                    return Err("coinbase flags exceed script limit".into());
                }
                coinbase_flags.extend(hex::decode(encoded).map_err(|_| "invalid coinbase flags")?);
            }
        }
        if coinbase_flags.len() > 100 {
            return Err("coinbase flags exceed script limit".into());
        }
        Ok(Self {
            previous_hash: display_hash(text(value, "previousblockhash")?)?,
            version: word(value, "version")?,
            bits,
            target,
            min_time,
            current_time,
            height: word(value, "height")?,
            size_limit,
            coinbase_value,
            parent_time: None,
            coinbase_flags,
            merkle_path: coinbase_path(&transaction_hashes).into(),
            transactions: transactions.into(),
            txids: transaction_hashes.into(),
            index: Default::default(),
            tokens: None,
            origin: Origin::Own,
            jd: None,
            p2pool: None,
            p2pool_donation: None,
            declaration_id: None,
            work: Work::Block,
        })
    }

    /// #### PR #42: a Template Distribution provider's template
    /// What: built from what a provider announced, checked as a node's are:
    /// the prefix a minimal BIP34 height push (at most 8 bytes, its rest the
    /// node's flags), the value within the money supply, the provider's
    /// target no easier than `nBits`'s (the block target is `nBits`'s), the
    /// transactions full, witness-free and in CTOR order (at most 65,535),
    /// and their coinbase merkle path the one announced. Its size limit is
    /// the transactions, the header and the largest coinbase the reserve
    /// allows, which `coinbase_with_aux` then keeps to.
    /// Why: TDP carries no size limit and no chain; a provider must not make
    /// this server mine an invalid or oversized block.
    /// Look here if: a provider's templates are refused, or a block from one
    /// is too large.
    pub fn from_provided(provided: Provided<'_>) -> Result<Self, String> {
        let prefix = provided.prefix;
        if prefix.len() > 8 {
            return Err("template provider sent a coinbase prefix over 8 bytes".into());
        }
        let height = match prefix.first() {
            Some(&op) if (0x51..=0x60).contains(&op) => u32::from(op - 0x50),
            Some(&length @ 1..=4) => {
                let bytes = prefix
                    .get(1..1 + usize::from(length))
                    .ok_or("template provider sent a truncated height push")?;
                let mut word = [0; 4];
                word[..bytes.len()].copy_from_slice(bytes);
                u32::from_le_bytes(word)
            }
            _ => return Err("template provider's coinbase prefix is not a height push".into()),
        };
        let push = height_script(height);
        if height == 0 || height >= 1 << 31 || !prefix.starts_with(&push) {
            return Err("template provider's height push is not minimal".into());
        }
        if provided.value > 21_000_000 * 100_000_000 {
            return Err("coinbase value exceeds BCH monetary range".into());
        }
        let target = compact_target(provided.bits)?;
        if !meets_target(&provided.target, &target) {
            return Err("template provider's target is easier than its bits".into());
        }
        if provided.transactions.len() > 65_535 {
            return Err("template too large for SV2".into());
        }
        let mut hashes = Vec::with_capacity(provided.transactions.len());
        let mut previous_display: Option<Hash> = None;
        let mut size = 0u64;
        for bytes in &provided.transactions {
            let hash = checked_transaction(bytes)?;
            let mut display = hash;
            display.reverse();
            if previous_display.is_some_and(|previous| previous >= display) {
                return Err("template transactions are duplicated or not in CTOR order".into());
            }
            previous_display = Some(display);
            size += bytes.len() as u64;
            hashes.push(hash);
        }
        let merkle_path = coinbase_path(&hashes);
        if merkle_path != provided.merkle_path {
            return Err("template provider's merkle path disagrees with its transactions".into());
        }
        let mut count = Vec::new();
        compact_size(provided.transactions.len() + 1, &mut count);
        Ok(Self {
            previous_hash: provided.previous_hash,
            version: provided.version,
            bits: provided.bits,
            target,
            min_time: provided.ntime,
            current_time: provided.ntime,
            height,
            size_limit: 80
                + count.len() as u64
                + size
                + super::tdp::COINBASE_FIXED
                + u64::from(provided.reserve),
            coinbase_value: provided.value,
            parent_time: None,
            coinbase_flags: prefix[push.len()..].to_vec(),
            transactions: provided.transactions.into_iter().map(Arc::from).collect(),
            txids: hashes.into(),
            index: Default::default(),
            merkle_path: merkle_path.into(),
            tokens: None,
            origin: Origin::Own,
            jd: None,
            p2pool: None,
            p2pool_donation: None,
            declaration_id: None,
            work: Work::Block,
        })
    }

    /// #### PR #42
    /// A Coinbase-only custom job's template: the pool's parent, bits,
    /// target, height and limits, with the client's version, start time and
    /// merkle path. The pool never sees the client's transactions, so it can
    /// check headers and shares against it but never build its block.
    pub fn custom(
        context: &BchTemplate,
        version: u32,
        min_ntime: u32,
        merkle_path: Vec<Hash>,
    ) -> Self {
        Self {
            previous_hash: context.previous_hash,
            version,
            bits: context.bits,
            target: context.target,
            min_time: min_ntime,
            current_time: min_ntime,
            height: context.height,
            size_limit: context.size_limit,
            coinbase_value: context.coinbase_value,
            parent_time: context.parent_time,
            coinbase_flags: Vec::new(),
            transactions: Vec::new().into(),
            txids: Vec::new().into(),
            index: Default::default(),
            merkle_path: merkle_path.into(),
            tokens: None,
            origin: Origin::HeaderOnly,
            jd: None,
            p2pool: None,
            p2pool_donation: None,
            declaration_id: None,
            work: Work::Block,
        }
    }

    /// #### PR #42
    /// A Full-Template declaration's template, once a pool holds every one
    /// of its transactions: the pool's parent, bits, target, height, size
    /// limit and times (`context`) with the client's version and
    /// transactions, in the declared order (`txids` their ids). Its blocks
    /// are whole, built around the client's coinbase.
    pub fn declared(
        context: &BchTemplate,
        version: u32,
        transactions: Vec<Arc<[u8]>>,
        txids: Vec<Hash>,
    ) -> Self {
        Self {
            previous_hash: context.previous_hash,
            version,
            bits: context.bits,
            target: context.target,
            min_time: context.min_time,
            current_time: context.current_time,
            height: context.height,
            size_limit: context.size_limit,
            coinbase_value: context.coinbase_value,
            parent_time: context.parent_time,
            coinbase_flags: Vec::new(),
            merkle_path: coinbase_path(&txids).into(),
            transactions: transactions.into(),
            txids: txids.into(),
            index: Default::default(),
            tokens: None,
            origin: Origin::Declared,
            jd: None,
            p2pool: None,
            p2pool_donation: None,
            declaration_id: None,
            work: Work::Block,
        }
    }

    /// #### PR #42
    /// An ASIC-exclusive token's job: no transactions and no BCH value; the
    /// previous hash, bits and target (at full precision) are the token's,
    /// and devices start at `ntime_start`. `height` only shapes a Case A
    /// coinbase's script.
    pub fn token_only(
        job: Arc<TokenJob>,
        previous_hash: Hash,
        version: u32,
        bits: u32,
        target: Hash,
        ntime_start: u32,
        height: u32,
    ) -> Self {
        Self {
            previous_hash,
            version,
            bits,
            target,
            min_time: ntime_start,
            current_time: ntime_start,
            height,
            // No block: a size that keeps the coinbase checks inert.
            size_limit: 1_000_000,
            coinbase_value: 0,
            parent_time: None,
            coinbase_flags: Vec::new(),
            transactions: Vec::new().into(),
            txids: Vec::new().into(),
            index: Default::default(),
            merkle_path: Vec::new().into(),
            tokens: None,
            origin: Origin::Own,
            jd: None,
            p2pool: None,
            p2pool_donation: None,
            declaration_id: None,
            work: Work::Token(job),
        }
    }

    /// #### PR #42: the token job, for token work.
    pub fn token_job(&self) -> Option<&Arc<TokenJob>> {
        match &self.work {
            Work::Block => None,
            Work::Token(job) => Some(job),
        }
    }

    /// #### PR #42: whether this is BCH block work.
    pub fn is_block(&self) -> bool {
        matches!(self.work, Work::Block)
    }

    // #### PR #48: Chipnet's 20-minute rule
    // What: a header more than 20 minutes after the parent's time needs the
    // proof-of-work limit's bits on Chipnet; one on any other bits is
    // refused by the node (bad-diffbits). Only a template that knows its
    // parent's time (Chipnet's) can tell.
    // Why: BCHN's GetNextASERTWorkRequired returns the limit when the block's
    // time is strictly greater than the parent's plus twice the target
    // spacing, and ContextualCheckBlockHeader requires those exact bits.
    // A share on an older normal-bits job may carry such a time (shares may
    // run to a minute past the server's clock).
    // Look here if: a Chipnet block is refused as bad-diffbits, or a share
    // that meets the block target is not sent as a block.
    /// Whether a header at `time` is past Chipnet's 20-minute point on bits
    /// other than the limit's. (The reverse, the limit's bits before that
    /// point, is `wrong_bits`'s other half.)
    pub fn stale_bits(&self, time: u32) -> bool {
        self.stale_from().is_some_and(|from| time >= from)
    }

    // #### PR #48: the limit's bits before the 20-minute point
    // What: on a template that knows its parent's time (Chipnet's), a header
    // at or before the parent's time plus 1200 s on the limit's bits is
    // refused unless ASERT itself gives the limit for this parent (from
    // Chipnet's anchor, the parent's height and time); with `stale_bits`,
    // whatever the node would refuse as bad-diffbits. Normal bits there are
    // the node's own ASERT result (a custom job must carry its pool's
    // template's bits) and are not recomputed. When ASERT cannot be computed
    // (a parent below the anchor) nothing is refused.
    // Why: BCHN requires ASERT's bits up to the 20-minute point, and a Job
    // Declaration custom job or solution on the limit's bits may carry an
    // earlier time than its pool's template.
    // Look here if: a Chipnet block on the limit's bits is refused as
    // bad-diffbits, or one that ASERT allows is not sent.
    /// Whether the node would refuse a header at `time` for this template's
    /// bits: `stale_bits`, or the limit's bits at or before the 20-minute
    /// point while ASERT gives other bits.
    pub fn wrong_bits(&self, time: u32) -> bool {
        self.stale_bits(time)
            || (self.bits == MIN_DIFFICULTY_BITS
                && self.parent_time.is_some_and(|parent| {
                    u64::from(time) <= u64::from(parent) + u64::from(MIN_DIFFICULTY_GAP)
                })
                && self
                    .asert_bits()
                    .is_some_and(|bits| bits != MIN_DIFFICULTY_BITS))
    }

    /// #### PR #48: the bits ASERT gives this template's block, from
    /// Chipnet's anchor: only Chipnet templates carry their parent's time
    /// (`MiningNetwork::allows_min_difficulty_blocks`).
    pub fn asert_bits(&self) -> Option<u32> {
        super::asert::next_bits(
            &super::asert::CHIPNET,
            self.height.checked_sub(1)?,
            self.parent_time?,
        )
    }

    /// #### PR #48: the first header time `stale_bits` refuses (the parent's
    /// time plus 1201 seconds), on a template that knows its parent's time
    /// and is not on the limit's bits; the node thread refreshes from then
    /// (see `min_difficulty_due` in the server).
    pub fn stale_from(&self) -> Option<u32> {
        if self.bits == MIN_DIFFICULTY_BITS {
            return None;
        }
        self.parent_time?.checked_add(MIN_DIFFICULTY_GAP + 1)
    }

    // #### PR #42: the per-template version rule
    // What: the version bits a device may roll: BIP320's for BCH blocks and
    // Case A token jobs, the token's own rule for a header-shaped token (none
    // when the token fixes its version slot).
    // Why: a header-shaped token's covenant checks the version slot.
    // Look here if: shares are refused with invalid-version on token work.
    /// The version bits a device may roll.
    pub fn version_mask(&self) -> u32 {
        match &self.work {
            Work::Block => super::channel::VERSION_ROLLING_MASK,
            Work::Token(job) => job.version_mask,
        }
    }

    /// Whether this template's coinbase is the forwarder.
    fn is_safa(&self) -> bool {
        self.token_job()
            .is_some_and(|job| job.layout == Layout::Safa)
    }

    /// #### PR #42: this template for a custom job set on it: the job's
    /// version and start time, the same transactions.
    pub fn with_header(&self, version: u32, min_ntime: u32) -> Self {
        Self {
            version,
            min_time: min_ntime,
            current_time: min_ntime,
            ..self.clone()
        }
    }

    /// #### PR #42: builds this template's jobs for a pool through Job
    /// Declaration: they pay `plan`'s outputs, nest their extranonce inside
    /// the pool channel's, and carry its token set's merge-mining (see
    /// `declared_aux`).
    pub fn declare(&mut self, plan: Arc<JdPlan>) {
        self.jd = Some(plan);
    }

    pub fn jd_plan(&self) -> Option<&Arc<JdPlan>> {
        self.jd.as_ref()
    }

    /// #### PR #65: obtain P2Pool's exact PPLNS coinbase before publishing.
    /// It cannot change the independently sourced transaction list or revenue.
    pub fn prepare_p2pool(&mut self) -> Result<(), String> {
        self.p2pool = None;
        // A token set does not stop it: P2Pool's coinbase carries no token
        // commitment, so its jobs mine BCH alone (see `tokens_refused`).
        if let Some(plan) = self.jd.as_ref().and_then(|plan| plan.p2pool.as_ref()) {
            self.p2pool = Some(Arc::new(plan.prepare(
                self.height,
                self.bits,
                &self.previous_hash,
                self.coinbase_value,
                &self.script_head(),
                None,
            )?));
        }
        Ok(())
    }

    pub fn p2pool_token(&self) -> Option<&[u8]> {
        self.p2pool
            .as_ref()
            .map(|prepared| prepared.token.as_slice())
    }

    pub fn is_p2pool(&self) -> bool {
        self.jd.as_ref().is_some_and(|plan| plan.p2pool.is_some())
    }

    pub fn set_declaration_id(&mut self, id: u64) {
        self.declaration_id = Some(id);
    }

    pub fn declaration_id(&self, generation: u64) -> u64 {
        self.declaration_id.unwrap_or(generation)
    }

    fn check_p2pool_prepared(&self) -> Result<(), String> {
        if self.is_p2pool() && self.p2pool.is_none() {
            return Err("P2Pool coinbase has not been prepared".into());
        }
        Ok(())
    }

    pub fn set_p2pool_donation(&mut self, template: BchTemplate) {
        self.p2pool_donation = Some(Arc::new(template));
    }

    pub fn p2pool_donation(&self) -> Option<&Arc<BchTemplate>> {
        self.p2pool_donation.as_ref()
    }

    /// #### PR #42: the coinbase script's head: the BIP34 height push, the
    /// node's flags and the pool's name; a custom job's `coinbase_prefix`.
    pub fn script_head(&self) -> Vec<u8> {
        let mut head = height_script(self.height);
        head.extend_from_slice(&self.coinbase_flags);
        head
    }

    /// #### PR #42: a custom job's template, without transactions.
    pub fn is_header_only(&self) -> bool {
        self.origin == Origin::HeaderOnly
    }

    /// #### PR #42: a Full-Template declaration's template.
    pub fn is_declared(&self) -> bool {
        self.origin == Origin::Declared
    }

    /// #### PR #42: the transactions' ids, in block order.
    pub fn transaction_ids(&self) -> &[Hash] {
        &self.txids
    }

    /// #### PR #42: the transaction with id `txid`, from an index built on
    /// the first call.
    pub fn transaction(&self, txid: &Hash) -> Option<&Arc<[u8]>> {
        let index = self.index.get_or_init(|| {
            self.txids
                .iter()
                .enumerate()
                .map(|(position, txid)| (*txid, position as u32))
                .collect()
        });
        index
            .get(txid)
            .map(|position| &self.transactions[*position as usize])
    }

    /// #### PR #42: a Job Declaration job's coinbase split around its whole
    /// extranonce (the pool channel's prefix and the bytes the pool lets the
    /// client roll), as DeclareMiningJob carries it: the same bytes as the
    /// local channels' coinbases, merge-mining outputs included.
    pub fn declared_parts(&self) -> Result<CoinbaseParts, String> {
        self.check_p2pool_prepared()?;
        if let Some(prepared) = &self.p2pool {
            return Ok(CoinbaseParts {
                prefix: prepared.coinbase.prefix.clone(),
                suffix: prepared.coinbase.suffix.clone(),
                merkle_path: self.merkle_path.to_vec(),
            });
        }
        let plan = self.jd.as_ref().ok_or("not a Job Declaration template")?;
        let extranonce = plan.upstream_prefix.len() + plan.rollable();
        let aux = self.declared_aux()?;
        let coinbase = self.coinbase_with_outputs(
            &vec![0; extranonce],
            plan.outputs(self.coinbase_value),
            aux.as_ref().map(|aux| &aux.output),
        )?;
        Ok(self.split(coinbase, extranonce))
    }

    // #### PR #42: merge-mined tokens under Job Declaration
    // What: a Job Declaration job with a token set carries the commitment as
    // output 0 before the pool's outputs, in every local coinbase, the
    // declared one and the custom job's outputs alike. Its leaves bind the
    // miner's own script (the pool's first output); no donation work runs
    // under Job Declaration (the pool's rule takes the donation from the
    // coinbase).
    // Why: the miner's node builds the blocks there, so merge-mined tokens
    // keep earning at a pool with no hashrate taken from BCH; the pool's
    // rule allows exactly this shape (output 0, worth 0). Only shares that
    // meet the pool's own target are sent to it (`jd::client::for_pool`).
    // Look here if: a pool refuses declarations with a commitment, the local
    // coinbases differ from the declared one, or token wins under Job
    // Declaration name another payout.
    /// The merge-mining of this template's Job Declaration jobs, or `None`
    /// without a plan or a token set. Every job on the plan shares it.
    pub fn declared_aux(&self) -> Result<Option<AuxJob>, String> {
        let (Some(plan), Some(set)) = (self.jd.as_ref(), self.tokens.as_ref()) else {
            return Ok(None);
        };
        if self.is_p2pool() {
            return Ok(None);
        }
        let miner = plan
            .token_payout()
            .ok_or("the pool's plan has no outputs")?;
        // #### merge-mining v2: the pool's split
        // What: a split leaf on a Job Declaration plan pays the miner and
        // puts the pool's donation (its whole rate to its donation output)
        // in the donation slot and the pool's fee (its rate to its fee
        // output) in the fee slot, as the declared coinbase pays them.
        // Why: no donation work runs under Job Declaration, so the pool's
        // rule takes BCH's donation and fee from the coinbase; tokens follow
        // the same rule. The pool checks it in a later change.
        // Look here if: a Job Declaration leaf's split differs from the
        // pool's rates.
        let slot = |bps: u16, output: Option<u8>| match output
            .and_then(|index| plan.scripts.get(usize::from(index)))
        {
            Some(script) => Slot::new(bps, script),
            None => Ok(Slot::EMPTY),
        };
        let split = Split::new(
            slot(plan.rates.donation_bps, plan.rates.donation_output).map_err(|e| e.to_string())?,
            slot(plan.rates.fee_bps, plan.rates.fee_output).map_err(|e| e.to_string())?,
        );
        let terms = LeafTerms {
            payout: miner,
            split,
            plain_payout: miner,
        };
        Ok(Some(AuxJob::build(set, &terms)))
    }

    /// The outputs a custom job carries for this Job Declaration template:
    /// the commitment first when tokens are merge-mined, then the pool's
    /// outputs.
    pub fn declared_outputs(&self) -> Result<Vec<(u64, Vec<u8>)>, String> {
        self.check_p2pool_prepared()?;
        if let Some(prepared) = &self.p2pool {
            return Ok(prepared.coinbase.outputs.clone());
        }
        let plan = self.jd.as_ref().ok_or("not a Job Declaration template")?;
        let mut outputs = plan.outputs(self.coinbase_value);
        if let Some(aux) = self.declared_aux()? {
            // A whole output: 8 value bytes, the script's length and the
            // script.
            outputs.insert(0, (0, aux.output[9..].to_vec()));
        }
        Ok(outputs)
    }
    // #### end PR #42 ####

    /// #### PR #40
    /// Writes the pool's name (`--pool-tag`) into the coinbase script of every
    /// block built from this template, after the node's flags and before the
    /// extranonce; the coinbase script stays within 100 bytes, which
    /// `coinbase_with_payout` checks.
    pub fn tag(&mut self, tag: &[u8]) {
        self.coinbase_flags.extend_from_slice(tag);
    }

    /// #### PR #42
    /// Attaches the token set to merge-mine in jobs built from this template.
    /// It only feeds `aux_job`: a coinbase carries the commitment only when
    /// the caller passes that job's output 0 to `coinbase_with_aux` or
    /// `coinbase_parts_with_aux`, which do not check it against this set.
    /// `coinbase_with_payout` and `coinbase_parts_with_payout` always build
    /// token-free coinbases. Without a call (no token registered) jobs and
    /// coinbases stay as they were.
    pub fn commit(&mut self, set: Arc<TokenSet>) {
        self.tokens = Some(set);
    }

    pub fn tokens(&self) -> Option<&Arc<TokenSet>> {
        self.tokens.as_ref()
    }

    // #### merge-mining v2: P2Pool
    // What: under P2Pool's own templates a token set is not merge-mined:
    // `aux_job` gives no commitment, the job mines BCH alone, and this
    // reason reaches the dashboard.
    // Why: P2Pool builds the coinbase and does not put this format's output
    // 0 in it yet; a token set must not stop BCH work there.
    // Look here if: tokens are registered at P2Pool and devices get no job,
    // or P2Pool builds output 0 in this format.
    /// Why this template's token set is not merge-mined, if it has one that
    /// is not.
    pub fn tokens_refused(&self) -> Option<&'static str> {
        (self.tokens.is_some() && self.is_p2pool())
            .then_some("P2Pool's coinbase does not carry the token commitment yet")
    }

    // #### merge-mining v2: pool and role rules
    // What: with a token set, a job for the operator's own devices (solo,
    // any channel) commits the operator's root, its leaves binding the
    // miner or, in donation-work jobs, the donation; a public pool's job
    // (it names an operator) carries the empty commitment; a Job
    // Declaration job carries the declared coinbase's.
    // Why: an operator-chosen token root must never sit on other people's
    // devices, and the pool fee never reaches tokens.
    // Look here if: a public pool's job carries a non-empty root, or a solo
    // job carries none while tokens are merge-mined.
    /// One job's merge-mining for this template's token set, or `None`
    /// without one.
    pub fn aux_job(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        policy: crate::donation::bch::BchPayout,
    ) -> Result<Option<AuxJob>, String> {
        // #### PR #42: under Job Declaration, the declared coinbase's.
        if self.jd.is_some() {
            return self.declared_aux();
        }
        let Some(set) = self.tokens.as_ref() else {
            return Ok(None);
        };
        if operator.is_some() {
            return Ok(Some(AuxJob::empty()));
        }
        let scripts = super::payout::scripts(network, payout, None)?;
        // #### merge-mining v2: the donation's split
        // What: a miner job's split leaves pay the miner with the donation
        // setting's block-reward share (1% at the 1.5% default) in the
        // donation slot; a BCH donation-work job's leaves pay the donation
        // with ext 0, the setting's work share. Tokens without split support
        // pay the job's plain token payout with ext 0.
        // Why: merge-mined tokens split as BCH does: two thirds from each
        // win, one third as work.
        // Look here if: a token's donation differs from BCH's at the same
        // setting.
        let split = if policy.donation_work {
            Split::NONE
        } else {
            Split::donation(policy.donation.reward_bps(), &scripts[1]).map_err(|e| e.to_string())?
        };
        let terms = LeafTerms {
            payout: super::payout::split_token_payout(&scripts, policy),
            split,
            plain_payout: super::payout::token_payout(&scripts, policy),
        };
        Ok(Some(AuxJob::build(set, &terms)))
    }

    /// Build one channel's coinbase. The caller assigns a unique extranonce;
    /// devices never control the payout or the transaction ordering.
    pub fn coinbase(
        &self,
        network: MiningNetwork,
        payout: &str,
        extranonce: &[u8],
    ) -> Result<Coinbase, String> {
        self.coinbase_with_payout(network, payout, None, extranonce, Default::default())
    }

    pub fn coinbase_with_payout(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce: &[u8],
        policy: crate::donation::bch::BchPayout,
    ) -> Result<Coinbase, String> {
        self.coinbase_with_aux(network, payout, operator, extranonce, policy, None)
    }

    /// The coinbase with a job's merge-mining output 0, if any.
    pub fn coinbase_with_aux(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce: &[u8],
        policy: crate::donation::bch::BchPayout,
        aux: Option<&[u8; OUTPUT_LEN]>,
    ) -> Result<Coinbase, String> {
        self.check_p2pool_prepared()?;
        if let Some(prepared) = &self.p2pool {
            if aux.is_some() {
                return Err("P2Pool coinbase does not carry this token proof format".into());
            }
            let bytes = prepared
                .coinbase
                .coinbase(extranonce)
                .ok_or("P2Pool extranonce length changed")?;
            self.check_block_size(bytes.len())?;
            return Ok(Coinbase {
                merkle_root: fold(double_sha256(&bytes), &self.merkle_path),
                bytes,
            });
        }
        // #### PR #42: the script coinbase
        // What: on a SAFA job the coinbase is the forwarder: a push of the
        // tag and the extranonce, OP_DROP, and the rule that forwards the
        // claim to the job's beneficiary (the miner, or the donation in its
        // work jobs). With no transactions the merkle root is its HASH256,
        // the payout field the token checks.
        // Why: devices hash a coinbase they never parse, so any SV1 or SV2
        // device mines the token unchanged.
        // Look here if: a SAFA win names another payout than its job's.
        if self.is_safa() {
            let scripts = super::payout::scripts(network, payout, operator)?;
            return forwarder_coinbase(extranonce, super::payout::beneficiary(&scripts, policy));
        }
        // #### PR #42: Job Declaration jobs pay the pool's outputs.
        let outputs = match &self.jd {
            Some(plan) => plan.outputs(self.coinbase_value),
            None => {
                let scripts = super::payout::scripts(network, payout, operator)?;
                super::payout::outputs(self.coinbase_value, &scripts, policy)
            }
        };
        self.coinbase_with_outputs(extranonce, outputs, aux)
    }

    /// #### PR #42: the coinbase paying `outputs`, with a job's merge-mining
    /// output 0, if any.
    fn coinbase_with_outputs(
        &self,
        extranonce: &[u8],
        outputs: Vec<(u64, Vec<u8>)>,
        commitment: Option<&[u8; OUTPUT_LEN]>,
    ) -> Result<Coinbase, String> {
        if extranonce.len() > 64 {
            return Err("extranonce exceeds coinbase budget".into());
        }
        let mut script = height_script(self.height);
        script.extend_from_slice(&self.coinbase_flags);
        script.extend_from_slice(extranonce);
        // Keep coinbases at least 100 bytes, valid before and after BCH's
        // minimum-transaction-size change. This is not a block-size cap.
        script.resize(script.len().max(15), 0);
        if script.len() > 100 {
            return Err("coinbase script exceeds 100 bytes".into());
        }
        let mut bytes = 2u32.to_le_bytes().to_vec();
        bytes.push(1);
        bytes.extend_from_slice(&[0; 32]);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        compact_size(script.len(), &mut bytes);
        bytes.extend(script);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        // #### PR #42: merge-mining commitment in output 0
        // What: with tokens, output 0 is the 53-byte commitment (value 0)
        // before the payouts. Without tokens (`commitment` is `None`, the
        // default on both networks) the coinbase is byte for byte what it
        // was.
        // Why: a covenant finds output 0 from the fixed-shape head alone,
        // with no search, and output 0 does not compete with P2Pool's
        // last-output share commitment.
        // The payouts, the script, the extranonce offset and the parts are
        // unchanged, so devices and SV1 firmware see only a longer suffix.
        // Look here if: a token-enabled block is refused by the node, or the
        // output count reaches 0xfd.
        let count = outputs.len() + usize::from(commitment.is_some());
        if commitment.is_some() && count >= 0xfd {
            return Err("a merge-mined coinbase needs fewer than 253 outputs".into());
        }
        compact_size(count, &mut bytes);
        if let Some(commitment) = commitment {
            bytes.extend_from_slice(commitment);
        }
        for (amount, script) in outputs {
            bytes.extend_from_slice(&amount.to_le_bytes());
            compact_size(script.len(), &mut bytes);
            bytes.extend(script);
        }
        // #### end PR #42 ####
        bytes.extend_from_slice(&0u32.to_le_bytes());
        self.check_block_size(bytes.len())?;
        // #### PR #42: the coinbase path is computed once per template
        // What: the merkle root folds the coinbase's hash up the branch
        // computed when the template arrived, instead of building the whole
        // tree for every coinbase.
        // Why: log2(n) hashes instead of n for each job and share.
        // Look here if: a block's merkle root mismatches.
        let merkle_root = fold(double_sha256(&bytes), &self.merkle_path);
        Ok(Coinbase { bytes, merkle_root })
    }

    pub fn coinbase_parts(
        &self,
        network: MiningNetwork,
        payout: &str,
        extranonce_len: usize,
    ) -> Result<CoinbaseParts, String> {
        self.coinbase_parts_with_payout(network, payout, None, extranonce_len, Default::default())
    }

    pub fn coinbase_parts_with_payout(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce_len: usize,
        policy: crate::donation::bch::BchPayout,
    ) -> Result<CoinbaseParts, String> {
        self.coinbase_parts_with_aux(network, payout, operator, extranonce_len, policy, None)
    }

    /// #### PR #42: the parts with a job's merge-mining output 0, which sits
    /// in the suffix; the prefix and the merkle path are unchanged.
    pub fn coinbase_parts_with_aux(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce_len: usize,
        policy: crate::donation::bch::BchPayout,
        aux: Option<&[u8; OUTPUT_LEN]>,
    ) -> Result<CoinbaseParts, String> {
        if extranonce_len > 64 {
            return Err("extranonce exceeds coinbase budget".into());
        }
        let coinbase = self.coinbase_with_aux(
            network,
            payout,
            operator,
            &vec![0; extranonce_len],
            policy,
            aux,
        )?;
        Ok(self.split(coinbase, extranonce_len))
    }

    /// The parts of `coinbase`, whose extranonce is `extranonce_len` bytes.
    fn split(&self, coinbase: Coinbase, extranonce_len: usize) -> CoinbaseParts {
        // The coinbase script is at most 100 bytes, so its CompactSize is one
        // byte; #### PR #42: a forwarder's extranonce follows its push and
        // tag.
        let offset = if self.is_safa() {
            1 + TAG.len()
        } else {
            4 + 1 + 32 + 4 + 1 + height_script(self.height).len() + self.coinbase_flags.len()
        };
        CoinbaseParts {
            prefix: coinbase.bytes[..offset].to_vec(),
            suffix: coinbase.bytes[offset + extranonce_len..].to_vec(),
            merkle_path: self.merkle_path.to_vec(),
        }
    }

    pub fn header(
        &self,
        coinbase: &Coinbase,
        version: u32,
        time: u32,
        nonce: u32,
    ) -> Result<[u8; 80], String> {
        if time < self.min_time {
            return Err("header time is below mintime".into());
        }
        let mut header = [0; 80];
        header[..4].copy_from_slice(&version.to_le_bytes());
        header[4..36].copy_from_slice(&self.previous_hash);
        header[36..68].copy_from_slice(&coinbase.merkle_root);
        header[68..72].copy_from_slice(&time.to_le_bytes());
        header[72..76].copy_from_slice(&self.bits.to_le_bytes());
        header[76..].copy_from_slice(&nonce.to_le_bytes());
        Ok(header)
    }

    /// Build only a full block; no light-job fallback can truncate its tx list.
    pub fn block(&self, coinbase: &Coinbase, header: [u8; 80]) -> Result<Vec<u8>, String> {
        if self.origin == Origin::HeaderOnly {
            return Err("a custom job's template has no transactions".into());
        }
        // #### PR #42: block suppression: token work never becomes a block.
        if !self.is_block() {
            return Err("token work is not a BCH block".into());
        }
        self.check_block_size(coinbase.bytes.len())?;
        if header[4..36] != self.previous_hash
            || header[36..68] != fold(double_sha256(&coinbase.bytes), &self.merkle_path)
            || header[72..76] != self.bits.to_le_bytes()
        {
            return Err("block header does not belong to this template".into());
        }
        Ok(self.assemble(&coinbase.bytes, &header))
    }

    /// #### PR #42: the block's bytes, unchecked: the header, the count,
    /// the coinbase and the template's transactions. A Template
    /// Distribution client's solution is assembled with it before its
    /// checks, so even a refused one can still reach the node (D24).
    pub fn assemble(&self, coinbase: &[u8], header: &[u8; 80]) -> Vec<u8> {
        let mut block = header.to_vec();
        compact_size(self.transactions.len() + 1, &mut block);
        block.extend_from_slice(coinbase);
        for tx in self.transactions.iter() {
            block.extend_from_slice(tx);
        }
        block
    }

    pub fn transaction_count(&self) -> usize {
        self.transactions.len() + 1
    }

    /// #### PR #42: the BIP34 height push that begins every coinbase
    /// script; a Template Distribution client's coinbase prefix.
    pub fn height_push(&self) -> Vec<u8> {
        height_script(self.height)
    }

    /// #### PR #42: the coinbase's merkle branch, deepest sibling first.
    pub fn merkle_path(&self) -> &[Hash] {
        &self.merkle_path
    }

    /// #### PR #42: the block's other transactions, in block (CTOR) order.
    pub fn transactions(&self) -> &[Arc<[u8]>] {
        &self.transactions
    }

    /// #### PR #42: the bytes a coinbase may take in a block of this
    /// template: the size limit less the header, the transaction count and
    /// every other transaction.
    pub fn coinbase_budget(&self) -> u64 {
        let mut count = Vec::new();
        compact_size(self.transaction_count(), &mut count);
        let others = self
            .transactions
            .iter()
            .map(|tx| tx.len() as u64)
            .sum::<u64>();
        self.size_limit
            .saturating_sub(80 + count.len() as u64 + others)
    }

    /// #### PR #42: the template with other transactions, unchecked (its
    /// merkle path is not recomputed), for transaction-data size tests.
    #[cfg(test)]
    pub(super) fn with_transactions(mut self, transactions: Vec<Vec<u8>>) -> Self {
        self.txids = transactions.iter().map(|tx| double_sha256(tx)).collect();
        self.index = Default::default();
        self.transactions = transactions.into_iter().map(Arc::from).collect();
        self
    }

    fn check_block_size(&self, coinbase_size: usize) -> Result<(), String> {
        if coinbase_size as u64 > self.coinbase_budget() {
            return Err("coinbase exceeds template block size budget".into());
        }
        Ok(())
    }
}

/// #### PR #42: a SAFA job's coinbase: the forwarder to `destination`
/// around `extranonce`, whose HASH256 is the merkle root.
fn forwarder_coinbase(extranonce: &[u8], destination: &[u8]) -> Result<Coinbase, String> {
    let pushed = TAG.len() + extranonce.len();
    if pushed > 75 {
        return Err("extranonce exceeds the forwarder's salt".into());
    }
    let mut bytes = vec![pushed as u8];
    bytes.extend_from_slice(&TAG);
    bytes.extend_from_slice(extranonce);
    bytes.push(0x75);
    bytes.extend(forwarder_body(destination)?);
    if bytes.len() > MAX_P2S {
        return Err("the forwarder exceeds 201 bytes".into());
    }
    let merkle_root = double_sha256(&bytes);
    Ok(Coinbase { bytes, merkle_root })
}

/// #### PR #42: what a Template Distribution provider announced for one
/// template, with its transactions.
pub struct Provided<'a> {
    pub previous_hash: Hash,
    pub version: u32,
    pub bits: u32,
    /// The provider's target, which may be harder than `bits`'s.
    pub target: Hash,
    pub ntime: u32,
    /// The coinbase prefix: the height push, then any flags.
    pub prefix: &'a [u8],
    pub value: u64,
    pub transactions: Vec<Vec<u8>>,
    pub merkle_path: &'a [Hash],
    /// The coinbase output bytes this server declared it may add.
    pub reserve: u32,
}

/// A template transaction's hash after the BCH checks: it decodes, is no
/// coinbase, has inputs and no witness encoding. Bitcoin's decoder treats
/// CashTokens prefixes as opaque output script bytes; BCH txids commit to
/// every byte. No Bitcoin consensus validation is used.
/// #### PR #42: also a Job Declaration client's provided transactions.
pub fn checked_transaction(bytes: &[u8]) -> Result<Hash, String> {
    let tx: Transaction =
        consensus::deserialize(bytes).map_err(|_| "malformed template transaction")?;
    if tx.is_coinbase()
        || tx.input.is_empty()
        || tx.input.iter().any(|input| !input.witness.is_empty())
        || bytes.get(4) == Some(&0)
    {
        return Err("template contains coinbase, empty inputs or witness encoding".into());
    }
    Ok(double_sha256(bytes))
}

pub fn double_sha256(bytes: &[u8]) -> Hash {
    Sha256::digest(Sha256::digest(bytes)).into()
}

/// Decode nBits with Bitcoin/BCH's sign, zero and overflow checks. The node
/// computes ASERT; callers must not substitute Bitcoin's difficulty epochs.
pub fn compact_target(bits: u32) -> Result<Hash, String> {
    let size = bits >> 24;
    let word = bits & 0x007f_ffff;
    if word == 0
        || bits & 0x0080_0000 != 0
        || size > 34
        || (word > 0xff && size > 33)
        || (word > 0xffff && size > 32)
    {
        return Err("invalid compact target".into());
    }
    let value = if size <= 3 {
        num_bigint::BigUint::from(word >> (8 * (3 - size)))
    } else {
        num_bigint::BigUint::from(word) << (8 * (size - 3)) as usize
    };
    let bytes = value.to_bytes_le();
    if bytes.len() > 32 || bytes.iter().all(|&b| b == 0) {
        return Err("invalid compact target".into());
    }
    let mut result = [0; 32];
    result[..bytes.len()].copy_from_slice(&bytes);
    Ok(result)
}

pub fn meets_target(hash: &Hash, target: &Hash) -> bool {
    hash.iter().rev().cmp(target.iter().rev()).is_le()
}

/// #### PR #42: the merkle branch of the coinbase (index 0) over the
/// block's other transactions: one sibling per level.
pub fn coinbase_path(txids: &[Hash]) -> Vec<Hash> {
    let mut hashes = vec![[0; 32]];
    hashes.extend_from_slice(txids);
    let mut path = Vec::new();
    while hashes.len() > 1 {
        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }
        path.push(hashes[1]);
        hashes = hashes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let mut bytes = [0; 64];
                bytes[..32].copy_from_slice(&pair[0]);
                bytes[32..].copy_from_slice(&pair[1]);
                double_sha256(&bytes)
            })
            .collect();
    }
    path
}

/// #### PR #42: folds the coinbase's hash up its branch at index 0 to the
/// merkle root.
pub fn fold(leaf: Hash, path: &[Hash]) -> Hash {
    path.iter().fold(leaf, |current, sibling| {
        let mut joined = [0; 64];
        joined[..32].copy_from_slice(&current);
        joined[32..].copy_from_slice(sibling);
        double_sha256(&joined)
    })
}

/// The whole tree's root, the reference the branch is checked against.
#[cfg(test)]
pub(super) fn merkle_root(mut hashes: Vec<Hash>) -> Hash {
    while hashes.len() > 1 {
        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }
        hashes = hashes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let mut joined = [0; 64];
                joined[..32].copy_from_slice(&pair[0]);
                joined[32..].copy_from_slice(&pair[1]);
                double_sha256(&joined)
            })
            .collect();
    }
    hashes[0]
}

fn display_hash(value: &str) -> Result<Hash, String> {
    let mut bytes: Hash = hex::decode(value)
        .map_err(|_| "invalid hash hex")?
        .try_into()
        .map_err(|_| "hash must have 32 bytes")?;
    bytes.reverse();
    Ok(bytes)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("template omitted {key}"))
}
fn number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("invalid template {key}"))
}
fn word(value: &Value, key: &str) -> Result<u32, String> {
    u32::try_from(number(value, key)?).map_err(|_| format!("template {key} exceeds u32"))
}
fn compact_size(value: usize, out: &mut Vec<u8>) {
    match value {
        0..=252 => out.push(value as u8),
        253..=65535 => {
            out.push(253);
            out.extend_from_slice(&(value as u16).to_le_bytes());
        }
        65536..=4294967295 => {
            out.push(254);
            out.extend_from_slice(&(value as u32).to_le_bytes());
        }
        _ => {
            out.push(255);
            out.extend_from_slice(&(value as u64).to_le_bytes());
        }
    }
}
/// The minimal BIP34 push of `height`.
pub fn height_script(height: u32) -> Vec<u8> {
    if height == 0 {
        return vec![0];
    }
    if height <= 16 {
        return vec![0x50 + height as u8];
    }
    let mut bytes = height.to_le_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.last().unwrap() & 0x80 != 0 {
        bytes.push(0);
    }
    let mut script = vec![bytes.len() as u8];
    script.extend(bytes);
    script
}
