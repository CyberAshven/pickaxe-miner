//! Templates from an SV2 Template Provider (another Pickaxe's template
//! server, a node bridge, or a node with one built in), as a
//! `TemplateSource` the server fails over with, like a node over JSON-RPC.
//! A provider that grants 0x4C54 pushes each
//! template's transactions (see `large`).

use super::super::{
    channel::MAX_ACTIVE_JOBS,
    extensions::{
        ExtensionMessage, RequestExtensions, RequestExtensionsError, RequestExtensionsSuccess,
        LARGE_TEMPLATES, NEGOTIATION, REQUEST_EXTENSIONS_ERROR, REQUEST_EXTENSIONS_SUCCESS,
        REQUEST_TRANSACTION_DATA_KEPT, REQUEST_TRANSACTION_DATA_TRANSACTIONS,
    },
    journal::PendingBlock,
    provider::{SourceKind, SubmissionOutcome, TemplateSource},
    template::{double_sha256, fold, BchTemplate, Hash, Provided},
    transport::{Limits, Receiver, Sender, Session},
    wire::encoded,
};
use super::large::Received;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2,
    bitcoin::{consensus, Transaction},
    codec_sv2::SerializedFrame,
    common_messages_sv2::{self as common, Protocol, SetupConnection},
    template_distribution_sv2::{
        CoinbaseOutputConstraints, NewTemplate, RequestTransactionData,
        RequestTransactionDataError, RequestTransactionDataSuccess, SetNewPrevHash, SubmitSolution,
        MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, MESSAGE_TYPE_NEW_TEMPLATE,
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR,
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS, MESSAGE_TYPE_SET_NEW_PREV_HASH,
        MESSAGE_TYPE_SUBMIT_SOLUTION,
    },
};

/// How long a refresh waits for a template's transactions.
const TX_DATA_WAIT: Duration = Duration::from_secs(2);
/// How long a new connection waits for its first template.
const FIRST_TEMPLATE_WAIT: Duration = Duration::from_secs(10);
/// Future templates a provider may announce before activating one.
const MAX_FUTURES: usize = 8;
/// Parents remembered to recognise this server's own blocks.
const CONFIRMED: usize = 64;
/// The request id of this client's
/// RequestExtensions.
const EXTENSIONS_REQUEST: u16 = 1;
/// The longest a refresh waits for a push that
/// keeps coming; each of its frames gives `TX_DATA_WAIT` more.
const PUSH_WAIT: Duration = Duration::from_secs(60);

pub const BROKE_PROTOCOL: &str = "template provider broke the protocol";
pub const CLOSED: &str = "template provider closed the connection";

/// Where a provider listens: `HOST:PORT` and its authority key, which may
/// be left out only on this computer (loopback).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdpAddress {
    pub address: String,
    pub authority: Option<[u8; 32]>,
}

impl TdpAddress {
    /// `sv2tp://HOST:PORT/KEY`, the key as SV2 authorities are printed
    /// (Base58Check of version 1 and the 32-byte x-only key).
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let rest = text
            .strip_prefix("sv2tp://")
            .ok_or("a template provider is sv2tp://HOST:PORT/KEY")?;
        let (address, key) = match rest.split_once('/') {
            Some((address, key)) => (address, Some(key.trim_end_matches('/'))),
            None => (rest, None),
        };
        let (host, _) = address
            .rsplit_once(':')
            .filter(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok())
            .ok_or("a template provider is sv2tp://HOST:PORT/KEY")?;
        let authority = match key.filter(|key| !key.is_empty()) {
            Some(key) => {
                let invalid = "the template provider's key is not an SV2 authority key";
                let decoded =
                    stratum_core::bitcoin::base58::decode_check(key).map_err(|_| invalid)?;
                match decoded.as_slice() {
                    [1, 0, key @ ..] => Some(<[u8; 32]>::try_from(key).map_err(|_| invalid)?),
                    _ => return Err(invalid.into()),
                }
            }
            None => None,
        };
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if authority.is_none() && !loopback {
            return Err("a template provider on another computer needs its key".into());
        }
        Ok(Self {
            address: address.to_owned(),
            authority,
        })
    }
}

/// Confirms a provider's new parent is on this server's network, given its
/// hash and the template's height. Gives the parent's time on a
/// network that allows minimum-difficulty blocks (see
/// `BchTemplate::stale_bits`).
pub type Guard = Box<dyn FnMut(&Hash, u32) -> Result<Option<u32>, String> + Send>;

#[derive(Clone)]
struct Announced {
    id: u64,
    version: u32,
    prefix: Vec<u8>,
    value: u64,
    outputs_count: u32,
    outputs: Vec<u8>,
    merkle_path: Vec<Hash>,
}

#[derive(Clone, Copy)]
struct Tip {
    prev: Hash,
    ntime: u32,
    bits: u32,
    target: Hash,
}

struct Ready {
    id: u64,
    template: BchTemplate,
}

struct Link {
    sender: Sender,
    receiver: Receiver,
}

/// A template's transactions and excess data, as a provider sent them.
type Transactions = (Vec<Arc<[u8]>>, Vec<u8>);

/// Transaction data a provider sent, or its error code.
type Data = Result<Transactions, String>;

/// 0x4C54 on a provider's link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Negotiation {
    /// Not asked for: the provider closed the last link on the request.
    Unasked,
    /// Asked for, and not answered yet.
    Asked,
    /// Granted: the provider pushes each template's transactions.
    Granted,
    /// Refused, or the provider went on without answering.
    Refused,
}

pub struct TdpSource {
    address: TdpAddress,
    reserve: u32,
    guard: Guard,
    link: Option<Link>,
    futures: BTreeMap<u64, Announced>,
    last_id: Option<u64>,
    tip: Option<Tip>,
    newest: Option<Announced>,
    data: HashMap<u64, Data>,
    current: Option<Ready>,
    previous: VecDeque<Ready>,
    generation: u64,
    confirmed: VecDeque<Hash>,
    /// The parent `guard` confirmed last, and its time when given.
    guarded: Option<(Hash, Option<u32>)>,
    /// What the provider answered on this link,
    /// and its pushes once it granted 0x4C54.
    negotiation: Negotiation,
    received: Received,
}

