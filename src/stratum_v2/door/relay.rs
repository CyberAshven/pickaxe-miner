//! #### Pool modes: one SV2 connection at the door, with the Pickaxe
//! donation mined as work.
//!
//! Each device channel opens at P2Pool under its miner's own identity (its
//! personal lane) and, while the donation is above 0%, a second channel
//! under the donation's identity (its donation lane), on the same link.
//! The device sees one channel: the door gives it one lane's jobs at a
//! time, with that lane's extranonce prefix moved into the coinbase part,
//! maps its job ids and shares back to the lane they came from, and
//! switches lanes by the SV1 adapter's rule (`WorkAllocation::
//! pool_donation_work`, repaid by accepted work). Verdicts come back under
//! the device's own channel and sequence numbers.

use super::super::{
    channel::MAX_ACTIVE_JOBS,
    own_donation::{charged, OwnDonation},
    payout::{effective_donation, is_donation},
    server::WorkSplit,
    sv1::{miner_login, DonationRoute, MinerLogins, DONATION_REFUSALS, P2PKH_ONLY},
    telemetry::{expected_hashes, ShareEvent},
    wire::mining,
    work_allocation::WorkAllocation,
};
use crate::donation::bch::BchDonation;
use std::{collections::BTreeMap, time::Instant};
use stratum_core::{
    binary_sv2::{self, Sv2Option},
    codec_sv2::SerializedFrame,
    common_messages_sv2::MESSAGE_TYPE_SETUP_CONNECTION,
    mining_sv2::*,
    parsers_sv2::Mining,
};

/// Channel requests of one connection the pool has not answered yet.
pub(super) const MAX_OPENING: usize = 256;
/// Shares a lane waits on the pool's verdict for; the oldest goes first.
const MAX_PENDING: usize = 64;

/// A frame for the pool or for the device.
pub(super) enum Out {
    Up(SerializedFrame),
    Down(SerializedFrame),
}

/// What the workers page learns from a step of the relay.
#[derive(Default)]
pub(super) struct Noted {
    /// The device's username, once its first miner channel opened.
    pub name: Option<String>,
    pub verdicts: Vec<ShareEvent>,
    pub work: WorkSplit,
    /// The miner's effective Pickaxe donation and their own Pickaxe's
    /// credit, while a miner channel is open.
    pub terms: Option<(BchDonation, BchDonation)>,
}

/// What a device asked its channel for, kept for its donation lane.
#[derive(Clone, Copy, Debug)]
struct OpenParams {
    standard: bool,
    nominal: f32,
    max_target: [u8; 32],
    min_extranonce: u16,
}

/// A channel request the door sent the pool, by the door's request id.
enum Request {
    /// The device's own, under its request id `id`.
    Device {
        id: u32,
        name: String,
        pays_donation: bool,
        open: OpenParams,
    },
    /// The donation lane of the device channel `channel`.
    Donation { channel: u32, retried: bool },
}

/// One channel at the pool, as the door follows it.
struct Lane {
    channel: u32,
    group: u32,
    prefix: Vec<u8>,
    size: usize,
    target: [u8; 32],
    /// Future jobs by the pool's job id, as the pool sent them.
    future: BTreeMap<u32, Vec<u8>>,
    /// The parent in force.
    prev: Option<Vec<u8>>,
    /// The newest job on that parent: the pool's job id and the job.
    latest: Option<(u32, Vec<u8>)>,
    /// The device's job ids of this lane's jobs it has, by the pool's.
    sent: BTreeMap<u32, u32>,
    sequence: u32,
    pending: BTreeMap<u32, Pending>,
}

/// A share forwarded to the pool and waiting for its verdict.
struct Pending {
    /// The device's sequence number.
    down: u32,
    target: [u8; 32],
    /// The donation rate the share was mined under, and what the credit
    /// spared it (`own_donation::OwnDonation::record`).
    rate: BchDonation,
    forgone: u16,
}

enum Slot {
    Off,
    Requested,
    Open(Box<Lane>),
    /// The pool refused or broke the donation lane; the device mines its
    /// own.
    Unavailable,
}

/// A device channel: its own lane and the donation's.
struct Relayed {
    /// The device's extranonce size on an extended channel; `None` for a
    /// standard channel.
    extranonce: Option<usize>,
    personal: Lane,
    donation: Slot,
    /// The channel's login pays the donation's own address: a joining
    /// Pickaxe's donation channel, whose work is the connection's own
    /// donation. It gets no donation lane.
    pays_donation: bool,
    on_donation: bool,
    allocation: WorkAllocation,
    /// The device's job ids: lane (true: donation), the pool's job id and
    /// the target it was given at.
    jobs: BTreeMap<u32, (bool, u32, [u8; 32])>,
    next_job: u32,
    refusals: u32,
    open: OpenParams,
}

/// One relayed SV2 connection.
pub(super) struct Relay {
    miners: Option<MinerLogins>,
    route: Option<DonationRoute>,
    requests: BTreeMap<u32, Request>,
    next_request: u32,
    channels: BTreeMap<u32, Relayed>,
    /// The connection's donation choice, fixed by its first miner channel.
    choice: Option<Option<BchDonation>>,
    own: OwnDonation,
    named: bool,
    pub(super) noted: Noted,
}

fn invalid(what: &'static str) -> impl FnOnce(binary_sv2::Error) -> String {
    move |_| what.to_owned()
}

impl Lane {
    fn new(channel: u32, group: u32, prefix: &[u8], size: usize, target: &[u8]) -> Self {
        Self {
            channel,
            group,
            prefix: prefix.to_vec(),
            size,
            target: target.try_into().unwrap_or([0xff; 32]),
            future: BTreeMap::new(),
            prev: None,
            latest: None,
            sent: BTreeMap::new(),
            sequence: 0,
            pending: BTreeMap::new(),
        }
    }

    fn addressed(&self, channel: u32) -> bool {
        self.channel == channel || (self.group != 0 && self.group == channel)
    }
}

impl Relayed {
    fn lane(&self, donation: bool) -> Option<&Lane> {
        match (donation, &self.donation) {
            (false, _) => Some(&self.personal),
            (true, Slot::Open(lane)) => Some(lane),
            _ => None,
        }
    }

    fn lane_mut(&mut self, donation: bool) -> Option<&mut Lane> {
        match (donation, &mut self.donation) {
            (false, _) => Some(&mut self.personal),
            (true, Slot::Open(lane)) => Some(lane),
            _ => None,
        }
    }

