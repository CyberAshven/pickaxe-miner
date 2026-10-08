//! #### PR #38
//! BCH work ownership and share checking, independent of socket/firmware.
//! Version rolling is restricted to BIP320's general-purpose bits. Devices
//! receive disjoint coinbases and cannot alter payouts or template tx order.

use super::telemetry::expected_hashes;
use super::template::{double_sha256, meets_target, BchTemplate, Coinbase, CoinbaseParts, Hash};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Standard,
    Extended,
}

pub struct Channel {
    pub id: u32,
    pub kind: ChannelKind,
    pub target: Hash,
    pub extranonce_prefix: [u8; 16],
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
    vardiff: VardiffState,
    /// Vardiff's latest hash rate estimate, the baseline for its next check.
    hashrate: f32,
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
        let mut extranonce_prefix = [0; 16];
        extranonce_prefix[..12].copy_from_slice(&session_salt);
        extranonce_prefix[12..].copy_from_slice(&id.to_le_bytes());
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
            vardiff: VardiffState::new_with_min(MIN_HASHRATE)
                .map_err(|_| "invalid system clock")?,
            hashrate: hash_rate_from_target(target.into(), f64::from(SHARES_PER_MINUTE))
                .map_or(MIN_HASHRATE, |rate| rate as f32),
        })
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
        let mut extra = id.to_le_bytes().to_vec();
        extra.extend(self.extranonce_prefix);
        let standard_coinbase = template.coinbase_with_payout(
            self.network,
            &self.payout,
            self.operator.as_deref(),
            &extra,
            payout,
        )?;
        let mut parts = template.coinbase_parts_with_payout(
            self.network,
            &self.payout,
            self.operator.as_deref(),
            extra.len() + DEVICE_EXTRANONCE_SIZE,
            payout,
        )?;
        parts.prefix.extend(id.to_le_bytes());
        // #### PR #38
        // A mempool/time refresh on the same parent does not invalidate work
        // already in an ASIC pipeline. Retain exact coinbases and tx lists;
        // a new parent or changed bits still revokes every preceding job.
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
        self.job = Some(Job {
            id,
            generation,
            template,
            standard_coinbase,
            parts,
            target: self.target,
            payout,
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
    /// whether the target changed; vardiff then counts afresh.
    pub fn settle(&mut self, block: &Hash, maximum: &Hash) -> bool {
        let mut target = if meets_target(&self.desired, block) {
            *block
        } else {
            self.desired
        };
        if !meets_target(&target, maximum) {
            target = *maximum;
        }
        if target == self.target {
            return false;
        }
        self.target = target;
        let _ = self.vardiff.reset_counter();
        true
    }

    /// #### PR #38
    /// Vardiff: once SRI's reference rules see the share rate drift from the
    /// aim, the desired target follows the measured hash rate. Returns the
    /// new target when the device must be told.
    pub fn retarget(&mut self, maximum: &Hash) -> Option<Hash> {
        let block = self.job.as_ref()?.template.target;
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
                &Target::from_le_bytes(self.target),
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
        self.settle(&block, maximum).then_some(self.target)
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
        if share.time < job.template.current_time
            || share.time > now.saturating_add(60)
            || share.time > job.template.current_time.saturating_add(60)
        {
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
                if share.extranonce.len() != DEVICE_EXTRANONCE_SIZE {
                    return Err("invalid-extranonce-size");
                }
                let mut extra = job.id.to_le_bytes().to_vec();
                extra.extend(self.extranonce_prefix);
                extra.extend_from_slice(share.extranonce);
                job.template
                    .coinbase_with_payout(
                        self.network,
                        &self.payout,
                        self.operator.as_deref(),
                        &extra,
                        job.payout,
                    )
                    .map_err(|_| "invalid-coinbase")?
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
        // Vardiff counts shares at the current target only, so work still
        // arriving at an older, easier target cannot inflate its estimate.
        if meets_target(&hash, &self.target) {
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
    use super::super::template::compact_target;
    use super::super::template_tests::{payout, rpc_template};
    use super::*;
    use stratum_core::bitcoin::{consensus, Block};
    fn channel(id: u32, kind: ChannelKind) -> Channel {
        let mut channel = Channel::new(
            id,
            kind,
            [255; 32],
            [9; 12],
            MiningNetwork::Chipnet,
            &payout(),
        )
        .unwrap();
        channel
            .install(
                1,
                3,
                Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
            )
            .unwrap();
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
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut channel = channel(1, kind);
            let template = channel.job().unwrap().template.clone();
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
                assert_eq!(
                    values,
                    [
                        vec![309_375_000, 3_125_000],
                        vec![308_333_334, 4_166_666],
                        vec![312_500_000]
                    ][index]
                );
                assert_eq!(values.iter().sum::<u64>(), template.coinbase_value);
            }
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
        assert!(channel.settle(&easy, &[255; 32]));
        assert_eq!(channel.target, easy);
        // ...and return to the share difficulty on the next normal template.
        assert!(channel.settle(&compact_target(0x1a00ffff).unwrap(), &[255; 32]));
        assert_eq!(channel.target, start);
        // A device maximum caps how easy the target gets.
        assert!(!channel.settle(&easy, &start));
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
        assert!(channel.settle(&block, &[255; 32]));
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
        assert!(channel.settle(&block, &[255; 32]));
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
        assembled.extend(channel.extranonce_prefix);
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
}