impl TdpSource {
    /// A provider at `address`, told this server may add `reserve` bytes of
    /// coinbase outputs, whose new parents `guard` confirms.
    pub fn new(address: TdpAddress, reserve: u32, guard: Guard) -> Self {
        Self {
            address,
            reserve,
            guard,
            link: None,
            futures: BTreeMap::new(),
            last_id: None,
            tip: None,
            newest: None,
            data: HashMap::new(),
            current: None,
            previous: VecDeque::new(),
            generation: 0,
            confirmed: VecDeque::new(),
            guarded: None,
            negotiation: Negotiation::Unasked,
            received: Received::default(),
        }
    }

    /// Whether this link's provider granted
    /// 0x4C54.
    #[cfg(test)]
    pub(super) fn large_templates(&self) -> bool {
        self.negotiation == Negotiation::Granted
    }

    /// Opens a link; with `ask`, asking for BCH large templates.
    fn connect(&mut self, ask: bool) -> Result<(), String> {
        let address = self
            .address
            .address
            .to_socket_addrs()
            .map_err(|_| "cannot resolve the template provider")?
            .next()
            .ok_or("cannot resolve the template provider")?;
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
            .map_err(|_| "cannot reach the template provider")?;
        let session = match self.address.authority {
            Some(authority) => Session::initiate_with(stream, authority, Limits::TDP_CLIENT)?,
            None => Session::initiate_unpinned(stream, Limits::TDP_CLIENT)?,
        };
        let (mut sender, mut receiver) = session.split();
        let (host, port) = self
            .address
            .address
            .rsplit_once(':')
            .and_then(|(host, port)| Some((host, port.parse::<u16>().ok()?)))
            .ok_or("the template provider's address has no port")?;
        let firmware = format!("pickaxe {}", env!("CARGO_PKG_VERSION"));
        sender.send(encoded(
            SetupConnection {
                protocol: Protocol::TemplateDistributionProtocol,
                min_version: 2,
                max_version: 2,
                flags: 0,
                endpoint_host: host.try_into().map_err(|_| "host too long")?,
                endpoint_port: port,
                vendor: "Pickaxe".try_into().map_err(|_| "vendor too long")?,
                hardware_version: "".try_into().map_err(|_| "version too long")?,
                firmware: firmware
                    .as_str()
                    .try_into()
                    .map_err(|_| "firmware too long")?,
                device_id: "".try_into().map_err(|_| "device too long")?,
            },
            common::MESSAGE_TYPE_SETUP_CONNECTION,
            false,
        )?)?;
        let deadline = Instant::now() + FIRST_TEMPLATE_WAIT;
        let reply = loop {
            // An extension's frame is
            // not the answer, and is discarded (see `handle`).
            if let Some(frame) = receiver.receive(Duration::from_millis(100))? {
                if frame.header().ext_type_without_channel_msg() == 0 {
                    break frame;
                }
            }
            if Instant::now() >= deadline {
                return Err("template provider did not answer the setup".into());
            }
        };
        if reply.header().msg_type() != common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS {
            return Err("template provider refused the setup".into());
        }
        // What: right after the setup the client asks for 0x4C54 (request
        // 1), then sends its constraints without waiting: a provider that
        // negotiates answers before it reads them, and one that does not
        // goes on to its templates, which leaves the extension off.
        // Why: Knuth's provider takes RequestExtensions only before the
        // constraints; a provider that ignores it costs no wait.
        // Look here if: a provider's link never gets the extension, or one
        // closes it right after the setup.
        if ask {
            sender.send(
                RequestExtensions {
                    request_id: EXTENSIONS_REQUEST,
                    requested: vec![LARGE_TEMPLATES],
                }
                .frame()?,
            )?;
        }
        sender
            .send(encoded(
                CoinbaseOutputConstraints {
                    coinbase_output_max_additional_size: self.reserve,
                    coinbase_output_max_additional_sigops: 0,
                },
                MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS,
                false,
            )?)
            // A provider that closed on the request, before the constraints
            // went out.
            .map_err(|error| if ask { CLOSED.to_owned() } else { error })?;
        self.forget();
        if ask {
            self.negotiation = Negotiation::Asked;
        }
        self.link = Some(Link { sender, receiver });
        Ok(())
    }

    /// Forgets the protocol state of a closed link.
    fn forget(&mut self) {
        self.futures.clear();
        self.last_id = None;
        self.tip = None;
        self.newest = None;
        self.data.clear();
        self.negotiation = Negotiation::Unasked;
        self.received = Received::default();
    }

    fn close(&mut self) {
        self.link = None;
        self.forget();
    }

    /// Reads every frame waiting; an error closes the link.
    fn drain(&mut self, wait: Duration) -> Result<(), String> {
        let result = (|| {
            let mut idle = wait;
            loop {
                let Some(link) = self.link.as_mut() else {
                    return Err(CLOSED.to_owned());
                };
                let Some(mut frame) = link.receiver.receive(idle).map_err(|_| CLOSED)? else {
                    return Ok(());
                };
                self.handle(&mut frame)?;
                idle = Duration::from_millis(1);
            }
        })();
        if result.is_err() {
            self.close();
        }
        result
    }

