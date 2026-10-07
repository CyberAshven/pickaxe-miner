//! #### PR #38
//! BCH work ownership and share checking, independent of socket/firmware.
//! Version rolling is restricted to BIP320's general-purpose bits. Devices
//! receive disjoint coinbases and cannot alter payouts or template tx order.

use super::template::{double_sha256, meets_target, BchTemplate, Coinbase, CoinbaseParts, Hash};
use crate::config::{validate_payout_address, MiningNetwork};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

pub const VERSION_ROLLING_MASK: u32 = 0x1fff_e000;
const MAX_SHARES_PER_JOB: usize = 16_384;
pub const DEVICE_EXTRANONCE_SIZE: usize = 8;
pub const MAX_ACTIVE_JOBS: usize = 8;

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
    job: Option<Job>,
    previous: VecDeque<Job>,
    sequence: Option<u32>,
    seen: HashMap<u32, HashSet<Hash>>,
    pub accepted: u64,
    pub rejected: u64,
}

#[derive(Clone)]
pub struct Job {
    pub id: u32,
    pub generation: u64,
    pub template: Arc<BchTemplate>,
    pub standard_coinbase: Coinbase,
    pub parts: CoinbaseParts,
    pub target: Hash,
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
        let payout = validate_payout_address(network, payout)
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
            job: None,
            previous: VecDeque::new(),
            sequence: None,
            seen: HashMap::new(),
            accepted: 0,
            rejected: 0,
        })
    }

    pub fn install(
        &mut self,
        id: u32,
        generation: u64,
        template: Arc<BchTemplate>,
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
        let standard_coinbase = template.coinbase(self.network, &self.payout, &extra)?;
        let mut parts = template.coinbase_parts(
            self.network,
            &self.payout,
            extra.len() + DEVICE_EXTRANONCE_SIZE,
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
                    .coinbase(self.network, &self.payout, &extra)
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
        // SetTarget applies to subsequent jobs; retained active jobs keep the
        // difficulty they advertised. Validate and account against that target.
        if !meets_target(&hash, &job.target) && !block {
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
        Ok(ValidatedShare {
            generation: job.generation,
            template: job.template.clone(),
            coinbase,
            header,
            block,
            share_target: job.target,
        })
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
    fn active_jobs_keep_their_difficulty_across_target_updates() {
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
        let mut old_strict = share(1);
        old_strict.job_id = 2;
        old_strict.nonce = strict_nonce;
        assert!(matches!(
            channel.check(old_strict, 1700000010),
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