    /// The device's job of `lane`'s job `payload`, under a new job id, with
    /// the pool's extranonce prefix (and any padding) in the coinbase part;
    /// a future job when `future`.
    fn job_out(
        &mut self,
        down: u32,
        donation: bool,
        payload: &[u8],
        future: bool,
    ) -> Result<SerializedFrame, String> {
        let (prefix, pad, target) = {
            let lane = self.lane(donation).ok_or("unknown lane")?;
            let pad = self
                .extranonce
                .map_or(0, |size| lane.size.saturating_sub(size));
            (lane.prefix.clone(), pad, lane.target)
        };
        self.next_job = self.next_job.wrapping_add(1).max(1);
        let job_id = self.next_job;
        let mut bytes = payload.to_vec();
        let (frame, pool_job) = match self.extranonce {
            Some(_) => {
                let mut job: NewExtendedMiningJob =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid mining job"))?;
                let pool_job = job.job_id;
                let mut coinbase = job.coinbase_tx_prefix.as_ref().to_vec();
                coinbase.extend(&prefix);
                coinbase.resize(coinbase.len() + pad, 0);
                job.coinbase_tx_prefix = coinbase
                    .as_slice()
                    .try_into()
                    .map_err(invalid("invalid coinbase"))?;
                job.channel_id = down;
                job.job_id = job_id;
                if future {
                    job.min_ntime = Sv2Option::new(None);
                }
                (mining(Mining::NewExtendedMiningJob(job))?, pool_job)
            }
            None => {
                let mut job: NewMiningJob =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid mining job"))?;
                let pool_job = job.job_id;
                job.channel_id = down;
                job.job_id = job_id;
                if future {
                    job.min_ntime = Sv2Option::new(None);
                }
                (mining(Mining::NewMiningJob(job))?, pool_job)
            }
        };
        self.jobs.insert(job_id, (donation, pool_job, target));
        while self.jobs.len() > 2 * MAX_ACTIVE_JOBS {
            self.jobs.pop_first();
        }
        if let Some(lane) = self.lane_mut(donation) {
            lane.sent.insert(pool_job, job_id);
            while lane.sent.len() > 2 * MAX_ACTIVE_JOBS {
                lane.sent.pop_first();
            }
        }
        Ok(frame)
    }

    /// The parent `payload` for the device's job `job_id`.
    fn prev_out(down: u32, payload: &[u8], job_id: u32) -> Result<SerializedFrame, String> {
        let mut bytes = payload.to_vec();
        let mut prev: SetNewPrevHash =
            binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid job activation"))?;
        prev.channel_id = down;
        prev.job_id = job_id;
        mining(Mining::SetNewPrevHash(prev))
    }

    /// The device moves to the lane it now mines (`on_donation`): that
    /// lane's target, its newest job as a future job and its parent, which
    /// drops the other lane's work; nothing until the lane has both.
    fn switch(&mut self, down: u32) -> Result<Vec<Out>, String> {
        let donation = self.on_donation;
        let Some(lane) = self.lane_mut(donation) else {
            return Ok(Vec::new());
        };
        let (Some((_, job)), Some(prev)) = (lane.latest.clone(), lane.prev.clone()) else {
            return Ok(Vec::new());
        };
        lane.sent.clear();
        let target = lane.target;
        self.jobs.clear();
        let set = mining(Mining::SetTarget(SetTarget {
            channel_id: down,
            maximum_target: (&target).into(),
        }))?;
        let job_frame = self.job_out(down, donation, &job, true)?;
        let job_id = self.next_job;
        Ok(vec![
            Out::Down(set),
            Out::Down(job_frame),
            Out::Down(Self::prev_out(down, &prev, job_id)?),
        ])
    }
}

impl Relay {
    /// A connection whose channels open under `miners`' logins, with the
    /// donation's `route` (none: no donation lanes).
    pub(super) fn new(miners: Option<MinerLogins>, route: Option<DonationRoute>) -> Self {
        Self {
            miners,
            route,
            requests: BTreeMap::new(),
            next_request: 0,
            channels: BTreeMap::new(),
            choice: None,
            own: OwnDonation::default(),
            named: false,
            noted: Noted::default(),
        }
    }

    fn request_id(&mut self) -> u32 {
        self.next_request = self.next_request.wrapping_add(1).max(1);
        self.next_request
    }

    // #### one donation, the higher rate, at the door
    // What: the miner pays the higher of the server's rate and the rate
    // their login names (`payout::effective_donation`), once; a joining
    // Pickaxe's visible donation channel on the same connection is
    // credited (`own_donation`), and the door's donation lane takes the
    // rest (`own_donation::charged`) of the miner's own channels' work.
    // Why: the host runs Pickaxe, so every miner it serves pays the rate;
    // a typed `,pickaxe-donation=0` lowers nothing, only donation work the
    // door sees does.
    // Look here if: a door miner's donation work is off its rate, or a
    // joining Pickaxe pays twice.
    /// The effective rate, the own donation's credit, and what the door's
    /// donation lanes take.
    pub(super) fn rates(&self) -> (BchDonation, BchDonation, BchDonation) {
        let server = self.route.as_ref().map_or_else(
            || BchDonation::try_from(0).unwrap_or_default(),
            DonationRoute::rate,
        );
        let effective = effective_donation(server, self.choice.flatten());
        let credit = self.own.credit(effective);
        (effective, credit, charged(effective, credit))
    }

    /// The device channels whose lanes `channel` (a channel or group id)
    /// reaches: the device's channel id and whether it is the donation lane.
    fn addressed(&self, channel: u32) -> Vec<(u32, bool)> {
        let mut lanes = Vec::new();
        for (down, relayed) in &self.channels {
            for donation in [false, true] {
                if relayed
                    .lane(donation)
                    .is_some_and(|lane| lane.addressed(channel))
                {
                    lanes.push((*down, donation));
                }
            }
        }
        lanes
    }

    /// The device channel and lane of the pool's channel `channel` exactly.
    fn lane_of(&self, channel: u32) -> Option<(u32, bool)> {
        self.channels.iter().find_map(|(down, relayed)| {
            [false, true].into_iter().find_map(|donation| {
                relayed
                    .lane(donation)
                    .filter(|lane| lane.channel == channel)
                    .map(|_| (*down, donation))
            })
        })
    }

    /// A channel request refused here.
    fn refuse(request_id: u32, code: &str) -> Result<Out, String> {
        Ok(Out::Down(mining(Mining::OpenMiningChannelError(
            OpenMiningChannelError {
                request_id,
                error_code: code.try_into().map_err(invalid("invalid channel error"))?,
            },
        ))?))
    }

    /// A share refused here, under the device's channel and sequence.
    fn share_error(channel_id: u32, sequence_number: u32, code: &str) -> Result<Out, String> {
        Ok(Out::Down(mining(Mining::SubmitSharesError(
            SubmitSharesError {
                channel_id,
                sequence_number,
                error_code: code.try_into().map_err(invalid("invalid share error"))?,
            },
        ))?))
    }