    // A provider's protocol
    // What: template ids only rise; a future template waits for the
    // SetNewPrevHash naming it (at most 8 wait); a current template needs
    // an earlier SetNewPrevHash; anything else or the channel bit closes the
    // link and the server moves to its next source.
    // Why: a provider that breaks the protocol cannot be trusted with the
    // work of every device.
    // Look here if: a provider's link keeps closing with "broke the
    // protocol".
    // What: a frame of a non-zero extension type (the channel bit aside) is
    // an extension's (see `extension`). Template Distribution has no
    // channel messages, so the channel bit on extension 0 still breaks the
    // protocol. #### BCH large templates: a Template Distribution message
    // before the answer to RequestExtensions means the provider went on
    // without it, which leaves 0x4C54 off.
    // Why: SV2 requires frames of an unknown extension meant for this end to
    // be discarded and ignored; closing the link on them left a provider
    // that sends them for the next source.
    // Look here if: a provider's extension frames reach the template
    // handling, or close its link.
    fn handle(&mut self, frame: &mut SerializedFrame) -> Result<(), String> {
        let header = frame.header();
        if header.ext_type_without_channel_msg() != 0 {
            return self.extension(frame);
        }
        if header.channel_msg() {
            return Err(BROKE_PROTOCOL.into());
        }
        if self.negotiation == Negotiation::Asked {
            self.negotiation = Negotiation::Refused;
        }
        let granted = self.negotiation == Negotiation::Granted;
        match header.msg_type() {
            MESSAGE_TYPE_NEW_TEMPLATE => {
                let message: NewTemplate =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| BROKE_PROTOCOL)?;
                if self.last_id.is_some_and(|last| message.template_id <= last) {
                    return Err(BROKE_PROTOCOL.into());
                }
                self.last_id = Some(message.template_id);
                if granted {
                    self.received.announced(message.template_id);
                }
                let announced = Announced {
                    id: message.template_id,
                    version: message.version,
                    prefix: message.coinbase_prefix.as_ref().to_vec(),
                    value: message.coinbase_tx_value_remaining,
                    outputs_count: message.coinbase_tx_outputs_count,
                    outputs: message.coinbase_tx_outputs.as_ref().to_vec(),
                    merkle_path: message
                        .merkle_path
                        .iter()
                        .map(|hash| Hash::try_from(hash.as_ref()).map_err(|_| BROKE_PROTOCOL))
                        .collect::<Result<_, _>>()?,
                };
                if message.future_template {
                    if self.futures.len() >= MAX_FUTURES {
                        return Err(BROKE_PROTOCOL.into());
                    }
                    self.futures.insert(announced.id, announced);
                } else {
                    if self.tip.is_none() {
                        return Err(BROKE_PROTOCOL.into());
                    }
                    self.newest = Some(announced);
                }
            }
            MESSAGE_TYPE_SET_NEW_PREV_HASH => {
                let message: SetNewPrevHash =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| BROKE_PROTOCOL)?;
                let announced = self
                    .futures
                    .remove(&message.template_id)
                    .ok_or(BROKE_PROTOCOL)?;
                self.futures.clear();
                let tip = Tip {
                    prev: Hash::try_from(message.prev_hash.as_ref()).map_err(|_| BROKE_PROTOCOL)?,
                    ntime: message.ntime_start,
                    bits: message.n_bits,
                    target: Hash::try_from(message.target.as_ref()).map_err(|_| BROKE_PROTOCOL)?,
                };
                self.confirmed.push_back(tip.prev);
                while self.confirmed.len() > CONFIRMED {
                    self.confirmed.pop_front();
                }
                self.tip = Some(tip);
                self.newest = Some(announced);
            }
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS => {
                let message: RequestTransactionDataSuccess =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| BROKE_PROTOCOL)?;
                if granted {
                    return self.finish_push(&message);
                }
                let transactions = message
                    .transaction_list
                    .iter()
                    .map(|tx| Arc::from(tx.as_ref()))
                    .collect();
                self.data.insert(
                    message.template_id,
                    Ok((transactions, message.excess_data.as_ref().to_vec())),
                );
            }
            // With 0x4C54 this client asks for
            // nothing, so no answer comes.
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR if granted => {
                return Err(BROKE_PROTOCOL.into())
            }
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR => {
                let message: RequestTransactionDataError =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| BROKE_PROTOCOL)?;
                self.data.insert(
                    message.template_id,
                    Err(String::from_utf8_lossy(message.error_code.as_ref()).into_owned()),
                );
            }
            _ => return Err(BROKE_PROTOCOL.into()),
        }
        Ok(())
    }

    // What: an extension's frame. While RequestExtensions is unanswered,
    // its Success (request 1) grants 0x4C54 when it lists it; its Error
    // grants nothing, unless it requires an extension this client does not
    // implement, which ends the link. Once 0x4C54 is granted, its Kept and
    // pages build the push due, and any other of its messages, or one with
    // the channel bit, breaks the protocol. Any other extension's frame is
    // discarded, as before.
    // Why: Knuth's provider sends only Kept and pages on Template
    // Distribution, and the client checks what they rebuild before it
    // mines on it (`Received`).
    // Look here if: a provider's grant goes unused, or its pushes close the
    // link.
    fn extension(&mut self, frame: &mut SerializedFrame) -> Result<(), String> {
        let header = frame.header();
        let kind = header.msg_type();
        match (header.ext_type_without_channel_msg(), self.negotiation) {
            (NEGOTIATION, Negotiation::Asked)
                if !header.channel_msg()
                    && matches!(kind, REQUEST_EXTENSIONS_SUCCESS | REQUEST_EXTENSIONS_ERROR) =>
            {
                self.negotiation = if grants_large_templates(kind, frame.payload())? {
                    Negotiation::Granted
                } else {
                    Negotiation::Refused
                };
            }
            (LARGE_TEMPLATES, Negotiation::Granted) => {
                let read = match kind {
                    _ if header.channel_msg() => Err("a large templates message on a channel"),
                    REQUEST_TRANSACTION_DATA_TRANSACTIONS => self.received.page(frame.payload()),
                    REQUEST_TRANSACTION_DATA_KEPT => self.received.kept(frame.payload()),
                    _ => Err("a large templates message only a client sends"),
                };
                read.map_err(|_| BROKE_PROTOCOL)?;
            }
            _ => (),
        }
        Ok(())
    }

    /// The core Success that ends a push, checked
    /// against its template's merkle path.
    fn finish_push(&mut self, message: &RequestTransactionDataSuccess<'_>) -> Result<(), String> {
        if !message.excess_data.as_ref().is_empty() {
            return Err(BROKE_PROTOCOL.into());
        }
        let id = message.template_id;
        let path = match &self.newest {
            Some(newest) if newest.id == id => newest.merkle_path.clone(),
            _ => self
                .futures
                .get(&id)
                .map(|announced| announced.merkle_path.clone())
                .ok_or(BROKE_PROTOCOL)?,
        };
        let last = message
            .transaction_list
            .iter()
            .map(|tx| Arc::from(tx.as_ref()))
            .collect();
        self.received
            .finish(id, last, &path)
            .map_err(|_| BROKE_PROTOCOL.into())
    }

    /// Waits for the newest template's push,
    /// while it keeps coming; a newer template's push replaces it.
    fn await_push(&mut self) -> Result<(), String> {
        let started = Instant::now();
        let mut deadline = started + TX_DATA_WAIT;
        loop {
            let newest = self
                .newest
                .as_ref()
                .ok_or("template provider sent no template")?
                .id;
            if self
                .current
                .as_ref()
                .is_some_and(|ready| ready.id == newest)
                || self.received.pushed(newest).is_some()
            {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline || now >= started + PUSH_WAIT {
                return Err("template provider sent no transaction data in time".into());
            }
            let progress = self.received.progress;
            self.drain(Duration::from_millis(50))?;
            if self.received.progress != progress {
                deadline = Instant::now() + TX_DATA_WAIT;
            }
        }
    }

    /// The transactions of template `id`, asked for and awaited.
    fn transactions(&mut self, id: u64) -> Result<Transactions, String> {
        if !self.data.contains_key(&id) {
            let link = self.link.as_mut().ok_or(CLOSED)?;
            link.sender
                .send(encoded(
                    RequestTransactionData { template_id: id },
                    MESSAGE_TYPE_REQUEST_TRANSACTION_DATA,
                    false,
                )?)
                .map_err(|_| CLOSED)?;
            let deadline = Instant::now() + TX_DATA_WAIT;
            while !self.data.contains_key(&id) {
                if Instant::now() >= deadline {
                    return Err("template provider sent no transaction data in time".into());
                }
                self.drain(Duration::from_millis(50))?;
            }
        }
        match self.data.remove(&id).ok_or(CLOSED)? {
            Ok(data) => Ok(data),
            Err(code) if code == "template-too-large" => Err("template too large for SV2".into()),
            Err(_) => Err("template provider sent no transaction data in time".into()),
        }
    }
}

