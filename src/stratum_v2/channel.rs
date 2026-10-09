//! #### PR #38
//! BCH work ownership and share checking, independent of socket/firmware.
//! Version rolling is restricted to BIP320's general-purpose bits. Devices
//! receive disjoint coinbases and cannot alter payouts or template tx order.

use super::merge::{
    proof::{assemble, AuxProof, ProofError},
    set::AuxJob,
};
use super::telemetry::expected_hashes;
use super::template::{
    double_sha256, fold, meets_target, BchTemplate, Coinbase, CoinbaseParts, Hash,
};
use crate::config::MiningNetwork;
use crate::donation::bch::BchPayout;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use stratum_core::{
    bitcoin::Target,
    channels_sv2::{
        target::{hash_rate_from_target, hash_rate_to_target},
        Vardiff, VardiffState,
    },
};

pub const VERSION_ROLLING_MASK: u32 = 0x1fff_e000;
const MAX_SHARES_PER_JOB: usize = 16_384;
pub const DEVICE_EXTRANONCE_SIZE: usize = 8;
pub const MAX_ACTIVE_JOBS: usize = 8;
// #### PR #38
// Vardiff (SRI's reference rules) aims every device at about 20 shares a
// minute, one every three seconds, from a 1 TH/s miner to a 1 PH/s one:
// steady rate estimates for little LAN traffic. The floor keeps a silent or
// misreported device from asking for a near-zero difficulty.
pub const SHARES_PER_MINUTE: f32 = 20.0;
const MIN_HASHRATE: f32 = 1.0e6;
// #### PR #40
// Calmer than SRI's rules alone, as ckpool and P2Poolv2 are: decide only on
// enough evidence (72 shares, four minutes, or a silent minute), and ignore
// changes under a quarter, which are share luck rather than a different device.
const VARDIFF_SHARES: u32 = 72;
const VARDIFF_SECONDS: u64 = 240;
const VARDIFF_SILENT_SECONDS: u64 = 60;
const VARDIFF_HYSTERESIS: f64 = 1.25;
/// #### PR #42: the share-target floor for merge-mined tokens is at most
/// this many times easier than vardiff's target: about 5 shares a second at
/// vardiff's 20 a minute.
pub const TOKEN_FLOOR_CAP: u8 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Standard,
    Extended,
}

pub struct Channel {
    pub id: u32,
    pub kind: ChannelKind,
    pub target: Hash,
    /// The session salt and the channel id (16 bytes), or under Job
    /// Declaration the channel's lane (4 bytes, #### PR #42).
    pub extranonce_prefix: Vec<u8>,
    network: MiningNetwork,
    payout: String,
    /// #### PR #40: a public pool operator's fee address.
    operator: Option<String>,
    job: Option<Job>,
    previous: VecDeque<Job>,
    sequence: Option<u32>,
    seen: HashMap<u32, HashSet<Hash>>,
    pub accepted: u64,
    pub rejected: u64,
    /// The share target vardiff wants. `target` is what the device was last
    /// told: this one, made easier while the block target is easier, and
    /// never easier than the device's maximum.
    desired: Hash,
    /// #### PR #42: the target vardiff counts shares at and estimates from:
    /// `target` without the merge-mined token floor (the same while no
    /// token is merge-mined).
    vardiff_target: Hash,
    vardiff: VardiffState,
    /// Vardiff's latest hash rate estimate, the baseline for its next check.
    hashrate: f32,
    /// #### PR #42: a Job Declaration client's channel: it mines only the
    /// custom jobs the client sets, with 16 rollable extranonce bytes.
    pub custom_only: bool,
}

/// #### PR #42: whose job a channel mines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// The pool's own, from its template.
    Own,
    /// A Job Declaration client's, set with SetCustomMiningJob; shares may
    /// not be older than its start time.
    Custom { min_ntime: u32 },
}

#[derive(Clone)]
pub struct Job {
    pub id: u32,
    pub generation: u64,
    pub template: Arc<BchTemplate>,
    pub standard_coinbase: Coinbase,
    pub parts: CoinbaseParts,
    pub target: Hash,
    pub payout: BchPayout,
    /// #### PR #42: this job's merge-mining (its leaves bind this job's
    /// payout), or `None` while no token is merge-mined.
    pub aux: Option<Arc<AuxJob>>,
    /// #### PR #42: the pool's job or a client's custom job.
    pub kind: JobKind,
}

pub struct Share<'a> {
    pub channel_id: u32,
    pub job_id: u32,
    pub sequence: u32,
    pub version: u32,
    pub time: u32,
    pub nonce: u32,
    pub extranonce: &'a [u8],
}

pub struct ValidatedShare {
    pub generation: u64,
    pub template: Arc<BchTemplate>,
    pub coinbase: Coinbase,
    pub header: [u8; 80],
    pub block: bool,
    pub share_target: Hash,
    pub payout: BchPayout,
    /// #### PR #40: who the block pays, which a public pool's journal records.
    pub miner: String,
    pub operator: Option<String>,
    /// #### PR #42: the header's hash, computed once by the check.
    pub hash: Hash,
    /// #### PR #42: the job's merge-mining, the indices of the entries this
    /// share wins (Case A entries whose target the hash meets, Case B
    /// entries when it is a block), and the coinbase's merkle branch, which
    /// is filled only when it wins one.
    pub aux: Option<Arc<AuxJob>>,
    pub token_wins: Vec<u16>,
    pub merkle_path: Vec<Hash>,
    /// #### PR #42: the share as the pool takes it, on a Job Declaration
    /// job.
    pub forward: Option<super::jd::ForwardShare>,
}

impl ValidatedShare {
    /// #### PR #42
    /// What a share that wins merge-mined tokens hands on, or `None` for a
    /// share that wins none (the usual case, which costs nothing).
    pub fn token_win(&self) -> Option<TokenWin> {
        if self.token_wins.is_empty() {
            return None;
        }
        Some(TokenWin {
            aux: self.aux.clone()?,
            entries: self.token_wins.clone(),
            coinbase: self.coinbase.bytes.clone(),
            merkle_path: self.merkle_path.clone(),
            header: self.header,
            hash: self.hash,
            block: self.block,
            height: self.template.height,
            payout: self.payout,
            miner: self.miner.clone(),
            operator: self.operator.clone(),
        })
    }
}

/// #### PR #42
/// A share that wins merge-mined tokens, with everything their proofs need.
/// It is built only for a win, and the server hands it on without waiting
/// for disk or network. No `Debug`: it names the miner's payout address.
pub struct TokenWin {
    /// The job's merge-mining: its entries, tree and commitment.
    pub aux: Arc<AuxJob>,
    /// Indices into `aux.entries` of the entries won.
    pub entries: Vec<u16>,
    pub coinbase: Vec<u8>,
    /// The coinbase's merkle branch (the coinbase is at index 0).
    pub merkle_path: Vec<Hash>,
    pub header: [u8; 80],
    pub hash: Hash,
    /// Also a BCH block: its Case B tickets mature 100 blocks after
    /// `height`.
    pub block: bool,
    pub height: u32,
    /// Whose work it was, as a block records it: the policy and addresses
    /// that give the payout and split the leaves bind.
    pub payout: BchPayout,
    pub miner: String,
    pub operator: Option<String>,
}

impl TokenWin {
    /// The `AuxProof` of entry `entry` (an index into `aux.entries`).
    pub fn proof(&self, entry: u16) -> Result<AuxProof, ProofError> {
        assemble(
            &self.aux,
            entry,
            &self.coinbase,
            &self.merkle_path,
            Some(self.header),
        )
    }
}

impl Channel {
    pub fn new(
        id: u32,
        kind: ChannelKind,
        target: Hash,
        session_salt: [u8; 12],
        network: MiningNetwork,
        payout: &str,
    ) -> Result<Self, String> {
        if target == [0; 32] {
            return Err("max-target-out-of-range".into());
        }
        let payout = crate::config::validate_coinbase_address(network, payout)
            .map_err(|_| "invalid payout for selected network")?;
        let mut extranonce_prefix = session_salt.to_vec();
        extranonce_prefix.extend_from_slice(&id.to_le_bytes());
        Ok(Self {
            id,
            kind,
            target,
            extranonce_prefix,
            network,
            payout,
            operator: None,
            job: None,
            previous: VecDeque::new(),
            sequence: None,
            seen: HashMap::new(),
            accepted: 0,
            rejected: 0,
            desired: target,
            vardiff_target: target,
            vardiff: VardiffState::new_with_min(MIN_HASHRATE)
                .map_err(|_| "invalid system clock")?,
            hashrate: hash_rate_from_target(target.into(), f64::from(SHARES_PER_MINUTE))
                .map_or(MIN_HASHRATE, |rate| rate as f32),
            custom_only: false,
        })
    }