    /// One frame from the device.
    pub(super) fn device_frame(&mut self, mut frame: SerializedFrame) -> Result<Vec<Out>, String> {
        let header = frame.header();
        if header.ext_type_without_channel_msg() != 0 {
            return Ok(vec![Out::Up(frame)]);
        }
        let kind = header.msg_type();
        let opening = matches!(
            kind,
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL | MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL
        );
        // A public port: requests the pool has not answered stay few.
        if opening && self.requests.len() >= MAX_OPENING {
            return Err("too many channel requests".into());
        }
        match kind {
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL => {
                let mut request: OpenExtendedMiningChannel =
                    binary_sv2::from_bytes(frame.payload())
                        .map_err(invalid("malformed channel request"))?;
                let name = request.user_identity.as_utf8_or_hex();
                let open = OpenParams {
                    standard: false,
                    nominal: request.nominal_hash_rate,
                    max_target: request
                        .max_target
                        .as_ref()
                        .try_into()
                        .map_err(|_| "invalid max target")?,
                    min_extranonce: request.min_extranonce_size,
                };
                match self.login(request.request_id, &name, open)? {
                    Ok((id, identity)) => {
                        request.request_id = id;
                        request.user_identity = identity
                            .as_str()
                            .try_into()
                            .map_err(|_| "identity too long")?;
                        Ok(vec![Out::Up(mining(Mining::OpenExtendedMiningChannel(
                            request,
                        ))?)])
                    }
                    Err(refused) => Ok(vec![refused]),
                }
            }
            MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL => {
                let mut request: OpenStandardMiningChannel =
                    binary_sv2::from_bytes(frame.payload())
                        .map_err(invalid("malformed channel request"))?;
                let name = request.user_identity.as_utf8_or_hex();
                let open = OpenParams {
                    standard: true,
                    nominal: request.nominal_hash_rate,
                    max_target: request
                        .max_target
                        .as_ref()
                        .try_into()
                        .map_err(|_| "invalid max target")?,
                    min_extranonce: 0,
                };
                match self.login(request.request_id, &name, open)? {
                    Ok((id, identity)) => {
                        request.request_id = id;
                        request.user_identity = identity
                            .as_str()
                            .try_into()
                            .map_err(|_| "identity too long")?;
                        Ok(vec![Out::Up(mining(Mining::OpenStandardMiningChannel(
                            request,
                        ))?)])
                    }
                    Err(refused) => Ok(vec![refused]),
                }
            }
            MESSAGE_TYPE_SUBMIT_SHARES_EXTENDED => {
                let mut share: SubmitSharesExtended =
                    binary_sv2::from_bytes(frame.payload()).map_err(invalid("invalid share"))?;
                let (down, sequence) = (share.channel_id, share.sequence_number);
                let (_, _, rate) = self.rates();
                let forgone = self.forgone();
                let Some(relayed) = self.channels.get_mut(&down) else {
                    return Ok(vec![Self::share_error(
                        down,
                        sequence,
                        "invalid-channel-id",
                    )?]);
                };
                let Some(&(donation, pool_job, target)) = relayed.jobs.get(&share.job_id) else {
                    return Ok(vec![Self::share_error(down, sequence, "invalid-job-id")?]);
                };
                let size = relayed.extranonce.unwrap_or(0);
                if share.extranonce.as_ref().len() != size {
                    return Ok(vec![Self::share_error(
                        down,
                        sequence,
                        "invalid-extranonce-size",
                    )?]);
                }
                let Some(lane) = relayed.lane_mut(donation) else {
                    return Ok(vec![Self::share_error(down, sequence, "stale-share")?]);
                };
                // The lane's miner bytes: any padding, then the device's.
                let mut extranonce = vec![0; lane.size.saturating_sub(size)];
                extranonce.extend(share.extranonce.as_ref());
                share.channel_id = lane.channel;
                share.job_id = pool_job;
                share.sequence_number = lane.sequence;
                share.extranonce = extranonce
                    .as_slice()
                    .try_into()
                    .map_err(invalid("invalid share"))?;
                Self::pending(lane, sequence, target, rate, forgone);
                Ok(vec![Out::Up(mining(Mining::SubmitSharesExtended(share))?)])
            }
            MESSAGE_TYPE_SUBMIT_SHARES_STANDARD => {
                let mut share: SubmitSharesStandard =
                    binary_sv2::from_bytes(frame.payload()).map_err(invalid("invalid share"))?;
                let (down, sequence) = (share.channel_id, share.sequence_number);
                let (_, _, rate) = self.rates();
                let forgone = self.forgone();
                let Some(relayed) = self.channels.get_mut(&down) else {
                    return Ok(vec![Self::share_error(
                        down,
                        sequence,
                        "invalid-channel-id",
                    )?]);
                };
                let Some(&(donation, pool_job, target)) = relayed.jobs.get(&share.job_id) else {
                    return Ok(vec![Self::share_error(down, sequence, "invalid-job-id")?]);
                };
                let Some(lane) = relayed.lane_mut(donation) else {
                    return Ok(vec![Self::share_error(down, sequence, "stale-share")?]);
                };
                share.channel_id = lane.channel;
                share.job_id = pool_job;
                share.sequence_number = lane.sequence;
                Self::pending(lane, sequence, target, rate, forgone);
                Ok(vec![Out::Up(mining(Mining::SubmitSharesStandard(share))?)])
            }
            MESSAGE_TYPE_UPDATE_CHANNEL => {
                let mut update: UpdateChannel = binary_sv2::from_bytes(frame.payload())
                    .map_err(invalid("invalid channel update"))?;
                let mut out = Vec::new();
                let donation_lane = self
                    .channels
                    .get(&update.channel_id)
                    .and_then(|relayed| relayed.lane(true))
                    .map(|lane| lane.channel);
                out.push(Out::Up(mining(Mining::UpdateChannel(update.clone()))?));
                if let Some(channel) = donation_lane {
                    update.channel_id = channel;
                    out.push(Out::Up(mining(Mining::UpdateChannel(update))?));
                }
                Ok(out)
            }
            MESSAGE_TYPE_CLOSE_CHANNEL => {
                let mut close: CloseChannel = binary_sv2::from_bytes(frame.payload())
                    .map_err(invalid("invalid channel close"))?;
                let mut out = Vec::new();
                let donation_lane = self
                    .channels
                    .remove(&close.channel_id)
                    .and_then(|relayed| relayed.lane(true).map(|lane| lane.channel));
                out.push(Out::Up(mining(Mining::CloseChannel(close.clone()))?));
                if let Some(channel) = donation_lane {
                    close.channel_id = channel;
                    out.push(Out::Up(mining(Mining::CloseChannel(close))?));
                }
                Ok(out)
            }
            MESSAGE_TYPE_SET_CUSTOM_MINING_JOB | MESSAGE_TYPE_SETUP_CONNECTION => {
                Err("unexpected device message".into())
            }
            _ => Ok(vec![Out::Up(frame)]),
        }
    }

    /// What the credit spares a miner's own share now, in hundredths of a
    /// percent.
    fn forgone(&self) -> u16 {
        let (effective, _, rate) = self.rates();
        u16::from(effective).saturating_sub(u16::from(rate))
    }

    fn pending(lane: &mut Lane, down: u32, target: [u8; 32], rate: BchDonation, forgone: u16) {
        if lane.pending.len() >= MAX_PENDING {
            lane.pending.pop_first();
        }
        lane.pending.insert(
            lane.sequence,
            Pending {
                down,
                target,
                rate,
                forgone,
            },
        );
        lane.sequence = lane.sequence.wrapping_add(1);
    }

