//! #### PR #38
//! Mining-protocol session using reference message parsers/serializers. The
//! server supplies templates and channel IDs; devices only submit header work.

use super::{
    channel::{
        share_work, Channel, ChannelKind, Job, Share, TokenWin, ValidatedShare,
        DEVICE_EXTRANONCE_SIZE, VERSION_ROLLING_MASK,
    },
    jd::{
        codec::parse_outputs,
        server::{Custom, Declarator},
        token::PxToken,
        ForwardShare, COMPACT_DEVICE_EXTRANONCE, CUSTOM_JOB_BIT, JD_ROLLABLE,
    },
    telemetry::ShareEvent,
    template::{meets_target, BchTemplate, CoinbaseParts, Hash},
};
use crate::config::MiningNetwork;
use crate::donation::bch::BchPayout;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
};
use stratum_core::{
    binary_sv2::{self, GetSize, Serialize, Sv2Option},
    codec_sv2::{EncodableFrame, MessageFrame, SerializedFrame},
    common_messages_sv2::{
        self as common, Protocol, SetupConnection, SetupConnectionError, SetupConnectionSuccess,
    },
    mining_sv2::*,
    parsers_sv2::{IsSv2Message, Mining},
};

const MAX_CHANNELS: usize = 32;
/// #### PR #42: channels on a connection whose extended channels are
/// grouped: a proxy such as SRI's translator opens one per miner behind it.
const MAX_GROUPED_CHANNELS: usize = 256;

/// #### PR #42: the lanes a Job Declaration client's local channels take,
/// from one server-wide counter. #### PR #56: and the layout of the last
/// plan a job carried, for channels opened while no plan is current.
#[derive(Default)]
pub struct Lanes {
    next: AtomicU32,
    compact: AtomicBool,
}

/// #### PR #42: a group channel: the extended channels of one payout
/// identity on a connection, which share each template's job frame.
struct Group {
    payout: String,
    members: BTreeSet<u32>,
}

pub struct MiningSession {
    network: MiningNetwork,
    payout: String,
    salt: [u8; 12],
    share_target: Hash,
    setup_flags: Option<u32>,
    next_channel: u32,
    pub channels: BTreeMap<u32, Channel>,
    maximum_targets: BTreeMap<u32, Hash>,
    current: Option<(u32, u64, Arc<BchTemplate>)>,
    payout_policy: BchPayout,
    /// #### PR #40: a public pool, where each channel pays its user.
    public: Option<super::payout::PublicPool>,
    /// #### PR #40: the latest channel's user identity, taken by the server
    /// to name the device on the workers page.
    pub identity: Option<String>,
    /// #### PR #40: the difficulty the latest accepted share's hash reached,
    /// taken by the server for best-share records.
    pub share_difficulty: Option<f64>,
    pub accepted: u64,
    pub rejected: u64,
    /// #### PR #42: a public pool's Job Declaration rules, when it accepts
    /// miners' own templates; this connection negotiated work selection;
    /// the custom jobs it set so far.
    declarator: Option<Arc<Declarator>>,
    work_selection: bool,
    custom_jobs: u32,
    /// #### PR #42: a Job Declaration client's local server: each channel
    /// takes a lane from this server-wide counter as its extranonce prefix.
    lanes: Option<Arc<Lanes>>,
    /// #### PR #42: whether this connection's extended channels are grouped,
    /// and its groups by id.
    grouping: bool,
    groups: BTreeMap<u32, Group>,
    /// #### PR #42: the server mines a token that fixes the version slot.
    fixed_version: bool,
    /// #### merge-mining v2: why token merge mining was refused for a job
    /// (the own-root check), taken by the server for the dashboard.
    pub token_refusal: Option<&'static str>,
    /// #### merge-mining v2: on a public pool, the donation choice of this
    /// connection's logins (`None` inside: the pool's setting), fixed by its
    /// first channel.
    donation_choice: Option<Option<crate::donation::bch::BchDonation>>,
    /// #### one donation, the higher rate: on a public pool, the server's
    /// donation rate as last given (`public_donation`), and the donation
    /// this connection's own Pickaxe visibly mines on its donation channels.
    server_donation: Option<crate::donation::bch::BchDonation>,
    own_donation: super::own_donation::OwnDonation,
    /// #### one donation, the higher rate: the target of the latest share
    /// accepted on a donation channel, taken by the server for the work
    /// split (such shares repay no quota of the connection's own jobs).
    pub donation_share: Option<Hash>,
    /// #### merge-mining v2: the client asked for the merge-mining
    /// disclosure extension (`merge::disclosure`).
    merge_disclosure: bool,
    /// The client was granted Worker-Specific Hashrate Tracking (0x0002):
    /// its extended shares may name their worker.
    worker_hashrate: bool,
    /// #### merge-mining v2: leaf log lines (JSON) for jobs on a public
    /// pool's standard channels, taken by the server for its leaf log.
    pub leaf_log: Vec<String>,
}

#[derive(Default)]
pub struct Responses {
    pub frames: Vec<SerializedFrame>,
    pub blocks: Vec<ValidatedShare>,
    /// #### PR #42: accepted shares that win merge-mined tokens, whether or
    /// not they are also blocks.
    pub token_wins: Vec<TokenWin>,
    pub share_event: Option<ShareEvent>,
    pub accepted_work: Option<(Hash, BchPayout)>,
    /// #### PR #42: a SetCustomMiningJob's outcome, its error code when
    /// refused.
    pub custom_job: Option<Result<(), &'static str>>,
    /// #### PR #42: accepted shares on Job Declaration jobs, for the pool.
    pub forward: Vec<ForwardShare>,
    /// #### PR #42: accepted shares that win a header-shaped token (never
    /// blocks).
    pub header_wins: Vec<ValidatedShare>,
    /// The worker the share named (extension 0x0002), for the workers
    /// table, which shows it as a name only (`telemetry::worker_name`).
    pub worker: Option<String>,
}

impl MiningSession {
    pub fn new(
        network: MiningNetwork,
        payout: String,
        salt: [u8; 12],
        share_target: Hash,
    ) -> Result<Self, String> {
        crate::config::validate_coinbase_address(network, &payout)
            .map_err(|_| "invalid payout for selected network")?;
        if share_target == [0; 32] {
            return Err("share target cannot be zero".into());
        }
        Ok(Self {
            network,
            payout,
            salt,
            share_target,
            setup_flags: None,
            next_channel: 0,
            channels: BTreeMap::new(),
            maximum_targets: BTreeMap::new(),
            current: None,
            payout_policy: BchPayout::default(),
            public: None,
            identity: None,
            share_difficulty: None,
            accepted: 0,
            rejected: 0,
            declarator: None,
            work_selection: false,
            custom_jobs: 0,
            lanes: None,
            grouping: false,
            groups: BTreeMap::new(),
            fixed_version: false,
            token_refusal: None,
            donation_choice: None,
            server_donation: None,
            own_donation: Default::default(),
            donation_share: None,
            merge_disclosure: false,
            worker_hashrate: false,
            leaf_log: Vec::new(),
        })
    }

    // #### merge-mining v2: disclosure to miners
    // What: after a job reaches a channel whose job commits a non-empty
    // root, a client that negotiated the extension gets its JobLeaves frame;
    // a public pool's standard channel (whose device cannot take the frame)
    // gets a leaf log line instead.
    // Why: miners on someone else's pool can check their leaves; an
    // operator can publish the log for standard-channel devices.
    // Look here if: leaves are missing after a job, or sent to a client
    // that never asked for them.
    /// The disclosure of `channel`'s current job: its frame for a client
    /// that asked, its log line on a public pool's standard channel.
    fn disclose(&mut self, channel: u32) -> Result<Option<SerializedFrame>, String> {
        use super::merge::disclosure::{Disclosure, JobLeaves, EXTENSION, JOB_LEAVES};
        let Some(channel) = self.channels.get(&channel) else {
            return Ok(None);
        };
        let Some(job) = channel.job() else {
            return Ok(None);
        };
        let Some(aux) = job.aux.as_deref().filter(|aux| !aux.is_empty()) else {
            return Ok(None);
        };
        if self.public.is_some() && channel.kind == ChannelKind::Standard {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            self.leaf_log.push(
                serde_json::json!({
                    "time": now,
                    "channel": channel.id,
                    "job": job.id,
                    "output0": hex::encode(aux.output),
                    "leaves": hex::encode(Disclosure::from_job(aux).encode()),
                })
                .to_string(),
            );
        }
        if !self.merge_disclosure {
            return Ok(None);
        }
        let leaves = JobLeaves::from_job(channel.id, job.id, aux);
        extension_frame(EXTENSION, JOB_LEAVES, true, &leaves.encode()).map(Some)
    }

    // #### merge-mining v2: one donation choice per connection
    // What: a public pool connection's first channel fixes its donation
    // choice (its login's `,donation=`, or none for the pool's setting); a
    // later channel whose login chooses otherwise is refused with
    // `donation-mismatch`. #### one donation, the higher rate: a donation
    // channel (one paying the donation's own address) neither fixes nor
    // needs a choice.
    // Why: the work allocation that makes donation-work jobs runs per
    // connection, at one rate; a proxy carrying miners with different
    // choices would otherwise donate at the wrong rate for some of them.
    // Look here if: a proxy's channels are refused with donation-mismatch.
    /// The donation this connection's miners chose, if they chose one.
    pub fn donation_choice(&self) -> Option<crate::donation::bch::BchDonation> {
        self.donation_choice.flatten()
    }

    // #### one donation, the higher rate
    // What: on a public pool, the rate this connection's miner channels'
    // jobs carry: the effective rate (the higher of the server's and the
    // login's, `payout::effective_donation`), less what the connection's own
    // Pickaxe visibly donates on its donation channel over the last hour
    // (`own_donation`), the rest taken as a share of the work left on the
    // miner's own channels. Each miner channel keeps it (`Channel::
    // set_donation`), so a channel's first job carries it too.
    // Why: the miner pays the higher rate once; a login's text never lowers
    // it, only donation work the server sees does.
    // Look here if: a miner with their own Pickaxe pays twice, or a typed
    // `,pickaxe-donation=0` lowers what the server takes.
    /// The rate this public pool connection's miner channels carry, with the
    /// server's rate `server`; the channels are given it too.
    pub fn public_donation(
        &mut self,
        server: crate::donation::bch::BchDonation,
    ) -> crate::donation::bch::BchDonation {
        self.server_donation = Some(server);
        let rate = self.miner_donation();
        for channel in self.channels.values_mut() {
            if !channel.pays_donation() {
                channel.set_donation(Some(rate));
            }
        }
        rate
    }

    /// The effective rate: the higher of the server's (as last given, or
    /// before that the jobs') and this connection's login's.
    fn effective_donation(&self) -> crate::donation::bch::BchDonation {
        let server = self.server_donation.unwrap_or(self.payout_policy.donation);
        super::payout::effective_donation(server, self.donation_choice())
    }

    /// The rate the server takes on the miner's own channels: the effective
    /// rate less the verified own donation.
    fn miner_donation(&self) -> crate::donation::bch::BchDonation {
        let effective = self.effective_donation();
        super::own_donation::charged(effective, self.own_donation.credit(effective))
    }

    /// #### one donation, the higher rate: on a public pool with a miner
    /// channel open, the effective rate and the own Pickaxe's credit, for
    /// the workers page; never an address.
    pub fn donation_terms(
        &self,
    ) -> Option<(
        crate::donation::bch::BchDonation,
        crate::donation::bch::BchDonation,
    )> {
        self.public.as_ref()?;
        self.channels
            .values()
            .any(|channel| !channel.pays_donation())
            .then(|| {
                let effective = self.effective_donation();
                (effective, self.own_donation.credit(effective))
            })
    }

    /// #### PR #42: the server mines a token that fixes the version slot:
    /// clients that require version rolling are refused, others are told
    /// the version is fixed.
    pub fn set_fixed_version(&mut self, fixed: bool) {
        self.fixed_version = fixed;
    }

    /// #### PR #42: a Job Declaration client's local server, whose channels
    /// take lanes from `lanes`.
    pub fn set_lanes(&mut self, lanes: Option<Arc<Lanes>>) {
        self.lanes = lanes;
    }

    /// #### PR #42: a public pool that accepts miners' own templates.
    pub fn set_declarator(&mut self, declarator: Option<Arc<Declarator>>) {
        self.declarator = declarator;
    }

    /// #### PR #42: whether the connection's SetupConnection was accepted.
    pub fn is_set_up(&self) -> bool {
        self.setup_flags.is_some()
    }

    /// Test hook: adds a channel together with its device maximum target.
    #[cfg(test)]
    pub fn insert_channel(&mut self, channel: Channel, maximum: Hash) {
        self.maximum_targets.insert(channel.id, maximum);
        self.channels.insert(channel.id, channel);
    }

    /// Revoke validation immediately without destroying the authenticated
    /// transport. Recovery must send a clean activation even on the same tip.
    pub fn revoke_job(&mut self) {
        self.current = None;
        for channel in self.channels.values_mut() {
            channel.revoke();
        }
    }

    pub fn set_job(
        &mut self,
        id: u32,
        generation: u64,
        template: Arc<BchTemplate>,
    ) -> Result<Vec<SerializedFrame>, String> {
        self.set_job_with_payout(id, generation, template, BchPayout::default())
    }

    pub fn set_job_with_payout(
        &mut self,
        id: u32,
        generation: u64,
        template: Arc<BchTemplate>,
        payout: BchPayout,
    ) -> Result<Vec<SerializedFrame>, String> {
        self.current = Some((id, generation, template.clone()));
        self.payout_policy = payout;
        if let (Some(lanes), Some(plan)) = (&self.lanes, template.jd_plan()) {
            lanes.compact.store(plan.compact, Ordering::Relaxed);
        }
        let mut frames = Vec::new();
        // #### PR #42: the group broadcast
        // What: on each template, a group of two or more extended channels
        // (one payout identity on a connection) gets one job frame addressed
        // to the group instead of one per channel; each member still gets
        // its own SetTarget, and the channel installs its job as before. A
        // single-member group, standard and custom-only channels get their
        // own frames.
        // Why: a proxy such as SRI's translator carries every miner behind
        // it on one connection; one frame per template instead of one per
        // miner keeps its link and the server light.
        // Look here if: a proxy's miners stop getting jobs, or their shares
        // are refused after a template change.
        let broadcast: BTreeSet<u32> = self
            .groups
            .values()
            .filter(|group| group.members.len() >= 2)
            .flat_map(|group| group.members.iter().copied())
            .collect();
        let mut immediates = BTreeMap::new();
        for channel in self.channels.values_mut() {
            let immediate = channel.job().is_some_and(|job| {
                job.template.previous_hash == template.previous_hash
                    && job.template.bits == template.bits
            });
            // Chipnet can have easier block work than a normal ASIC share.
            // Never ask firmware to discard headers that could win a block.
            let maximum = self
                .maximum_targets
                .get(&channel.id)
                .ok_or("channel target missing")?;
            if !meets_target(&template.target, maximum) {
                return Err("device maximum target would discard valid block work".into());
            }
            // #### PR #42: custom-only channels
            // What: a Job Declaration client's channel never gets the pool's
            // jobs; a new parent (or bits) revokes its custom jobs, and its
            // target follows the block target as a device's does.
            // Why: the client mines its own templates; a job on an old parent
            // can no longer make a block, so its shares are stale.
            // Look here if: a JD client gets pool jobs, or its shares on the
            // previous parent are still accepted.
            if channel.custom_only {
                if !immediate {
                    channel.revoke();
                }
                if channel.settle(&template.target, None, maximum) {
                    frames.push(mining(Mining::SetTarget(SetTarget {
                        channel_id: channel.id,
                        target: (&channel.target).into(),
                    }))?);
                }
                continue;
            }
            // #### end PR #42 ####
            // #### PR #38
            // Follow an easier block target down and, on the next normal
            // template, back up to the share difficulty. SetTarget precedes
            // the job so the job carries the new target.
            // #### PR #42: and floor it at the easiest merge-mined token
            // target, within 15 times vardiff's (see `Channel::settle`), on
            // this server's own devices only (`Channel::floor_for`).
            let tokens = channel.floor_for(&template);
            if channel.settle(&template.target, tokens.as_ref(), maximum) {
                frames.push(mining(Mining::SetTarget(SetTarget {
                    channel_id: channel.id,
                    target: (&channel.target).into(),
                }))?);
            }
            channel.install_with_payout(id, generation, template.clone(), payout)?;
            if let Some(reason) = channel.take_token_refusal() {
                self.token_refusal = Some(reason);
                // The job mines BCH alone: drop the token floor (the job's
                // own target too) before it goes out, so firmware sends no
                // shares that cannot win.
                if tokens.is_some() && channel.drop_token_floor(maximum) {
                    frames.push(mining(Mining::SetTarget(SetTarget {
                        channel_id: channel.id,
                        target: (&channel.target).into(),
                    }))?);
                }
            }
            if broadcast.contains(&channel.id) {
                immediates.insert(channel.id, immediate);
                continue;
            }
            frames.extend(job_frames(channel, immediate)?);
        }
        for (group, members) in self
            .groups
            .iter()
            .filter(|(_, group)| group.members.len() >= 2)
        {
            frames.extend(group_frames(
                *group,
                &members.members,
                &self.channels,
                &immediates,
            )?);
        }
        // #### merge-mining v2: each channel's leaves after its job.
        let ids: Vec<u32> = self.channels.keys().copied().collect();
        for id in ids {
            frames.extend(self.disclose(id)?);
        }
        Ok(frames)
    }