    /// #### PR #42: a Job Declaration client's local channel: its
    /// extranonce prefix is its lane, unique across the server, so that the
    /// job id, the lane and the device's 8 bytes fit the 16 the pool lets
    /// the client roll.
    pub fn set_lane(&mut self, lane: u32) {
        self.extranonce_prefix = lane.to_le_bytes().to_vec();
    }

    /// #### PR #42: the rollable extranonce bytes this channel's shares
    /// carry: 8 from a device, 16 from a Job Declaration client.
    pub fn rollable(&self) -> usize {
        if self.custom_only {
            super::jd::JD_ROLLABLE
        } else {
            DEVICE_EXTRANONCE_SIZE
        }
    }

    pub fn install(
        &mut self,
        id: u32,
        generation: u64,
        template: Arc<BchTemplate>,
    ) -> Result<&Job, String> {
        self.install_with_payout(id, generation, template, BchPayout::default())
    }

    pub fn install_with_payout(
        &mut self,
        id: u32,
        generation: u64,
        template: Arc<BchTemplate>,
        payout: BchPayout,
    ) -> Result<&Job, String> {
        if self.job.as_ref().is_some_and(|job| job.id == id)
            || self.previous.iter().any(|job| job.id == id)
        {
            return Err("job identifier already in use".into());
        }
        // #### PR #38
        // Time rolling overlaps across same-tip refreshes. Commit the job ID
        // before the channel/device extranonce so restarting the firmware's
        // nonce search cannot repeat headers from a retained job. The device
        // cannot overwrite this prefix; its extranonce size stays unchanged.
        // #### PR #42: the nested extranonce layout
        // What: under Job Declaration the script carries the pool channel's
        // prefix, then the job id, the plan's pad, this channel's lane and
        // the device's 8 bytes; the job id onwards is what the pool lets
        // the client roll. A standard channel's device bytes are zeros. The
        // standalone layout (job id, salt, channel id, device) is unchanged.
        // Why: the pool rebuilds the coinbase from its own prefix and the
        // client's rolled bytes, so the local server's coinbases must have
        // exactly that shape.
        // Look here if: the pool refuses forwarded shares, or the local
        // devices' coinbases differ from the custom job's.
        let (upstream, pad) = template.jd_plan().map_or((&[][..], &[][..]), |plan| {
            (plan.upstream_prefix.as_slice(), plan.pad.as_slice())
        });
        let mut extra = upstream.to_vec();
        extra.extend(id.to_le_bytes());
        extra.extend_from_slice(pad);
        extra.extend_from_slice(&self.extranonce_prefix);
        let parts_extranonce = extra.len() + DEVICE_EXTRANONCE_SIZE;
        if template.jd_plan().is_some() && self.kind == ChannelKind::Standard {
            extra.extend([0; DEVICE_EXTRANONCE_SIZE]);
        }
        // #### end PR #42 ####
        // #### PR #42: merge-mined tokens in each job
        // What: a job built from a template with a token set gets its own
        // merge-mining (`aux`): leaves that bind this job's beneficiary (the
        // miner, or the donation or the pool operator in their work jobs)
        // and the donation's split, their tree, and the commitment (output
        // 0) and tickets its coinbases carry. The standard coinbase, the
        // extended parts and the extended per-share rebuild in
        // `check_inner` all use them.
        // Why: a token win must pay whoever the job works for, so leaves are
        // built per job. With no token merge-mined (the default on both
        // networks) `aux` is `None` and every coinbase is byte for byte what
        // it was.
        // Look here if: a token job's coinbase lacks output 0 or a ticket,
        // its leaves name another payout, or a share's rebuilt coinbase does
        // not reach the merkle root the device hashed.
        let aux = template
            .aux_job(self.network, &self.payout, self.operator.as_deref(), payout)?
            .map(Arc::new);
        let outputs = aux.as_deref().map(|aux| &aux.outputs);
        let standard_coinbase = template.coinbase_with_aux(
            self.network,
            &self.payout,
            self.operator.as_deref(),
            &extra,
            payout,
            outputs,
        )?;
        let mut parts = template.coinbase_parts_with_aux(
            self.network,
            &self.payout,
            self.operator.as_deref(),
            parts_extranonce,
            payout,
            outputs,
        )?;
        // #### end PR #42 ####
        parts.prefix.extend_from_slice(upstream);
        parts.prefix.extend(id.to_le_bytes());
        parts.prefix.extend_from_slice(pad);
        self.retain_previous(&template);
        self.job = Some(Job {
            id,
            generation,
            template,
            standard_coinbase,
            parts,
            target: self.target,
            payout,
            aux,
            kind: JobKind::Own,
        });
        Ok(self.job.as_ref().unwrap())
    }

    /// #### PR #38
    /// A mempool/time refresh on the same parent does not invalidate work
    /// already in an ASIC pipeline. Retain exact coinbases and tx lists;
    /// a new parent or changed bits still revokes every preceding job.
    fn retain_previous(&mut self, template: &BchTemplate) {
        if let Some(previous) = self.job.take() {
            if previous.template.previous_hash == template.previous_hash
                && previous.template.bits == template.bits
            {
                self.previous.push_back(previous);
                while self.previous.len() >= MAX_ACTIVE_JOBS {
                    if let Some(expired) = self.previous.pop_front() {
                        self.seen.remove(&expired.id);
                    }
                }
            } else {
                self.previous.clear();
                self.seen.clear();
            }
        }
    }

    /// #### PR #42
    /// Installs a Job Declaration client's custom job: `template` is the
    /// header-only template of `BchTemplate::custom`, and `parts` the
    /// client's coinbase around this channel's extranonce prefix and its 16
    /// rollable bytes. Older jobs on the same parent stay valid, as for the
    /// pool's own jobs.
    pub fn install_custom(
        &mut self,
        id: u32,
        template: Arc<BchTemplate>,
        parts: CoinbaseParts,
    ) -> Result<&Job, String> {
        if !self.custom_only {
            return Err("only a Job Declaration channel takes custom jobs".into());
        }
        if self.job.as_ref().is_some_and(|job| job.id == id)
            || self.previous.iter().any(|job| job.id == id)
        {
            return Err("job identifier already in use".into());
        }
        // The coinbase with zeroed rollable bytes, the job's reference build.
        let mut bytes = parts.prefix.clone();
        bytes.extend_from_slice(&self.extranonce_prefix);
        bytes.extend(std::iter::repeat_n(0, self.rollable()));
        bytes.extend_from_slice(&parts.suffix);
        let merkle_root = fold(double_sha256(&bytes), &parts.merkle_path);
        let min_ntime = template.min_time;
        self.retain_previous(&template);
        self.job = Some(Job {
            id,
            generation: 0,
            template,
            standard_coinbase: Coinbase { bytes, merkle_root },
            parts,
            target: self.target,
            payout: BchPayout::default(),
            aux: None,
            kind: JobKind::Custom { min_ntime },
        });
        Ok(self.job.as_ref().unwrap())
    }

    pub fn revoke(&mut self) {
        self.job = None;
        self.previous.clear();
        self.seen.clear();
    }
    pub fn job(&self) -> Option<&Job> {
        self.job.as_ref()
    }

    /// #### PR #38
    /// Recomputes the target the device mines at: the desired one, made
    /// easier while the block target is easier (a share that wins a block
    /// must reach the server), never easier than the device's maximum. On
    /// Chipnet's difficulty-1 windows this follows the block target down and,
    /// on the next normal template, back up; before, the device stayed on
    /// the easy target and flooded shares until it reconnected. Returns
    /// whether the target changed. Vardiff counts afresh when its own target
    /// changes (#### PR #42: `tokens`, the easiest Case A token target, can
    /// make the device's target easier still; see below).
    pub fn settle(&mut self, block: &Hash, tokens: Option<&Hash>, maximum: &Hash) -> bool {
        let mut target = if meets_target(&self.desired, block) {
            *block
        } else {
            self.desired
        };
        if !meets_target(&target, maximum) {
            target = *maximum;
        }
        // #### PR #42: the share-target floor for merge-mined tokens
        // What: while Case A tokens are merge-mined, the device's target is
        // made easier, up to the easiest token target but never more than
        // TOKEN_FLOOR_CAP (15) times easier than vardiff's target, and never
        // easier than the device's maximum. Vardiff keeps its own target:
        // it counts only shares that meet it (`check_inner`) and estimates
        // from it (`retarget`), so the floor never drags vardiff harder.
        // Why: firmware sends only hashes that meet its target, so a target
        // harder than a token's would hold back every win between the two;
        // with several miners racing for one baton, those wins are lost.
        // The cap bounds the extra shares (vardiff aims at 20 a minute). A
        // token target easier than the cap or the device's maximum is mined
        // best effort: only hashes that meet the capped target reach the
        // server. Without tokens nothing changes.
        // Look here if: devices get a SetTarget easier than vardiff's when
        // tokens change, share rates jump towards several a second, or
        // vardiff drifts while tokens are merge-mined.
        if target != self.vardiff_target {
            self.vardiff_target = target;
            let _ = self.vardiff.reset_counter();
        }
        if let Some(floor) = tokens.map(|easiest| token_floor(&target, easiest)) {
            if !meets_target(&floor, &target) {
                target = if meets_target(&floor, maximum) {
                    floor
                } else {
                    *maximum
                };
            }
        }
        // #### end PR #42 ####
        if target == self.target {
            return false;
        }
        self.target = target;
        true
    }