impl TemplateSource for TdpSource {
    fn kind(&self) -> SourceKind {
        SourceKind::TemplateProvider
    }

    fn current(&self) -> Option<(u64, &BchTemplate)> {
        self.current
            .as_ref()
            .map(|ready| (self.generation, &ready.template))
    }

    fn tip_is_current(&mut self) -> Result<bool, String> {
        if self.link.is_none() {
            return Ok(false);
        }
        self.drain(Duration::from_millis(1))?;
        Ok(self.current.as_ref().map(|ready| ready.id) == self.newest.as_ref().map(|new| new.id))
    }

    fn refresh(&mut self) -> Result<(u64, &BchTemplate), String> {
        if self.link.is_none() {
            // A provider that closes the link on
            // RequestExtensions (a Pickaxe before it took extension frames)
            // is asked nothing on a second one, here or below.
            match self.connect(true) {
                Err(error) if error == CLOSED => self.connect(false)?,
                other => other?,
            }
            let deadline = Instant::now() + FIRST_TEMPLATE_WAIT;
            while self.newest.is_none() || self.tip.is_none() {
                if Instant::now() >= deadline {
                    self.close();
                    return Err("template provider sent no template".into());
                }
                let unanswered = self.negotiation == Negotiation::Asked;
                match self.drain(Duration::from_millis(100)) {
                    Ok(()) => (),
                    Err(error) if unanswered && error == CLOSED => self.connect(false)?,
                    Err(error) => return Err(error),
                }
            }
        }
        self.drain(Duration::from_millis(1))?;
        let granted = self.negotiation == Negotiation::Granted;
        if granted {
            self.await_push()?;
        }
        let newest = self
            .newest
            .clone()
            .ok_or("template provider sent no template")?;
        let tip = self.tip.ok_or("template provider sent no template")?;
        if self
            .current
            .as_ref()
            .is_some_and(|ready| ready.id == newest.id)
        {
            let ready = self.current.as_ref().ok_or(CLOSED)?;
            return Ok((self.generation, &ready.template));
        }
        let (transactions, excess) = if granted {
            let pushed = self
                .received
                .pushed(newest.id)
                .ok_or("template provider sent no transaction data in time")?;
            (pushed.transactions.clone(), Vec::new())
        } else if newest.merkle_path.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            self.transactions(newest.id)?
        };
        // BCH: no required coinbase outputs and no excess data, which would
        // mean a Bitcoin provider (a segwit commitment).
        if newest.outputs_count != 0 || !newest.outputs.is_empty() || !excess.is_empty() {
            return Err("template provider requires coinbase outputs (a Bitcoin template?)".into());
        }
        let mut template = BchTemplate::from_provided(Provided {
            previous_hash: tip.prev,
            version: newest.version,
            bits: tip.bits,
            target: tip.target,
            ntime: tip.ntime,
            prefix: &newest.prefix,
            value: newest.value,
            transactions,
            merkle_path: &newest.merkle_path,
            reserve: self.reserve,
        })
        .map_err(|_| "template provider sent an invalid template")?;
        // The network guard
        // What: a provider's new parent is confirmed on this server's
        // network (by the miner's node, or the network's Fulcrum servers)
        // before its template is mined.
        // Why: TDP carries no chain; a provider on another network would
        // waste every device's work.
        // Look here if: a provider is refused with "network is not
        // confirmed".
        let parent_time = match self.guarded {
            Some((parent, time)) if parent == tip.prev => time,
            _ => {
                let time = (self.guard)(&tip.prev, template.height)
                    .map_err(|_| "template provider's network is not confirmed")?;
                self.guarded = Some((tip.prev, time));
                time
            }
        };
        template.parent_time = parent_time;
        if let Some(old) = self.current.take() {
            if old.template.previous_hash == template.previous_hash {
                self.previous.push_back(old);
                while self.previous.len() >= MAX_ACTIVE_JOBS {
                    self.previous.pop_front();
                }
            } else {
                self.previous.clear();
            }
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("template generation exhausted")?;
        let ready = self.current.insert(Ready {
            id: newest.id,
            template,
        });
        Ok((self.generation, &ready.template))
    }