    /// #### PR #38
    /// Vardiff for every channel. A changed target goes out as SetTarget and
    /// at once as a fresh job on the same template, because SetTarget only
    /// applies to jobs sent after it. Job IDs come from the session's
    /// counter; the previous job stays valid for work in flight.
    pub fn retarget(&mut self, next_id: &mut u32) -> Result<Vec<SerializedFrame>, String> {
        let mut frames = Vec::new();
        let mut retargeted = Vec::new();
        for channel in self.channels.values_mut() {
            let maximum = self
                .maximum_targets
                .get(&channel.id)
                .ok_or("channel target missing")?;
            let Some(target) = channel.retarget(maximum) else {
                continue;
            };
            // #### PR #42: a custom-only channel's client re-issues its own
            // jobs; it gets the target alone.
            if channel.custom_only {
                frames.push(mining(Mining::SetTarget(SetTarget {
                    channel_id: channel.id,
                    target: (&target).into(),
                }))?);
                continue;
            }
            let (generation, template, payout) = {
                let job = channel.job().ok_or("channel has no job")?;
                (job.generation, job.template.clone(), job.payout)
            };
            *next_id = next_id.checked_add(1).ok_or("job identifiers exhausted")?;
            frames.push(mining(Mining::SetTarget(SetTarget {
                channel_id: channel.id,
                target: (&target).into(),
            }))?);
            channel.install_with_payout(*next_id, generation, template, payout)?;
            if let Some(reason) = channel.take_token_refusal() {
                self.token_refusal = Some(reason);
            }
            frames.extend(job_frames(channel, true)?);
            retargeted.push(channel.id);
        }
        // #### merge-mining v2: each new job's leaves.
        for id in retargeted {
            frames.extend(self.disclose(id)?);
        }
        Ok(frames)
    }

