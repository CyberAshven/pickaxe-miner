//! #### PR #38
//! Mining-protocol session using reference message parsers/serializers. The
//! server supplies templates and channel IDs; devices only submit header work.

use super::{
    channel::{share_work, Channel, ChannelKind, Share, ValidatedShare, DEVICE_EXTRANONCE_SIZE},
    telemetry::ShareEvent,
    template::{meets_target, BchTemplate, Hash},
};
use crate::config::MiningNetwork;
use crate::donation::bch::BchPayout;
use std::{collections::BTreeMap, sync::Arc};
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
    pub accepted: u64,
    pub rejected: u64,
}

pub struct Responses {
    pub frames: Vec<SerializedFrame>,
    pub blocks: Vec<ValidatedShare>,
    pub share_event: Option<ShareEvent>,
}

impl MiningSession {
    pub fn new(
        network: MiningNetwork,
        payout: String,
        salt: [u8; 12],
        share_target: Hash,
    ) -> Result<Self, String> {
        crate::config::validate_payout_address(network, &payout)
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
            accepted: 0,
            rejected: 0,
        })
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
        let mut frames = Vec::new();
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
            // #### PR #38
            // Follow an easier block target down and, on the next normal
            // template, back up to the share difficulty. SetTarget precedes
            // the job so the job carries the new target.
            if channel.settle(&template.target, maximum) {
                frames.push(mining(Mining::SetTarget(SetTarget {
                    channel_id: channel.id,
                    maximum_target: (&channel.target).into(),
                }))?);
            }
            channel.install_with_payout(id, generation, template.clone(), payout)?;
            frames.extend(job_frames(channel, immediate)?);
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
        for channel in self.channels.values_mut() {
            let maximum = self
                .maximum_targets
                .get(&channel.id)
                .ok_or("channel target missing")?;
            let Some(target) = channel.retarget(maximum) else {
                continue;
            };
            let (generation, template, payout) = {
                let job = channel.job().ok_or("channel has no job")?;
                (job.generation, job.template.clone(), job.payout)
            };
            *next_id = next_id.checked_add(1).ok_or("job identifiers exhausted")?;
            frames.push(mining(Mining::SetTarget(SetTarget {
                channel_id: channel.id,
                maximum_target: (&target).into(),
            }))?);
            channel.install_with_payout(*next_id, generation, template, payout)?;
            frames.extend(job_frames(channel, true)?);
        }
        Ok(frames)
    }

    pub fn receive(&mut self, mut frame: SerializedFrame, now: u32) -> Result<Responses, String> {
        let header = frame.header();
        if header.ext_type_without_channel_msg() != 0 {
            return Err("unsupported SV2 extension".into());
        }
        let mut frames = Vec::new();
        let mut blocks = Vec::new();
        let mut share_event = None;
        if header.msg_type() == common::MESSAGE_TYPE_SETUP_CONNECTION {
            if self.setup_flags.is_some() || header.channel_msg() {
                return Err("unexpected setup message".into());
            }
            let setup: SetupConnection =
                binary_sv2::from_bytes(frame.payload()).map_err(|_| "malformed setup message")?;
            let error = if setup.protocol != Protocol::MiningProtocol {
                Some((0, "unsupported-protocol"))
            } else if setup.min_version > 2 || setup.max_version < 2 {
                Some((0, "protocol-version-mismatch"))
            }
            // 0: header-only channels; 2: version rolling. Work selection is
            // not offered until BCH Job Declaration is implemented.
            else if setup.flags & !0b101 != 0 {
                Some((setup.flags & !0b101, "unsupported-feature-flags"))
            } else {
                None
            };
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
                // Upstream flag bit 0 means REQUIRES_FIXED_VERSION. Leave it
                // clear: ASIC hardware, including Bitaxe, needs version rolling.
                frames.push(encoded(
                    SetupConnectionSuccess {
                        used_version: 2,
                        flags: 0,
                    },
                    common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
                    false,
                )?);
            }
            return Ok(Responses {
                frames,
                blocks,
                share_event,
            });
        }
        let flags = self
            .setup_flags
            .ok_or("SetupConnection is required first")?;
        let msg: Mining = (header.msg_type(), frame.payload())
            .try_into()
            .map_err(|_| "malformed mining message")?;
        if msg.channel_bit() != header.channel_msg() {
            return Err("incorrect SV2 channel flag".into());
        }
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
                    )?);
                }
            }
            Mining::SubmitSharesStandard(request) => {
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
                )?);
            }
            Mining::SubmitSharesExtended(request) => {
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
                )?);
            }
            Mining::UpdateChannel(request) => {
                let result = self
                    .channels
                    .get(&request.channel_id)
                    .ok_or("invalid-channel-id")
                    .and_then(|channel| {
                        let maximum: Hash = request
                            .maximum_target
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
                            maximum_target: (&target).into(),
                        }))?
                    }
                    Err(code) => mining(Mining::UpdateChannelError(UpdateChannelError {
                        channel_id: request.channel_id,
                        error_code: code.try_into().unwrap(),
                    }))?,
                });
            }
            Mining::CloseChannel(request) => {
                self.channels.remove(&request.channel_id);
                self.maximum_targets.remove(&request.channel_id);
            }
            _ => return Err("unsupported downstream mining message".into()),
        }
        Ok(Responses {
            frames,
            blocks,
            share_event,
        })
    }

    fn open(
        &mut self,
        request: u32,
        kind: ChannelKind,
        rate: f32,
        maximum: Hash,
        extra: u16,
    ) -> Result<Vec<SerializedFrame>, String> {
        let error = if !rate.is_finite() || rate < 0.0 {
            Some("invalid-nominal-hashrate")
        } else if extra as usize > DEVICE_EXTRANONCE_SIZE {
            Some("unsupported-min-extranonce-size")
        } else if maximum == [0; 32] {
            Some("max-target-out-of-range")
        } else if self.channels.len() >= MAX_CHANNELS || self.next_channel == u32::MAX {
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
        self.next_channel += 1;
        // Every device starts at the configured share target; vardiff moves
        // it from there.
        let mut channel = Channel::new(
            self.next_channel,
            kind,
            self.share_target,
            self.salt,
            self.network,
            &self.payout,
        )?;
        let (id, generation, template) = self.current.as_ref().unwrap();
        channel.settle(&template.target, &maximum);
        let target = channel.target;
        channel.install_with_payout(*id, *generation, template.clone(), self.payout_policy)?;
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
                    extranonce_size: DEVICE_EXTRANONCE_SIZE as u16,
                    extranonce_prefix: channel.extranonce_prefix.as_slice().try_into().unwrap(),
                    group_channel_id: 0,
                },
            ))?,
        }];
        frames.extend(job_frames(&channel, false)?);
        self.channels.insert(channel.id, channel);
        self.maximum_targets.insert(self.next_channel, maximum);
        Ok(frames)
    }

    fn submit(
        &mut self,
        share: Share<'_>,
        kind: ChannelKind,
        now: u32,
        frames: &mut Vec<SerializedFrame>,
        blocks: &mut Vec<ValidatedShare>,
    ) -> Result<ShareEvent, String> {
        let id = share.channel_id;
        let sequence = share.sequence;
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
                let work = share_work(&share.share_target);
                self.accepted = self.accepted.saturating_add(1);
                frames.push(mining(Mining::SubmitSharesSuccess(SubmitSharesSuccess {
                    channel_id: id,
                    last_sequence_number: sequence,
                    new_submits_accepted_count: 1,
                    new_shares_sum: work,
                }))?);
                if share.block {
                    blocks.push(share);
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
    let new = match channel.kind {
        ChannelKind::Standard => Mining::NewMiningJob(NewMiningJob {
            channel_id: channel.id,
            job_id: job.id,
            min_ntime: Sv2Option::new(immediate.then_some(job.template.current_time)),
            version: job.template.version,
            merkle_root: (&job.standard_coinbase.merkle_root).into(),
        }),
        ChannelKind::Extended => Mining::NewExtendedMiningJob(NewExtendedMiningJob {
            channel_id: channel.id,
            job_id: job.id,
            min_ntime: Sv2Option::new(immediate.then_some(job.template.current_time)),
            version: job.template.version,
            version_rolling_allowed: true,
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
            channel_id: channel.id,
            job_id: job.id,
            prev_hash: (&job.template.previous_hash).into(),
            min_ntime: job.template.current_time,
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
        template::double_sha256,
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
        header[68..72].copy_from_slice(&prev.min_ntime.to_le_bytes());
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
        assert_eq!(next.min_ntime.into_inner(), Some(1700000025));
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
}