    /// #### PR #38
    /// Vardiff: once SRI's reference rules see the share rate drift from the
    /// aim, the desired target follows the measured hash rate. Returns the
    /// new target when the device must be told.
    pub fn retarget(&mut self, maximum: &Hash) -> Option<Hash> {
        // #### PR #42: the share-target floor for merge-mined tokens (see
        // `settle`): a vardiff retarget keeps the floor at the current
        // template's easiest Case A token target.
        let (block, tokens) = {
            let template = &self.job.as_ref()?.template;
            let tokens = template.tokens().and_then(|set| set.easiest_a()).copied();
            (template.target, tokens)
        };
        let shares = self.vardiff.shares_since_last_update;
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |now| now.as_secs())
            .saturating_sub(self.vardiff.timestamp_of_last_update);
        if shares < VARDIFF_SHARES
            && elapsed < VARDIFF_SECONDS
            && !(shares == 0 && elapsed >= VARDIFF_SILENT_SECONDS)
        {
            return None;
        }
        let hashrate = self
            .vardiff
            .try_vardiff(
                self.hashrate,
                // #### PR #42: the shares counted are those that meet
                // vardiff's own target, not the token floor's.
                &Target::from_le_bytes(self.vardiff_target),
                SHARES_PER_MINUTE,
            )
            .ok()
            .flatten()?;
        let desired = hash_rate_to_target(f64::from(hashrate), f64::from(SHARES_PER_MINUTE))
            .ok()?
            .to_le_bytes();
        // The floor only stops the target from easing further. A device
        // below it (or started easier than it) never gets a harder target
        // from the floor itself.
        if hashrate <= MIN_HASHRATE {
            if meets_target(&desired, &self.desired) {
                return None;
            }
        } else {
            // Small changes are share luck; at the floor, easing always applies.
            let change = expected_hashes(&desired) / expected_hashes(&self.desired);
            if (1.0 / VARDIFF_HYSTERESIS..=VARDIFF_HYSTERESIS).contains(&change) {
                return None;
            }
        }
        self.hashrate = hashrate;
        self.desired = desired;
        self.settle(&block, tokens.as_ref(), maximum)
            .then_some(self.target)
    }

    /// Test hook: pretend `seconds` passed with `shares` counted.
    #[cfg(test)]
    pub fn vardiff_window(&mut self, seconds: u64, shares: u32) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.vardiff.timestamp_of_last_update = now - seconds;
        self.vardiff.set_shares_since_last_update(shares);
    }

    pub fn check(&mut self, share: Share<'_>, now: u32) -> Result<ValidatedShare, &'static str> {
        let result = self.check_inner(share, now);
        if result.is_ok() {
            self.accepted = self.accepted.saturating_add(1);
        } else {
            self.rejected = self.rejected.saturating_add(1);
        }
        result
    }

    fn check_inner(&mut self, share: Share<'_>, now: u32) -> Result<ValidatedShare, &'static str> {
        if share.channel_id != self.id {
            return Err("invalid-channel-id");
        }
        let current = self.job.as_ref().ok_or("stale-share")?;
        let job = if share.job_id == current.id {
            current
        } else {
            self.previous
                .iter()
                .find(|job| job.id == share.job_id)
                .ok_or("invalid-job-id")?
        };
        if self.sequence.is_some_and(|last| {
            let delta = share.sequence.wrapping_sub(last);
            delta == 0 || delta >= 1 << 31
        }) {
            return Err("invalid-sequence-number");
        }
        self.sequence = Some(share.sequence);
        if (share.version ^ job.template.version) & !VERSION_ROLLING_MASK != 0 {
            return Err("invalid-version");
        }
        let late = match job.kind {
            JobKind::Own => {
                share.time > now.saturating_add(60)
                    || share.time > job.template.current_time.saturating_add(60)
            }
            // #### PR #42: a custom job's shares follow the client's start
            // time, up to a minute past it or past now.
            JobKind::Custom { min_ntime } => share.time > now.max(min_ntime).saturating_add(60),
        };
        if share.time < job.template.current_time || late {
            return Err("invalid-ntime");
        }
        let coinbase = match self.kind {
            ChannelKind::Standard => {
                if !share.extranonce.is_empty() {
                    return Err("invalid-extranonce-size");
                }
                job.standard_coinbase.clone()
            }
            ChannelKind::Extended => {
                if share.extranonce.len() != self.rollable() {
                    return Err("invalid-extranonce-size");
                }
                // #### PR #42: extended shares rebuild from the job's parts
                // What: the coinbase is the job's prefix (ending in the job
                // id), this channel's extranonce prefix, the device's
                // extranonce and the job's suffix (with any merge-mining
                // outputs), hashed once and folded up the job's merkle path.
                // Why: these are exactly the bytes the device hashed;
                // rebuilding the payouts and the whole merkle tree for every
                // share cost a CashAddr decode and n hashes.
                // Look here if: an extended share gets invalid-coinbase, or a
                // block's merkle root mismatches.
                let mut bytes = Vec::with_capacity(
                    job.parts.prefix.len()
                        + self.extranonce_prefix.len()
                        + share.extranonce.len()
                        + job.parts.suffix.len(),
                );
                bytes.extend_from_slice(&job.parts.prefix);
                bytes.extend_from_slice(&self.extranonce_prefix);
                bytes.extend_from_slice(share.extranonce);
                bytes.extend_from_slice(&job.parts.suffix);
                let merkle_root = fold(double_sha256(&bytes), &job.parts.merkle_path);
                Coinbase { bytes, merkle_root }
            }
        };
        let header = job
            .template
            .header(&coinbase, share.version, share.time, share.nonce)
            .map_err(|_| "invalid-ntime")?;
        let hash = double_sha256(&header);
        let block = meets_target(&hash, &job.template.target);
        // #### PR #38
        // A job keeps the target it was issued with. After vardiff lowers the
        // difficulty, firmware may apply the new one to work it already holds
        // (cgminer does for new work on the current job), so an older job also
        // accepts the current target when that is easier. Each share is
        // credited at the target it was accepted under.
        let accepted = if meets_target(&self.target, &job.target) {
            job.target
        } else {
            self.target
        };
        if !meets_target(&hash, &accepted) && !block {
            return Err("difficulty-too-low");
        }
        if self.seen.values().any(|seen| seen.contains(&hash)) {
            return Err("duplicate-share");
        }
        let seen = self.seen.entry(job.id).or_default();
        if seen.len() >= MAX_SHARES_PER_JOB {
            return Err("job-share-limit");
        }
        seen.insert(hash);
        // #### PR #42: token wins on the share path
        // What: after the duplicate check and the per-job cap, one compare
        // with the job's easiest Case A target (or a found block) decides
        // whether to look at its entries: Case A entries win when the hash
        // meets their target, Case B entries when the share is a block. The
        // coinbase's merkle branch is copied only for a win.
        // Why: a duplicate must never yield a second proof. A share over the
        // per-job cap is refused, so it yields no proof. Shares that win no
        // token (all of them while none is merge-mined) cost one compare at
        // most.
        // Look here if: a share that meets a token's target yields no win,
        // or one share yields two proofs of the same entry.
        let token_wins = match job.aux.as_deref() {
            Some(aux)
                if block
                    || aux
                        .easiest_a
                        .as_ref()
                        .is_some_and(|easiest| meets_target(&hash, easiest)) =>
            {
                aux.wins(&hash, block)
            }
            _ => Vec::new(),
        };
        let merkle_path = if token_wins.is_empty() {
            Vec::new()
        } else {
            job.parts.merkle_path.clone()
        };
        // #### end PR #42 ####
        // #### PR #42: shares forwarded to the pool (Job Declaration): the
        // rolled bytes start at the job id, at the end of the job's prefix.
        let forward = job.template.jd_plan().map(|plan| {
            let start = job.parts.prefix.len() - plan.pad.len() - 4;
            super::jd::ForwardShare {
                serial: job.generation,
                version: share.version,
                ntime: share.time,
                nonce: share.nonce,
                extranonce: coinbase.bytes[start..start + plan.rollable()].to_vec(),
                hash,
            }
        });
        // Vardiff counts shares at the current target only, so work still
        // arriving at an older, easier target cannot inflate its estimate.
        // #### PR #42: the share-target floor (see `settle`): only shares
        // that meet vardiff's own target count, never those that only the
        // token floor accepts.
        if meets_target(&hash, &self.vardiff_target) {
            self.vardiff.increment_shares_since_last_update();
        }
        Ok(ValidatedShare {
            generation: job.generation,
            template: job.template.clone(),
            coinbase,
            header,
            block,
            share_target: accepted,
            payout: job.payout,
            miner: self.payout.clone(),
            operator: self.operator.clone(),
            hash,
            aux: job.aux.clone(),
            token_wins,
            merkle_path,
            forward,
        })
    }

    /// #### PR #40
    /// A public pool's operator fee address, set before the first job.
    pub fn set_operator(&mut self, operator: Option<&str>) -> Result<(), String> {
        self.operator = operator
            .map(|operator| crate::config::validate_coinbase_address(self.network, operator))
            .transpose()
            .map_err(|_| "invalid pool fee address for selected network")?;
        Ok(())
    }

    /// The address this channel's blocks pay.
    pub fn payout(&self) -> &str {
        &self.payout
    }
}

/// #### PR #42
/// The share-target floor for a device whose vardiff target is `vardiff`:
/// the easiest Case A token target, but at most `TOKEN_FLOOR_CAP` times
/// easier than `vardiff`.
fn token_floor(vardiff: &Hash, easiest: &Hash) -> Hash {
    let cap = times(vardiff, TOKEN_FLOOR_CAP);
    if meets_target(easiest, &cap) {
        *easiest
    } else {
        cap
    }
}

/// `target × factor` (little-endian), saturating at the easiest target.
fn times(target: &Hash, factor: u8) -> Hash {
    let mut product = [0; 32];
    let mut carry = 0u16;
    for (out, byte) in product.iter_mut().zip(target) {
        let value = u16::from(*byte) * u16::from(factor) + carry;
        *out = value.to_le_bytes()[0];
        carry = value >> 8;
    }
    if carry == 0 {
        product
    } else {
        [0xff; 32]
    }
}

/// Report difficulty-one work units in SubmitSharesSuccess, not merely the
/// number of accepted headers. Dashboard rate uses the same target accounting.
pub fn share_work(target: &Hash) -> u64 {
    let numerator = num_bigint::BigUint::from(1u8) << 256usize;
    let denominator = num_bigint::BigUint::from_bytes_le(target) + num_bigint::BigUint::from(1u8);
    let hashes = numerator / denominator;
    let words = (hashes >> 32usize).to_u64_digits();
    if words.len() > 1 {
        u64::MAX
    } else {
        words.first().copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::merge::{
        leaf::Mode,
        registry::TEST_TOKEN,
        set::{SetEntry, TokenSet, TokenState},
        verify::{verify_a, verify_b, ClaimView},
        OutPoint,
    };
    use super::super::template::compact_target;
    use super::super::template_tests::{payout, rpc_template, transaction};
    use super::*;
    use stratum_core::bitcoin::{consensus, Block, Transaction};
    fn channel(id: u32, kind: ChannelKind) -> Channel {
        channel_on(
            id,
            kind,
            Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
        )
    }
    /// #### PR #42: the same on any template, one with tokens too.
    fn channel_on(id: u32, kind: ChannelKind, template: Arc<BchTemplate>) -> Channel {
        let mut channel = Channel::new(
            id,
            kind,
            [255; 32],
            [9; 12],
            MiningNetwork::Chipnet,
            &payout(),
        )
        .unwrap();
        channel.install(1, 3, template).unwrap();
        channel
    }
    fn share(sequence: u32) -> Share<'static> {
        Share {
            channel_id: 1,
            job_id: 1,
            sequence,
            version: 0x20000000,
            time: 1700000010,
            nonce: 0,
            extranonce: &[],
        }
    }
    #[test]
    fn retained_jobs_keep_exact_payouts_across_rate_and_work_rotations() {
        use crate::donation::bch::BchDonation;
        // #### PR #42: merge-mined tokens in each job
        // With tokens, each retained job also keeps its own commitment (worth
        // 0, leading) and ticket (worth 0, trailing): a payout rotation
        // changes what the leaves bind.
        let jobs = [false, true].into_iter().flat_map(|tokens| {
            [ChannelKind::Standard, ChannelKind::Extended].map(|kind| (tokens, kind))
        });
        for (tokens, kind) in jobs {
            let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
            if tokens {
                template.commit(token_set(
                    &[Mode::ShareTarget, Mode::BlockRequired],
                    TOKEN_BITS,
                ));
            }
            let mut channel = channel_on(1, kind, Arc::new(template));
            let template = channel.job().unwrap().template.clone();
            let mut roots = Vec::new();
            let plans = [
                BchPayout::default(),
                BchPayout {
                    donation: "2".parse().unwrap(),
                    donation_work: false,
                    ..BchPayout::default()
                },
                BchPayout {
                    donation: BchDonation::default(),
                    donation_work: true,
                    ..BchPayout::default()
                },
            ];
            for (index, payout) in plans.iter().enumerate().skip(1) {
                channel
                    .install_with_payout(index as u32 + 1, 3, template.clone(), *payout)
                    .unwrap();
            }
            for (index, payout) in plans.iter().enumerate() {
                let mut submission = share(index as u32);
                submission.job_id = index as u32 + 1;
                if kind == ChannelKind::Extended {
                    submission.extranonce = &[7; 8];
                }
                let result = channel.check(submission, template.current_time).unwrap();
                assert_eq!(result.payout, *payout);
                let tx: stratum_core::bitcoin::Transaction =
                    consensus::deserialize(&result.coinbase.bytes).unwrap();
                let values = tx
                    .output
                    .iter()
                    .map(|o| o.value.to_sat())
                    .collect::<Vec<_>>();
                let payouts = [
                    vec![309_375_000, 3_125_000],
                    vec![308_333_334, 4_166_666],
                    vec![312_500_000],
                ][index]
                    .clone();
                let expected = if tokens {
                    [vec![0], payouts, vec![0]].concat()
                } else {
                    payouts
                };
                assert_eq!(values, expected);
                assert_eq!(values.iter().sum::<u64>(), template.coinbase_value);
                match &result.aux {
                    Some(aux) => {
                        assert_eq!(
                            tx.output[0].script_pubkey.as_bytes(),
                            aux.commitment.script()
                        );
                        roots.push(aux.commitment.root);
                    }
                    None => assert!(!tokens),
                }
            }
            roots.sort_unstable();
            roots.dedup();
            assert_eq!(roots.len(), if tokens { 3 } else { 0 });
        }
    }
    #[test]
    fn channels_are_disjoint_and_share_header_round_trips() {
        let mut first = channel(1, ChannelKind::Standard);
        let second = channel(2, ChannelKind::Standard);
        assert_ne!(
            first.job().unwrap().standard_coinbase.merkle_root,
            second.job().unwrap().standard_coinbase.merkle_root
        );
        let result = first.check(share(0), 1700000010).unwrap();
        let block = first
            .job()
            .unwrap()
            .template
            .block(&result.coinbase, result.header)
            .unwrap();
        let decoded: Block = consensus::deserialize(&block).unwrap();
        assert!(decoded.check_merkle_root());
        assert!(first.check(share(1), 1700000010).is_err());
        first.revoke();
        assert!(first.check(share(2), 1700000010).is_err());
    }
    #[test]
    fn share_validation_rejects_mutations_and_accepts_only_version_mask() {
        for field in 0..7 {
            let mut channel = channel(1, ChannelKind::Standard);
            let mut input = share(0);
            match field {
                0 => input.channel_id = 2,
                1 => input.job_id = 2,
                2 => input.version ^= 1,
                3 => input.time -= 1,
                4 => input.time += 61,
                5 => input.extranonce = &[1],
                _ => channel.revoke(),
            }
            assert!(channel.check(input, 1700000010).is_err());
        }
        let mut channel = channel(1, ChannelKind::Standard);
        let mut input = share(u32::MAX);
        input.version ^= VERSION_ROLLING_MASK;
        assert!(channel.check(input, 1700000010).is_ok());
        let mut input = share(0);
        input.nonce = 1;
        assert!(channel.check(input, 1700000010).is_ok());
        assert!(channel.check(share(0), 1700000010).is_err());
    }
    #[test]
    fn same_tip_retains_inflight_work_without_resetting_duplicate_protection() {
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut channel = channel(1, kind);
            let mut input = share(0);
            if kind == ChannelKind::Extended {
                input.extranonce = &[0; DEVICE_EXTRANONCE_SIZE];
            }
            let first = channel.check(input, 1700000010).unwrap();
            for id in 2..=3 {
                channel
                    .install(
                        id,
                        u64::from(id) + 2,
                        Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
                    )
                    .unwrap();
            }
            // The same device nonce/time/extranonce is fresh work in a new job.
            // This prevents firmware resets on a refresh from repeating hashes.
            let mut alias = share(1);
            alias.job_id = 3;
            if kind == ChannelKind::Extended {
                alias.extranonce = &[0; DEVICE_EXTRANONCE_SIZE];
            }
            let fresh = channel.check(alias, 1700000010).unwrap();
            assert_ne!(fresh.header, first.header);
            assert_ne!(fresh.coinbase.bytes, first.coinbase.bytes);
            // The actual old header remains a duplicate, even after refresh.
            let mut duplicate = share(2);
            if kind == ChannelKind::Extended {
                duplicate.extranonce = &[0; DEVICE_EXTRANONCE_SIZE];
            }
            assert!(matches!(
                channel.check(duplicate, 1700000010),
                Err("duplicate-share")
            ));
            let mut delayed = share(3);
            delayed.nonce = 1;
            if kind == ChannelKind::Extended {
                delayed.extranonce = &[0; DEVICE_EXTRANONCE_SIZE];
            }
            let retained = channel.check(delayed, 1700000010).unwrap();
            assert_eq!(retained.generation, first.generation);
            let decoded: Block = consensus::deserialize(
                &retained
                    .template
                    .block(&retained.coinbase, retained.header)
                    .unwrap(),
            )
            .unwrap();
            assert!(decoded.check_merkle_root());
            let mut changed = rpc_template();
            changed["previousblockhash"] = serde_json::json!("cd".repeat(32));
            channel
                .install(4, 6, Arc::new(BchTemplate::from_rpc(&changed).unwrap()))
                .unwrap();
            assert!(matches!(
                channel.check(share(4), 1700000010),
                Err("invalid-job-id")
            ));
            assert!(channel.previous.is_empty());
        }
    }

    #[test]
    fn job_history_is_bounded_and_revoke_clears_it() {
        let mut channel = channel(1, ChannelKind::Standard);
        for id in 2..=MAX_ACTIVE_JOBS as u32 + 2 {
            channel
                .install(
                    id,
                    u64::from(id),
                    Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
                )
                .unwrap();
        }
        assert_eq!(channel.previous.len(), MAX_ACTIVE_JOBS - 1);
        assert!(matches!(
            channel.check(share(0), 1700000010),
            Err("invalid-job-id")
        ));
        let mut recent = share(1);
        recent.job_id = MAX_ACTIVE_JOBS as u32 + 1;
        assert!(channel.check(recent, 1700000010).is_ok());
        channel.revoke();
        assert!(channel.previous.is_empty());
        assert!(channel.seen.is_empty());
    }

    #[test]
    fn older_jobs_keep_an_easier_difficulty_and_accept_a_lowered_one() {
        let mut channel = channel(1, ChannelKind::Standard);
        let non_block_nonce = |job: &Job| {
            (0..1000)
                .find(|nonce| {
                    let header = job
                        .template
                        .header(
                            &job.standard_coinbase,
                            job.template.version,
                            job.template.current_time,
                            *nonce,
                        )
                        .unwrap();
                    !meets_target(&double_sha256(&header), &job.template.target)
                })
                .unwrap()
        };
        let old_nonce = non_block_nonce(channel.job().unwrap());
        channel.target = [1; 32];
        channel
            .install(
                2,
                4,
                Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
            )
            .unwrap();
        let strict_nonce = non_block_nonce(channel.job().unwrap());
        let mut retained = share(0);
        retained.nonce = old_nonce;
        let validated = channel.check(retained, 1700000010).unwrap();
        assert!(!validated.block);
        assert_eq!(validated.share_target, [255; 32]);
        channel.target = [255; 32];
        channel
            .install(
                3,
                5,
                Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
            )
            .unwrap();
        // After the difficulty is lowered, firmware may already apply it to
        // the older job's work: accepted, and credited at the lowered target.
        let mut old_strict = share(1);
        old_strict.job_id = 2;
        old_strict.nonce = strict_nonce;
        let lowered = channel.check(old_strict, 1700000010).unwrap();
        assert!(!lowered.block);
        assert_eq!(lowered.share_target, [255; 32]);
    }

    fn hard_template() -> Arc<BchTemplate> {
        let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        // Mainnet-like block difficulty, so test shares are never blocks.
        template.target = compact_target(0x1a00ffff).unwrap();
        Arc::new(template)
    }

    fn device_channel(target: Hash) -> Channel {
        let mut channel = Channel::new(
            1,
            ChannelKind::Extended,
            target,
            [9; 12],
            MiningNetwork::Chipnet,
            &payout(),
        )
        .unwrap();
        channel.install(1, 3, hard_template()).unwrap();
        channel
    }

    /// Runs vardiff for `minutes` against a device of `hashrate`, in ten
    /// second steps, feeding the share count it would produce at each
    /// moment's target.
    fn simulate(channel: &mut Channel, hashrate: f64, minutes: u64) {
        let mut elapsed = 0;
        for _ in 0..minutes * 6 {
            elapsed += 10;
            let shares = elapsed as f64 * hashrate / expected_hashes(&channel.target);
            channel.vardiff_window(elapsed, shares.round() as u32);
            let window = channel.vardiff.timestamp_of_last_update;
            channel.retarget(&[255; 32]);
            if channel.vardiff.timestamp_of_last_update != window {
                elapsed = 0;
            }
        }
    }

    #[test]
    fn vardiff_settles_every_device_size_near_twenty_shares_a_minute() {
        let start = compact_target(0x1b0ffff0).unwrap();
        for hashrate in [1.0e12, 4.0e12, 9.0e13, 2.0e14, 1.0e15] {
            let mut channel = device_channel(start);
            simulate(&mut channel, hashrate, 10);
            // SRI's rules leave a deviation under 15% alone.
            let per_minute = 60.0 * hashrate / expected_hashes(&channel.target);
            assert!(
                (15.0..=25.0).contains(&per_minute),
                "{hashrate}: {per_minute} shares a minute"
            );
        }
        // A silent device steps down to the floor and stays there.
        let mut silent = device_channel(start);
        simulate(&mut silent, 0.0, 30);
        let floor = hash_rate_to_target(f64::from(MIN_HASHRATE), f64::from(SHARES_PER_MINUTE))
            .unwrap()
            .to_le_bytes();
        assert_eq!(silent.target, floor);
        // A device started easier than the floor, with few or no shares,
        // is never made harder by the floor.
        let mut easy = device_channel([255; 32]);
        for shares in [0, 5] {
            easy.vardiff_window(240, shares);
            assert_eq!(easy.retarget(&[255; 32]), None);
            assert_eq!(easy.target, [255; 32]);
        }
    }

    #[test]
    fn vardiff_waits_for_evidence_and_ignores_share_luck() {
        let rate = 4.0e12;
        let ideal = hash_rate_to_target(rate, f64::from(SHARES_PER_MINUTE))
            .unwrap()
            .to_le_bytes();
        let mut channel = device_channel(ideal);
        let per_share = expected_hashes(&ideal);
        let shares =
            |seconds: f64, factor: f64| (factor * seconds * rate / per_share).round() as u32;
        // A lucky burst, three times the aim for 16 seconds: too little
        // evidence to act on.
        channel.vardiff_window(16, shares(16.0, 3.0));
        assert_eq!(channel.retarget(&[255; 32]), None);
        // Five minutes 15% above the aim: share luck, not a different device.
        channel.vardiff_window(300, shares(300.0, 1.15));
        assert_eq!(channel.retarget(&[255; 32]), None);
        assert_eq!(channel.target, ideal);
        // 60% more hash rate is a real change.
        channel.vardiff_window(300, shares(300.0, 1.6));
        assert!(channel.retarget(&[255; 32]).is_some());
        assert!(meets_target(&channel.target, &ideal));
        assert_ne!(channel.target, ideal);
    }

    #[test]
    fn device_target_follows_an_easier_block_target_and_returns_after() {
        let start = compact_target(0x1b0ffff0).unwrap();
        let mut channel = device_channel(start);
        // Chipnet's difficulty-1 window: follow the block target down...
        let easy = compact_target(0x1d00ffff).unwrap();
        assert!(channel.settle(&easy, None, &[255; 32]));
        assert_eq!(channel.target, easy);
        // ...and return to the share difficulty on the next normal template.
        assert!(channel.settle(&compact_target(0x1a00ffff).unwrap(), None, &[255; 32]));
        assert_eq!(channel.target, start);
        // A device maximum caps how easy the target gets.
        assert!(!channel.settle(&easy, None, &start));
        assert_eq!(channel.target, start);
    }

    #[test]
    fn difficulty_changes_never_reject_work_in_flight() {
        let hard = compact_target(0x1b0ffff0).unwrap();
        let mut channel = device_channel(hard);
        let block = channel.job().unwrap().template.target;
        let mut first = share(0);
        first.extranonce = &[0; DEVICE_EXTRANONCE_SIZE];
        assert!(matches!(
            channel.check(first, 1700000010),
            Err("difficulty-too-low")
        ));
        // Lowered: the next job carries the easier target, and the older job
        // accepts it too, credited at it and counted by vardiff.
        channel.desired = [255; 32];
        assert!(channel.settle(&block, None, &[255; 32]));
        assert_eq!(channel.vardiff.shares_since_last_update, 0);
        channel.install(2, 4, hard_template()).unwrap();
        let mut retry = share(1);
        retry.extranonce = &[0; DEVICE_EXTRANONCE_SIZE];
        assert_eq!(
            channel.check(retry, 1700000010).unwrap().share_target,
            [255; 32]
        );
        assert_eq!(channel.vardiff.shares_since_last_update, 1);
        // Raised: the older job keeps its own easier target, but vardiff does
        // not count that work; the new job needs the new target.
        channel.desired = hard;
        assert!(channel.settle(&block, None, &[255; 32]));
        channel.install(3, 5, hard_template()).unwrap();
        let mut older = share(2);
        older.job_id = 2;
        older.extranonce = &[1; DEVICE_EXTRANONCE_SIZE];
        assert_eq!(
            channel.check(older, 1700000010).unwrap().share_target,
            [255; 32]
        );
        assert_eq!(channel.vardiff.shares_since_last_update, 0);
        let mut newer = share(3);
        newer.job_id = 3;
        newer.extranonce = &[2; DEVICE_EXTRANONCE_SIZE];
        assert!(matches!(
            channel.check(newer, 1700000010),
            Err("difficulty-too-low")
        ));
    }

    #[test]
    fn extended_extranonce_and_coinbase_parts_agree_with_full_block() {
        let mut channel = channel(1, ChannelKind::Extended);
        let mut input = share(0);
        input.extranonce = &[0x23; DEVICE_EXTRANONCE_SIZE];
        let result = channel.check(input, 1700000010).unwrap();
        let job = channel.job().unwrap();
        let mut assembled = job.parts.prefix.clone();
        assembled.extend(&channel.extranonce_prefix);
        assembled.extend([0x23; DEVICE_EXTRANONCE_SIZE]);
        assembled.extend(&job.parts.suffix);
        assert_eq!(assembled, result.coinbase.bytes);
        let block = job.template.block(&result.coinbase, result.header).unwrap();
        let decoded: Block = consensus::deserialize(&block).unwrap();
        assert!(decoded.check_merkle_root());
        let mut input = share(1);
        input.extranonce = &[0x24; DEVICE_EXTRANONCE_SIZE];
        assert!(channel.check(input, 1700000010).is_ok());
        assert!(channel.check(share(2), 1700000010).is_err());
    }

    /// #### PR #42: the test token's Case A target in these tests, 2^248:
    /// about one share in 256 wins.
    const TOKEN_BITS: u32 = 0x2001_0000;
    /// #### PR #42: the test token's baton.
    const BATON: OutPoint = OutPoint {
        txid: [0x22; 32],
        vout: 0,
    };

    /// #### PR #42: the test token in `modes`, its Case A target `bits`.
    fn token_set(modes: &[Mode], bits: u32) -> Arc<TokenSet> {
        let state = TokenState::new(BATON, bits).unwrap();
        let entries = modes
            .iter()
            .map(|mode| match mode {
                Mode::ShareTarget => SetEntry::share_target(&TEST_TOKEN, state),
                Mode::BlockRequired => SetEntry::block_required(&TEST_TOKEN),
            })
            .collect();
        Arc::new(TokenSet::new(MiningNetwork::Chipnet, entries, 1).unwrap())
    }

    /// #### PR #42: a template with three transactions (a two-level
    /// coinbase branch) and mainnet-like block difficulty (no test share is
    /// a block), merge-mining `set`.
    fn token_template(set: Arc<TokenSet>) -> Arc<BchTemplate> {
        let mut raw = rpc_template();
        let mut txs = vec![transaction(1), transaction(2), transaction(3)];
        txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
        raw["transactions"] = serde_json::json!(txs);
        let mut template = BchTemplate::from_rpc(&raw).unwrap();
        template.target = compact_target(0x1a00ffff).unwrap();
        template.commit(set);
        Arc::new(template)
    }

    /// #### PR #42: a Chipnet job's payout, donation and (with an
    /// operator) fee scripts.
    fn scripts(miner: &str, operator: Option<&str>) -> Vec<Vec<u8>> {
        super::super::payout::scripts(MiningNetwork::Chipnet, miner, operator).unwrap()
    }

    /// #### PR #42: what the token's covenant sees in the claim of a miner
    /// job's Case A win at the default donation: the miner's output, and
    /// two thirds of the 1.5% donation as the split.
    fn claim_view(scripts: &[Vec<u8>]) -> ClaimView<'_> {
        ClaimView {
            category: TEST_TOKEN.category,
            anchor: BATON,
            ticket: None,
            payout: &scripts[0],
            split: Some((100, &scripts[1])),
            target_bits: TOKEN_BITS,
            ext: [0; 32],
            max_aux_height: TEST_TOKEN.max_aux_height,
            start_height: 0,
        }
    }

    /// #### PR #42: submits nonces 0.. on job 1 until a share wins the Case
    /// A token, checking that every share before it misses the token's
    /// target and carries no token work. Returns the win and its nonce.
    fn submit_until_win(
        channel: &mut Channel,
        sequence: &mut u32,
        extranonce: &'static [u8],
    ) -> (ValidatedShare, u32) {
        let target = channel
            .job()
            .unwrap()
            .aux
            .as_ref()
            .unwrap()
            .easiest_a
            .unwrap();
        for nonce in 0..10_000 {
            let mut input = share(*sequence);
            *sequence += 1;
            input.nonce = nonce;
            input.extranonce = extranonce;
            let share = channel.check(input, 1700000010).unwrap();
            assert!(!share.block);
            assert_eq!(share.hash, double_sha256(&share.header));
            if meets_target(&share.hash, &target) {
                return (share, nonce);
            }
            assert!(share.token_wins.is_empty() && share.merkle_path.is_empty());
            assert!(share.token_win().is_none());
        }
        panic!("no share won the token");
    }

    /// #### PR #42: a device extranonce for `kind`'s shares.
    fn device_extranonce(kind: ChannelKind) -> &'static [u8] {
        match kind {
            ChannelKind::Standard => &[],
            ChannelKind::Extended => &[0x23; DEVICE_EXTRANONCE_SIZE],
        }
    }

    // #### PR #42: token wins on the share path
    #[test]
    fn token_win_carries_the_exact_header_coinbase_and_branches() {
        let set = token_set(&[Mode::ShareTarget, Mode::BlockRequired], TOKEN_BITS);
        let scripts = scripts(&payout(), None);
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut channel = channel_on(1, kind, token_template(set.clone()));
            let job = channel.job().unwrap().clone();
            let aux = job.aux.clone().unwrap();
            assert_eq!(job.parts.merkle_path.len(), 2);
            let mut sequence = 0;
            let (share, _) = submit_until_win(&mut channel, &mut sequence, device_extranonce(kind));
            // The Case A entry only: Case B needs a block.
            assert_eq!(share.token_wins, [0]);
            assert!(Arc::ptr_eq(share.aux.as_ref().unwrap(), &aux));
            let win = share.token_win().unwrap();
            assert!(Arc::ptr_eq(&win.aux, &aux));
            assert_eq!(win.entries, [0]);
            assert_eq!(win.header, share.header);
            assert_eq!(win.hash, share.hash);
            assert_eq!(win.coinbase, share.coinbase.bytes);
            assert_eq!(win.merkle_path, job.parts.merkle_path);
            assert!(!win.block);
            assert_eq!(win.height, 325_909);
            assert_eq!((win.payout, &win.miner), (BchPayout::default(), &payout()));
            assert_eq!(win.operator, None);
            // The coinbase carries the job's commitment as output 0, and its
            // branch folds to the header's merkle root.
            let head = 47 + usize::from(win.coinbase[41]);
            assert_eq!(win.coinbase[head..head + 53], aux.outputs.commitment);
            let mut root = double_sha256(&win.coinbase);
            for sibling in &win.merkle_path {
                root = double_sha256(&[&root[..], &sibling[..]].concat());
            }
            assert_eq!(root, win.header[36..68]);
            // The proof verifies as the token's covenant would check it,
            // also after the journal's bytes.
            let proof = win.proof(0).unwrap();
            verify_a(&proof, &claim_view(&scripts)).unwrap();
            let bytes = proof.to_bytes().unwrap();
            verify_a(
                &AuxProof::from_bytes(&bytes).unwrap(),
                &claim_view(&scripts),
            )
            .unwrap();
            // The same coinbase and header make a whole block.
            let block = job.template.block(&share.coinbase, share.header).unwrap();
            let decoded: Block = consensus::deserialize(&block).unwrap();
            assert!(decoded.check_merkle_root());
        }
    }

    // #### PR #42: token wins on the share path
    #[test]
    fn shares_without_tokens_have_no_token_work_and_carry_their_hash() {
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut channel = channel(1, kind);
            let job = channel.job().unwrap();
            assert!(job.aux.is_none());
            // The coinbase is the one built without tokens.
            let plain = job
                .template
                .coinbase_parts_with_payout(
                    MiningNetwork::Chipnet,
                    &payout(),
                    None,
                    28,
                    BchPayout::default(),
                )
                .unwrap();
            assert_eq!(job.parts.suffix, plain.suffix);
            // Blocks (about half of these shares) and other shares alike.
            for nonce in 0..32 {
                let mut input = share(nonce);
                input.nonce = nonce;
                input.extranonce = device_extranonce(kind);
                let share = channel.check(input, 1700000010).unwrap();
                assert_eq!(share.hash, double_sha256(&share.header));
                assert!(share.aux.is_none());
                assert!(share.token_wins.is_empty() && share.merkle_path.is_empty());
                assert!(share.token_win().is_none());
            }
        }
    }

    // #### PR #42: token wins on the share path
    #[test]
    fn a_block_wins_the_case_b_tokens_and_other_shares_do_not() {
        // The easy test template: about half the shares are blocks.
        let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        template.commit(token_set(&[Mode::BlockRequired], TOKEN_BITS));
        let mut channel = channel_on(1, ChannelKind::Standard, Arc::new(template));
        let aux = channel.job().unwrap().aux.clone().unwrap();
        assert_eq!(aux.easiest_a, None);
        let scripts = scripts(&payout(), None);
        let (mut blocks, mut others) = (0, 0);
        for nonce in 0..64 {
            let mut input = share(nonce);
            input.nonce = nonce;
            let share = channel.check(input, 1700000010).unwrap();
            if !share.block {
                others += 1;
                assert!(share.token_wins.is_empty() && share.token_win().is_none());
                continue;
            }
            blocks += 1;
            assert_eq!(share.token_wins, [0]);
            let win = share.token_win().unwrap();
            assert!(win.block);
            // Its claim spends the ticket of this very coinbase.
            let ticket = OutPoint {
                txid: double_sha256(&win.coinbase),
                vout: aux.entries[0].ticket_vout.unwrap(),
            };
            let view = ClaimView {
                ticket: Some(ticket),
                ..claim_view(&scripts)
            };
            verify_b(&win.proof(0).unwrap(), &view).unwrap();
        }
        assert!(blocks > 0 && others > 0);
    }

    // #### PR #42: token wins on the share path
    #[test]
    fn a_duplicate_share_never_yields_a_second_token_win() {
        let set = token_set(&[Mode::ShareTarget], TOKEN_BITS);
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut channel = channel_on(1, kind, token_template(set.clone()));
            let mut sequence = 0;
            let (first, nonce) =
                submit_until_win(&mut channel, &mut sequence, device_extranonce(kind));
            assert_eq!(first.token_wins, [0]);
            // The same header again, under a new sequence number.
            let mut again = share(sequence);
            again.nonce = nonce;
            again.extranonce = device_extranonce(kind);
            assert!(matches!(
                channel.check(again, 1700000010),
                Err("duplicate-share")
            ));
            // Still a duplicate once a same-tip refresh has retained the job.
            channel.install(2, 4, token_template(set.clone())).unwrap();
            let mut retained = share(sequence + 1);
            retained.nonce = nonce;
            retained.extranonce = device_extranonce(kind);
            assert!(matches!(
                channel.check(retained, 1700000010),
                Err("duplicate-share")
            ));
        }
    }

    // #### PR #42: merge-mined tokens in each job
    #[test]
    fn public_pool_leaves_bind_each_channels_payout() {
        use crate::donation::bch::{FeeMode, PoolFee};
        let network = MiningNetwork::Chipnet;
        let template = token_template(token_set(
            &[Mode::ShareTarget, Mode::BlockRequired],
            TOKEN_BITS,
        ));
        let operator = crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], network).unwrap();
        let other = crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x56; 20], network).unwrap();
        let policy = BchPayout {
            fee: Some(PoolFee {
                rate: "2".parse().unwrap(),
                mode: FeeMode::Coinbase,
            }),
            ..BchPayout::default()
        };
        let jobs: Vec<_> = [payout(), other]
            .into_iter()
            .zip(1u32..)
            .map(|(miner, id)| {
                let mut channel = Channel::new(
                    id,
                    ChannelKind::Extended,
                    [255; 32],
                    [9; 12],
                    network,
                    &miner,
                )
                .unwrap();
                channel.set_operator(Some(&operator)).unwrap();
                let job = channel
                    .install_with_payout(1, 3, template.clone(), policy)
                    .unwrap()
                    .clone();
                (job, scripts(&miner, Some(&operator)))
            })
            .collect();
        for (job, scripts) in &jobs {
            let aux = job.aux.as_ref().unwrap();
            for entry in &aux.entries {
                assert_eq!(entry.leaf.payout_hash, double_sha256(&scripts[0]));
                assert_eq!(entry.leaf.split_bps, 100);
                assert_eq!(entry.leaf.split_hash, double_sha256(&scripts[1]));
            }
            // SV1 firmware and extended channels find the commitment in the
            // suffix, after the sequence and the output count, and the
            // ticket after the miner's, the donation's and the fee's outputs.
            assert_eq!(job.parts.suffix[5..58], aux.outputs.commitment);
            assert_eq!(aux.entries[1].ticket_vout, Some(4));
        }
        let first = jobs[0].0.aux.as_ref().unwrap();
        let second = jobs[1].0.aux.as_ref().unwrap();
        assert_ne!(first.commitment.root, second.commitment.root);
        assert_eq!(first.commitment.height, second.commitment.height);
        assert_eq!(first.commitment.nonce, second.commitment.nonce);
        let slots = |aux: &AuxJob| aux.entries.iter().map(|e| e.slot).collect::<Vec<_>>();
        assert_eq!(slots(first), slots(second));
    }

    // #### PR #42: merge-mined tokens in each job
    #[test]
    fn donation_and_fee_work_jobs_bind_their_payout_and_no_split() {
        use crate::donation::bch::{FeeMode, PoolFee};
        let network = MiningNetwork::Chipnet;
        let template = token_template(token_set(&[Mode::ShareTarget], TOKEN_BITS));
        let operator = crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], network).unwrap();
        let scripts = scripts(&payout(), Some(&operator));
        let miner_job = BchPayout {
            fee: Some(PoolFee {
                rate: "2".parse().unwrap(),
                mode: FeeMode::Work,
            }),
            ..BchPayout::default()
        };
        let donation_job = BchPayout {
            donation_work: true,
            ..miner_job
        };
        let fee_job = BchPayout {
            fee_work: true,
            ..miner_job
        };
        let mut channel = Channel::new(
            1,
            ChannelKind::Standard,
            [255; 32],
            [9; 12],
            network,
            &payout(),
        )
        .unwrap();
        channel.set_operator(Some(&operator)).unwrap();
        let cases = [
            (miner_job, &scripts[0], Some(&scripts[1])),
            (donation_job, &scripts[1], None),
            (fee_job, &scripts[2], None),
        ];
        for (id, (policy, beneficiary, split)) in (1u32..).zip(cases) {
            let job = channel
                .install_with_payout(id, 3, template.clone(), policy)
                .unwrap();
            let leaf = job.aux.as_ref().unwrap().entries[0].leaf;
            assert_eq!(leaf.payout_hash, double_sha256(beneficiary));
            match split {
                Some(donation) => {
                    assert_eq!(leaf.split_bps, 100);
                    assert_eq!(leaf.split_hash, double_sha256(donation));
                }
                None => {
                    assert_eq!(leaf.split_bps, 0);
                    assert_eq!(leaf.split_hash, [0; 32]);
                }
            }
            // The commitment (worth 0) first, then the job's own payouts.
            let tx: Transaction = consensus::deserialize(&job.standard_coinbase.bytes).unwrap();
            assert_eq!(tx.output[0].value.to_sat(), 0);
            assert_eq!(tx.output[1].script_pubkey.as_bytes(), &beneficiary[..]);
        }
    }

    // #### PR #42: the share-target floor for merge-mined tokens
    #[test]
    fn share_target_floors_at_the_easiest_token_target_within_15x() {
        let vardiff = compact_target(0x1d00ffff).unwrap();
        let mut channel = device_channel(vardiff);
        let block = channel.job().unwrap().template.target;
        let open = [255; 32];
        // No token: vardiff's target.
        assert!(!channel.settle(&block, None, &open));
        assert_eq!(channel.target, vardiff);
        // A token target harder than vardiff's changes nothing.
        let harder = compact_target(0x1c00ffff).unwrap();
        assert!(!channel.settle(&block, Some(&harder), &open));
        assert_eq!(channel.target, vardiff);
        // Up to 15 times easier: the token's own target. Vardiff keeps its
        // count, since its own target did not change.
        channel.vardiff_window(30, 5);
        let four = compact_target(0x1d03fffc).unwrap();
        assert_eq!(four, times(&vardiff, 4));
        assert!(channel.settle(&block, Some(&four), &open));
        assert_eq!(channel.target, four);
        assert_eq!(channel.vardiff.shares_since_last_update, 5);
        // Easier still: 15 times vardiff's target, no more.
        let easy = compact_target(0x1e00ffff).unwrap();
        assert!(channel.settle(&block, Some(&easy), &open));
        assert_eq!(channel.target, times(&vardiff, TOKEN_FLOOR_CAP));
        let ratio = expected_hashes(&vardiff) / expected_hashes(&channel.target);
        assert!((14.99..=15.01).contains(&ratio), "{ratio}");
        // The device's maximum still caps it.
        let maximum = times(&vardiff, 2);
        assert!(channel.settle(&block, Some(&easy), &maximum));
        assert_eq!(channel.target, maximum);
        // Vardiff's own target never moved, and the device's target comes
        // back to it once the tokens are gone.
        assert_eq!(channel.vardiff_target, vardiff);
        assert_eq!(channel.vardiff.shares_since_last_update, 5);
        assert!(channel.settle(&block, None, &open));
        assert_eq!(channel.target, vardiff);
        // When vardiff's own target changes, it counts afresh.
        channel.desired = four;
        assert!(channel.settle(&block, Some(&easy), &open));
        assert_eq!(channel.vardiff_target, four);
        assert_eq!(channel.target, times(&four, TOKEN_FLOOR_CAP));
        assert_eq!(channel.vardiff.shares_since_last_update, 0);
        // The arithmetic carries between bytes and saturates at the easiest
        // target.
        let mut small = [0; 32];
        small[0] = 0x12;
        let mut product = [0; 32];
        product[..2].copy_from_slice(&[0x0e, 0x01]);
        assert_eq!(times(&small, 15), product);
        assert_eq!(times(&[0x11; 32], 15), [0xff; 32]);
        assert_eq!(times(&[0x12; 32], 15), [0xff; 32]);
        assert_eq!(token_floor(&[0x12; 32], &[0xfe; 32]), [0xfe; 32]);
        assert_eq!(times(&[0; 32], 15), [0; 32]);
    }

    // #### PR #42: the share-target floor for merge-mined tokens
    #[test]
    fn vardiff_ignores_shares_accepted_only_by_the_floor() {
        // Counting: vardiff aims at about one share in 256; the token's
        // target, eight times easier, is what the device mines at.
        let vardiff = compact_target(TOKEN_BITS).unwrap();
        let token_bits = 0x2008_0000;
        let floor = compact_target(token_bits).unwrap();
        assert_eq!(floor, times(&vardiff, 8));
        let template = token_template(token_set(&[Mode::ShareTarget], token_bits));
        let mut channel = Channel::new(
            1,
            ChannelKind::Standard,
            vardiff,
            [9; 12],
            MiningNetwork::Chipnet,
            &payout(),
        )
        .unwrap();
        let tokens = template.tokens().and_then(|set| set.easiest_a());
        assert!(channel.settle(&template.target, tokens, &[255; 32]));
        assert_eq!(channel.target, floor);
        channel.install(1, 3, template).unwrap();
        assert_eq!(channel.job().unwrap().target, floor);
        let (mut counted, mut floor_only) = (0, 0);
        for nonce in 0..4_000 {
            let mut input = share(nonce);
            input.nonce = nonce;
            let Ok(share) = channel.check(input, 1700000010) else {
                continue;
            };
            // Accepted and credited at the floor, and every one wins the
            // token: no win waits behind a harder share target.
            assert_eq!(share.share_target, floor);
            assert_eq!(share.token_wins, [0]);
            if meets_target(&share.hash, &vardiff) {
                counted += 1;
            } else {
                floor_only += 1;
            }
        }
        assert!(
            counted > 0 && floor_only > counted,
            "{counted} {floor_only}"
        );
        assert_eq!(channel.vardiff.shares_since_last_update, counted);

        // Estimating, for a real device (difficulty 1, above vardiff's
        // 1 MH/s floor) under a token floor eight times easier: 20 shares a
        // minute at vardiff's own target is the aim, so nothing changes.
        // Read against the floor, the same count would be an eighth of the
        // hash rate and ease vardiff's target.
        let hard = compact_target(0x1d00ffff).unwrap();
        let template = token_template(token_set(&[Mode::ShareTarget], 0x1d07_fff8));
        let mut channel = Channel::new(
            1,
            ChannelKind::Standard,
            hard,
            [9; 12],
            MiningNetwork::Chipnet,
            &payout(),
        )
        .unwrap();
        let tokens = template.tokens().and_then(|set| set.easiest_a());
        assert!(channel.settle(&template.target, tokens, &[255; 32]));
        assert_eq!(channel.target, times(&hard, 8));
        channel.install(1, 3, template).unwrap();
        channel.vardiff_window(300, 100);
        assert_eq!(channel.retarget(&[255; 32]), None);
        assert_eq!(channel.desired, hard);
        assert_eq!(channel.vardiff_target, hard);
        assert_eq!(channel.target, times(&hard, 8));
        // Twice the hash rate is a real change: vardiff's target halves,
        // and the floor, capped at 15 times it, now sits below the token's.
        channel.vardiff_window(300, 200);
        let told = channel.retarget(&[255; 32]).unwrap();
        assert!(meets_target(&channel.vardiff_target, &hard));
        assert_ne!(channel.vardiff_target, hard);
        assert_eq!(told, times(&channel.vardiff_target, TOKEN_FLOOR_CAP));
        assert_eq!(channel.target, told);
    }

    // #### PR #42
    // What: under a Job Declaration plan, a local channel's coinbase script
    // is the head, the pool's prefix, the job id, the pad, the lane and the
    // device's bytes; a share's forwarded bytes are exactly the rolled ones
    // the pool rebuilds with, and the coinbase pays the plan's outputs.
    // Look here if: the nested layout or ForwardShare changes.
    #[test]
    fn nested_layout_puts_the_pool_prefix_before_the_lane() {
        use super::super::jd::{plan::JdPlan, token::PoolRates};
        let mut template =
            BchTemplate::from_rpc(&super::super::template_tests::rpc_template()).unwrap();
        let plan = JdPlan {
            serial: 3,
            upstream_prefix: vec![0xee; 16],
            pad: vec![0; 2],
            scripts: vec![vec![0x51], vec![0x52]],
            rates: PoolRates {
                donation_bps: 0,
                fee_bps: 100,
                donation_output: None,
                fee_output: Some(1),
            },
            pool_target: [0xff; 32],
        };
        template.declare(Arc::new(plan.clone()));
        let template = Arc::new(template);
        let mut channel = Channel::new(
            1,
            ChannelKind::Extended,
            [255; 32],
            [9; 12],
            MiningNetwork::Chipnet,
            &super::super::template_tests::payout(),
        )
        .unwrap();
        channel.set_lane(7);
        let job = channel.install(5, 11, template.clone()).unwrap();
        let head = template.script_head();
        let mut expected_prefix = job.parts.prefix[..42 + head.len()].to_vec();
        expected_prefix.extend([0xee; 16]);
        expected_prefix.extend(5u32.to_le_bytes());
        expected_prefix.extend([0, 0]);
        assert_eq!(job.parts.prefix, expected_prefix);
        assert_eq!(
            job.parts.prefix[41] as usize,
            head.len() + 16 + plan.rollable()
        );
        let device = [0x42; 8];
        let mut coinbase = job.parts.prefix.clone();
        coinbase.extend(7u32.to_le_bytes());
        coinbase.extend(device);
        coinbase.extend(&job.parts.suffix);
        let outputs = super::super::jd::codec::serialize_outputs(&plan.outputs(312_500_000));
        assert!(job.parts.suffix[4..].starts_with(&outputs));
        let mut header = [0; 80];
        header[..4].copy_from_slice(&template.version.to_le_bytes());
        header[4..36].copy_from_slice(&template.previous_hash);
        header[36..68].copy_from_slice(&double_sha256(&coinbase));
        header[68..72].copy_from_slice(&template.current_time.to_le_bytes());
        header[72..76].copy_from_slice(&template.bits.to_le_bytes());
        let share = channel
            .check(
                Share {
                    channel_id: 1,
                    job_id: 5,
                    sequence: 1,
                    version: template.version,
                    time: template.current_time,
                    nonce: 0,
                    extranonce: &device,
                },
                template.current_time,
            )
            .unwrap();
        let forward = share.forward.unwrap();
        assert_eq!(forward.serial, 11);
        let mut rolled = 5u32.to_le_bytes().to_vec();
        rolled.extend([0, 0]);
        rolled.extend(7u32.to_le_bytes());
        rolled.extend(device);
        assert_eq!(forward.extranonce, rolled);
        assert_eq!(share.coinbase.bytes, coinbase);
    }
}