    /// A device's channel request under its login: the door's request id
    /// and the identity at the pool, or the refusal to send back.
    fn login(
        &mut self,
        id: u32,
        name: &str,
        open: OpenParams,
    ) -> Result<Result<(u32, String), Out>, String> {
        let (identity, payout, donation) = match self.miners {
            Some(logins) => match miner_login(logins, name) {
                Ok(login) => (login.identity, Some(login.payout), login.donation),
                Err(reason) => return Ok(Err(Self::refuse(id, refusal_code(&reason))?)),
            },
            None => (name.to_owned(), None, None),
        };
        let pays_donation = match (self.miners, &payout) {
            (Some(logins), Some(payout)) => is_donation(logins.network, payout),
            _ => false,
        };
        // As a Public Solo Endpoint does: one donation choice per
        // connection, which a donation channel neither fixes nor checks.
        if !pays_donation {
            match self.choice {
                Some(choice) if choice != donation => {
                    return Ok(Err(Self::refuse(id, "donation-mismatch")?));
                }
                _ => self.choice = Some(donation),
            }
        }
        let request = self.request_id();
        self.requests.insert(
            request,
            Request::Device {
                id,
                name: name.to_owned(),
                pays_donation,
                open,
            },
        );
        Ok(Ok((request, identity)))
    }

    /// The donation lane's request for the device channel `down`.
    fn donation_request(
        &mut self,
        down: u32,
        identity: &str,
        retried: bool,
    ) -> Result<Option<Out>, String> {
        let Some(relayed) = self.channels.get(&down) else {
            return Ok(None);
        };
        let open = relayed.open;
        let size = relayed.extranonce;
        let request_id = self.request_id();
        self.requests.insert(
            request_id,
            Request::Donation {
                channel: down,
                retried,
            },
        );
        let user_identity = identity.try_into().map_err(|_| "identity too long")?;
        let max_target = (&open.max_target).into();
        let frame = if open.standard {
            mining(Mining::OpenStandardMiningChannel(
                OpenStandardMiningChannel {
                    request_id,
                    user_identity,
                    nominal_hash_rate: open.nominal,
                    max_target,
                },
            ))?
        } else {
            mining(Mining::OpenExtendedMiningChannel(
                OpenExtendedMiningChannel {
                    request_id,
                    user_identity,
                    nominal_hash_rate: open.nominal,
                    max_target,
                    min_extranonce_size: size.map_or(open.min_extranonce, |size| size as u16),
                },
            ))?
        };
        Ok(Some(Out::Up(frame)))
    }

    /// Stops donating on the device channel `down`: the lane closes at the
    /// pool and the device goes back to its own jobs.
    fn drop_donation(&mut self, down: u32, close: bool) -> Result<Vec<Out>, String> {
        let Some(relayed) = self.channels.get_mut(&down) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if let (true, Slot::Open(lane)) = (close, &relayed.donation) {
            out.push(Out::Up(mining(Mining::CloseChannel(CloseChannel {
                channel_id: lane.channel,
                reason_code: "donation-stopped"
                    .try_into()
                    .map_err(invalid("invalid close"))?,
            }))?));
        }
        relayed.donation = Slot::Unavailable;
        if relayed.on_donation {
            relayed.on_donation = false;
            out.extend(relayed.switch(down)?);
        }
        Ok(out)
    }

    /// The pool's verdicts on shares of the lane `(down, donation)`:
    /// accepted (`accepted`) or refused for `reason`, for the workers page,
    /// the work allocation and the own donation measure, at unix time
    /// `unix`.
    fn verdicts(
        &mut self,
        down: u32,
        donation: bool,
        shares: Vec<Pending>,
        refused: Option<&'static str>,
        unix: u32,
    ) -> Result<Vec<Out>, String> {
        let Some(relayed) = self.channels.get_mut(&down) else {
            return Ok(Vec::new());
        };
        let pays_donation = relayed.pays_donation;
        let mut refusals = 0;
        for share in &shares {
            match refused {
                None => {
                    relayed
                        .allocation
                        .pool_accepted(&share.target, share.rate, donation);
                    self.noted.work.add(&share.target, donation, false);
                    self.noted.verdicts.push(ShareEvent::Accepted(share.target));
                    // A joining Pickaxe's donation channel is its own
                    // donation; the miner's own lane's shares carry what
                    // the credit spared them; the door's donation lane is
                    // the door's.
                    let work = expected_hashes(&share.target);
                    self.own.record(
                        unix,
                        work,
                        pays_donation,
                        if pays_donation || donation {
                            0
                        } else {
                            share.forgone
                        },
                    );
                    if donation {
                        relayed.refusals = 0;
                    }
                }
                Some(reason) => {
                    self.noted.verdicts.push(ShareEvent::Rejected(reason));
                    refusals += 1;
                }
            }
        }
        if donation && refusals > 0 {
            relayed.refusals = relayed.refusals.saturating_add(refusals);
            if relayed.refusals >= DONATION_REFUSALS {
                return self.drop_donation(down, true);
            }
        }
        Ok(Vec::new())
    }