    pub fn receive(&mut self, mut frame: SerializedFrame, now: u32) -> Result<Responses, String> {
        let header = frame.header();
        // #### merge-mining v2: the extensions negotiation (type 0x0001).
        // What: RequestExtensions after setup grants the merge-mining
        // disclosure (0x4D4D) and Worker-Specific Hashrate Tracking (0x0002)
        // when asked, in the order asked; a request for neither gets
        // RequestExtensions.Error naming what was asked and requiring
        // nothing. Every other extension frame (an extension this server
        // does not implement or did not grant, or 0x0001 with the channel
        // bit) is discarded.
        // Why: sv2-spec 3.2.1 has a receiver ignore extension messages it
        // does not know; Knuth answers and discards the same way, so a
        // Knuth or SRI proxy that asks for 0x0002 keeps its connection.
        // Look here if: a client that negotiates extensions is closed, or a
        // worker name is read without the grant.
        let extension = header.ext_type_without_channel_msg();
        if extension != 0 {
            use super::extensions::{
                negotiation_answer, ExtensionMessage, RequestExtensions, NEGOTIATION,
                REQUEST_EXTENSIONS, WORKER_HASHRATE,
            };
            use super::merge::disclosure::EXTENSION as DISCLOSURE;
            if extension != NEGOTIATION
                || header.msg_type() != REQUEST_EXTENSIONS
                || header.channel_msg()
            {
                return Ok(Responses::default());
            }
            if self.setup_flags.is_none() {
                return Err("extensions requested before setup".into());
            }
            let request =
                RequestExtensions::decode(frame.payload()).ok_or("malformed extensions request")?;
            self.merge_disclosure |= request.requested.contains(&DISCLOSURE);
            self.worker_hashrate |= request.requested.contains(&WORKER_HASHRATE);
            return Ok(Responses {
                frames: vec![negotiation_answer(&request, |extension| {
                    matches!(extension, DISCLOSURE | WORKER_HASHRATE)
                })?],
                ..Responses::default()
            });
        }
        let mut frames = Vec::new();
        let mut blocks = Vec::new();
        let mut token_wins = Vec::new();
        let mut share_event = None;
        let mut accepted_work = None;
        if header.msg_type() == common::MESSAGE_TYPE_SETUP_CONNECTION {
            if self.setup_flags.is_some() || header.channel_msg() {
                return Err("unexpected setup message".into());
            }
            let setup: SetupConnection =
                binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed setup message")?;
            // 0: header-only channels; 2: version rolling.
            // #### PR #42: work selection accepted with a Declarator
            // What: bit 1 (REQUIRES_WORK_SELECTION) is accepted when this
            // public pool accepts miners' own templates; such a connection's
            // extended channels are custom-only.
            // Why: a Job Declaration client sets its own jobs on its mining
            // connection; without Job Declaration the bit stays refused.
            // Look here if: a JD client's mining setup is refused, or a device
            // gets custom-only channels.
            let allowed = if self.declarator.is_some() {
                0b111
            } else {
                0b101
            };
            // #### PR #42: the Fixed-version setup
            // What: when the server mines a token that fixes the version
            // slot, a client that requires version rolling (bit 2, as every
            // Bitaxe sets) is refused with that bit, and any other client is
            // told the version is fixed (Success bit 0).
            // Why: shares with rolled version bits would never be valid for
            // such a token; refusing at setup says why.
            // Look here if: a device cannot connect in token mode.
            let error = if setup.protocol != Protocol::MiningProtocol {
                Some((0, "unsupported-protocol"))
            } else if setup.min_version > 2 || setup.max_version < 2 {
                Some((0, "protocol-version-mismatch"))
            } else if setup.flags & !allowed != 0 {
                Some((setup.flags & !allowed, "unsupported-feature-flags"))
            } else if self.fixed_version && setup.flags & 0b100 != 0 {
                Some((0b100, "unsupported-feature-flags"))
            } else {
                None
            };
            self.work_selection = error.is_none() && setup.flags & 0b010 != 0;
            if let Some((flags, code)) = error {
                frames.push(encoded(
                    SetupConnectionError {
                        flags,
                        error_code: code.try_into().unwrap(),
                    },
                    common::MESSAGE_TYPE_SETUP_CONNECTION_ERROR,
                    false,
                )?);
            } else {
                self.setup_flags = Some(setup.flags);
                // #### PR #42: group channels
                // What: extended channels are grouped unless the connection
                // asked for standard jobs, or it is a Pickaxe SV1 adapter from
                // before group support (its firmware field was empty).
                // Why: such an adapter would drop group-addressed jobs.
                // Look here if: an older adapter's devices get no jobs.
                self.grouping = setup.flags & 1 == 0
                    && !(setup.vendor.as_utf8_or_hex() == "Pickaxe SV1 adapter"
                        && setup.firmware.as_utf8_or_hex().is_empty());
                // Upstream flag bit 0 means REQUIRES_FIXED_VERSION. Leave it
                // clear: ASIC hardware, including Bitaxe, needs version rolling.
                // #### PR #42: set only for a token that fixes the version.
                frames.push(encoded(
                    SetupConnectionSuccess {
                        used_version: 2,
                        flags: u32::from(self.fixed_version),
                    },
                    common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
                    false,
                )?);
            }
            return Ok(Responses {
                frames,
                blocks,
                token_wins,
                share_event,
                accepted_work,
                ..Responses::default()
            });
        }
        let flags = self
            .setup_flags
            .ok_or("SetupConnection is required first")?;
        // What: the bytes after a SubmitSharesExtended's own fields (its
        // 24 fixed bytes and the B0_32 extranonce) are TLV fields
        // (sv2-spec 3.4.3), read by `extensions::share_worker`; a
        // SubmitSharesStandard is exactly its 24 bytes.
        // Why: the reference parser stops at the message's own fields and
        // takes any trailing bytes; Knuth closes on bytes that are not whole
        // fields, and on anything after a standard share.
        // Look here if: a proxy's named shares close its connection.
        let payload_len = frame.payload().len();
        let trailing = if header.msg_type() == MESSAGE_TYPE_SUBMIT_SHARES_EXTENDED {
            let payload = frame.payload();
            payload
                .get(24)
                .and_then(|extranonce| payload.get(25 + usize::from(*extranonce)..))
                .map(<[u8]>::to_vec)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let msg: Mining = (header.msg_type(), frame.payload())
            .try_into()
            .map_err(|_| "malformed mining message")?;
        if msg.channel_bit() != header.channel_msg() {
            return Err("incorrect SV2 channel flag".into());
        }
        let mut custom_job = None;
        let mut forward = Vec::new();
        let mut header_wins = Vec::new();
        let mut worker = None;
        match msg {
            Mining::OpenStandardMiningChannel(request) => {
                let maximum = request
                    .max_target
                    .as_ref()
                    .try_into()
                    .map_err(|_| "invalid max target")?;
                frames.extend(self.open(
                    request.request_id,
                    ChannelKind::Standard,
                    request.nominal_hash_rate,
                    maximum,
                    0,
                    request.user_identity.as_utf8_or_hex().as_str(),
                )?);
            }
            Mining::OpenExtendedMiningChannel(request) => {
                if flags & 1 != 0 {
                    frames.push(open_error(
                        request.request_id,
                        "extended-channels-not-supported-for-standard-jobs",
                    )?);
                } else {
                    let maximum = request
                        .max_target
                        .as_ref()
                        .try_into()
                        .map_err(|_| "invalid max target")?;
                    frames.extend(self.open(
                        request.request_id,
                        ChannelKind::Extended,
                        request.nominal_hash_rate,
                        maximum,
                        request.min_extranonce_size,
                        request.user_identity.as_utf8_or_hex().as_str(),
                    )?);
                }
            }
            Mining::SubmitSharesStandard(request) => {
                if payload_len != 24 {
                    return Err("malformed mining message".into());
                }
                let share = Share {
                    channel_id: request.channel_id,
                    job_id: request.job_id,
                    sequence: request.sequence_number,
                    version: request.version,
                    time: request.ntime,
                    nonce: request.nonce,
                    extranonce: &[],
                };
                share_event = Some(self.submit(
                    share,
                    ChannelKind::Standard,
                    now,
                    &mut frames,
                    &mut blocks,
                    &mut token_wins,
                    &mut forward,
                    &mut header_wins,
                    &mut accepted_work,
                )?);
            }
            Mining::SubmitSharesExtended(request) => {
                // Extension 0x0002: the worker behind the share, once
                // granted; a name over 32 bytes refuses the share.
                match super::extensions::share_worker(&trailing, self.worker_hashrate) {
                    Ok(named) => {
                        worker = named;
                        let share = Share {
                            channel_id: request.channel_id,
                            job_id: request.job_id,
                            sequence: request.sequence_number,
                            version: request.version,
                            time: request.ntime,
                            nonce: request.nonce,
                            extranonce: request.extranonce.as_ref(),
                        };
                        share_event = Some(self.submit(
                            share,
                            ChannelKind::Extended,
                            now,
                            &mut frames,
                            &mut blocks,
                            &mut token_wins,
                            &mut forward,
                            &mut header_wins,
                            &mut accepted_work,
                        )?);
                    }
                    Err(Some(code)) => {
                        self.rejected = self.rejected.saturating_add(1);
                        frames.push(mining(Mining::SubmitSharesError(SubmitSharesError {
                            channel_id: request.channel_id,
                            sequence_number: request.sequence_number,
                            error_code: code.try_into().map_err(|_| "error code too long")?,
                        }))?);
                        share_event = Some(ShareEvent::Rejected(code));
                    }
                    Err(None) => return Err("malformed mining message".into()),
                }
            }
            Mining::UpdateChannel(request) => {
                let result = self
                    .channels
                    .get(&request.channel_id)
                    .ok_or("invalid-channel-id")
                    .and_then(|channel| {
                        let maximum: Hash = request
                            .max_target
                            .as_ref()
                            .try_into()
                            .map_err(|_| "max-target-out-of-range")?;
                        if !request.nominal_hash_rate.is_finite() || request.nominal_hash_rate < 0.0
                        {
                            return Err("invalid-nominal-hashrate");
                        }
                        if !meets_target(&channel.target, &maximum) {
                            return Err("max-target-out-of-range");
                        }
                        Ok((channel.target, maximum))
                    });
                frames.push(match result {
                    Ok((target, maximum)) => {
                        self.maximum_targets.insert(request.channel_id, maximum);
                        mining(Mining::SetTarget(SetTarget {
                            channel_id: request.channel_id,
                            target: (&target).into(),
                        }))?
                    }
                    Err(code) => mining(Mining::UpdateChannelError(UpdateChannelError {
                        channel_id: request.channel_id,
                        error_code: code.try_into().unwrap(),
                    }))?,
                });
            }
            Mining::CloseChannel(request) => {
                // #### PR #42: closing a group closes its members.
                let closed: Vec<u32> = match self.groups.remove(&request.channel_id) {
                    Some(group) => group.members.into_iter().collect(),
                    None => vec![request.channel_id],
                };
                for id in closed {
                    self.channels.remove(&id);
                    self.maximum_targets.remove(&id);
                    for group in self.groups.values_mut() {
                        group.members.remove(&id);
                    }
                }
                self.groups.retain(|_, group| !group.members.is_empty());
            }
            // #### PR #42: SetCustomMiningJob (Coinbase-only)
            // What: a Job Declaration client sets its own job on its
            // custom-only channel with a token the pool allocated; the pool
            // checks the parent, bits, version, start time, prefix and
            // coinbase version against its own template and the coinbase's
            // outputs against the token's payout rule, then answers with a
            // job id carrying 0x8000_0000.
            // Why: Coinbase-only Job Declaration lets a miner mine its own
            // node's templates at this pool while the coinbase still pays the
            // pool's fee and the Pickaxe donation.
            // Look here if: a JD client's custom jobs are refused, or its
            // shares are credited to the wrong job.
            Mining::SetCustomMiningJob(request) => {
                let (channel_id, request_id) = (request.channel_id, request.request_id);
                let outcome = self.custom_job(&request, now);
                custom_job = Some(outcome.map(|_| ()));
                frames.push(match outcome {
                    Ok(job_id) => mining(Mining::SetCustomMiningJobSuccess(
                        SetCustomMiningJobSuccess {
                            channel_id,
                            request_id,
                            job_id,
                        },
                    ))?,
                    Err(code) => {
                        mining(Mining::SetCustomMiningJobError(SetCustomMiningJobError {
                            channel_id,
                            request_id,
                            error_code: code.try_into().map_err(|_| "error code too long")?,
                        }))?
                    }
                });
            }
            _ => return Err("unsupported downstream mining message".into()),
        }
        Ok(Responses {
            frames,
            blocks,
            token_wins,
            share_event,
            accepted_work,
            custom_job,
            forward,
            header_wins,
            worker,
        })
    }

    /// #### PR #42: the checks of a Coinbase-only custom job, in the order of
    /// their error codes, and the job id of one that passes.
    fn custom_job(&mut self, request: &SetCustomMiningJob, now: u32) -> Result<u32, &'static str> {
        let declarator = self
            .declarator
            .clone()
            .filter(|_| self.work_selection)
            .ok_or("jd-not-supported")?;
        let (payout, extranonce) = self
            .channels
            .get(&request.channel_id)
            .filter(|channel| channel.custom_only)
            .map(|channel| {
                (
                    channel.payout().to_owned(),
                    channel.extranonce_prefix.len() + channel.rollable(),
                )
            })
            .ok_or("invalid-channel-id")?;
        let context = self
            .current
            .as_ref()
            .map(|(_, _, template)| template.clone())
            .ok_or("stale-chain-tip")?;
        // A declared job's token (Full-Template) says so; the token book
        // checks the flag is the token's own.
        if PxToken::decode(request.token.as_ref()).is_some_and(|token| token.declared) {
            return self.declared_job(request, now, &declarator, &payout, &context);
        }
        if request.prev_hash.as_ref() != context.previous_hash.as_slice() {
            return Err("stale-chain-tip");
        }
        if request.nbits != context.bits {
            return Err("invalid-nbits");
        }
        if (request.version ^ context.version) & !VERSION_ROLLING_MASK != 0 {
            return Err("invalid-version");
        }
        if request.ntime_start < context.min_time || request.ntime_start > now.saturating_add(600) {
            return Err("invalid-ntime-start");
        }
        // #### PR #48: on Chipnet, the bits its start time needs: not normal
        // bits past the parent's time plus 1200 s, nor the limit's at or
        // before it while ASERT gives other bits (`BchTemplate::wrong_bits`).
        if context.wrong_bits(request.ntime_start) {
            return Err("invalid-nbits");
        }
        let prefix = request.coinbase_prefix.as_ref();
        // The script: the client's prefix, this channel's 16-byte prefix and
        // the client's rollable bytes (16, #### PR #56: or what this channel
        // was granted), at most 100 bytes.
        let script = prefix.len() + extranonce;
        if !prefix.starts_with(&context.height_push()) || script > 100 {
            return Err("invalid-coinbase-prefix");
        }
        if !matches!(request.coinbase_tx_version, 1 | 2) {
            return Err("invalid-coinbase-tx-version");
        }
        let outputs = request.coinbase_tx_outputs.as_ref();
        let mut head = request.coinbase_tx_version.to_le_bytes().to_vec();
        head.push(1);
        head.extend_from_slice(&[0; 32]);
        head.extend_from_slice(&u32::MAX.to_le_bytes());
        head.push(script as u8);
        head.extend_from_slice(prefix);
        let mut suffix = request.coinbase_tx_input_n_sequence.to_le_bytes().to_vec();
        suffix.extend_from_slice(outputs);
        suffix.extend_from_slice(&request.coinbase_tx_locktime.to_le_bytes());
        if head.len() + extranonce + suffix.len() < 65 {
            return Err("invalid-coinbase-tx");
        }
        let merkle_path = request
            .merkle_path
            .iter()
            .map(|hash| Hash::try_from(hash.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid-merkle-path")?;
        let Custom::CoinbaseOnly(rule) =
            declarator.redeem(request.token.as_ref(), &payout, std::time::Instant::now())?
        else {
            return Err("invalid-mining-job-token");
        };
        let outputs = parse_outputs(outputs).map_err(|_| "invalid-coinbase-tx-outputs")?;
        rule.check(&outputs)
            .map_err(|_| "invalid-coinbase-tx-outputs")?;
        // #### merge-mining v2: no token root without its declared leaves.
        super::jd::policy::check_no_root(&outputs).map_err(|_| "invalid-coinbase-tx-outputs")?;
        let id = self.next_custom_id()?;
        let template = Arc::new(BchTemplate::custom(
            &context,
            request.version,
            request.ntime_start,
            merkle_path.clone(),
        ));
        self.channels
            .get_mut(&request.channel_id)
            .ok_or("invalid-channel-id")?
            .install_custom(
                id,
                template,
                CoinbaseParts {
                    prefix: head,
                    suffix,
                    merkle_path,
                },
            )
            .map_err(|_| "invalid-coinbase-tx")?;
        Ok(id)
    }

    /// #### PR #42: the next custom job id, which carries 0x8000_0000.
    fn next_custom_id(&mut self) -> Result<u32, &'static str> {
        let count = self
            .custom_jobs
            .checked_add(1)
            .filter(|count| *count < CUSTOM_JOB_BIT);
        self.custom_jobs = count.ok_or("invalid-coinbase-tx")?;
        Ok(CUSTOM_JOB_BIT | self.custom_jobs)
    }

    // #### PR #42: SetCustomMiningJob on a declared job (Full-Template)
    // What: a declared token sets exactly the job the pool checked: its
    // parent (still the pool's), its bits, its version within the rolling
    // bits, a start time from the pool's mintime to 10 minutes ahead, and
    // the coinbase's version, script head, sequence, outputs, locktime and
    // merkle path as declared, around this channel's 16-byte prefix and 16
    // rollable bytes. The job's template holds the declared transactions,
    // so its blocks are whole.
    // Why: the declaration is what the pool (and its node) checked; the job
    // must not differ from it.
    // Look here if: a Full-Template client's custom jobs are refused after
    // DeclareMiningJob.Success.
    fn declared_job(
        &mut self,
        request: &SetCustomMiningJob,
        now: u32,
        declarator: &Declarator,
        payout: &str,
        context: &BchTemplate,
    ) -> Result<u32, &'static str> {
        let Custom::Declared(job) =
            declarator.redeem(request.token.as_ref(), payout, std::time::Instant::now())?
        else {
            return Err("invalid-mining-job-token");
        };
        let template = &job.template;
        if request.prev_hash.as_ref() != template.previous_hash.as_slice()
            || template.previous_hash != context.previous_hash
        {
            return Err("stale-chain-tip");
        }
        if request.nbits != template.bits {
            return Err("invalid-nbits");
        }
        if (request.version ^ template.version) & !VERSION_ROLLING_MASK != 0 {
            return Err("invalid-version");
        }
        if request.ntime_start < context.min_time || request.ntime_start > now.saturating_add(600) {
            return Err("invalid-ntime-start");
        }
        // #### PR #48: the bits its start time needs, as above.
        if template.wrong_bits(request.ntime_start) {
            return Err("invalid-nbits");
        }
        let shape = &job.shape;
        if request.coinbase_tx_version != shape.tx_version {
            return Err("invalid-coinbase-tx-version");
        }
        if request.coinbase_prefix.as_ref() != shape.head.as_slice() {
            return Err("invalid-coinbase-prefix");
        }
        if request.coinbase_tx_input_n_sequence != shape.sequence {
            return Err("invalid-coinbase-tx-input-n-sequence");
        }
        if request.coinbase_tx_outputs.as_ref() != shape.outputs_bytes.as_slice() {
            return Err("invalid-coinbase-tx-outputs");
        }
        if request.coinbase_tx_locktime != shape.locktime {
            return Err("invalid-coinbase-tx-locktime");
        }
        let merkle_path = request
            .merkle_path
            .iter()
            .map(|hash| Hash::try_from(hash.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid-merkle-path")?;
        if merkle_path != template.merkle_path() {
            return Err("invalid-merkle-path");
        }
        let channel = self
            .channels
            .get(&request.channel_id)
            .ok_or("invalid-channel-id")?;
        if shape.extranonce_len() != channel.extranonce_prefix.len() + channel.rollable() {
            return Err("invalid-coinbase-tx");
        }
        let id = self.next_custom_id()?;
        let template = Arc::new(template.with_header(request.version, request.ntime_start));
        self.channels
            .get_mut(&request.channel_id)
            .ok_or("invalid-channel-id")?
            .install_custom(
                id,
                template,
                CoinbaseParts {
                    prefix: shape.prefix.clone(),
                    suffix: shape.suffix.clone(),
                    merkle_path,
                },
            )
            .map_err(|_| "invalid-coinbase-tx")?;
        Ok(id)
    }

    /// #### PR #40
    /// Makes this connection a public pool's: each channel pays the address
    /// its user connects with, and the operator's fee address is added.
    /// #### merge-mining v2: a public pool's session, whose channels carry
    /// the empty commitment.
    pub fn is_public(&self) -> bool {
        self.public.is_some()
    }

    pub fn set_public(&mut self, public: Option<super::payout::PublicPool>) {
        self.public = public;
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        &mut self,
        request: u32,
        kind: ChannelKind,
        rate: f32,
        maximum: Hash,
        extra: u16,
        identity: &str,
    ) -> Result<Vec<SerializedFrame>, String> {
        // #### PR #42: a work-selection connection's extended channels are
        // custom-only, with 16 rollable bytes; its standard channels are
        // refused, as the spec names.
        let custom_only = self.work_selection && kind == ChannelKind::Extended;
        // #### PR #56: a Job Declaration lane takes the current plan's
        // layout, or the last plan's while none is current; a compact lane's
        // device rolls 4 bytes.
        let compact = self.lanes.as_ref().is_some_and(|lanes| {
            self.current
                .as_ref()
                .and_then(|(_, _, template)| template.jd_plan())
                .map_or_else(
                    || lanes.compact.load(Ordering::Relaxed),
                    |plan| plan.compact,
                )
        });
        let rollable = if custom_only {
            self.declarator
                .as_ref()
                .map_or(JD_ROLLABLE, |declarator| declarator.grant())
        } else if compact {
            COMPACT_DEVICE_EXTRANONCE
        } else {
            DEVICE_EXTRANONCE_SIZE
        };
        let error = if self.work_selection && kind == ChannelKind::Standard {
            Some("standard-channels-not-supported-for-custom-work")
        } else if !rate.is_finite() || rate < 0.0 {
            Some("invalid-nominal-hashrate")
        } else if extra as usize > rollable {
            Some("unsupported-min-extranonce-size")
        } else if maximum == [0; 32] {
            Some("max-target-out-of-range")
        } else if self.channels.len()
            >= if self.grouping {
                MAX_GROUPED_CHANNELS
            } else {
                MAX_CHANNELS
            }
            || self.next_channel >= u32::MAX - 1
        {
            // #### PR #42: a grouped connection carries up to 256 channels.
            Some("channel-capacity-exhausted")
        } else if self.current.is_none() {
            Some("no-template-available")
        } else {
            None
        };
        if let Some(code) = error {
            return Ok(vec![open_error(request, code)?]);
        }
        let (_, _, template) = self.current.as_ref().unwrap();
        if !meets_target(&template.target, &maximum) {
            return Ok(vec![open_error(request, "max-target-out-of-range")?]);
        }
        // #### PR #40: in a public pool, the user's own address; ####
        // merge-mining v2: and their donation choice.
        let login = self
            .public
            .as_ref()
            .map(|_| super::payout::identity_login(self.network, identity));
        // #### one donation, the higher rate: the donation's channel
        // What: on a public pool, a channel whose login pays the Pickaxe
        // donation's own address (the SV1 adapter's donation channel at a
        // remote pool, on the device's own connection) mines donation work
        // only: every job pays the donation whole, with no fee, and its
        // accepted work is the connection's verified own donation. It
        // neither fixes nor checks the connection's donation choice, its
        // login's options are ignored, and it never names the device.
        // Why: the miner's own Pickaxe donates there; the server credits
        // that work instead of taking the donation a second time.
        // Look here if: an adapter's donation channel is refused with
        // donation-mismatch, or its work is charged a fee.
        let pays_donation = matches!(
            &login,
            Some(Ok(login)) if super::payout::is_donation(self.network, &login.payout)
        );
        if identity != super::sv1::LOCAL_IDENTITY && !pays_donation {
            self.identity = Some(identity.to_owned());
        }
        let (payout, donation) = match login {
            Some(Ok(login)) => (login.payout, login.donation),
            Some(Err(error)) if error == super::payout::DONATION_OPTION_ERROR => {
                return Ok(vec![open_error(request, "invalid-donation")?]);
            }
            Some(Err(_)) => return Ok(vec![open_error(request, "unknown-user")?]),
            None => (self.payout.clone(), None),
        };
        if self.public.is_some() && !pays_donation {
            match self.donation_choice {
                Some(choice) if choice != donation => {
                    return Ok(vec![open_error(request, "donation-mismatch")?]);
                }
                _ => self.donation_choice = Some(donation),
            }
        }
        self.next_channel += 1;
        // Every device starts at the configured share target; vardiff moves
        // it from there.
        let mut channel = Channel::new(
            self.next_channel,
            kind,
            self.share_target,
            self.salt,
            self.network,
            &payout,
        )?;
        if let Some(public) = &self.public {
            channel.set_operator(Some(&public.address))?;
            // #### one donation, the higher rate: a miner channel's first
            // job carries the connection's rate with this login's choice
            // (see `public_donation`); a donation channel's is donation work.
            if pays_donation {
                channel.set_pays_donation();
            } else {
                channel.set_donation(Some(self.miner_donation()));
            }
        }
        if let Some(lanes) = &self.lanes {
            channel.set_lane(lanes.next.fetch_add(1, Ordering::Relaxed), compact);
        }
        if custom_only {
            channel.set_custom_only(rollable);
            let (_, _, template) = self.current.as_ref().unwrap();
            channel.settle(&template.target, None, &maximum);
            let frame = mining(Mining::OpenExtendedMiningChannelSuccess(
                OpenExtendedMiningChannelSuccess {
                    request_id: request,
                    channel_id: channel.id,
                    target: (&channel.target).into(),
                    extranonce_size: rollable as u16,
                    extranonce_prefix: channel.extranonce_prefix.as_slice().try_into().unwrap(),
                    group_channel_id: 0,
                },
            ))?;
            self.maximum_targets.insert(channel.id, maximum);
            self.channels.insert(channel.id, channel);
            return Ok(vec![frame]);
        }
        let (id, generation, template) = self.current.as_ref().unwrap();
        // #### PR #42: a new channel starts with the token floor too (see
        // `Channel::settle`).
        let tokens = channel.floor_for(template);
        channel.settle(&template.target, tokens.as_ref(), &maximum);
        channel.install_with_payout(*id, *generation, template.clone(), self.payout_policy)?;
        if let Some(reason) = channel.take_token_refusal() {
            self.token_refusal = Some(reason);
            // BCH alone: no token floor (see `Channel::drop_token_floor`).
            channel.drop_token_floor(&maximum);
        }
        let target = channel.target;
        // #### PR #42: an extended channel joins its payout's group, made
        // with the next id when it is the first; the group is announced here
        // and never moved (no SetGroupChannel).
        let group = if self.grouping && kind == ChannelKind::Extended {
            let existing = self
                .groups
                .iter()
                .find(|(_, group)| group.payout == payout)
                .map(|(id, _)| *id);
            let group = match existing {
                Some(group) => group,
                None => {
                    self.next_channel += 1;
                    self.groups.insert(
                        self.next_channel,
                        Group {
                            payout: payout.clone(),
                            members: BTreeSet::new(),
                        },
                    );
                    self.next_channel
                }
            };
            if let Some(members) = self.groups.get_mut(&group) {
                members.members.insert(channel.id);
            }
            group
        } else {
            0
        };
        let mut frames = vec![match kind {
            ChannelKind::Standard => mining(Mining::OpenStandardMiningChannelSuccess(
                OpenStandardMiningChannelSuccess {
                    request_id: request,
                    channel_id: channel.id,
                    target: (&target).into(),
                    extranonce_prefix: channel.extranonce_prefix.as_slice().try_into().unwrap(),
                    group_channel_id: 0,
                },
            ))?,
            ChannelKind::Extended => mining(Mining::OpenExtendedMiningChannelSuccess(
                OpenExtendedMiningChannelSuccess {
                    request_id: request,
                    channel_id: channel.id,
                    target: (&target).into(),
                    extranonce_size: channel.rollable() as u16,
                    extranonce_prefix: channel.extranonce_prefix.as_slice().try_into().unwrap(),
                    group_channel_id: group,
                },
            ))?,
        }];
        frames.extend(job_frames(&channel, false)?);
        self.maximum_targets.insert(channel.id, maximum);
        let id = channel.id;
        self.channels.insert(channel.id, channel);
        frames.extend(self.disclose(id)?);
        Ok(frames)
    }

    #[allow(clippy::too_many_arguments)]
    fn submit(
        &mut self,
        share: Share<'_>,
        kind: ChannelKind,
        now: u32,
        frames: &mut Vec<SerializedFrame>,
        blocks: &mut Vec<ValidatedShare>,
        token_wins: &mut Vec<TokenWin>,
        forward: &mut Vec<ForwardShare>,
        header_wins: &mut Vec<ValidatedShare>,
        accepted_work: &mut Option<(Hash, BchPayout)>,
    ) -> Result<ShareEvent, String> {
        let id = share.channel_id;
        let sequence = share.sequence;
        let job_id = share.job_id;
        let result = self
            .channels
            .get_mut(&id)
            .ok_or("invalid-channel-id")
            .and_then(|channel| {
                if channel.kind != kind {
                    return Err("invalid-channel-type");
                }
                channel.check(share, now)
            });
        match result {
            Ok(share) => {
                let event = ShareEvent::Accepted(share.share_target);
                // #### PR #48: only this server's own jobs carry its BCH
                // payout policy. A Job Declaration job's coinbase is the
                // client's (its payout here is a placeholder), so its work
                // repays no quota and is not counted as anyone's here.
                if !share.template.is_header_only() && !share.template.is_declared() {
                    // #### one donation, the higher rate: on a public pool,
                    // each share on the server's own jobs counts toward the
                    // connection's own donation measure: a donation
                    // channel's as verified donation work, a miner
                    // channel's with what the credit spared it. A donation
                    // channel's share repays no quota of the miner's jobs.
                    let channel = self.channels.get(&id);
                    let pays_donation = channel.is_some_and(Channel::pays_donation);
                    if self.public.is_some() && !channel.is_some_and(|channel| channel.custom_only)
                    {
                        let forgone = u16::from(self.effective_donation())
                            .saturating_sub(u16::from(share.payout.donation));
                        self.own_donation.record(
                            now,
                            super::telemetry::expected_hashes(&share.share_target),
                            pays_donation,
                            forgone,
                        );
                    }
                    if pays_donation {
                        self.donation_share = Some(share.share_target);
                    } else {
                        *accepted_work = Some((share.share_target, share.payout));
                    }
                }
                // #### PR #42: best share from the carried hash
                // What: the best-share difficulty uses the hash the channel's
                // check computed, instead of hashing the header again.
                // Why: one double SHA-256 fewer per share, more than the
                // token check costs.
                // Look here if: best-share difficulties disagree with the
                // shares' headers.
                self.share_difficulty = Some(super::telemetry::share_difficulty(&share.hash));
                // #### end PR #42 ####
                let work = share_work(&share.share_target);
                self.accepted = self.accepted.saturating_add(1);
                frames.push(mining(Mining::SubmitSharesSuccess(SubmitSharesSuccess {
                    channel_id: id,
                    last_sequence_number: sequence,
                    new_submits_accepted_count: 1,
                    new_shares_sum: work,
                }))?);
                // #### PR #42: token wins leave with the responses
                // What: every accepted share that wins merge-mined tokens is
                // handed on as a `TokenWin`, whether or not it is also a
                // block (a block also goes to `blocks`, as before).
                // Why: Case A tokens win on shares that are not blocks; only
                // blocks used to leave this function.
                // Look here if: a winning share is acknowledged but its
                // token win never reaches the server.
                if let Some(win) = share.token_win() {
                    // #### merge-mining v2: the miner gets each won entry's
                    // proof, when it asked for the disclosure.
                    if self.merge_disclosure {
                        frames.extend(win_proofs(id, job_id, &win)?);
                    }
                    token_wins.push(win);
                }
                // #### end PR #42 ####
                if let Some(share) = share.forward.clone() {
                    forward.push(share);
                }
                if share.block {
                    blocks.push(share);
                } else if share.header_win {
                    header_wins.push(share);
                }
                Ok(event)
            }
            Err(code) => {
                self.rejected = self.rejected.saturating_add(1);
                frames.push(mining(Mining::SubmitSharesError(SubmitSharesError {
                    channel_id: id,
                    sequence_number: sequence,
                    error_code: code.try_into().unwrap(),
                }))?);
                Ok(ShareEvent::Rejected(code))
            }
        }
    }
}

fn job_frames(channel: &Channel, immediate: bool) -> Result<Vec<SerializedFrame>, String> {
    let job = channel.job().ok_or("channel has no job")?;
    frames_for(channel.id, channel.kind, job, immediate)
}

/// #### PR #42: one job frame for a group's members when they hold the same
/// job (its id, coinbase parts, merkle path, version and parent), activated
/// at once only when every member's would be; otherwise each member's own.
fn group_frames(
    group: u32,
    members: &BTreeSet<u32>,
    channels: &BTreeMap<u32, Channel>,
    immediates: &BTreeMap<u32, bool>,
) -> Result<Vec<SerializedFrame>, String> {
    let members: Vec<&Channel> = members.iter().filter_map(|id| channels.get(id)).collect();
    let first = members
        .first()
        .and_then(|channel| channel.job())
        .ok_or("group has no job")?;
    let shared = members.iter().all(|channel| {
        channel.job().is_some_and(|job| {
            job.id == first.id
                && job.parts.prefix == first.parts.prefix
                && job.parts.suffix == first.parts.suffix
                && job.parts.merkle_path == first.parts.merkle_path
                && job.template.version == first.template.version
                && job.template.previous_hash == first.template.previous_hash
        })
    });
    let immediate = |channel: &Channel| immediates.get(&channel.id).copied().unwrap_or(false);
    if !shared {
        let mut frames = Vec::new();
        for channel in members {
            frames.extend(job_frames(channel, immediate(channel))?);
        }
        return Ok(frames);
    }
    frames_for(
        group,
        ChannelKind::Extended,
        first,
        members.iter().all(|channel| immediate(channel)),
    )
}

/// A job's frames addressed to `channel_id` (a channel, or a group).
fn frames_for(
    channel_id: u32,
    kind: ChannelKind,
    job: &Job,
    immediate: bool,
) -> Result<Vec<SerializedFrame>, String> {
    let new = match kind {
        ChannelKind::Standard => Mining::NewMiningJob(NewMiningJob {
            channel_id,
            job_id: job.id,
            ntime_start: Sv2Option::new(immediate.then_some(job.template.current_time)),
            version: job.template.version,
            merkle_root: (&job.standard_coinbase.merkle_root).into(),
        }),
        ChannelKind::Extended => Mining::NewExtendedMiningJob(NewExtendedMiningJob {
            channel_id,
            job_id: job.id,
            ntime_start: Sv2Option::new(immediate.then_some(job.template.current_time)),
            version: job.template.version,
            // #### PR #42: none for a token that fixes the version.
            version_rolling_allowed: job.template.version_mask() != 0,
            merkle_path: job
                .parts
                .merkle_path
                .iter()
                .map(|hash| hash.into())
                .collect::<Vec<_>>()
                .try_into()
                .map_err(|_| "merkle path too long")?,
            coinbase_tx_prefix: job
                .parts
                .prefix
                .as_slice()
                .try_into()
                .map_err(|_| "coinbase prefix too long")?,
            coinbase_tx_suffix: job
                .parts
                .suffix
                .as_slice()
                .try_into()
                .map_err(|_| "coinbase suffix too long")?,
        }),
    };
    let mut frames = vec![mining(new)?];
    if !immediate {
        frames.push(mining(Mining::SetNewPrevHash(SetNewPrevHash {
            channel_id,
            job_id: job.id,
            prev_hash: (&job.template.previous_hash).into(),
            ntime_start: job.template.current_time,
            nbits: job.template.bits,
        }))?);
    }
    Ok(frames)
}

fn open_error(id: u32, code: &'static str) -> Result<SerializedFrame, String> {
    mining(Mining::OpenMiningChannelError(OpenMiningChannelError {
        request_id: id,
        error_code: code.try_into().unwrap(),
    }))
}

/// #### merge-mining v2: a frame of extension `extension` (with the
/// channel bit when `channel`) and message `kind` around `payload`, as the
/// SV2 framing writes it: the type (u16 LE), the message type, the length
/// (u24 LE) and the payload.
pub fn extension_frame(
    extension: u16,
    kind: u8,
    channel: bool,
    payload: &[u8],
) -> Result<SerializedFrame, String> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|length| *length < 1 << 24)
        .ok_or("SV2 extension message exceeds frame size")?;
    let mut bytes = Vec::with_capacity(6 + payload.len());
    let extension = extension | if channel { 0x8000 } else { 0 };
    bytes.extend_from_slice(&extension.to_le_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(&length.to_le_bytes()[..3]);
    bytes.extend_from_slice(payload);
    SerializedFrame::from_bytes(bytes).map_err(|_| "SV2 frame encoding failed".into())
}

/// #### merge-mining v2: a WinProof frame for each entry `win` won.
fn win_proofs(channel: u32, job: u32, win: &TokenWin) -> Result<Vec<SerializedFrame>, String> {
    use super::merge::disclosure::{WinProof, EXTENSION, WIN_PROOF};
    let mut frames = Vec::new();
    for entry in &win.entries {
        let Some(item) = win.aux.entries.get(usize::from(*entry)) else {
            continue;
        };
        let Ok(proof) = win.proof(*entry).and_then(|proof| proof.to_bytes()) else {
            continue;
        };
        let message = WinProof {
            channel,
            job,
            category: item.token.category,
            proof,
            split: item.split,
        };
        frames.push(extension_frame(
            EXTENSION,
            WIN_PROOF,
            true,
            &message.encode(),
        )?);
    }
    Ok(frames)
}

pub fn mining(msg: Mining<'_>) -> Result<SerializedFrame, String> {
    let kind = msg.message_type();
    let channel = msg.channel_bit();
    encoded(msg, kind, channel)
}

pub fn encoded<T: Serialize + GetSize>(
    msg: T,
    kind: u8,
    channel: bool,
) -> Result<SerializedFrame, String> {
    let frame = MessageFrame::from_message(msg, kind, 0, channel)
        .map_err(|_| "SV2 message exceeds frame size")?;
    let mut bytes = vec![0; frame.encoded_length()];
    frame
        .encode_into(&mut bytes)
        .map_err(|_| "SV2 message encoding failed")?;
    SerializedFrame::from_bytes(bytes).map_err(|_| "SV2 frame encoding failed".into())
}

#[cfg(test)]
mod tests {
    use super::super::{
        template::{compact_target, double_sha256},
        template_tests::{payout, rpc_template},
    };
    use super::*;
    use stratum_core::bitcoin::{consensus, Block};
    fn setup(flags: u32) -> SerializedFrame {
        encoded(
            SetupConnection {
                protocol: Protocol::MiningProtocol,
                min_version: 2,
                max_version: 2,
                flags,
                endpoint_host: "localhost".try_into().unwrap(),
                endpoint_port: 3336,
                vendor: "test".try_into().unwrap(),
                hardware_version: "".try_into().unwrap(),
                firmware: "".try_into().unwrap(),
                device_id: "".try_into().unwrap(),
            },
            0,
            false,
        )
        .unwrap()
    }
    fn session() -> MiningSession {
        let mut session =
            MiningSession::new(MiningNetwork::Chipnet, payout(), [7; 12], [255; 32]).unwrap();
        session
            .set_job(
                9,
                3,
                Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
            )
            .unwrap();
        session
    }
    fn open() -> SerializedFrame {
        mining(Mining::OpenStandardMiningChannel(
            OpenStandardMiningChannel {
                request_id: 1,
                user_identity: "device".try_into().unwrap(),
                nominal_hash_rate: 1e12,
                max_target: (&[255; 32]).into(),
            },
        ))
        .unwrap()
    }
    #[test]
    fn standard_wire_device_solves_cpu_block_and_rejects_replay_and_stale() {
        let mut server = session();
        assert!(server.receive(open(), 1700000010).is_err());
        let result = server.receive(setup(5), 1700000010).unwrap();
        assert_eq!(result.frames[0].header().msg_type(), 1);
        let mut result = server.receive(open(), 1700000010).unwrap();
        assert_eq!(result.frames.len(), 3);
        let success: OpenStandardMiningChannelSuccess =
            binary_sv2::from_bytes(result.frames[0].payload()).unwrap();
        let id = success.channel_id;
        let job: NewMiningJob = binary_sv2::from_bytes(result.frames[1].payload()).unwrap();
        let jobid = job.job_id;
        let root: Hash = job.merkle_root.as_ref().try_into().unwrap();
        let version = job.version;
        assert!(job.is_future());
        let prev: SetNewPrevHash = binary_sv2::from_bytes(result.frames[2].payload()).unwrap();
        let mut header = [0; 80];
        header[..4].copy_from_slice(&version.to_le_bytes());
        header[4..36].copy_from_slice(prev.prev_hash.as_ref());
        header[36..68].copy_from_slice(&root);
        header[68..72].copy_from_slice(&prev.ntime_start.to_le_bytes());
        header[72..76].copy_from_slice(&prev.nbits.to_le_bytes());
        let target = super::super::template::compact_target(prev.nbits).unwrap();
        let nonce = (0u32..1000)
            .find(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                meets_target(&double_sha256(&header), &target)
            })
            .unwrap();
        let submit = |sequence| {
            mining(Mining::SubmitSharesStandard(SubmitSharesStandard {
                channel_id: id,
                sequence_number: sequence,
                job_id: jobid,
                nonce,
                ntime: 1700000010,
                version,
            }))
            .unwrap()
        };
        // Retain an in-flight solution through an immediate same-tip refresh.
        let mut refresh = rpc_template();
        refresh["curtime"] = serde_json::json!(1700000025);
        let mut update = server
            .set_job(10, 4, Arc::new(BchTemplate::from_rpc(&refresh).unwrap()))
            .unwrap();
        assert_eq!(update.len(), 1, "same-tip work must not reset the parent");
        let next: NewMiningJob = binary_sv2::from_bytes(update[0].payload()).unwrap();
        assert!(!next.is_future());
        assert_eq!(next.ntime_start.into_inner(), Some(1700000025));
        let result = server.receive(submit(0), 1700000010).unwrap();
        assert_eq!(result.blocks.len(), 1);
        let solved = &result.blocks[0];
        assert_eq!(solved.header, header);
        let block = solved
            .template
            .block(&solved.coinbase, solved.header)
            .unwrap();
        let decoded: Block = consensus::deserialize(&block).unwrap();
        assert!(decoded.check_merkle_root());
        assert_eq!(
            server.receive(submit(1), 1700000010).unwrap().frames[0]
                .header()
                .msg_type(),
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR
        );
        let mut changed_tip = rpc_template();
        changed_tip["previousblockhash"] = serde_json::json!("cd".repeat(32));
        server
            .set_job(
                11,
                5,
                Arc::new(BchTemplate::from_rpc(&changed_tip).unwrap()),
            )
            .unwrap();
        assert!(server
            .receive(submit(2), 1700000010)
            .unwrap()
            .blocks
            .is_empty());
    }
    #[test]
    fn setup_flags_and_channel_kind_are_enforced() {
        let mut server = session();
        assert_eq!(
            server.receive(setup(2), 1700000010).unwrap().frames[0]
                .header()
                .msg_type(),
            2
        );
        assert!(server.setup_flags.is_none());
        server.receive(setup(1), 1700000010).unwrap();
        assert!(server.receive(setup(1), 1700000010).is_err());
        let extended = mining(Mining::OpenExtendedMiningChannel(
            OpenExtendedMiningChannel {
                request_id: 4,
                user_identity: "device".try_into().unwrap(),
                nominal_hash_rate: 1.0,
                max_target: (&[255; 32]).into(),
                min_extranonce_size: 8,
            },
        ))
        .unwrap();
        assert_eq!(
            server.receive(extended, 1700000010).unwrap().frames[0]
                .header()
                .msg_type(),
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR
        );
    }

