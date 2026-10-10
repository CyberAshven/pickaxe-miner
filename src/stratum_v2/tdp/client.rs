//! #### PR #42
//! Templates from an SV2 Template Provider (another Pickaxe's template
//! server, a node bridge, or a node with one built in), as a
//! `TemplateSource` the server fails over with, like a node over JSON-RPC.

use super::super::{
    channel::MAX_ACTIVE_JOBS,
    journal::PendingBlock,
    provider::{SourceKind, SubmissionOutcome, TemplateSource},
    template::{double_sha256, fold, BchTemplate, Hash, Provided},
    transport::{Limits, Receiver, Sender, Session},
    wire::encoded,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::{TcpStream, ToSocketAddrs},
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
/// hash and the template's height.
pub type Guard = Box<dyn FnMut(&Hash, u32) -> Result<(), String> + Send>;

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

/// Transaction data a provider sent, or its error code.
type Data = Result<(Vec<Vec<u8>>, Vec<u8>), String>;

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
    guarded: Option<Hash>,
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
        }
    }

    fn connect(&mut self) -> Result<(), String> {
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
            if let Some(frame) = receiver.receive(Duration::from_millis(100))? {
                break frame;
            }
            if Instant::now() >= deadline {
                return Err("template provider did not answer the setup".into());
            }
        };
        if reply.header().msg_type() != common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS {
            return Err("template provider refused the setup".into());
        }
        sender.send(encoded(
            CoinbaseOutputConstraints {
                coinbase_output_max_additional_size: self.reserve,
                coinbase_output_max_additional_sigops: 0,
            },
            MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS,
            false,
        )?)?;
        self.forget();
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

    // #### PR #42: a provider's protocol
    // What: template ids only rise; a future template waits for the
    // SetNewPrevHash naming it (at most 8 wait); a current template needs
    // an earlier SetNewPrevHash; anything else, an extension or the channel
    // bit closes the link and the server moves to its next source.
    // Why: a provider that breaks the protocol cannot be trusted with the
    // work of every device.
    // Look here if: a provider's link keeps closing with "broke the
    // protocol".
    fn handle(&mut self, frame: &mut SerializedFrame) -> Result<(), String> {
        let header = frame.header();
        if header.channel_msg() || header.ext_type_without_channel_msg() != 0 {
            return Err(BROKE_PROTOCOL.into());
        }
        match header.msg_type() {
            MESSAGE_TYPE_NEW_TEMPLATE => {
                let message: NewTemplate =
                    binary_sv2::from_bytes(frame.payload()).map_err(|_| BROKE_PROTOCOL)?;
                if self.last_id.is_some_and(|last| message.template_id <= last) {
                    return Err(BROKE_PROTOCOL.into());
                }
                self.last_id = Some(message.template_id);
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
                    ntime: message.header_timestamp,
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
                let transactions = message
                    .transaction_list
                    .iter()
                    .map(|tx| tx.as_ref().to_vec())
                    .collect();
                self.data.insert(
                    message.template_id,
                    Ok((transactions, message.excess_data.as_ref().to_vec())),
                );
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

    /// The transactions of template `id`, asked for and awaited.
    fn transactions(&mut self, id: u64) -> Result<(Vec<Vec<u8>>, Vec<u8>), String> {
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
            self.connect()?;
            let deadline = Instant::now() + FIRST_TEMPLATE_WAIT;
            while self.newest.is_none() || self.tip.is_none() {
                if Instant::now() >= deadline {
                    self.close();
                    return Err("template provider sent no template".into());
                }
                self.drain(Duration::from_millis(100))?;
            }
        }
        self.drain(Duration::from_millis(1))?;
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
        let (transactions, excess) = if newest.merkle_path.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            self.transactions(newest.id)?
        };
        // BCH: no required coinbase outputs and no excess data, which would
        // mean a Bitcoin provider (a segwit commitment).
        if newest.outputs_count != 0 || !newest.outputs.is_empty() || !excess.is_empty() {
            return Err("template provider requires coinbase outputs (a Bitcoin template?)".into());
        }
        let template = BchTemplate::from_provided(Provided {
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
        .map_err(|error| {
            if error == "template too large for SV2" {
                error
            } else {
                "template provider sent an invalid template".to_owned()
            }
        })?;
        // #### PR #42: the network guard
        // What: a provider's new parent is confirmed on this server's
        // network (by the miner's node, or the network's Fulcrum servers)
        // before its template is mined.
        // Why: TDP carries no chain; a provider on another network would
        // waste every device's work.
        // Look here if: a provider is refused with "network is not
        // confirmed".
        if self.guarded != Some(tip.prev) {
            (self.guard)(&tip.prev, template.height)
                .map_err(|_| "template provider's network is not confirmed")?;
            self.guarded = Some(tip.prev);
        }
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

    // #### PR #42: solutions to a provider
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
                header_timestamp: word(68),
                header_nonce: word(76),
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
    use super::super::super::{template_tests::rpc_template, transport::Limits, wire::encoded};
    use super::*;
    use std::net::TcpListener;
    use stratum_core::template_distribution_sv2::MESSAGE_TYPE_SET_NEW_PREV_HASH;

    // #### PR #42
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
        let secret = [21; 32];
        let public = super::super::super::server::authority_public(&secret).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = TdpAddress {
            address: listener.local_addr().unwrap().to_string(),
            authority: Some(public),
        };
        let thread = std::thread::spawn(move || {
            let session = Session::accept_with(
                listener.accept().unwrap().0,
                &public,
                &secret,
                Limits::TDP_SERVER,
            )
            .unwrap();
            let (mut sender, mut receiver) = session.split();
            receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
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
            receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
            for frame in frames {
                sender.send(frame).unwrap();
            }
            let _ = receiver.receive(Duration::from_secs(2));
        });
        (address, thread)
    }

    // #### PR #42
    // What: a SetNewPrevHash naming a template never announced breaks the
    // protocol; the refresh fails with that reason and the link closes.
    // Look here if: TdpSource::handle changes.
    #[test]
    fn a_provider_breaking_the_protocol_fails_the_refresh() {
        let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
        let stray = super::super::convert::set_new_prev_hash(9, &template).unwrap();
        assert_eq!(stray.header().msg_type(), MESSAGE_TYPE_SET_NEW_PREV_HASH);
        let (address, provider) = scripted(vec![stray]);
        let mut source = TdpSource::new(address, 122, Box::new(|_, _| Ok(())));
        assert_eq!(source.refresh().err().as_deref(), Some(BROKE_PROTOCOL));
        assert!(source.link.is_none());
        provider.join().unwrap();
    }

    // #### PR #42
    // What: a template whose parent the guard does not confirm is refused
    // with "network is not confirmed"; the same template passes once the
    // guard confirms it.
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
                Ok(())
            }),
        );
        let (generation, refreshed) = source.refresh().unwrap();
        assert_eq!(generation, 1);
        assert_eq!(refreshed.height, 325_909);
        assert_eq!(refreshed.size_limit, 80 + 1 + 153 + 122);
        provider.join().unwrap();
    }

    // #### PR #42
    // What: the reserve is 122 bytes for the payouts alone, and 221 with
    // the Chipnet test token (a 53-byte commitment and one 46-byte ticket).
    // Look here if: tdp::reserve changes.
    #[test]
    fn the_reserve_is_122_and_221_with_the_test_token() {
        assert_eq!(super::super::reserve(None), 122);
        let directory = super::super::super::journal::TestDirectory::new();
        let hub = super::super::super::merge::hub::TokenHub::test_token(
            crate::config::MiningNetwork::Chipnet,
            directory.0.join("proofs.json"),
            0x207f_ffff,
        )
        .unwrap();
        assert_eq!(super::super::reserve(hub.current().as_deref()), 221);
    }
}