    /// One frame from the pool, at `unix` seconds.
    pub(super) fn pool_frame(
        &mut self,
        mut frame: SerializedFrame,
        unix: u32,
    ) -> Result<Vec<Out>, String> {
        let header = frame.header();
        if header.ext_type_without_channel_msg() != 0 {
            return Ok(vec![Out::Down(frame)]);
        }
        let kind = header.msg_type();
        let channel_message = header.channel_msg();
        let payload = frame.payload().to_vec();
        let mut bytes = payload.clone();
        let mut out = Vec::new();
        match kind {
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS
            | MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL_SUCCESS => {
                let extended = kind == MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS;
                let (request_id, lane, mut reply) = if extended {
                    let open: OpenExtendedMiningChannelSuccess = binary_sv2::from_bytes(&mut bytes)
                        .map_err(invalid("invalid channel reply"))?;
                    let lane = Lane::new(
                        open.channel_id,
                        open.group_channel_id,
                        open.extranonce_prefix.as_ref(),
                        usize::from(open.extranonce_size),
                        open.target.as_ref(),
                    );
                    (open.request_id, lane, Some(open))
                } else {
                    let open: OpenStandardMiningChannelSuccess = binary_sv2::from_bytes(&mut bytes)
                        .map_err(invalid("invalid channel reply"))?;
                    let lane = Lane::new(
                        open.channel_id,
                        open.group_channel_id,
                        &[],
                        0,
                        open.target.as_ref(),
                    );
                    (open.request_id, lane, None)
                };
                match self.requests.remove(&request_id) {
                    Some(Request::Device {
                        id,
                        name,
                        pays_donation,
                        open,
                    }) => {
                        let down = lane.channel;
                        // The device's own request id and its channel,
                        // outside any group: the door sends each channel
                        // its own frames, and the pool's prefix goes into
                        // the coinbase part of each job.
                        let frame = match reply.take() {
                            Some(mut success) => {
                                success.request_id = id;
                                success.group_channel_id = 0;
                                let empty: &[u8] = &[];
                                success.extranonce_prefix =
                                    empty.try_into().map_err(invalid("invalid prefix"))?;
                                mining(Mining::OpenExtendedMiningChannelSuccess(success))?
                            }
                            None => {
                                let mut copy = payload.clone();
                                let mut success: OpenStandardMiningChannelSuccess =
                                    binary_sv2::from_bytes(&mut copy)
                                        .map_err(invalid("invalid channel reply"))?;
                                success.request_id = id;
                                success.group_channel_id = 0;
                                mining(Mining::OpenStandardMiningChannelSuccess(success))?
                            }
                        };
                        self.channels.insert(
                            down,
                            Relayed {
                                extranonce: extended.then_some(lane.size),
                                personal: lane,
                                donation: Slot::Off,
                                pays_donation,
                                on_donation: false,
                                allocation: WorkAllocation::new(rand::random()),
                                jobs: BTreeMap::new(),
                                next_job: 0,
                                refusals: 0,
                                open,
                            },
                        );
                        if !pays_donation && !self.named {
                            self.noted.name = Some(name);
                            self.named = true;
                        }
                        out.push(Out::Down(frame));
                    }
                    Some(Request::Donation { channel, .. }) => {
                        if let Some(relayed) = self.channels.get_mut(&channel) {
                            let fits = relayed.extranonce.is_none_or(|size| lane.size >= size)
                                && extended == relayed.extranonce.is_some()
                                && lane.channel != relayed.personal.channel;
                            relayed.donation = if fits {
                                Slot::Open(Box::new(lane))
                            } else {
                                Slot::Unavailable
                            };
                        }
                    }
                    None => out.push(Out::Down(frame)),
                }
            }
            MESSAGE_TYPE_OPEN_MINING_CHANNEL_ERROR => {
                let mut error: OpenMiningChannelError =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid channel error"))?;
                match self.requests.remove(&error.request_id) {
                    Some(Request::Device { id, .. }) => {
                        error.request_id = id;
                        out.push(Out::Down(mining(Mining::OpenMiningChannelError(error))?));
                    }
                    // Asked once more under the route's fallback identity
                    // (the bare donation address), as the SV1 adapter does.
                    Some(Request::Donation { channel, retried }) => {
                        let fallback = self
                            .route
                            .as_ref()
                            .and_then(|route| route.fallback.clone())
                            .filter(|_| !retried);
                        match fallback {
                            Some(identity) => {
                                out.extend(self.donation_request(channel, &identity, true)?)
                            }
                            None => out.extend(self.drop_donation(channel, false)?),
                        }
                    }
                    None => out.push(Out::Down(frame)),
                }
            }
            MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB | MESSAGE_TYPE_NEW_MINING_JOB => {
                let extended = kind == MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB;
                let (channel, pool_job, future) = if extended {
                    let job: NewExtendedMiningJob = binary_sv2::from_bytes(&mut bytes)
                        .map_err(invalid("invalid mining job"))?;
                    (job.channel_id, job.job_id, job.is_future())
                } else {
                    let job: NewMiningJob = binary_sv2::from_bytes(&mut bytes)
                        .map_err(invalid("invalid mining job"))?;
                    (job.channel_id, job.job_id, job.is_future())
                };
                let lanes = self.addressed(channel);
                if lanes.is_empty() {
                    return Ok(vec![Out::Down(frame)]);
                }
                for (down, donation) in lanes {
                    let Some(relayed) = self.channels.get_mut(&down) else {
                        continue;
                    };
                    if relayed.extranonce.is_some() != extended {
                        continue;
                    }
                    let Some(lane) = relayed.lane_mut(donation) else {
                        continue;
                    };
                    if future {
                        lane.future.insert(pool_job, payload.clone());
                        while lane.future.len() > MAX_ACTIVE_JOBS {
                            lane.future.pop_first();
                        }
                    } else {
                        lane.latest = Some((pool_job, payload.clone()));
                    }
                    if donation == relayed.on_donation {
                        out.push(Out::Down(relayed.job_out(down, donation, &payload, false)?));
                    }
                }
            }
            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH => {
                let prev: SetNewPrevHash = binary_sv2::from_bytes(&mut bytes)
                    .map_err(invalid("invalid job activation"))?;
                let (channel, pool_job) = (prev.channel_id, prev.job_id);
                let lanes = self.addressed(channel);
                if lanes.is_empty() {
                    return Ok(vec![Out::Down(frame)]);
                }
                for (down, donation) in lanes {
                    let Some(relayed) = self.channels.get_mut(&down) else {
                        continue;
                    };
                    let current = donation == relayed.on_donation;
                    let Some(lane) = relayed.lane_mut(donation) else {
                        continue;
                    };
                    let Some(job) = lane.future.remove(&pool_job) else {
                        continue;
                    };
                    lane.future.clear();
                    lane.prev = Some(payload.clone());
                    lane.latest = Some((pool_job, job.clone()));
                    let sent = lane.sent.get(&pool_job).copied();
                    if !current {
                        lane.sent.clear();
                        continue;
                    }
                    let job_id = match sent {
                        Some(job_id) => job_id,
                        None => {
                            out.push(Out::Down(relayed.job_out(down, donation, &job, true)?));
                            relayed.next_job
                        }
                    };
                    // The device drops every other job on a new parent.
                    relayed.jobs.retain(|id, _| *id == job_id);
                    if let Some(lane) = relayed.lane_mut(donation) {
                        lane.sent.retain(|job, _| *job == pool_job);
                    }
                    out.push(Out::Down(Relayed::prev_out(down, &payload, job_id)?));
                }
            }
            MESSAGE_TYPE_SET_TARGET => {
                let set: SetTarget =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid target"))?;
                let target: [u8; 32] = set
                    .maximum_target
                    .as_ref()
                    .try_into()
                    .map_err(|_| "invalid target")?;
                let lanes = self.addressed(set.channel_id);
                if lanes.is_empty() {
                    return Ok(vec![Out::Down(frame)]);
                }
                for (down, donation) in lanes {
                    let Some(relayed) = self.channels.get_mut(&down) else {
                        continue;
                    };
                    let current = donation == relayed.on_donation;
                    if let Some(lane) = relayed.lane_mut(donation) {
                        lane.target = target;
                    }
                    if current {
                        out.push(Out::Down(mining(Mining::SetTarget(SetTarget {
                            channel_id: down,
                            maximum_target: (&target).into(),
                        }))?));
                    }
                }
            }
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS => {
                let ack: SubmitSharesSuccess = binary_sv2::from_bytes(&mut bytes)
                    .map_err(invalid("invalid share response"))?;
                let Some((down, donation)) = self.lane_of(ack.channel_id) else {
                    return Ok(Vec::new());
                };
                let mut accepted = Vec::new();
                if let Some(lane) = self
                    .channels
                    .get_mut(&down)
                    .and_then(|relayed| relayed.lane_mut(donation))
                {
                    // Pools may acknowledge a batch: every share up to the
                    // last sequence number.
                    while let Some(entry) = lane.pending.first_entry() {
                        if *entry.key() > ack.last_sequence_number {
                            break;
                        }
                        accepted.push(entry.remove());
                    }
                }
                let Some(last) = accepted.iter().map(|share| share.down).max() else {
                    return Ok(Vec::new());
                };
                out.push(Out::Down(mining(Mining::SubmitSharesSuccess(
                    SubmitSharesSuccess {
                        channel_id: down,
                        last_sequence_number: last,
                        new_submits_accepted_count: accepted.len() as u32,
                        new_shares_sum: ack.new_shares_sum,
                    },
                ))?));
                out.extend(self.verdicts(down, donation, accepted, None, unix)?);
            }
            MESSAGE_TYPE_SUBMIT_SHARES_ERROR => {
                let mut error: SubmitSharesError =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid share error"))?;
                let Some((down, donation)) = self.lane_of(error.channel_id) else {
                    return Ok(Vec::new());
                };
                let Some(share) = self
                    .channels
                    .get_mut(&down)
                    .and_then(|relayed| relayed.lane_mut(donation))
                    .and_then(|lane| lane.pending.remove(&error.sequence_number))
                else {
                    return Ok(Vec::new());
                };
                let reason = match error.error_code.as_ref() {
                    b"stale-share" => "stale-share",
                    b"duplicate-share" => "duplicate-share",
                    b"difficulty-too-low" => "difficulty-too-low",
                    b"invalid-job-id" => "invalid-job-id",
                    _ => "rejected-by-pool",
                };
                error.channel_id = down;
                error.sequence_number = share.down;
                out.push(Out::Down(mining(Mining::SubmitSharesError(error))?));
                out.extend(self.verdicts(down, donation, vec![share], Some(reason), unix)?);
            }
            MESSAGE_TYPE_SET_EXTRANONCE_PREFIX => {
                let set: SetExtranoncePrefix =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid prefix"))?;
                // The device's extranonce never changes: the new prefix
                // goes into the coinbase part of the lane's next jobs.
                let Some((down, donation)) = self.lane_of(set.channel_id) else {
                    return Ok(vec![Out::Down(frame)]);
                };
                let prefix = set.extranonce_prefix.as_ref().to_vec();
                if let Some(lane) = self
                    .channels
                    .get_mut(&down)
                    .and_then(|relayed| relayed.lane_mut(donation))
                {
                    lane.prefix = prefix;
                }
            }
            MESSAGE_TYPE_SET_GROUP_CHANNEL => {
                let set: SetGroupChannel =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid group channel"))?;
                let members = set.channel_ids.into_inner();
                for relayed in self.channels.values_mut() {
                    for donation in [false, true] {
                        if let Some(lane) = relayed.lane_mut(donation) {
                            if members.contains(&lane.channel) {
                                lane.group = set.group_channel_id;
                            }
                        }
                    }
                }
            }
            MESSAGE_TYPE_CLOSE_CHANNEL => {
                let close: CloseChannel =
                    binary_sv2::from_bytes(&mut bytes).map_err(invalid("invalid channel close"))?;
                let lanes = self.addressed(close.channel_id);
                if lanes.is_empty() {
                    return Ok(vec![Out::Down(frame)]);
                }
                for (down, donation) in lanes {
                    if donation {
                        out.extend(self.drop_donation(down, false)?);
                    } else if let Some(relayed) = self.channels.remove(&down) {
                        let mut copy = payload.clone();
                        let mut closed: CloseChannel = binary_sv2::from_bytes(&mut copy)
                            .map_err(invalid("invalid channel close"))?;
                        closed.channel_id = down;
                        out.push(Out::Down(mining(Mining::CloseChannel(closed))?));
                        if let Slot::Open(lane) = relayed.donation {
                            out.push(Out::Up(mining(Mining::CloseChannel(CloseChannel {
                                channel_id: lane.channel,
                                reason_code: "channel-closed"
                                    .try_into()
                                    .map_err(invalid("invalid close"))?,
                            }))?));
                        }
                    }
                }
            }
            _ => {
                // A message for a donation lane is the door's; anything else
                // goes on to the device as it came.
                let channel = payload
                    .first_chunk::<4>()
                    .map(|id| u32::from_le_bytes(*id))
                    .filter(|_| channel_message);
                let mine = channel.is_some_and(|channel| {
                    self.lane_of(channel).is_some_and(|(_, donation)| donation)
                });
                if !mine {
                    out.push(Out::Down(frame));
                }
            }
        }
        Ok(out)
    }