    // Solutions to a provider
    // What: a saved block goes back as SubmitSolution on the template it was
    // built from; it counts as accepted once the provider names it as a
    // later parent. Until then the journal retries it, and the server's
    // whole-block fallback sends it to a node too.
    // Why: TDP has no reply to a solution; a new parent is the provider's
    // answer.
    // Look here if: blocks on a provider's templates stay pending.
    fn submit_saved(&mut self, pending: &PendingBlock) -> SubmissionOutcome {
        if self.link.is_some() && self.drain(Duration::from_millis(1)).is_err() {
            return SubmissionOutcome::Pending("template-provider-closed");
        }
        let Some((header, coinbase)) = block_parts(&pending.block) else {
            return SubmissionOutcome::Pending("template-provider-template-gone");
        };
        let hash = double_sha256(&header);
        if self.confirmed.contains(&hash) {
            return SubmissionOutcome::Accepted;
        }
        let leaf = double_sha256(&coinbase);
        let found = self
            .current
            .iter()
            .chain(self.previous.iter())
            .find(|ready| {
                header[4..36] == ready.template.previous_hash[..]
                    && header[36..68] == fold(leaf, ready.template.merkle_path())[..]
            })
            .map(|ready| ready.id);
        let (Some(id), Some(link)) = (found, self.link.as_mut()) else {
            return SubmissionOutcome::Pending("template-provider-template-gone");
        };
        let word =
            |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap_or_default());
        let Ok(coinbase_tx) = coinbase.as_slice().try_into() else {
            return SubmissionOutcome::Pending("template-provider-template-gone");
        };
        let sent = encoded(
            SubmitSolution {
                template_id: id,
                version: word(0),
                ntime: word(68),
                nonce: word(76),
                coinbase_tx,
            },
            MESSAGE_TYPE_SUBMIT_SOLUTION,
            false,
        )
        .and_then(|frame| link.sender.send(frame));
        if sent.is_err() {
            self.close();
            return SubmissionOutcome::Pending("template-provider-closed");
        }
        SubmissionOutcome::Pending("submitted-to-template-provider")
    }

    fn reset(&mut self) {
        self.close();
        self.current = None;
        self.previous.clear();
        self.guarded = None;
    }
}

/// Whether a provider's answer to
/// RequestExtensions (`kind`, `payload`) grants 0x4C54, as the Job
/// Declaration client reads its pool's.
fn grants_large_templates(kind: u8, payload: &[u8]) -> Result<bool, String> {
    if kind == REQUEST_EXTENSIONS_SUCCESS {
        let success = RequestExtensionsSuccess::decode(payload)
            .filter(|success| success.request_id == EXTENSIONS_REQUEST)
            .ok_or(BROKE_PROTOCOL)?;
        return Ok(success.supported.contains(&LARGE_TEMPLATES));
    }
    let error = RequestExtensionsError::decode(payload)
        .filter(|error| error.request_id == EXTENSIONS_REQUEST)
        .ok_or(BROKE_PROTOCOL)?;
    match error.required.iter().find(|required| **required != LARGE_TEMPLATES) {
        Some(missing) => Err(format!(
            "the template provider requires SV2 extension 0x{missing:04x}, which this client does not implement"
        )),
        None => Ok(false),
    }
}