    #[test]
    fn retarget_sends_the_target_then_a_fresh_job_on_the_same_block() {
        let mut server = session();
        server.share_target = super::super::template::compact_target(0x1b0ffff0).unwrap();
        let mut template = (*server.current.as_ref().unwrap().2).clone();
        template.target = super::super::template::compact_target(0x1a00ffff).unwrap();
        server.set_job(10, 4, Arc::new(template)).unwrap();
        server.receive(setup(5), 1700000010).unwrap();
        server.receive(open(), 1700000010).unwrap();
        let id = *server.channels.keys().next().unwrap();
        assert_eq!(server.channels[&id].target, server.share_target);
        // A silent device: a minute without shares eases its target.
        server.channels.get_mut(&id).unwrap().vardiff_window(60, 0);
        let mut next_id = 10;
        let mut frames = server.retarget(&mut next_id).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].header().msg_type(), MESSAGE_TYPE_SET_TARGET);
        assert_eq!(frames[1].header().msg_type(), MESSAGE_TYPE_NEW_MINING_JOB);
        let job: NewMiningJob = binary_sv2::from_bytes(frames[1].payload()).unwrap();
        // Same block: an immediate job, with no SetNewPrevHash.
        assert!(!job.is_future());
        assert_eq!(job.job_id, 11);
        assert_eq!(next_id, 11);
        let channel = &server.channels[&id];
        assert_eq!(channel.job().unwrap().id, 11);
        assert!(meets_target(&server.share_target, &channel.target));
        assert_ne!(channel.target, server.share_target);
        // Nothing more until the next window.
        assert!(server.retarget(&mut next_id).unwrap().is_empty());
    }

    #[test]
    fn easy_network_work_is_not_discarded_by_asic_share_difficulty() {
        let mut server = session();
        server.share_target = super::super::template::compact_target(0x1b0ffff0).unwrap();
        server.receive(setup(5), 1700000010).unwrap();
        let mut replies = server.receive(open(), 1700000010).unwrap().frames;
        let opened: OpenStandardMiningChannelSuccess =
            binary_sv2::from_bytes(replies[0].payload()).unwrap();
        let initial = server.current.as_ref().unwrap().2.target;
        assert_eq!(opened.target.as_ref(), initial);
        let id = opened.channel_id;
        // The next template becomes easier; notify target before publishing work.
        let mut template = (*server.current.as_ref().unwrap().2).clone();
        template.target = [254; 32];
        let replies = server.set_job(11, 4, Arc::new(template.clone())).unwrap();
        assert_eq!(replies[0].header().msg_type(), MESSAGE_TYPE_SET_TARGET);
        assert_eq!(server.channels[&id].target, template.target);
        // Back on normal block work, the share difficulty returns too.
        let mut normal = template.clone();
        normal.target = super::super::template::compact_target(0x1a00ffff).unwrap();
        let replies = server.set_job(12, 5, Arc::new(normal)).unwrap();
        assert_eq!(replies[0].header().msg_type(), MESSAGE_TYPE_SET_TARGET);
        assert_eq!(server.channels[&id].target, server.share_target);
        // A firmware target ceiling cannot silently hide a later network win.
        server.maximum_targets.insert(id, initial);
        template.target = [255; 32];
        assert!(server.set_job(13, 6, Arc::new(template)).is_err());

        let mut server = session();
        server.receive(setup(5), 1700000010).unwrap();
        let request = mining(Mining::OpenStandardMiningChannel(
            OpenStandardMiningChannel {
                request_id: 2,
                user_identity: "device".try_into().unwrap(),
                nominal_hash_rate: 1e12,
                max_target: (&[1; 32]).into(),
            },
        ))
        .unwrap();
        assert_eq!(
            server.receive(request, 1700000010).unwrap().frames[0]
                .header()
                .msg_type(),
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR
        );
    }

    const NOW: u32 = 1700000010;
    /// #### PR #42: the extranonce test devices roll on extended channels.
    const DEVICE_EXTRANONCE: [u8; DEVICE_EXTRANONCE_SIZE] = [0x23; DEVICE_EXTRANONCE_SIZE];

    /// #### PR #42: the test template merge-mining the test token (its
    /// target `bits`), with block difficulty no test share reaches when
    /// `hard`.
    fn token_template(bits: u32, hard: bool) -> BchTemplate {
        use super::super::merge::{
            registry::TEST_TOKEN,
            set::{SetEntry, TokenSet, TokenState},
        };
        let state = TokenState::new([0x22; 32], [0x33; 32], bits).unwrap();
        let entries = vec![SetEntry::new(&TEST_TOKEN, state)];
        let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        if hard {
            template.target = compact_target(0x1a00ffff).unwrap();
        }
        template.commit(Arc::new(
            TokenSet::new(MiningNetwork::Chipnet, entries, 1).unwrap(),
        ));
        template
    }

    /// #### PR #42: what a device hashes for its first job: the header
    /// (nonce still open) and, on an extended channel, the coinbase it
    /// assembled from the job's prefix and suffix.
    struct DeviceJob {
        channel: u32,
        job: u32,
        version: u32,
        header: [u8; 80],
        coinbase: Vec<u8>,
    }

    /// #### PR #42: opens a channel of `kind` and builds its first job as a
    /// device does, from the frames alone.
    fn open_device(server: &mut MiningSession, kind: ChannelKind) -> DeviceJob {
        open_device_as(server, kind, "device")
    }

    /// #### one donation, the higher rate: `open_device` under `identity`
    /// (an extended channel's; a standard channel's is always "device").
    fn open_device_as(server: &mut MiningSession, kind: ChannelKind, identity: &str) -> DeviceJob {
        let request = match kind {
            ChannelKind::Standard => open(),
            ChannelKind::Extended => mining(Mining::OpenExtendedMiningChannel(
                OpenExtendedMiningChannel {
                    request_id: 2,
                    user_identity: identity.try_into().unwrap(),
                    nominal_hash_rate: 1e12,
                    max_target: (&[255; 32]).into(),
                    min_extranonce_size: DEVICE_EXTRANONCE_SIZE as u16,
                },
            ))
            .unwrap(),
        };
        let mut frames = server.receive(request, NOW).unwrap().frames;
        assert_eq!(frames.len(), 3);
        let (previous, time, bits) = {
            let prev: SetNewPrevHash = binary_sv2::from_bytes(frames[2].payload()).unwrap();
            let previous: Hash = prev.prev_hash.as_ref().try_into().unwrap();
            (previous, prev.ntime_start, prev.nbits)
        };
        let (channel, job, version, root, coinbase) = match kind {
            ChannelKind::Standard => {
                let channel = {
                    let success: OpenStandardMiningChannelSuccess =
                        binary_sv2::from_bytes(frames[0].payload()).unwrap();
                    success.channel_id
                };
                let job: NewMiningJob = binary_sv2::from_bytes(frames[1].payload()).unwrap();
                let root: Hash = job.merkle_root.as_ref().try_into().unwrap();
                (channel, job.job_id, job.version, root, Vec::new())
            }
            ChannelKind::Extended => {
                let (channel, prefix) = {
                    let success: OpenExtendedMiningChannelSuccess =
                        binary_sv2::from_bytes(frames[0].payload()).unwrap();
                    (
                        success.channel_id,
                        success.extranonce_prefix.as_ref().to_vec(),
                    )
                };
                let job: NewExtendedMiningJob =
                    binary_sv2::from_bytes(frames[1].payload()).unwrap();
                let mut coinbase = job.coinbase_tx_prefix.as_ref().to_vec();
                coinbase.extend(prefix);
                coinbase.extend(DEVICE_EXTRANONCE);
                coinbase.extend(job.coinbase_tx_suffix.as_ref());
                let (id, version) = (job.job_id, job.version);
                let mut root = double_sha256(&coinbase);
                for sibling in job.merkle_path.into_inner() {
                    root = double_sha256(&[&root[..], sibling.as_ref()].concat());
                }
                (channel, id, version, root, coinbase)
            }
        };
        let mut header = [0; 80];
        header[..4].copy_from_slice(&version.to_le_bytes());
        header[4..36].copy_from_slice(&previous);
        header[36..68].copy_from_slice(&root);
        header[68..72].copy_from_slice(&time.to_le_bytes());
        header[72..76].copy_from_slice(&bits.to_le_bytes());
        DeviceJob {
            channel,
            job,
            version,
            header,
            coinbase,
        }
    }

    /// #### PR #42: the device's share of `nonce` on its first job.
    fn submit_share(
        device: &DeviceJob,
        kind: ChannelKind,
        sequence: u32,
        nonce: u32,
    ) -> SerializedFrame {
        mining(match kind {
            ChannelKind::Standard => Mining::SubmitSharesStandard(SubmitSharesStandard {
                channel_id: device.channel,
                sequence_number: sequence,
                job_id: device.job,
                nonce,
                ntime: NOW,
                version: device.version,
            }),
            ChannelKind::Extended => Mining::SubmitSharesExtended(SubmitSharesExtended {
                channel_id: device.channel,
                sequence_number: sequence,
                job_id: device.job,
                nonce,
                ntime: NOW,
                version: device.version,
                extranonce: DEVICE_EXTRANONCE.as_slice().try_into().unwrap(),
            }),
        })
        .unwrap()
    }

    #[test]
    fn validated_work_keeps_the_old_jobs_policy_and_ignores_replays() {
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut server = session();
            server.receive(setup(4), NOW).unwrap();
            let device = open_device(&mut server, kind);
            let original = server.channels[&device.channel].job().unwrap().payout;
            let changed = BchPayout {
                donation: "9".parse().unwrap(),
                donation_work: true,
                ..Default::default()
            };
            server
                .set_job_with_payout(
                    10,
                    4,
                    Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap()),
                    changed,
                )
                .unwrap();
            let result = server
                .receive(submit_share(&device, kind, 0, 0), NOW)
                .unwrap();
            assert_eq!(result.accepted_work, Some(([255; 32], original)));
            let replay = server
                .receive(submit_share(&device, kind, 1, 0), NOW)
                .unwrap();
            assert!(replay.accepted_work.is_none());
            assert!(matches!(
                replay.share_event,
                Some(ShareEvent::Rejected("duplicate-share"))
            ));
        }
    }

    // #### PR #42: token wins leave with the responses
    #[test]
    fn responses_carry_token_wins_for_standard_and_extended_submits() {
        // A target of 2^248: about one share in 256 wins; no share is a
        // block.
        let template = Arc::new(token_template(0x2001_0000, true));
        let target = *template.tokens().unwrap().easiest();
        for kind in [ChannelKind::Standard, ChannelKind::Extended] {
            let mut server =
                MiningSession::new(MiningNetwork::Chipnet, payout(), [7; 12], [255; 32]).unwrap();
            server.set_job(9, 3, template.clone()).unwrap();
            server.receive(setup(4), NOW).unwrap();
            let mut device = open_device(&mut server, kind);
            let mut wins = 0;
            for nonce in 0..2_000u32 {
                device.header[76..].copy_from_slice(&nonce.to_le_bytes());
                let hash = double_sha256(&device.header);
                let mut responses = server
                    .receive(submit_share(&device, kind, nonce, nonce), NOW)
                    .unwrap();
                assert_eq!(
                    responses.frames[0].header().msg_type(),
                    MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS
                );
                assert!(responses.blocks.is_empty());
                if !meets_target(&hash, &target) {
                    assert!(responses.token_wins.is_empty());
                    continue;
                }
                wins += 1;
                assert_eq!(responses.token_wins.len(), 1);
                let win = responses.token_wins.remove(0);
                assert_eq!(win.entries, [0]);
                assert_eq!((win.header, win.hash), (device.header, hash));
                // The coinbase is the one the device hashed, and its output
                // 0 is the job's commitment.
                if kind == ChannelKind::Extended {
                    assert_eq!(win.coinbase, device.coinbase);
                }
                let head = 47 + usize::from(win.coinbase[41]);
                assert_eq!(win.coinbase[head..head + 53], win.aux.output);
                win.proof(0).unwrap();
            }
            assert!(wins > 0, "{kind:?}");
        }
        // A share that is a block and wins tokens takes both paths.
        let template = Arc::new(token_template(0x2001_0000, false));
        let mut server =
            MiningSession::new(MiningNetwork::Chipnet, payout(), [7; 12], [255; 32]).unwrap();
        server.set_job(9, 3, template.clone()).unwrap();
        server.receive(setup(5), NOW).unwrap();
        let mut device = open_device(&mut server, ChannelKind::Standard);
        let nonce = (0u32..)
            .find(|nonce| {
                device.header[76..].copy_from_slice(&nonce.to_le_bytes());
                meets_target(&double_sha256(&device.header), &template.target)
            })
            .unwrap();
        let hash = double_sha256(&device.header);
        let responses = server
            .receive(submit_share(&device, ChannelKind::Standard, 0, nonce), NOW)
            .unwrap();
        assert_eq!(responses.blocks.len(), 1);
        // A block wins the token only when it meets the token's target.
        if meets_target(&hash, &target) {
            assert_eq!(responses.token_wins.len(), 1);
            let win = &responses.token_wins[0];
            assert_eq!(win.entries, [0]);
            assert_eq!(win.coinbase, responses.blocks[0].coinbase.bytes);
        } else {
            assert!(responses.token_wins.is_empty());
        }
    }

    // #### PR #42: best share from the carried hash
    #[test]
    fn best_share_uses_the_carried_hash() {
        let mut server = session();
        server.receive(setup(5), NOW).unwrap();
        let mut device = open_device(&mut server, ChannelKind::Standard);
        for nonce in 0..8u32 {
            device.header[76..].copy_from_slice(&nonce.to_le_bytes());
            server
                .receive(
                    submit_share(&device, ChannelKind::Standard, nonce, nonce),
                    NOW,
                )
                .unwrap();
            let expected =
                super::super::telemetry::share_difficulty(&double_sha256(&device.header));
            assert_eq!(server.share_difficulty.take(), Some(expected));
        }
        // A rejected share (a repeat) records none.
        server
            .receive(submit_share(&device, ChannelKind::Standard, 8, 0), NOW)
            .unwrap();
        assert_eq!(server.share_difficulty.take(), None);
    }

    // #### PR #42: the share-target floor for merge-mined tokens
    #[test]
    fn new_channels_and_jobs_carry_the_token_floor() {
        // Vardiff's target: one share in 4,096 (2^244); the token's, eight
        // times easier (2^247), is the floor.
        let mut share_target = [0; 32];
        share_target[30] = 0x10;
        let floor = compact_target(0x2000_8000).unwrap();
        let tokens = token_template(0x2000_8000, true);
        let mut plain = BchTemplate::from_rpc(&rpc_template()).unwrap();
        plain.target = tokens.target;
        let mut server =
            MiningSession::new(MiningNetwork::Chipnet, payout(), [7; 12], share_target).unwrap();
        server.set_job(9, 3, Arc::new(tokens.clone())).unwrap();
        server.receive(setup(5), NOW).unwrap();
        // A new channel starts at the floor.
        let mut frames = server.receive(open(), NOW).unwrap().frames;
        let opened: OpenStandardMiningChannelSuccess =
            binary_sv2::from_bytes(frames[0].payload()).unwrap();
        assert_eq!(opened.target.as_ref(), floor);
        // Without tokens, SetTarget brings it back to vardiff's target
        // before the job...
        let mut frames = server.set_job(10, 4, Arc::new(plain)).unwrap();
        assert_eq!(frames.len(), 2);
        let set: SetTarget = binary_sv2::from_bytes(frames[0].payload()).unwrap();
        assert_eq!(set.target.as_ref(), share_target);
        // ...and with them again, to the floor, which the job carries.
        let mut frames = server.set_job(11, 5, Arc::new(tokens)).unwrap();
        let set: SetTarget = binary_sv2::from_bytes(frames[0].payload()).unwrap();
        assert_eq!(set.target.as_ref(), floor);
        let channel = server.channels.values().next().unwrap();
        assert_eq!(channel.job().unwrap().target, floor);
    }

    /// #### PR #42: a public pool's session that accepts Job Declaration,
    /// set up with `flags`, its declarator, and the miner's channel when
    /// `open` (its id and extranonce prefix).
    fn jd_session(flags: u32) -> (MiningSession, Arc<Declarator>, u32, Vec<u8>) {
        let declarator = Arc::new(super::super::jd::server::tests::declarator());
        let mut server = session();
        server.set_public(Some(declarator.public.clone()));
        server.set_declarator(Some(declarator.clone()));
        let reply = server.receive(setup(flags), NOW).unwrap();
        assert_eq!(reply.frames[0].header().msg_type(), 1, "setup accepted");
        let mut frames = server
            .receive(
                mining(Mining::OpenExtendedMiningChannel(
                    OpenExtendedMiningChannel {
                        request_id: 2,
                        user_identity: payout().as_str().try_into().unwrap(),
                        nominal_hash_rate: 1e12,
                        max_target: (&[255; 32]).into(),
                        min_extranonce_size: if flags & 0b010 != 0 {
                            JD_ROLLABLE as u16
                        } else {
                            DEVICE_EXTRANONCE_SIZE as u16
                        },
                    },
                ))
                .unwrap(),
                NOW,
            )
            .unwrap()
            .frames;
        let success: OpenExtendedMiningChannelSuccess =
            binary_sv2::from_bytes(frames[0].payload()).unwrap();
        let (channel, prefix) = (
            success.channel_id,
            success.extranonce_prefix.as_ref().to_vec(),
        );
        if flags & 0b010 != 0 {
            assert_eq!(success.extranonce_size, JD_ROLLABLE as u16);
            assert_eq!(
                frames.len(),
                1,
                "no job of the pool's on a custom-only channel"
            );
        }
        (server, declarator, channel, prefix)
    }

    /// The coinbase fields of a custom job, as a JD client sets them.
    struct CustomJob {
        token: Vec<u8>,
        version: u32,
        prev: Hash,
        ntime_start: u32,
        bits: u32,
        tx_version: u32,
        prefix: Vec<u8>,
        outputs: Vec<u8>,
        path: Vec<Hash>,
    }

    impl CustomJob {
        /// A job paying the pool's rule exactly, with a fresh token.
        fn valid(declarator: &Declarator) -> Self {
            let rule = super::super::jd::policy::PayoutRule::public_pool(
                MiningNetwork::Chipnet,
                &payout(),
                &declarator.public,
                Default::default(),
            )
            .unwrap();
            let (allocated, rates) = rule.allocated_outputs();
            let token = declarator
                .book
                .lock()
                .unwrap()
                .allocate(0, payout(), rates, false, std::time::Instant::now())
                .unwrap()
                .encode()
                .to_vec();
            let amounts = [304_734_375, 3_078_125, 4_687_500];
            let outputs: Vec<(u64, Vec<u8>)> = allocated
                .into_iter()
                .zip(amounts)
                .map(|((_, script), amount)| (amount, script))
                .collect();
            let mut prefix = vec![0x03, 0x15, 0xf9, 0x04];
            prefix.extend(b"jd");
            Self {
                token,
                version: 0x2000_0000,
                prev: [0xab; 32],
                ntime_start: NOW,
                bits: 0x207f_ffff,
                tx_version: 2,
                prefix,
                outputs: super::super::jd::codec::serialize_outputs(&outputs),
                path: Vec::new(),
            }
        }

        fn frame(&self, channel: u32) -> SerializedFrame {
            mining(Mining::SetCustomMiningJob(SetCustomMiningJob {
                channel_id: channel,
                request_id: 4,
                token: self.token.as_slice().try_into().unwrap(),
                version: self.version,
                prev_hash: (&self.prev).into(),
                ntime_start: self.ntime_start,
                nbits: self.bits,
                coinbase_tx_version: self.tx_version,
                coinbase_prefix: self.prefix.as_slice().try_into().unwrap(),
                coinbase_tx_input_n_sequence: u32::MAX,
                coinbase_tx_outputs: self.outputs.as_slice().try_into().unwrap(),
                coinbase_tx_locktime: 0,
                merkle_path: self
                    .path
                    .iter()
                    .map(Into::into)
                    .collect::<Vec<binary_sv2::U256>>()
                    .try_into()
                    .unwrap(),
            }))
            .unwrap()
        }

        /// The coinbase with this channel's prefix and `extranonce`.
        fn coinbase(&self, channel_prefix: &[u8], extranonce: &[u8]) -> Vec<u8> {
            let mut bytes = self.tx_version.to_le_bytes().to_vec();
            bytes.push(1);
            bytes.extend([0; 32]);
            bytes.extend(u32::MAX.to_le_bytes());
            bytes.push((self.prefix.len() + channel_prefix.len() + extranonce.len()) as u8);
            bytes.extend(&self.prefix);
            bytes.extend(channel_prefix);
            bytes.extend(extranonce);
            bytes.extend(u32::MAX.to_le_bytes());
            bytes.extend(&self.outputs);
            bytes.extend(0u32.to_le_bytes());
            bytes
        }
    }

    /// The reply to `job` on `channel`: its job id, or its error code.
    fn set_custom(
        server: &mut MiningSession,
        channel: u32,
        job: &CustomJob,
    ) -> Result<u32, String> {
        let mut responses = server.receive(job.frame(channel), NOW).unwrap();
        let mut frame = responses.frames.remove(0);
        match frame.header().msg_type() {
            MESSAGE_TYPE_SET_CUSTOM_MINING_JOB_SUCCESS => {
                let success: SetCustomMiningJobSuccess =
                    binary_sv2::from_bytes(frame.payload()).unwrap();
                assert_eq!(responses.custom_job, Some(Ok(())));
                Ok(success.job_id)
            }
            MESSAGE_TYPE_SET_CUSTOM_MINING_JOB_ERROR => {
                let error: SetCustomMiningJobError =
                    binary_sv2::from_bytes(frame.payload()).unwrap();
                let code = String::from_utf8(error.error_code.as_ref().to_vec()).unwrap();
                assert!(matches!(responses.custom_job, Some(Err(refused)) if refused == code));
                Err(code)
            }
            other => panic!("unexpected reply {other:#x}"),
        }
    }

    // #### PR #42
    // What: every Coinbase-only rule has its own error code, one case each,
    // and a job that meets them all gets id 0x8000_0001, a second
    // 0x8000_0002; a token works once and only for its identity.
    // Look here if: MiningSession::custom_job changes.
    #[test]
    fn set_custom_mining_job_checks_every_coinbase_only_rule() {
        let (mut plain, declarator, channel, _) = jd_session(0b100);
        assert_eq!(
            set_custom(&mut plain, channel, &CustomJob::valid(&declarator)),
            Err("jd-not-supported".into())
        );
        let (mut server, declarator, channel, _) = jd_session(0b110);
        type Change = Box<dyn Fn(&mut CustomJob)>;
        let cases: Vec<(&str, Change)> = vec![
            ("stale-chain-tip", Box::new(|job| job.prev = [0xcd; 32])),
            ("invalid-nbits", Box::new(|job| job.bits = 0x207f_fffe)),
            ("invalid-version", Box::new(|job| job.version ^= 1)),
            // stratum-core 0.7 renamed SRI's code from `invalid-min-ntime`;
            // the server's literal must keep matching SRI's.
            (
                ERROR_CODE_SET_CUSTOM_MINING_JOB_INVALID_NTIME_START,
                Box::new(|job| job.ntime_start = 1_699_999_999),
            ),
            (
                "invalid-coinbase-prefix",
                Box::new(|job| job.prefix[1] ^= 1),
            ),
            (
                "invalid-coinbase-prefix",
                Box::new(|job| job.prefix.resize(69, 0)),
            ),
            (
                "invalid-coinbase-tx-version",
                Box::new(|job| job.tx_version = 3),
            ),
            (
                "invalid-mining-job-token",
                Box::new(|job| job.token[25] ^= 1),
            ),
            (
                "invalid-coinbase-tx-outputs",
                Box::new(|job| {
                    let mut outputs = super::super::jd::codec::parse_outputs(&job.outputs).unwrap();
                    outputs[1].0 -= 1;
                    outputs[0].0 += 1;
                    job.outputs = super::super::jd::codec::serialize_outputs(&outputs);
                }),
            ),
        ];
        for (code, change) in cases {
            let mut job = CustomJob::valid(&declarator);
            change(&mut job);
            assert_eq!(
                set_custom(&mut server, channel, &job),
                Err(code.into()),
                "{code}"
            );
        }
        assert_eq!(
            set_custom(&mut server, 99, &CustomJob::valid(&declarator)),
            Err("invalid-channel-id".into())
        );
        let job = CustomJob::valid(&declarator);
        assert_eq!(set_custom(&mut server, channel, &job), Ok(0x8000_0001));
        assert_eq!(
            set_custom(&mut server, channel, &job),
            Err("invalid-mining-job-token".into()),
            "a token works once"
        );
        assert_eq!(
            set_custom(&mut server, channel, &CustomJob::valid(&declarator)),
            Ok(0x8000_0002)
        );
    }

    // #### PR #48
    // What: on Chipnet a Coinbase-only custom job's bits must be those its
    // start time needs: normal bits past the parent's time plus 1200 s, or
    // the limit's at or before it while ASERT gives the parent other bits,
    // are refused with invalid-nbits; normal bits before the point, the
    // limit's after it, and the limit's before it when ASERT itself gives
    // the limit (a parent on schedule from Chipnet's anchor) are set. The
    // pool's template carries the parent's time and the job's bits.
    // Look here if: the bits check in MiningSession::custom_job changes.
    #[test]
    fn a_chipnet_custom_job_needs_the_bits_its_start_time_needs() {
        use super::super::template::MIN_DIFFICULTY_BITS;
        let (mut server, declarator, channel, _) = jd_session(0b110);
        let parent = NOW - 1100;
        // Parent 325908 on ASERT's schedule from Chipnet's anchor.
        let schedule = 1_605_451_779 + 600 * (325_908 - 16_844 + 1);
        let pool = |bits: u32, parent_time: u32| {
            let mut raw = rpc_template();
            if bits == MIN_DIFFICULTY_BITS {
                raw["bits"] = serde_json::json!("1d00ffff");
                raw["target"] = serde_json::json!(format!("00000000ffff{}", "00".repeat(26)));
            }
            let mut template = BchTemplate::from_rpc(&raw).unwrap();
            template.parent_time = Some(parent_time);
            Arc::new(template)
        };
        let cases = [
            (0x207f_ffff, parent, NOW, None),
            (0x207f_ffff, parent, NOW + 101, Some("invalid-nbits")),
            (
                MIN_DIFFICULTY_BITS,
                parent,
                NOW + 100,
                Some("invalid-nbits"),
            ),
            (MIN_DIFFICULTY_BITS, parent, NOW + 101, None),
            (MIN_DIFFICULTY_BITS, schedule, NOW, None),
        ];
        for (generation, (bits, parent_time, ntime_start, refused)) in (5..).zip(cases) {
            server
                .set_job(generation, generation.into(), pool(bits, parent_time))
                .unwrap();
            let job = CustomJob {
                bits,
                ntime_start,
                ..CustomJob::valid(&declarator)
            };
            let outcome = set_custom(&mut server, channel, &job);
            assert_eq!(
                outcome.err().as_deref(),
                refused,
                "{bits:#x} {parent_time} {ntime_start}"
            );
        }
    }

    // #### PR #42
    // What: a work-selection connection's extended channel is custom-only
    // with 16 rollable bytes and no job of the pool's; its standard channel
    // is refused; a share on a custom job rebuilds the client's coinbase,
    // and a block share comes back on a header-only template (the client's
    // node submits it); a new parent revokes the custom job.
    // Look here if: custom-only channels or Channel::install_custom change.
    #[test]
    fn work_selection_channels_get_16_rollable_bytes_and_mine_custom_jobs() {
        let (mut server, declarator, channel, prefix) = jd_session(0b110);
        let mut refused = server.receive(open(), NOW).unwrap().frames;
        let error: OpenMiningChannelError = binary_sv2::from_bytes(refused[0].payload()).unwrap();
        assert_eq!(
            error.error_code.as_ref(),
            b"standard-channels-not-supported-for-custom-work"
        );
        let job = CustomJob::valid(&declarator);
        let id = set_custom(&mut server, channel, &job).unwrap();
        let extranonce = [0x42u8; JD_ROLLABLE];
        let coinbase = job.coinbase(&prefix, &extranonce);
        let mut header = [0; 80];
        header[..4].copy_from_slice(&job.version.to_le_bytes());
        header[4..36].copy_from_slice(&job.prev);
        header[36..68].copy_from_slice(&double_sha256(&coinbase));
        header[68..72].copy_from_slice(&NOW.to_le_bytes());
        header[72..76].copy_from_slice(&job.bits.to_le_bytes());
        let nonce = (0..10_000u32)
            .find(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                meets_target(&double_sha256(&header), &compact_target(job.bits).unwrap())
            })
            .unwrap();
        let share = |extranonce: &[u8], sequence| {
            mining(Mining::SubmitSharesExtended(SubmitSharesExtended {
                channel_id: channel,
                sequence_number: sequence,
                job_id: id,
                nonce,
                ntime: NOW,
                version: job.version,
                extranonce: extranonce.try_into().unwrap(),
            }))
            .unwrap()
        };
        let short = server.receive(share(&extranonce[..8], 1), NOW).unwrap();
        assert_eq!(
            short.frames[0].header().msg_type(),
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR
        );
        let responses = server.receive(share(&extranonce, 2), NOW).unwrap();
        assert_eq!(responses.blocks.len(), 1);
        let block = &responses.blocks[0];
        assert!(block.template.is_header_only());
        assert_eq!(block.coinbase.bytes, coinbase);
        assert!(block.template.block(&block.coinbase, block.header).is_err());
        assert!(block.token_wins.is_empty());
        let mut next = rpc_template();
        next["previousblockhash"] = serde_json::json!("cd".repeat(32));
        server
            .set_job(10, 4, Arc::new(BchTemplate::from_rpc(&next).unwrap()))
            .unwrap();
        let mut stale = server.receive(share(&extranonce, 3), NOW).unwrap();
        let error: SubmitSharesError = binary_sv2::from_bytes(stale.frames[0].payload()).unwrap();
        assert_eq!(error.error_code.as_ref(), b"stale-share");
    }

    // #### PR #42
    // What: a declared token sets exactly its declared job: a parent, bits,
    // version, start time, coinbase version, prefix, outputs or merkle path
    // other than declared each has its own error code (and spends the
    // token); an allocated token whose declared flag was forged is refused;
    // the job's shares rebuild the declared coinbase, and a block share
    // comes back on the declared template, whole, with its 3 transactions.
    // #### PR #48: on Chipnet, a start time past the parent's time plus
    // 1200 s on normal bits is invalid-nbits.
    // Look here if: MiningSession::declared_job changes.
    #[test]
    fn set_custom_mining_job_on_a_declared_job_must_match_the_declaration() {
        use super::super::jd::declared::{tests::context, CoinbaseShape, DeclaredJob};
        let (mut server, declarator, channel, prefix) = jd_session(0b110);
        // #### PR #48: on Chipnet's normal bits, the parent 1100 s back.
        let mut pool = context(3);
        pool.parent_time = Some(NOW - 1100);
        let pool = Arc::new(pool);
        server.set_job(10, 4, pool.clone()).unwrap();
        let base = CustomJob::valid(&declarator);
        let mut head = base.tx_version.to_le_bytes().to_vec();
        head.push(1);
        head.extend([0; 32]);
        head.extend(u32::MAX.to_le_bytes());
        head.push((base.prefix.len() + 32) as u8);
        head.extend(&base.prefix);
        let mut suffix = u32::MAX.to_le_bytes().to_vec();
        suffix.extend(&base.outputs);
        suffix.extend(0u32.to_le_bytes());
        let declared = Arc::new(DeclaredJob {
            shape: CoinbaseShape::parse(&head, &suffix).unwrap(),
            template: Arc::new(BchTemplate::declared(
                &pool,
                pool.version,
                pool.transactions().to_vec(),
                pool.transaction_ids().to_vec(),
            )),
        });
        let rates = PxToken::decode(&base.token).unwrap().rates;
        // The book's lock is released before `valid` takes it again.
        let job = || {
            let token = declarator
                .book
                .lock()
                .unwrap()
                .declared(0, payout(), rates, &declared, std::time::Instant::now())
                .unwrap()
                .encode()
                .to_vec();
            CustomJob {
                token,
                path: pool.merkle_path().to_vec(),
                ..CustomJob::valid(&declarator)
            }
        };
        type Change = Box<dyn Fn(&mut CustomJob)>;
        let cases: Vec<(&str, Change)> = vec![
            ("stale-chain-tip", Box::new(|job| job.prev = [0xcd; 32])),
            ("invalid-nbits", Box::new(|job| job.bits = 0x207f_fffe)),
            ("invalid-version", Box::new(|job| job.version ^= 1)),
            (
                ERROR_CODE_SET_CUSTOM_MINING_JOB_INVALID_NTIME_START,
                Box::new(|job| job.ntime_start = 1_699_999_999),
            ),
            // #### PR #48: a start past the parent's time plus 1200 s needs
            // the limit's bits (see `BchTemplate::wrong_bits`).
            ("invalid-nbits", Box::new(|job| job.ntime_start = NOW + 101)),
            (
                "invalid-coinbase-tx-version",
                Box::new(|job| job.tx_version = 1),
            ),
            (
                "invalid-coinbase-prefix",
                Box::new(|job| job.prefix[1] ^= 1),
            ),
            (
                "invalid-coinbase-tx-outputs",
                Box::new(|job| job.outputs[1] ^= 1),
            ),
            ("invalid-merkle-path", Box::new(|job| job.path.clear())),
        ];
        for (code, change) in cases {
            let mut job = job();
            change(&mut job);
            assert_eq!(
                set_custom(&mut server, channel, &job),
                Err(code.into()),
                "{code}"
            );
        }
        let mut forged = CustomJob::valid(&declarator);
        forged.token[9] |= 1;
        assert_eq!(
            set_custom(&mut server, channel, &forged),
            Err("invalid-mining-job-token".into())
        );
        let job = job();
        let id = set_custom(&mut server, channel, &job).unwrap();
        let extranonce = [0x42u8; JD_ROLLABLE];
        let coinbase = job.coinbase(&prefix, &extranonce);
        let mut header = [0; 80];
        header[..4].copy_from_slice(&job.version.to_le_bytes());
        header[4..36].copy_from_slice(&job.prev);
        header[36..68].copy_from_slice(&super::super::template::fold(
            double_sha256(&coinbase),
            pool.merkle_path(),
        ));
        header[68..72].copy_from_slice(&NOW.to_le_bytes());
        header[72..76].copy_from_slice(&job.bits.to_le_bytes());
        let nonce = (0..10_000u32)
            .find(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                meets_target(&double_sha256(&header), &compact_target(job.bits).unwrap())
            })
            .unwrap();
        let responses = server
            .receive(
                mining(Mining::SubmitSharesExtended(SubmitSharesExtended {
                    channel_id: channel,
                    sequence_number: 1,
                    job_id: id,
                    nonce,
                    ntime: NOW,
                    version: job.version,
                    extranonce: extranonce.as_slice().try_into().unwrap(),
                }))
                .unwrap(),
                NOW,
            )
            .unwrap();
        assert_eq!(responses.blocks.len(), 1);
        let block = &responses.blocks[0];
        assert!(block.template.is_declared());
        assert_eq!(block.coinbase.bytes, coinbase);
        let bytes = block.template.block(&block.coinbase, block.header).unwrap();
        let decoded: Block = consensus::deserialize(&bytes).unwrap();
        assert_eq!(decoded.txdata.len(), 4);
        assert!(decoded.check_merkle_root());
    }

    /// #### PR #42: a session set up with `flags` by `vendor` running
    /// `firmware`.
    fn session_by(flags: u32, vendor: &str, firmware: &str) -> MiningSession {
        let mut server = session();
        let reply = server
            .receive(
                encoded(
                    SetupConnection {
                        protocol: Protocol::MiningProtocol,
                        min_version: 2,
                        max_version: 2,
                        flags,
                        endpoint_host: "localhost".try_into().unwrap(),
                        endpoint_port: 3336,
                        vendor: vendor.try_into().unwrap(),
                        hardware_version: "".try_into().unwrap(),
                        firmware: firmware.try_into().unwrap(),
                        device_id: "".try_into().unwrap(),
                    },
                    0,
                    false,
                )
                .unwrap(),
                NOW,
            )
            .unwrap();
        assert_eq!(reply.frames[0].header().msg_type(), 1);
        server
    }

    /// #### PR #42: opens an extended channel for `identity`: its id, its
    /// group's id and its extranonce prefix, or the refusal's code.
    fn open_extended(
        server: &mut MiningSession,
        identity: &str,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let mut frames = server
            .receive(
                mining(Mining::OpenExtendedMiningChannel(
                    OpenExtendedMiningChannel {
                        request_id: 2,
                        user_identity: identity.try_into().unwrap(),
                        nominal_hash_rate: 1e12,
                        max_target: (&[255; 32]).into(),
                        min_extranonce_size: DEVICE_EXTRANONCE_SIZE as u16,
                    },
                ))
                .unwrap(),
                NOW,
            )
            .unwrap()
            .frames;
        if frames[0].header().msg_type() == MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR {
            let error: OpenMiningChannelError =
                binary_sv2::from_bytes(frames[0].payload()).unwrap();
            return Err(String::from_utf8(error.error_code.as_ref().to_vec()).unwrap());
        }
        let success: OpenExtendedMiningChannelSuccess =
            binary_sv2::from_bytes(frames[0].payload()).unwrap();
        Ok((
            success.channel_id,
            success.group_channel_id,
            success.extranonce_prefix.as_ref().to_vec(),
        ))
    }

    /// #### PR #42: a template on another parent.
    fn next_parent() -> Arc<BchTemplate> {
        let mut next = rpc_template();
        next["previousblockhash"] = serde_json::json!("cd".repeat(32));
        Arc::new(BchTemplate::from_rpc(&next).unwrap())
    }

    /// #### PR #42: the channel ids the job frames among `frames` address.
    fn job_targets(frames: &mut [SerializedFrame]) -> Vec<(u8, u32)> {
        frames
            .iter_mut()
            .filter_map(|frame| match frame.header().msg_type() {
                MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB => {
                    let job: NewExtendedMiningJob =
                        binary_sv2::from_bytes(frame.payload()).unwrap();
                    Some((MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, job.channel_id))
                }
                MESSAGE_TYPE_NEW_MINING_JOB => {
                    let job: NewMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
                    Some((MESSAGE_TYPE_NEW_MINING_JOB, job.channel_id))
                }
                MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH => {
                    let prev: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
                    Some((MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, prev.channel_id))
                }
                _ => None,
            })
            .collect()
    }

    // #### PR #42
    // What: two extended channels of one payout on a connection share a
    // group, announced when each opens; a new template sends one job and one
    // SetNewPrevHash to the group, not one per channel; a Pickaxe SV1 adapter
    // from before group support (empty firmware) gets no group and its own
    // frames.
    // Look here if: the group broadcast or the legacy-adapter rule changes.
    #[test]
    fn grouped_extended_channels_get_one_job_frame_per_refresh() {
        let mut server = session_by(4, "SRI translator", "1.0");
        let (a, group, _) = open_extended(&mut server, "miner.a").unwrap();
        let (b, also, _) = open_extended(&mut server, "miner.b").unwrap();
        assert_ne!(group, 0);
        assert_eq!(group, also);
        assert!(group != a && group != b);
        let mut frames = server.set_job(10, 4, next_parent()).unwrap();
        assert_eq!(
            job_targets(&mut frames),
            vec![
                (MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, group),
                (MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, group),
            ]
        );
        let mut legacy = session_by(4, "Pickaxe SV1 adapter", "");
        let (a, group, _) = open_extended(&mut legacy, "sv1-device").unwrap();
        let (b, _, _) = open_extended(&mut legacy, "sv1-device").unwrap();
        assert_eq!(group, 0, "a legacy adapter is never grouped");
        let mut frames = legacy.set_job(10, 4, next_parent()).unwrap();
        let mut targets: Vec<u32> = job_targets(&mut frames)
            .into_iter()
            .map(|(_, channel)| channel)
            .collect();
        targets.dedup();
        assert_eq!(targets, vec![a, b]);
    }

    // #### PR #42
    // What: each member rebuilds its coinbase from the group's job, its own
    // prefix and its extranonce, and its share on the group's job id is
    // accepted on its own channel; a share or an UpdateChannel naming the
    // group is invalid-channel-id, and CloseChannel on the group closes every
    // member.
    // Look here if: grouped shares are refused, or group ids are taken as
    // channels.
    #[test]
    fn each_member_reconstructs_the_same_root_from_the_group_job_and_its_prefix() {
        let mut server = session_by(4, "SRI translator", "1.0");
        let (a, group, prefix_a) = open_extended(&mut server, "miner.a").unwrap();
        let (b, _, prefix_b) = open_extended(&mut server, "miner.b").unwrap();
        let mut frames = server.set_job(10, 4, next_parent()).unwrap();
        let job_frame = frames
            .iter_mut()
            .find(|frame| frame.header().msg_type() == MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB)
            .unwrap();
        let job: NewExtendedMiningJob = binary_sv2::from_bytes(job_frame.payload()).unwrap();
        let (job_id, version) = (job.job_id, job.version);
        let path: Vec<Hash> = job
            .merkle_path
            .iter()
            .map(|hash| hash.as_ref().try_into().unwrap())
            .collect();
        let (head, tail) = (
            job.coinbase_tx_prefix.as_ref().to_vec(),
            job.coinbase_tx_suffix.as_ref().to_vec(),
        );
        let previous = next_parent().previous_hash;
        for (sequence, (channel, prefix)) in [(a, prefix_a), (b, prefix_b)].into_iter().enumerate()
        {
            let mut coinbase = head.clone();
            coinbase.extend(&prefix);
            coinbase.extend(DEVICE_EXTRANONCE);
            coinbase.extend(&tail);
            let root = super::super::template::fold(double_sha256(&coinbase), &path);
            let mut header = [0; 80];
            header[..4].copy_from_slice(&version.to_le_bytes());
            header[4..36].copy_from_slice(&previous);
            header[36..68].copy_from_slice(&root);
            header[68..72].copy_from_slice(&NOW.to_le_bytes());
            header[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
            // A nonce that makes the member's own header a block, so the
            // server's block shows the header it rebuilt.
            let nonce = (0..1_000u32)
                .find(|nonce| {
                    header[76..].copy_from_slice(&nonce.to_le_bytes());
                    meets_target(
                        &double_sha256(&header),
                        &compact_target(0x207f_ffff).unwrap(),
                    )
                })
                .unwrap();
            let responses = server
                .receive(
                    mining(Mining::SubmitSharesExtended(SubmitSharesExtended {
                        channel_id: channel,
                        sequence_number: sequence as u32 + 1,
                        job_id,
                        nonce,
                        ntime: NOW,
                        version,
                        extranonce: DEVICE_EXTRANONCE.as_slice().try_into().unwrap(),
                    }))
                    .unwrap(),
                    NOW,
                )
                .unwrap();
            assert_eq!(
                responses.frames[0].header().msg_type(),
                MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS,
                "member {channel}"
            );
            assert_eq!(responses.blocks.len(), 1, "member {channel}");
            assert_eq!(responses.blocks[0].header, header, "member {channel}");
        }
        let mut refused = server
            .receive(
                mining(Mining::SubmitSharesExtended(SubmitSharesExtended {
                    channel_id: group,
                    sequence_number: 9,
                    job_id,
                    nonce: 0,
                    ntime: NOW,
                    version,
                    extranonce: DEVICE_EXTRANONCE.as_slice().try_into().unwrap(),
                }))
                .unwrap(),
                NOW,
            )
            .unwrap()
            .frames;
        let error: SubmitSharesError = binary_sv2::from_bytes(refused[0].payload()).unwrap();
        assert_eq!(error.error_code.as_ref(), b"invalid-channel-id");
        server
            .receive(
                mining(Mining::CloseChannel(CloseChannel {
                    channel_id: group,
                    reason_code: "done".try_into().unwrap(),
                }))
                .unwrap(),
                NOW,
            )
            .unwrap();
        assert!(server.channels.is_empty());
        assert!(server.groups.is_empty());
    }

    // #### PR #42
    // What: a connection that requires standard jobs gets no groups: its
    // standard channels announce group 0 and get their own job frames, as
    // before group support; one member alone gets its own frames too; a
    // custom-only channel is never grouped.
    // Look here if: grouping applies where it should not.
    #[test]
    fn requires_standard_jobs_and_single_members_get_per_channel_frames() {
        let mut standard = session_by(5, "firmware", "1.0");
        for _ in 0..2 {
            let mut frames = standard.receive(open(), NOW).unwrap().frames;
            let success: OpenStandardMiningChannelSuccess =
                binary_sv2::from_bytes(frames[0].payload()).unwrap();
            assert_eq!(success.group_channel_id, 0);
        }
        let mut frames = standard.set_job(10, 4, next_parent()).unwrap();
        let ids: Vec<u32> = standard.channels.keys().copied().collect();
        assert_eq!(
            job_targets(&mut frames),
            vec![
                (MESSAGE_TYPE_NEW_MINING_JOB, ids[0]),
                (MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, ids[0]),
                (MESSAGE_TYPE_NEW_MINING_JOB, ids[1]),
                (MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, ids[1]),
            ]
        );
        assert!(standard.groups.is_empty());
        let mut single = session_by(4, "SRI translator", "1.0");
        let (channel, group, _) = open_extended(&mut single, "miner").unwrap();
        assert_ne!(group, 0);
        let mut frames = single.set_job(10, 4, next_parent()).unwrap();
        assert_eq!(
            job_targets(&mut frames),
            vec![
                (MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, channel),
                (MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, channel),
            ]
        );
        let (mut jd, _, _, _) = jd_session(0b110);
        assert!(
            jd.groups.is_empty(),
            "custom-only channels are never grouped"
        );
        let frames = jd.set_job(10, 4, next_parent()).unwrap();
        assert!(frames
            .iter()
            .all(|frame| frame.header().msg_type() != MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB));
    }

    // #### PR #42
    // What: at a public pool each payout identity has its own group; the
    // two channels of one miner share a job frame and the other miner gets
    // its own.
    // Look here if: public-pool groups mix miners.
    #[test]
    fn public_pool_groups_by_payout() {
        let declarator = super::super::jd::server::tests::declarator();
        let mut server = session();
        server.set_public(Some(declarator.public.clone()));
        let reply = server.receive(setup(4), NOW).unwrap();
        assert_eq!(reply.frames[0].header().msg_type(), 1);
        let miner = payout();
        let other =
            crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x56; 20], MiningNetwork::Chipnet)
                .unwrap();
        let (_, group, _) = open_extended(&mut server, &format!("{miner}.rig1")).unwrap();
        let (_, same, _) = open_extended(&mut server, &format!("{miner}.rig2")).unwrap();
        let (alone, apart, _) = open_extended(&mut server, &other).unwrap();
        assert_eq!(group, same);
        assert_ne!(group, apart);
        let mut frames = server.set_job(10, 4, next_parent()).unwrap();
        let targets = job_targets(&mut frames);
        assert!(targets.contains(&(MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, group)));
        assert!(targets.contains(&(MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, alone)));
        assert_eq!(targets.len(), 4);
    }

    // #### merge-mining v2: disclosure to miners
    // What: a client that negotiates the disclosure extension gets the
    // granted type back, then a JobLeaves frame after each job that commits
    // a root (rebuilding output 0) and a WinProof after a share that wins
    // a token; a client that did not ask gets neither; an unknown extension
    // is left out of the answer. A public pool's standard channel gets a
    // leaf log line per job.
    // Look here if: the negotiation or `disclose` changes.
    #[test]
    fn a_client_that_asks_gets_its_leaves_and_proofs() {
        use super::super::merge::disclosure::{
            extensions_payload, requested_extensions, JobLeaves, WinProof, EXTENSION, JOB_LEAVES,
            NEGOTIATION, REQUEST_EXTENSIONS, REQUEST_EXTENSIONS_SUCCESS, WIN_PROOF,
        };
        let template = Arc::new(token_template(0x207f_ffff, true));
        let mut server =
            MiningSession::new(MiningNetwork::Chipnet, payout(), [7; 12], [255; 32]).unwrap();
        server.set_job(9, 3, template.clone()).unwrap();
        server.receive(setup(4), NOW).unwrap();
        // A device opened before the negotiation (its first job's frames
        // carry no leaves), whose shares come later.
        let device = open_device(&mut server, ChannelKind::Extended);
        let ask = extension_frame(
            NEGOTIATION,
            REQUEST_EXTENSIONS,
            false,
            &extensions_payload(5, &[EXTENSION, 0x0002]),
        )
        .unwrap();
        let mut reply = server.receive(ask, NOW).unwrap().frames;
        assert_eq!(
            reply[0].header().ext_type_without_channel_msg(),
            NEGOTIATION
        );
        assert_eq!(reply[0].header().msg_type(), REQUEST_EXTENSIONS_SUCCESS);
        // Worker-Specific Hashrate Tracking (0x0002) is granted too.
        assert_eq!(
            requested_extensions(reply[0].payload()),
            Some((5, vec![EXTENSION, 0x0002]))
        );
        let (channel, _, _) = open_extended(&mut server, "device").unwrap();
        // The next job's frames end with its leaves.
        let mut frames = server.set_job(10, 4, template.clone()).unwrap();
        let disclosed: Vec<JobLeaves> = frames
            .iter_mut()
            .filter(|frame| frame.header().ext_type_without_channel_msg() == EXTENSION)
            .map(|frame| {
                assert_eq!(frame.header().msg_type(), JOB_LEAVES);
                assert!(frame.header().channel_msg());
                JobLeaves::decode(frame.payload()).unwrap()
            })
            .collect();
        // One for each channel: the device's and this one.
        assert_eq!(disclosed.len(), 2);
        let leaves = disclosed
            .into_iter()
            .find(|leaves| leaves.channel == channel)
            .unwrap();
        let job = server.channels[&channel].job().unwrap();
        assert_eq!(leaves.job, job.id);
        assert_eq!(leaves.output, job.aux.as_ref().unwrap().output);
        // Every share wins at this target: the win's proof follows the
        // acknowledgement.
        let mut responses = server
            .receive(submit_share(&device, ChannelKind::Extended, 0, 0), NOW)
            .unwrap();
        assert_eq!(responses.token_wins.len(), 1);
        let proof = responses
            .frames
            .iter_mut()
            .find(|frame| {
                frame.header().msg_type() == WIN_PROOF
                    && frame.header().ext_type_without_channel_msg() == EXTENSION
            })
            .expect("proof");
        let proof = WinProof::decode(proof.payload()).unwrap();
        assert_eq!(proof.channel, device.channel);
        crate::stratum_v2::merge::proof::AuxProof::from_bytes(&proof.proof).unwrap();
        // A client that did not ask gets neither.
        let mut quiet =
            MiningSession::new(MiningNetwork::Chipnet, payout(), [7; 12], [255; 32]).unwrap();
        quiet.set_job(9, 3, template.clone()).unwrap();
        quiet.receive(setup(4), NOW).unwrap();
        open_extended(&mut quiet, "device").unwrap();
        let frames = quiet.set_job(10, 4, template.clone()).unwrap();
        assert!(frames
            .iter()
            .all(|frame| frame.header().ext_type_without_channel_msg() == 0));
        // A public pool's standard channel: a leaf log line per job.
        let declarator = super::super::jd::server::tests::declarator();
        let mut public = session();
        public.set_public(Some(declarator.public.clone()));
        public.set_job(9, 3, template.clone()).unwrap();
        public.receive(setup(4), NOW).unwrap();
        let miner = payout();
        let request = OpenStandardMiningChannel {
            request_id: 1,
            user_identity: miner.as_str().try_into().unwrap(),
            nominal_hash_rate: 1e12,
            max_target: (&[255; 32]).into(),
        };
        let open = mining(Mining::OpenStandardMiningChannel(request)).unwrap();
        public.receive(open, NOW).unwrap();
        assert_eq!(public.leaf_log.len(), 1);
        let line: serde_json::Value = serde_json::from_str(&public.leaf_log[0]).unwrap();
        assert!(line["leaves"].as_str().unwrap().starts_with("43544d4402"));
        assert!(!public.leaf_log[0].contains("bchtest"));
    }

    // #### merge-mining v2: one donation choice per connection
    // What: on a public pool a login's `,donation=` is fixed per
    // connection: a later channel choosing otherwise is refused
    // (donation-mismatch), and a malformed choice is refused
    // (invalid-donation). #### one donation, the higher rate: a typed 0
    // no longer lowers the pool's 1.5%: the coinbase still pays the
    // donation.
    // Look here if: login options or `donation_choice` change.
    #[test]
    fn a_public_pool_login_chooses_its_donation_per_connection() {
        let declarator = super::super::jd::server::tests::declarator();
        let miner = payout();
        let donor = crate::tx::cashaddr_to_p2pkh_locking(crate::donation::bch::address(
            MiningNetwork::Chipnet,
        ))
        .unwrap();
        let public = |server: &mut MiningSession| {
            server.set_public(Some(declarator.public.clone()));
            assert_eq!(
                server.receive(setup(4), NOW).unwrap().frames[0]
                    .header()
                    .msg_type(),
                1
            );
        };
        let mut server = session();
        public(&mut server);
        assert_eq!(server.donation_choice(), None);
        let (channel, _, _) =
            open_extended(&mut server, &format!("{miner}.rig1,donation=0")).unwrap();
        assert_eq!(server.donation_choice(), Some("0".parse().unwrap()));
        let job = server.channels[&channel].job().unwrap();
        assert_eq!(u16::from(job.payout.donation), 150);
        let tx: stratum_core::bitcoin::Transaction =
            stratum_core::bitcoin::consensus::deserialize(&job.standard_coinbase.bytes).unwrap();
        assert!(tx
            .output
            .iter()
            .any(|output| output.script_pubkey.as_bytes() == donor.as_slice()));
        // The same choice may open another channel; another may not.
        assert!(open_extended(&mut server, &format!("{miner}.rig2,donation=0")).is_ok());
        assert_eq!(
            open_extended(&mut server, &format!("{miner}.rig3")),
            Err("donation-mismatch".into())
        );
        assert_eq!(
            open_extended(&mut server, &format!("{miner}.rig3,donation=1")),
            Err("donation-mismatch".into())
        );
        // A malformed choice.
        let mut server = session();
        public(&mut server);
        assert_eq!(
            open_extended(&mut server, &format!("{miner},donation=0.3")),
            Err("invalid-donation".into())
        );
        // Without the option, the pool's setting, as before.
        let mut server = session();
        public(&mut server);
        let (channel, _, _) = open_extended(&mut server, &miner).unwrap();
        assert_eq!(server.donation_choice(), None);
        assert_eq!(
            server.channels[&channel].job().unwrap().payout.donation,
            Default::default()
        );
    }

    // #### one donation, the higher rate
    // What: on a public pool the higher of the pool's rate and the login's
    // applies from a channel's first job (host 0 and miner 1.5% is 1.5%,
    // host 1.5% and miner 3% is 3%, in the token leaves' donation slot as
    // well). The SV1 adapter's donation channel opens on the device's own
    // connection beside a login with `,pickaxe-donation=0` and mines
    // donation work, without a fee. The typed 0 credits nothing; once a
    // bucket closes with that channel's accepted work at 1.5% of the
    // connection's, a 2% host takes 0.51% of the device channel's work
    // (0.5% of all of it). Donation channel shares repay no quota.
    // Look here if: `public_donation`, donation channels or the own
    // donation credit change.
    #[test]
    fn a_public_pool_takes_the_higher_rate_once_and_credits_only_donation_work() {
        use super::super::merge::split::Slot;
        use crate::donation::bch::BchDonation;
        let declarator = super::super::jd::server::tests::declarator();
        let miner = payout();
        let network = MiningNetwork::Chipnet;
        let donation = crate::donation::bch::address(network);
        let donor = crate::tx::cashaddr_to_p2pkh_locking(donation).unwrap();
        let rate = |text: &str| text.parse::<BchDonation>().unwrap();
        let public = |server: &mut MiningSession, host: &str| {
            server.set_public(Some(declarator.public.clone()));
            server.receive(setup(4), NOW).unwrap();
            server.public_donation(rate(host));
        };
        // Host 0, a login's 1.5%: 1.5% from the first job.
        let mut server = session();
        public(&mut server, "0");
        let identity = format!("{miner}.rig1,donation=1.5");
        let device = open_device_as(&mut server, ChannelKind::Extended, &identity);
        let job = server.channels[&device.channel].job().unwrap();
        assert_eq!(job.payout.donation, rate("1.5"));
        assert_eq!(server.public_donation(rate("0")), rate("1.5"));
        // Host 1.5%, a login's 3%: 3%, also in the token leaves' slot (two
        // thirds of it).
        let mut server = MiningSession::new(network, payout(), [7; 12], [255; 32]).unwrap();
        server
            .set_job(9, 3, Arc::new(token_template(0x207f_ffff, true)))
            .unwrap();
        public(&mut server, "1.5");
        let (channel, _, _) = open_extended(&mut server, &format!("{miner},donation=3")).unwrap();
        let job = server.channels[&channel].job().unwrap();
        assert_eq!(job.payout.donation, rate("3"));
        let split = job.aux.as_ref().unwrap().entries[0].split;
        assert_eq!(split.donation_slot(), &Slot::new(200, &donor).unwrap());
        assert_eq!(server.donation_terms(), Some((rate("3"), rate("0"))));
        // The adapter at a 2% host: a typed `,pickaxe-donation=0` keeps 2%.
        let mut server = session();
        public(&mut server, "2");
        let identity = format!("{miner}.rig1,pickaxe-donation=0");
        let device = open_device_as(&mut server, ChannelKind::Extended, &identity);
        assert_eq!(
            server.channels[&device.channel]
                .job()
                .unwrap()
                .payout
                .donation,
            rate("2")
        );
        // Its donation channel on the same connection: never a mismatch,
        // donation work whole, no fee, no worker name.
        server.identity = None;
        let own = open_device_as(&mut server, ChannelKind::Extended, donation);
        assert!(server.identity.is_none());
        let job = server.channels[&own.channel].job().unwrap();
        assert!(job.payout.donation_work && !job.payout.fee_work);
        let tx: stratum_core::bitcoin::Transaction =
            stratum_core::bitcoin::consensus::deserialize(&job.standard_coinbase.bytes).unwrap();
        assert!(tx
            .output
            .iter()
            .filter(|output| output.value.to_sat() > 0)
            .all(|output| output.script_pubkey.as_bytes() == donor.as_slice()));
        // Device shares alone: the closed bucket credits nothing.
        let mut typed = session();
        public(&mut typed, "2");
        let alone = open_device_as(&mut typed, ChannelKind::Extended, &identity);
        for nonce in 0..10 {
            let share = submit_share(&alone, ChannelKind::Extended, nonce, nonce);
            typed.receive(share, NOW).unwrap();
        }
        let share = submit_share(&alone, ChannelKind::Extended, 10, 10);
        typed.receive(share, NOW + 300).unwrap();
        assert_eq!(typed.public_donation(rate("2")), rate("2"));
        assert_eq!(typed.donation_terms(), Some((rate("2"), rate("0"))));
        // 3 donation shares in every 200: once the bucket closes, 1.5% is
        // credited and the host takes 0.51% of the device channel's work.
        let mut sequence = [0u32; 2];
        for share in 0..200u32 {
            let (job, index) = if share % 67 == 0 {
                (&own, 1)
            } else {
                (&device, 0)
            };
            let frame = submit_share(job, ChannelKind::Extended, sequence[index], sequence[index]);
            sequence[index] += 1;
            let responses = server.receive(frame, NOW).unwrap();
            assert!(matches!(
                responses.share_event,
                Some(ShareEvent::Accepted(_))
            ));
            // A donation share repays no quota of the miner's jobs.
            assert_eq!(responses.accepted_work.is_none(), index == 1);
            assert_eq!(server.donation_share.take().is_some(), index == 1);
        }
        assert_eq!(sequence, [197, 3]);
        assert_eq!(server.public_donation(rate("2")), rate("2"), "before");
        let frame = submit_share(&device, ChannelKind::Extended, 197, 197);
        server.receive(frame, NOW + 300).unwrap();
        let charged = server.public_donation(rate("2"));
        assert_eq!(u16::from(charged), 51);
        assert_eq!(server.donation_terms(), Some((rate("2"), rate("1.5"))));
        let payout = BchPayout {
            donation: charged,
            ..BchPayout::default()
        };
        let template = Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap());
        server.set_job_with_payout(10, 4, template, payout).unwrap();
        assert_eq!(
            server.channels[&device.channel]
                .job()
                .unwrap()
                .payout
                .donation,
            charged
        );
        assert!(
            server.channels[&own.channel]
                .job()
                .unwrap()
                .payout
                .donation_work
        );
    }

    // #### PR #42
    // What: vardiff stays per channel inside a group: a member's new target
    // comes with its own SetTarget and its own job frame.
    // Look here if: retarget changes for grouped channels.
    #[test]
    fn vardiff_retarget_stays_per_channel_inside_a_group() {
        let mut server = session_by(4, "SRI translator", "1.0");
        server.share_target = super::super::template::compact_target(0x1b0ffff0).unwrap();
        let mut template = (*server.current.as_ref().unwrap().2).clone();
        template.target = super::super::template::compact_target(0x1a00ffff).unwrap();
        server.set_job(10, 4, Arc::new(template)).unwrap();
        let (a, _, _) = open_extended(&mut server, "miner.a").unwrap();
        open_extended(&mut server, "miner.b").unwrap();
        server.channels.get_mut(&a).unwrap().vardiff_window(60, 0);
        let mut next_id = 10;
        let mut frames = server.retarget(&mut next_id).unwrap();
        assert_eq!(frames[0].header().msg_type(), MESSAGE_TYPE_SET_TARGET);
        assert_eq!(
            job_targets(&mut frames),
            vec![(MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB, a)]
        );
    }

    // #### PR #42
    // What: a grouped connection carries 256 channels and refuses the
    // 257th; an ungrouped one stops at 32.
    // Look here if: MAX_GROUPED_CHANNELS or the capacity check changes.
    #[test]
    fn grouped_connections_accept_256_channels_and_refuse_the_257th() {
        let mut server = session_by(4, "SRI translator", "1.0");
        for _ in 0..MAX_GROUPED_CHANNELS {
            open_extended(&mut server, "miner").unwrap();
        }
        assert_eq!(
            open_extended(&mut server, "miner"),
            Err("channel-capacity-exhausted".into())
        );
        let mut legacy = session_by(4, "Pickaxe SV1 adapter", "");
        for _ in 0..MAX_CHANNELS {
            open_extended(&mut legacy, "sv1-device").unwrap();
        }
        assert_eq!(
            open_extended(&mut legacy, "sv1-device"),
            Err("channel-capacity-exhausted".into())
        );
    }

    // #### PR #42
    // What: for a token that fixes the version slot, a client requiring
    // version rolling (Bitaxe's 0b101) is refused with bit 2; another is
    // told the version is fixed (Success bit 0), and its extended jobs
    // disallow rolling.
    // Look here if: the Fixed-version setup changes.
    #[test]
    fn fixed_version_refuses_rolling_clients_and_disallows_rolling_in_jobs() {
        let fixed_template = Arc::new(BchTemplate::token_only(
            Arc::new(super::super::template::TokenJob {
                token: "test",
                layout: super::super::template::Layout::Safa,
                version_mask: 0,
                anchor: [5; 32],
                thread: super::super::merge::OutPoint {
                    txid: [6; 32],
                    vout: 0,
                },
                age: 71,
                mtp: NOW,
            }),
            [0xab; 32],
            0x2000_0000,
            0x207f_ffff,
            [0xff; 32],
            NOW,
            1,
        ));
        let mut bitaxe = session();
        bitaxe.set_fixed_version(true);
        let mut reply = bitaxe.receive(setup(0b101), NOW).unwrap();
        assert_eq!(reply.frames[0].header().msg_type(), 2);
        let error: SetupConnectionError =
            binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
        assert_eq!(error.flags, 0b100);
        let mut plain = session();
        plain.set_fixed_version(true);
        plain.set_job(10, 4, fixed_template).unwrap();
        let mut reply = plain.receive(setup(0), NOW).unwrap();
        let success: SetupConnectionSuccess =
            binary_sv2::from_bytes(reply.frames[0].payload()).unwrap();
        assert_eq!(success.flags, 1);
        let mut frames = plain
            .receive(
                mining(Mining::OpenExtendedMiningChannel(
                    OpenExtendedMiningChannel {
                        request_id: 2,
                        user_identity: "device".try_into().unwrap(),
                        nominal_hash_rate: 1e12,
                        max_target: (&[255; 32]).into(),
                        min_extranonce_size: DEVICE_EXTRANONCE_SIZE as u16,
                    },
                ))
                .unwrap(),
                NOW,
            )
            .unwrap()
            .frames;
        let job: NewExtendedMiningJob = binary_sv2::from_bytes(frames[1].payload()).unwrap();
        assert!(!job.version_rolling_allowed);
        let mut usual = session();
        let reply = usual.receive(setup(0b101), NOW).unwrap();
        assert_eq!(
            reply.frames[0].header().msg_type(),
            1,
            "BCH work is unchanged"
        );
    }

    /// `frame` (a share) with `trailing` bytes after its own fields.
    fn followed(mut frame: SerializedFrame, trailing: &[u8]) -> SerializedFrame {
        let header = frame.header();
        let mut payload = frame.payload().to_vec();
        payload.extend_from_slice(trailing);
        extension_frame(
            header.ext_type_without_channel_msg(),
            header.msg_type(),
            header.channel_msg(),
            &payload,
        )
        .unwrap()
    }

    // What: Knuth's extensions on the mining server. RequestExtensions is
    // granted for 0x0002 (with 0x4D4D when asked, in the order asked) and
    // refused with RequestExtensions.Error naming what was asked when it
    // asks for neither; a frame of an unknown or ungranted extension, and a
    // RequestExtensions with the channel bit, are discarded without closing;
    // a malformed RequestExtensions closes.
    // Look here if: the negotiation in `MiningSession::receive` changes.
    #[test]
    fn worker_tracking_is_granted_and_unknown_extensions_are_discarded() {
        use super::super::extensions::{
            ExtensionMessage, RequestExtensions, RequestExtensionsError, RequestExtensionsSuccess,
            NEGOTIATION, REQUEST_EXTENSIONS_ERROR, REQUEST_EXTENSIONS_SUCCESS, WORKER_HASHRATE,
        };
        let ask = |requested: Vec<u16>| {
            RequestExtensions {
                request_id: 5,
                requested,
            }
            .frame()
            .unwrap()
        };
        let mut server = session();
        server.receive(setup(4), NOW).unwrap();
        let mut reply = server
            .receive(ask(vec![0x0003, WORKER_HASHRATE, 0x4D4D]), NOW)
            .unwrap()
            .frames;
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].header().ext_type(), NEGOTIATION);
        assert_eq!(reply[0].header().msg_type(), REQUEST_EXTENSIONS_SUCCESS);
        assert_eq!(
            RequestExtensionsSuccess::decode(reply[0].payload()),
            Some(RequestExtensionsSuccess {
                request_id: 5,
                supported: vec![WORKER_HASHRATE, 0x4D4D],
            })
        );
        assert!(server.worker_hashrate && server.merge_disclosure);
        let mut refusing = session();
        refusing.receive(setup(4), NOW).unwrap();
        let mut reply = refusing.receive(ask(vec![0x0003]), NOW).unwrap().frames;
        assert_eq!(reply[0].header().msg_type(), REQUEST_EXTENSIONS_ERROR);
        assert_eq!(
            RequestExtensionsError::decode(reply[0].payload()),
            Some(RequestExtensionsError {
                request_id: 5,
                unsupported: vec![0x0003],
                required: Vec::new(),
            })
        );
        assert!(!refusing.worker_hashrate);
        // Discarded: another extension's frame, a Knuth JD extension's, the
        // negotiation with the channel bit, and a client's 0x4D4D frame;
        // the connection stays.
        for frame in [
            extension_frame(0x0003, 0x00, false, &[1, 2, 3]).unwrap(),
            extension_frame(0x5052, 0x00, false, &[]).unwrap(),
            extension_frame(NEGOTIATION, 0x00, true, &[5, 0, 1, 0, 2, 0]).unwrap(),
            extension_frame(0x4D4D, 0x00, true, &[]).unwrap(),
        ] {
            let responses = refusing.receive(frame, NOW).unwrap();
            assert!(responses.frames.is_empty());
        }
        assert!(!refusing.worker_hashrate);
        assert!(refusing
            .receive(
                extension_frame(NEGOTIATION, 0x00, false, &[1]).unwrap(),
                NOW
            )
            .is_err());
        // Before setup it is still refused.
        assert!(session().receive(ask(vec![WORKER_HASHRATE]), NOW).is_err());
    }

    // What: TLV fields after SubmitSharesExtended. Without the grant whole
    // fields are ignored and the share is taken; with it the first
    // user_identity names the worker (other fields ignored), a share
    // without one is taken, and one over 32 bytes is refused with
    // user-identity-too-long (counted, the connection stays). Bytes that are
    // not whole fields close the connection either way; any byte after a
    // SubmitSharesStandard closes it.
    // Look here if: shares from a proxy that names workers are refused or
    // close its connection.
    #[test]
    fn extended_shares_name_their_worker_once_granted() {
        use super::super::extensions::{
            ExtensionMessage, RequestExtensions, Tlv, USER_IDENTITY_TOO_LONG, WORKER_HASHRATE,
        };
        let field = |tlvs: &[Tlv]| -> Vec<u8> {
            tlvs.iter().flat_map(|tlv| tlv.encode().unwrap()).collect()
        };
        // knuth: src/node/test/sv2_mining_session.cpp:788 (worker-hashrate
        // head): user_identity "rig-7".
        let rig = [0x02, 0x00, 0x01, 0x05, 0x00, b'r', b'i', b'g', b'-', b'7'];
        let named = field(&[
            Tlv {
                extension: 0x0003,
                field: 0x01,
                value: vec![1, 2],
            },
            Tlv::user_identity("rig-7"),
            Tlv {
                extension: WORKER_HASHRATE,
                field: 0x02,
                value: vec![9],
            },
        ]);
        for granted in [false, true] {
            let mut server = session();
            server.receive(setup(4), NOW).unwrap();
            if granted {
                let ask = RequestExtensions {
                    request_id: 1,
                    requested: vec![WORKER_HASHRATE],
                };
                server.receive(ask.frame().unwrap(), NOW).unwrap();
            }
            let device = open_device(&mut server, ChannelKind::Extended);
            let share = |nonce: u32| submit_share(&device, ChannelKind::Extended, nonce, nonce);
            let taken = server.receive(followed(share(1), &rig), NOW).unwrap();
            assert!(matches!(taken.share_event, Some(ShareEvent::Accepted(_))));
            assert_eq!(taken.worker.as_deref(), granted.then_some("rig-7"));
            let taken = server.receive(followed(share(2), &named), NOW).unwrap();
            assert!(matches!(taken.share_event, Some(ShareEvent::Accepted(_))));
            assert_eq!(taken.worker.as_deref(), granted.then_some("rig-7"));
            let plain = server.receive(share(3), NOW).unwrap();
            assert!(matches!(plain.share_event, Some(ShareEvent::Accepted(_))));
            assert_eq!(plain.worker, None);
            let long = field(&[Tlv {
                extension: WORKER_HASHRATE,
                field: 0x01,
                value: vec![b'w'; 33],
            }]);
            let mut refused = server.receive(followed(share(4), &long), NOW).unwrap();
            if granted {
                let error: SubmitSharesError =
                    binary_sv2::from_bytes(refused.frames[0].payload()).unwrap();
                assert_eq!(error.error_code.as_ref(), USER_IDENTITY_TOO_LONG.as_bytes());
                assert_eq!(error.sequence_number, 4);
                assert!(matches!(
                    refused.share_event,
                    Some(ShareEvent::Rejected(USER_IDENTITY_TOO_LONG))
                ));
            } else {
                assert!(matches!(refused.share_event, Some(ShareEvent::Accepted(_))));
            }
            // knuth: src/node/test/sv2_mining_session.cpp:791-792.
            for malformed in [
                &[0x00][..],
                &[0x02, 0x00, 0x01],
                &[0x02, 0x00, 0x01, 0x05, 0x00, b'r', b'i'],
            ] {
                assert!(server.receive(followed(share(5), malformed), NOW).is_err());
            }
            let standard = open_device(&mut server, ChannelKind::Standard);
            let strict = submit_share(&standard, ChannelKind::Standard, 6, 6);
            assert!(server.receive(followed(strict, &rig), NOW).is_err());
            let strict = submit_share(&standard, ChannelKind::Standard, 7, 7);
            assert!(server.receive(strict, NOW).is_ok());
        }
    }
}