    /// Opens donation lanes and moves devices between lanes, at `now`.
    pub(super) fn tick(&mut self, now: Instant) -> Result<Vec<Out>, String> {
        let (effective, credit, rate) = self.rates();
        let mut out = Vec::new();
        let downs: Vec<u32> = self.channels.keys().copied().collect();
        for down in downs {
            let wants = self.channels.get(&down).is_some_and(|relayed| {
                !relayed.pays_donation
                    && matches!(relayed.donation, Slot::Off)
                    && relayed.personal.latest.is_some()
            }) && u16::from(rate) > 0;
            if let Some(identity) = self
                .route
                .as_ref()
                .map(|route| route.identity.clone())
                .filter(|_| wants)
            {
                out.extend(self.donation_request(down, &identity, false)?);
                if let Some(relayed) = self.channels.get_mut(&down) {
                    relayed.donation = Slot::Requested;
                }
            }
            let Some(relayed) = self.channels.get_mut(&down) else {
                continue;
            };
            if relayed.pays_donation {
                continue;
            }
            relayed
                .allocation
                .update(now, relayed.personal.latest.is_some());
            let usable = matches!(
                &relayed.donation,
                Slot::Open(lane) if lane.latest.is_some() && lane.prev.is_some()
            );
            let donate = usable && relayed.allocation.pool_donation_work(rate);
            if donate != relayed.on_donation {
                relayed.on_donation = donate;
                out.extend(relayed.switch(down)?);
            }
        }
        self.noted.terms = self
            .channels
            .values()
            .any(|relayed| !relayed.pays_donation)
            .then_some((effective, credit));
        Ok(out)
    }
}