/// A saved block's header and coinbase.
fn block_parts(block: &str) -> Option<([u8; 80], Vec<u8>)> {
    let bytes = hex::decode(block).ok()?;
    let header: [u8; 80] = bytes.get(..80)?.try_into().ok()?;
    let at = match *bytes.get(80)? {
        0xfd => 83,
        0xfe => 85,
        0xff => 89,
        _ => 81,
    };
    let (_, length): (Transaction, usize) =
        consensus::deserialize_partial(bytes.get(at..)?).ok()?;
    Some((header, bytes.get(at..at + length)?.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        template_tests::rpc_template,
        transport::Limits,
        wire::{encoded, extension_frame},
    };
    use super::super::large::{tests::template, Pusher};
    use super::*;
    use std::net::TcpListener;
    use stratum_core::template_distribution_sv2::{
        MESSAGE_TYPE_NEW_TEMPLATE, MESSAGE_TYPE_SET_NEW_PREV_HASH,
    };

    // What: a provider's address is sv2tp://HOST:PORT/KEY; without a key
    // only on this computer; a malformed key or address is refused.
    // Look here if: TdpAddress::parse changes.
    #[test]
    fn provider_addresses_need_a_key_unless_on_this_computer() {
        let mut encoded = vec![1, 0];
        encoded.extend([7; 32]);
        let key = stratum_core::bitcoin::base58::encode_check(&encoded);
        let pinned = TdpAddress::parse(&format!("sv2tp://tp.example:48442/{key}")).unwrap();
        assert_eq!(pinned.address, "tp.example:48442");
        assert_eq!(pinned.authority, Some([7; 32]));
        assert!(TdpAddress::parse("sv2tp://127.0.0.1:48442")
            .unwrap()
            .authority
            .is_none());
        assert!(TdpAddress::parse("sv2tp://localhost:8442/").is_ok());
        for bad in [
            "sv2tp://192.168.0.5:48442",
            "sv2tp://tp.example:48442/notakey",
            "stratum2+tcp://tp.example:48442",
            "sv2tp://tp.example/KEY",
        ] {
            assert!(TdpAddress::parse(bad).is_err(), "{bad}");
        }
    }

    /// A provider that answers the setup, then sends `frames`.
    fn scripted(frames: Vec<SerializedFrame>) -> (TdpAddress, std::thread::JoinHandle<()>) {
        scripted_with(Vec::new(), frames)
    }

    /// The same, sending `early` before its setup answer. It does not
    /// negotiate: the client's RequestExtensions is read and ignored, as a
    /// provider that knows no extension discards it.
    fn scripted_with(
        early: Vec<SerializedFrame>,
        frames: Vec<SerializedFrame>,
    ) -> (TdpAddress, std::thread::JoinHandle<()>) {
        provider(
            move |sender, receiver| {
                constraints(receiver);
                for frame in frames {
                    sender.send(frame).unwrap();
                }
                let _ = receiver.receive(Duration::from_secs(2));
            },
            early,
        )
    }

    /// A provider on loopback that answers one client's setup (after
    /// `early`), then runs `script` on the link.
    fn provider(
        script: impl FnOnce(&mut Sender, &mut Receiver) + Send + 'static,
        early: Vec<SerializedFrame>,
    ) -> (TdpAddress, std::thread::JoinHandle<()>) {
        let secret = [21; 32];
        let public = super::super::super::server::authority_public(&secret).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = TdpAddress {
            address: listener.local_addr().unwrap().to_string(),
            authority: Some(public),
        };
        let thread = std::thread::spawn(move || {
            let (mut sender, mut receiver) = set_up(&listener, &public, &secret, early);
            script(&mut sender, &mut receiver);
        });
        (address, thread)
    }

    /// Accepts the next client on `listener` and answers its setup, after
    /// `early`.
    fn set_up(
        listener: &TcpListener,
        public: &[u8; 32],
        secret: &[u8; 32],
        early: Vec<SerializedFrame>,
    ) -> (Sender, Receiver) {
        let session = Session::accept_with(
            listener.accept().unwrap().0,
            public,
            secret,
            Limits::TDP_SERVER,
        )
        .unwrap();
        let (mut sender, mut receiver) = session.split();
        receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
        for frame in early {
            sender.send(frame).unwrap();
        }
        sender
            .send(
                encoded(
                    stratum_core::common_messages_sv2::SetupConnectionSuccess {
                        used_version: 2,
                        flags: 0,
                    },
                    common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS,
                    false,
                )
                .unwrap(),
            )
            .unwrap();
        (sender, receiver)
    }

    /// Reads the client's frames up to its CoinbaseOutputConstraints.
    fn constraints(receiver: &mut Receiver) {
        while receiver
            .receive(Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .header()
            .msg_type()
            != MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS
        {}
    }

    // What: a SetNewPrevHash naming a template never announced breaks the
    // protocol; the refresh fails with that reason and the link closes.
    // Look here if: TdpSource::handle changes.
    #[test]
    fn a_provider_breaking_the_protocol_fails_the_refresh() {
        let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        let stray = super::super::convert::set_new_prev_hash(9, &template).unwrap();
        assert_eq!(stray.header().msg_type(), MESSAGE_TYPE_SET_NEW_PREV_HASH);
        let (address, provider) = scripted(vec![stray]);
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
        assert_eq!(source.refresh().err().as_deref(), Some(BROKE_PROTOCOL));
        assert!(source.link.is_none());
        provider.join().unwrap();
    }

    // What: a provider's frames of an extension this client never
    // negotiated, before its setup answer and between templates, with and
    // without the channel bit, are discarded: the link stays up and the
    // next template (new bits) arrives. A NewTemplate with the channel bit
    // still breaks the protocol.
    // Look here if: TdpSource::handle or the setup's answer changes.
    #[test]
    fn a_providers_unknown_extension_frames_are_discarded() {
        let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        let mut raw = rpc_template();
        raw["bits"] = serde_json::json!("1d00ffff");
        raw["target"] = serde_json::json!(format!("00000000ffff{}", "00".repeat(26)));
        let limit = BchTemplate::from_rpc(&raw).unwrap();
        let unknown = |channel| extension_frame(0x4c54, 0x00, channel, &[0; 8]).unwrap();
        let (address, provider) = scripted_with(
            vec![unknown(false)],
            vec![
                super::super::convert::new_template(5, true, &template).unwrap(),
                super::super::convert::set_new_prev_hash(5, &template).unwrap(),
                unknown(false),
                unknown(true),
                super::super::convert::new_template(6, true, &limit).unwrap(),
                super::super::convert::set_new_prev_hash(6, &limit).unwrap(),
            ],
        );
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
        let deadline = Instant::now() + Duration::from_secs(5);
        while source
            .current()
            .is_none_or(|(_, current)| current.bits != limit.bits)
        {
            assert!(Instant::now() < deadline, "the next template never arrived");
            source.refresh().unwrap();
        }
        assert!(source.link.is_some());
        provider.join().unwrap();
        let mut frame = super::super::convert::new_template(5, true, &template).unwrap();
        let payload = frame.payload().to_vec();
        let (address, provider) = scripted(vec![extension_frame(
            0,
            MESSAGE_TYPE_NEW_TEMPLATE,
            true,
            &payload,
        )
        .unwrap()]);
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
        assert_eq!(source.refresh().err().as_deref(), Some(BROKE_PROTOCOL));
        provider.join().unwrap();
    }

    // What: a template whose parent the guard does not confirm is refused
    // with "network is not confirmed"; the same template passes once the
    // guard confirms it. And carries the parent's time the
    // guard gave.
    // Look here if: the network guard changes.
    #[test]
    fn the_network_guard_refuses_a_provider_on_another_chain() {
        let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        let frames = || {
            vec![
                super::super::convert::new_template(5, true, &template).unwrap(),
                super::super::convert::set_new_prev_hash(5, &template).unwrap(),
            ]
        };
        let (address, provider) = scripted(frames());
        let mut source = TdpSource::new(
            address,
            122,
            Box::new(|_, _| Err("the parent is not on this network".into())),
        );
        assert_eq!(
            source.refresh().err().as_deref(),
            Some("template provider's network is not confirmed")
        );
        provider.join().unwrap();
        let (address, provider) = scripted(frames());
        let mut checked = Vec::new();
        let mut source = TdpSource::new(
            address,
            122,
            Box::new(move |previous, height| {
                checked.push((*previous, height));
                assert_eq!(height, 325_909);
                Ok(Some(1_699_999_990))
            }),
        );
        let (generation, refreshed) = source.refresh().unwrap();
        assert_eq!(generation, 1);
        assert_eq!(refreshed.height, 325_909);
        assert_eq!(refreshed.parent_time, Some(1_699_999_990));
        assert_eq!(refreshed.size_limit, 80 + 1 + 153 + 122);
        provider.join().unwrap();
    }

    // What: a second SetNewPrevHash on the same parent with new bits (a
    // provider at Chipnet's 20-minute point) gives a new template on those
    // bits and their target, keeping the parent's time the guard gave
    // without asking it again.
    // Look here if: TdpSource::handle's SetNewPrevHash or the guard's cache
    // changes.
    #[test]
    fn new_bits_on_the_same_parent_reach_the_template() {
        let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        let mut raw = rpc_template();
        raw["bits"] = serde_json::json!("1d00ffff");
        raw["target"] = serde_json::json!(format!("00000000ffff{}", "00".repeat(26)));
        let limit = BchTemplate::from_rpc(&raw).unwrap();
        let (address, provider) = scripted(vec![
            super::super::convert::new_template(5, true, &template).unwrap(),
            super::super::convert::set_new_prev_hash(5, &template).unwrap(),
            super::super::convert::new_template(6, true, &limit).unwrap(),
            super::super::convert::set_new_prev_hash(6, &limit).unwrap(),
        ]);
        let mut asked = 0;
        let mut source = TdpSource::new(
            address,
            122,
            Box::new(move |_, _| {
                asked += 1;
                assert_eq!(asked, 1, "the guard is asked once per parent");
                Ok(Some(1_699_999_990))
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while source
            .current()
            .is_none_or(|(_, current)| current.bits != limit.bits)
        {
            assert!(Instant::now() < deadline, "the new bits never arrived");
            source.refresh().unwrap();
        }
        let (_, refreshed) = source.current().unwrap();
        assert_eq!(refreshed.previous_hash, template.previous_hash);
        assert_eq!(refreshed.target, limit.target);
        assert_eq!(refreshed.parent_time, Some(1_699_999_990));
        provider.join().unwrap();
    }

    // What: the reserve is 122 bytes for the payouts alone, and 175 with
    // the Chipnet test token (the 53-byte commitment).
    // Look here if: tdp::reserve changes.
    #[test]
    fn the_reserve_is_122_and_175_with_the_test_token() {
        assert_eq!(super::super::reserve(None), 122);
        let directory = super::super::super::journal::TestDirectory::new();
        let hub = super::super::super::merge::hub::TokenHub::test_token(
            crate::config::MiningNetwork::Chipnet,
            directory.0.join("proofs.json"),
            0x207f_ffff,
            Default::default(),
        )
        .unwrap();
        assert_eq!(super::super::reserve(hub.current().as_deref()), 175);
    }

    /// A provider that answers the client's
    /// RequestExtensions (checked: request 1 for 0x4C54 alone) with
    /// `answer`, or ignores it, reads up to the constraints, then sends each
    /// batch it is given, and stays until the batches end.
    fn paced(
        answer: Option<SerializedFrame>,
    ) -> (
        TdpAddress,
        std::thread::JoinHandle<()>,
        std::sync::mpsc::Sender<Vec<SerializedFrame>>,
    ) {
        let (batches, inbox) = std::sync::mpsc::channel::<Vec<SerializedFrame>>();
        let (address, thread) = provider(
            move |sender, receiver| {
                let mut ask = receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
                assert_eq!(
                    (ask.header().ext_type(), ask.header().msg_type()),
                    (NEGOTIATION, 0x00)
                );
                assert_eq!(
                    RequestExtensions::decode(ask.payload()),
                    Some(RequestExtensions {
                        request_id: 1,
                        requested: vec![LARGE_TEMPLATES],
                    })
                );
                if let Some(answer) = answer {
                    sender.send(answer).unwrap();
                }
                constraints(receiver);
                for batch in inbox {
                    for frame in batch {
                        sender.send(frame).unwrap();
                    }
                }
            },
            Vec::new(),
        );
        (address, thread, batches)
    }

    fn granted() -> SerializedFrame {
        RequestExtensionsSuccess {
            request_id: 1,
            supported: vec![LARGE_TEMPLATES],
        }
        .frame()
        .unwrap()
    }

    /// Template `id`'s NewTemplate (and with `future` its SetNewPrevHash),
    /// then the push `pusher` makes of `data` for it: the provider's bytes.
    fn announce(
        pusher: &mut Pusher,
        id: u64,
        future: bool,
        template: &BchTemplate,
        data: &BchTemplate,
        frames: Option<usize>,
    ) -> Vec<SerializedFrame> {
        let mut sent = vec![super::super::convert::new_template(id, future, template).unwrap()];
        if future {
            sent.push(super::super::convert::set_new_prev_hash(id, template).unwrap());
        }
        pusher.push(id, data);
        for _ in 0..frames.unwrap_or(usize::MAX) {
            match pusher
                .next_frame(|asked| (asked == id).then_some((data, false)))
                .unwrap()
            {
                Some(frame) => sent.push(frame),
                None => break,
            }
        }
        sent
    }

    /// Refreshes `source` until its template has `template`'s transactions.
    fn refresh_to(source: &mut TdpSource, template: &BchTemplate) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while source
            .current()
            .is_none_or(|(_, current)| current.transactions() != template.transactions())
        {
            assert!(Instant::now() < deadline, "the template never arrived");
            source.refresh().unwrap();
        }
    }

    // What: a provider that grants 0x4C54 pushes each template; the client
    // rebuilds and mines it: 65,537 transactions in pages (more than the
    // core Success carries), then a refresh on the same parent as a delta,
    // then a push abandoned after its Kept and one page followed by a full
    // push on a new parent. A push whose list disagrees with its template's
    // merkle path breaks the protocol and closes the link. The client never
    // asks for transaction data.
    // Look here if: TdpSource's negotiation, push handling or refresh wait
    // changes.
    #[test]
    fn a_provider_granting_large_templates_has_its_pushes_rebuilt() {
        let (address, provider, batches) = paced(Some(granted()));
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
        let mut pusher = Pusher::default();
        let big = template(1..=65_537, 0, 0xab);
        batches
            .send(announce(&mut pusher, 5, true, &big, &big, None))
            .unwrap();
        refresh_to(&mut source, &big);
        assert_eq!(source.negotiation, Negotiation::Granted);
        assert_eq!(source.current().unwrap().1.transactions().len(), 65_537);
        let delta = template(
            (1..=65_537).filter(|n| n % 7 != 0).chain([70_000, 70_001]),
            0,
            0xab,
        );
        let frames = announce(&mut pusher, 6, false, &delta, &delta, None);
        assert_eq!(
            frames[1].header().msg_type(),
            super::super::super::extensions::REQUEST_TRANSACTION_DATA_KEPT
        );
        batches.send(frames).unwrap();
        refresh_to(&mut source, &delta);
        let unfinished = template(1..=2500, 1024, 0xcd);
        let full = template(1..=2501, 1024, 0xcd);
        let mut frames = announce(&mut pusher, 7, true, &unfinished, &unfinished, Some(2));
        let next = announce(&mut pusher, 8, true, &full, &full, None);
        assert_ne!(
            next[2].header().msg_type(),
            super::super::super::extensions::REQUEST_TRANSACTION_DATA_KEPT,
            "a full push"
        );
        frames.extend(next);
        batches.send(frames).unwrap();
        refresh_to(&mut source, &full);
        let other = template(1..=2502, 1024, 0xcd);
        let wrong = template(1..=2503, 1024, 0xcd);
        batches
            .send(announce(&mut pusher, 9, false, &other, &wrong, None))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let error = loop {
            assert!(Instant::now() < deadline, "the bad push was taken");
            if let Err(error) = source.refresh() {
                break error;
            }
        };
        assert_eq!(error, BROKE_PROTOCOL);
        assert!(source.link.is_none());
        drop(batches);
        provider.join().unwrap();
    }

    // What: a provider that refuses 0x4C54 (RequestExtensions.Error), or
    // ignores the request and goes on to its templates, is asked for a
    // template's transactions as before; one that requires an extension
    // this client does not implement fails the refresh and closes the link.
    // Look here if: TdpSource's negotiation changes.
    #[test]
    fn a_provider_without_large_templates_is_asked_for_data_as_before() {
        let small = template(1..=3, 0, 0xab);
        let refused = RequestExtensionsError {
            request_id: 1,
            unsupported: vec![LARGE_TEMPLATES],
            required: Vec::new(),
        }
        .frame()
        .unwrap();
        for (answer, negotiation) in [
            (Some(refused), Negotiation::Refused),
            (None, Negotiation::Refused),
        ] {
            let (address, provider) = provider(
                {
                    let small = small.clone();
                    move |sender, receiver| {
                        receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
                        if let Some(answer) = answer {
                            sender.send(answer).unwrap();
                        }
                        constraints(receiver);
                        sender
                            .send(super::super::convert::new_template(5, true, &small).unwrap())
                            .unwrap();
                        sender
                            .send(super::super::convert::set_new_prev_hash(5, &small).unwrap())
                            .unwrap();
                        let ask = receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
                        assert_eq!(
                            (ask.header().ext_type(), ask.header().msg_type()),
                            (0, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA)
                        );
                        sender
                            .send(super::super::convert::transaction_data(5, &small).unwrap())
                            .unwrap();
                        let _ = receiver.receive(Duration::from_secs(2));
                    }
                },
                Vec::new(),
            );
            let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
            let (_, refreshed) = source.refresh().unwrap();
            assert_eq!(refreshed.transactions(), small.transactions());
            assert_eq!(source.negotiation, negotiation);
            provider.join().unwrap();
        }
        let requires = RequestExtensionsError {
            request_id: 1,
            unsupported: vec![LARGE_TEMPLATES],
            required: vec![0x1234],
        }
        .frame()
        .unwrap();
        let (address, provider, batches) = paced(Some(requires));
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
        let error = source.refresh().unwrap_err();
        assert!(error.contains("requires SV2 extension 0x1234"), "{error}");
        assert!(source.link.is_none());
        drop(batches);
        provider.join().unwrap();
    }

    // What: a provider that closes the link on RequestExtensions (as a
    // Pickaxe template server did before it took extension frames) gets a
    // second link at once, whose first frame after the setup is the
    // constraints; its template is then mined.
    // Look here if: TdpSource::refresh's first link changes.
    #[test]
    fn a_provider_closing_on_the_request_is_asked_nothing_next() {
        let secret = [21; 32];
        let public = super::super::super::server::authority_public(&secret).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = TdpAddress {
            address: listener.local_addr().unwrap().to_string(),
            authority: Some(public),
        };
        let small = template(1..=3, 0, 0xab);
        let served = small.clone();
        let provider = std::thread::spawn(move || {
            let (_, mut receiver) = set_up(&listener, &public, &secret, Vec::new());
            let ask = receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
            assert_eq!(ask.header().ext_type(), NEGOTIATION);
            drop(receiver);
            let (mut sender, mut receiver) = set_up(&listener, &public, &secret, Vec::new());
            let first = receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
            assert_eq!(
                (first.header().ext_type(), first.header().msg_type()),
                (0, MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS)
            );
            sender
                .send(super::super::convert::new_template(5, true, &served).unwrap())
                .unwrap();
            sender
                .send(super::super::convert::set_new_prev_hash(5, &served).unwrap())
                .unwrap();
            receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
            sender
                .send(super::super::convert::transaction_data(5, &served).unwrap())
                .unwrap();
            let _ = receiver.receive(Duration::from_secs(2));
        });
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(None)));
        let (_, refreshed) = source.refresh().unwrap();
        assert_eq!(refreshed.transactions(), small.transactions());
        assert_eq!(source.negotiation, Negotiation::Unasked);
        provider.join().unwrap();
    }
}