/// The code a refused username is answered with: P2Pool's and SV2's own
/// codes where they exist.
pub(super) fn refusal_code(reason: &str) -> &'static str {
    if reason == super::super::payout::DONATION_OPTION_ERROR {
        "invalid-donation"
    } else if reason == P2PKH_ONLY {
        "p2pkh-payout-required"
    } else if reason.starts_with("p2p=") {
        "invalid-share-address"
    } else {
        "unknown-user"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MiningNetwork;
    use std::sync::{Arc, RwLock};

    const NETWORK: MiningNetwork = MiningNetwork::Chipnet;
    /// The start of an own donation bucket (five minutes: 6,000,000 of them).
    const T: u32 = 1_800_000_000;

    fn rate(text: &str) -> BchDonation {
        text.parse().unwrap()
    }

    fn miner(seed: u8) -> String {
        crate::tx::cashaddr_with_version(0, &[seed; 20], NETWORK)
    }

    fn share_identity(payout: &str) -> String {
        format!(
            "{payout},p2p={}",
            crate::tx::p2pool_share_address(payout).unwrap()
        )
    }

    /// The door's route at your own P2Pool: the donation key's own share
    /// address, the bare address as its fallback.
    fn relay(donation: &str) -> Relay {
        let address = crate::donation::bch::address(NETWORK);
        Relay::new(
            Some(MinerLogins {
                network: NETWORK,
                p2pool: true,
            }),
            Some(DonationRoute {
                identity: share_identity(address),
                rate: Arc::new(RwLock::new(rate(donation))),
                fallback: Some(address.to_owned()),
            }),
        )
    }

    fn frame(message: Mining<'_>) -> SerializedFrame {
        mining(message).unwrap()
    }

    /// The frames for the pool and for the device, in order.
    fn split(out: Vec<Out>) -> (Vec<SerializedFrame>, Vec<SerializedFrame>) {
        let (mut up, mut down) = (Vec::new(), Vec::new());
        for frame in out {
            match frame {
                Out::Up(frame) => up.push(frame),
                Out::Down(frame) => down.push(frame),
            }
        }
        (up, down)
    }

    fn open(request_id: u32, identity: &str) -> SerializedFrame {
        frame(Mining::OpenExtendedMiningChannel(
            OpenExtendedMiningChannel {
                request_id,
                user_identity: identity.try_into().unwrap(),
                nominal_hash_rate: 1e12,
                max_target: (&[0xffu8; 32]).into(),
                min_extranonce_size: 8,
            },
        ))
    }

    /// The pool's request id and identity of a channel request it got.
    fn opened(mut request: SerializedFrame) -> (u32, String, u16) {
        assert_eq!(
            request.header().msg_type(),
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL
        );
        let open: OpenExtendedMiningChannel = binary_sv2::from_bytes(request.payload()).unwrap();
        (
            open.request_id,
            open.user_identity.as_utf8_or_hex(),
            open.min_extranonce_size,
        )
    }

    fn success(request_id: u32, channel_id: u32, size: u16, prefix: &[u8]) -> SerializedFrame {
        frame(Mining::OpenExtendedMiningChannelSuccess(
            OpenExtendedMiningChannelSuccess {
                request_id,
                channel_id,
                group_channel_id: 0,
                target: (&[0xffu8; 32]).into(),
                extranonce_size: size,
                extranonce_prefix: prefix.try_into().unwrap(),
            },
        ))
    }

    fn job(channel_id: u32, job_id: u32, coinbase: &[u8]) -> SerializedFrame {
        frame(Mining::NewExtendedMiningJob(NewExtendedMiningJob {
            channel_id,
            job_id,
            min_ntime: Sv2Option::new(None),
            version: 0x2000_0000,
            version_rolling_allowed: true,
            merkle_path: Vec::<binary_sv2::U256>::new().try_into().unwrap(),
            coinbase_tx_prefix: coinbase.try_into().unwrap(),
            coinbase_tx_suffix: [7u8; 8].as_slice().try_into().unwrap(),
        }))
    }

    fn prev(channel_id: u32, job_id: u32) -> SerializedFrame {
        frame(Mining::SetNewPrevHash(SetNewPrevHash {
            channel_id,
            job_id,
            prev_hash: (&[0u8; 32]).into(),
            min_ntime: 1_700_000_000,
            nbits: 0x1d00_ffff,
        }))
    }

    fn share(channel_id: u32, sequence_number: u32, job_id: u32) -> SerializedFrame {
        frame(Mining::SubmitSharesExtended(SubmitSharesExtended {
            channel_id,
            sequence_number,
            job_id,
            nonce: 1,
            ntime: 1_700_000_001,
            version: 0x2000_0000,
            extranonce: [9u8; 8].as_slice().try_into().unwrap(),
        }))
    }

    fn ack(channel_id: u32, last_sequence_number: u32) -> SerializedFrame {
        frame(Mining::SubmitSharesSuccess(SubmitSharesSuccess {
            channel_id,
            last_sequence_number,
            new_submits_accepted_count: 1,
            new_shares_sum: 1,
        }))
    }

    /// A job the device got: its channel, job id and coinbase part.
    fn device_job(mut frame: SerializedFrame) -> (u32, u32, Vec<u8>) {
        assert_eq!(
            frame.header().msg_type(),
            MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB
        );
        let job: NewExtendedMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
        (
            job.channel_id,
            job.job_id,
            job.coinbase_tx_prefix.as_ref().to_vec(),
        )
    }

    /// A share the pool got: its channel, sequence, job and extranonce.
    fn pool_share(mut frame: SerializedFrame) -> (u32, u32, u32, Vec<u8>) {
        assert_eq!(
            frame.header().msg_type(),
            MESSAGE_TYPE_SUBMIT_SHARES_EXTENDED
        );
        let share: SubmitSharesExtended = binary_sv2::from_bytes(frame.payload()).unwrap();
        (
            share.channel_id,
            share.sequence_number,
            share.job_id,
            share.extranonce.as_ref().to_vec(),
        )
    }

    /// The device's channel `channel` opened at the pool as `pool_channel`
    /// (size 8, prefix `[pool_channel; 4]`) with job 1 and its parent; the
    /// device's job id.
    fn open_device(relay: &mut Relay, request: u32, identity: &str, pool_channel: u32) -> u32 {
        let (up, _) = split(relay.device_frame(open(request, identity)).unwrap());
        let (pool_request, _, _) = opened(up.into_iter().next().unwrap());
        let (_, down) = split(
            relay
                .pool_frame(
                    success(pool_request, pool_channel, 8, &[pool_channel as u8; 4]),
                    T,
                )
                .unwrap(),
        );
        assert_eq!(down.len(), 1);
        let (_, down) = split(relay.pool_frame(job(pool_channel, 1, &[1; 8]), T).unwrap());
        let (_, _, _) = device_job(down.into_iter().next().unwrap());
        let (_, down) = split(relay.pool_frame(prev(pool_channel, 1), T).unwrap());
        assert_eq!(down.len(), 1, "the parent");
        relay.channels[&pool_channel].next_job
    }

    // #### Pool modes: the Pickaxe donation at the door
    // What: an SV2 device's channel opens at P2Pool under its own identity
    // and, once it has work, a donation lane opens beside it under the
    // donation key's share address. At 100% the device is moved to the
    // donation lane's job: the pool's prefix (and the lane's extra
    // extranonce byte as padding) in its coinbase part, under the device's
    // own channel and a job id of the door's. Each share goes to the lane
    // whose job it solved, with its extranonce padded to that lane's, and
    // the pool's verdict comes back under the device's own channel and
    // sequence number, counted as donation or personal work. At 0% no
    // lane opens.
    // Look here if: `Relay`'s lanes, job ids or share mapping change.
    #[test]
    fn an_sv2_device_mines_the_donation_for_its_share_and_shares_reach_their_channel() {
        let mut relay = relay("100");
        let payout = miner(2);
        let (up, _) = split(relay.device_frame(open(7, &payout)).unwrap());
        let (request, identity, _) = opened(up.into_iter().next().unwrap());
        assert_eq!(identity, share_identity(&payout));
        let (_, mut down) = split(
            relay
                .pool_frame(success(request, 5, 8, &[5; 4]), T)
                .unwrap(),
        );
        let reply: OpenExtendedMiningChannelSuccess =
            binary_sv2::from_bytes(down[0].payload()).unwrap();
        assert_eq!(
            (reply.request_id, reply.channel_id, reply.extranonce_size),
            (7, 5, 8)
        );
        assert!(reply.extranonce_prefix.as_ref().is_empty());
        let (_, down) = split(relay.pool_frame(job(5, 1, &[1; 8]), T).unwrap());
        let (channel, own_job, coinbase) = device_job(down.into_iter().next().unwrap());
        assert_eq!(
            (channel, coinbase),
            (5, [[1u8; 8].as_slice(), &[5; 4]].concat())
        );
        relay.pool_frame(prev(5, 1), T).unwrap();
        // A share on the device's own job goes to its own channel.
        let (up, _) = split(relay.device_frame(share(5, 40, own_job)).unwrap());
        assert_eq!(
            pool_share(up.into_iter().next().unwrap()),
            (5, 0, 1, vec![9; 8])
        );
        // The donation lane opens beside it.
        let (up, down) = split(relay.tick(Instant::now()).unwrap());
        assert!(down.is_empty());
        let (request, identity, size) = opened(up.into_iter().next().unwrap());
        assert_eq!(
            identity,
            share_identity(crate::donation::bch::address(NETWORK))
        );
        assert_eq!(size, 8);
        let (_, down) = split(
            relay
                .pool_frame(success(request, 6, 9, &[6; 4]), T)
                .unwrap(),
        );
        assert!(down.is_empty(), "the donation lane is the door's");
        assert!(split(relay.pool_frame(job(6, 3, &[2; 8]), T).unwrap())
            .1
            .is_empty());
        assert!(split(relay.pool_frame(prev(6, 3), T).unwrap()).1.is_empty());
        // At 100% the device moves to the donation lane's job.
        let (_, down) = split(relay.tick(Instant::now()).unwrap());
        assert_eq!(down.len(), 3, "target, job, parent");
        assert_eq!(down[0].clone().header().msg_type(), MESSAGE_TYPE_SET_TARGET);
        let (channel, donation_job, coinbase) = device_job(down[1].clone());
        assert_eq!(channel, 5);
        assert_eq!(coinbase, [[2u8; 8].as_slice(), &[6; 4], &[0]].concat());
        assert_ne!(donation_job, own_job);
        // Its share goes to the donation lane, padded to its extranonce.
        let (up, _) = split(relay.device_frame(share(5, 41, donation_job)).unwrap());
        let mut padded = vec![0];
        padded.extend([9; 8]);
        assert_eq!(
            pool_share(up.into_iter().next().unwrap()),
            (6, 0, 3, padded)
        );
        // The verdicts come back under the device's channel and sequence.
        let (_, mut down) = split(relay.pool_frame(ack(6, 0), T).unwrap());
        let reply: SubmitSharesSuccess = binary_sv2::from_bytes(down[0].payload()).unwrap();
        assert_eq!((reply.channel_id, reply.last_sequence_number), (5, 41));
        let (_, mut down) = split(relay.pool_frame(ack(5, 0), T).unwrap());
        let reply: SubmitSharesSuccess = binary_sv2::from_bytes(down[0].payload()).unwrap();
        assert_eq!((reply.channel_id, reply.last_sequence_number), (5, 40));
        assert!(relay.noted.work.donation > 0.0 && relay.noted.work.personal > 0.0);
        assert_eq!(relay.noted.verdicts.len(), 2);
        // An old job of the other lane is gone with the switch.
        let (_, mut down) = split(relay.device_frame(share(5, 42, own_job)).unwrap());
        let error: SubmitSharesError = binary_sv2::from_bytes(down[0].payload()).unwrap();
        assert_eq!(error.error_code.as_ref(), b"invalid-job-id");
        // At 0%, no donation lane.
        let mut free = relay_at("0");
        open_device(&mut free, 1, &miner(3), 5);
        assert!(free.tick(Instant::now()).unwrap().is_empty());
    }

    fn relay_at(donation: &str) -> Relay {
        relay(donation)
    }

    // #### Pool modes: one donation, the higher rate, at the door
    // What: a typed `,pickaxe-donation=0` leaves the server's 2%; a joining
    // Pickaxe's donation channel (its login the donation's address) opens
    // under the donation key's share address and gets no lane of its own;
    // once a bucket closes with its accepted work at 1.5% of the
    // connection's, the credit is 1.5% and the door's lane takes 0.51% of
    // the miner's own work (0.5% of all of it). A P2SH payout and a second
    // channel naming another donation are refused.
    // Look here if: `Relay::rates`, `own_donation` use or `login` change.
    #[test]
    fn a_joining_pickaxes_visible_donation_is_credited_and_a_typed_zero_lowers_nothing() {
        let mut relay = relay("2");
        let payout = miner(2);
        let own_job = open_device(
            &mut relay,
            1,
            &format!("{payout}.rig,pickaxe-donation=0"),
            5,
        );
        assert_eq!(relay.rates(), (rate("2"), rate("0"), rate("2")));
        let donation = crate::donation::bch::address(NETWORK);
        let donation_job = open_device(&mut relay, 2, donation, 6);
        assert!(relay.channels[&6].pays_donation);
        // 3 shares of 200 on the donation channel, acknowledged one by one.
        let mut sequence = [0u32, 0u32];
        for index in 0..200u32 {
            let (channel, job_id, slot) = if index % 66 == 0 && index < 198 {
                (6, donation_job, 1)
            } else {
                (5, own_job, 0)
            };
            relay.device_frame(share(channel, index, job_id)).unwrap();
            relay.pool_frame(ack(channel, sequence[slot]), T).unwrap();
            sequence[slot] += 1;
        }
        // The next bucket: one more of the miner's own.
        relay.device_frame(share(5, 200, own_job)).unwrap();
        relay.pool_frame(ack(5, sequence[0]), T + 300).unwrap();
        assert_eq!(relay.rates(), (rate("2"), rate("1.5"), rate("0.51")));
        relay.tick(Instant::now()).unwrap();
        assert_eq!(relay.noted.terms, Some((rate("2"), rate("1.5"))));
        // The donation channel never gets a donation lane of its own.
        assert!(matches!(relay.channels[&6].donation, Slot::Off));
        // Refusals: a P2SH payout, and another donation on the connection.
        let p2sh = crate::tx::cashaddr_with_version(8, &[4; 20], NETWORK);
        let (_, mut down) = split(relay.device_frame(open(3, &p2sh)).unwrap());
        let error: OpenMiningChannelError = binary_sv2::from_bytes(down[0].payload()).unwrap();
        assert_eq!(error.error_code.as_ref(), b"p2pkh-payout-required");
        let (_, mut down) = split(
            relay
                .device_frame(open(4, &format!("{payout},donation=3")))
                .unwrap(),
        );
        let error: OpenMiningChannelError = binary_sv2::from_bytes(down[0].payload()).unwrap();
        assert_eq!(error.error_code.as_ref(), b"donation-mismatch");
        // A login naming more raises the rate.
        let mut more = relay_at("1.5");
        open_device(&mut more, 1, &format!("{payout},donation=3"), 5);
        assert_eq!(more.rates().0, rate("3"));
    }
}
