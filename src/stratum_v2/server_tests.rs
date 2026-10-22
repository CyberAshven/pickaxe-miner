//! #### PR #38
//! Exercise the real TCP/Noise server with CPU-solved headers. The node fixture
//! uses an independent block decoder/hash oracle. This is not live chain proof.

use super::{
    journal::TestDirectory,
    provider::NodeRpc,
    server::{self, ServerConfig, ServerStats},
    template_tests::{payout, rpc_template},
    transport::{Receiver, Sender, Session},
    wire::{encoded, mining},
};
use crate::config::MiningNetwork;
use serde_json::{json, Value};
use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stratum_core::{
    binary_sv2,
    bitcoin::{
        block::Header,
        consensus,
        hashes::{sha256d, Hash},
        Block,
    },
    codec_sv2::SerializedFrame,
    common_messages_sv2::{Protocol, SetupConnection},
    mining_sv2::*,
    parsers_sv2::Mining,
};

pub(super) struct Node {
    height: u32,
    tip: String,
    reject: bool,
    pub(super) submissions: usize,
    pub(super) lose_replies: bool,
    pub(super) submitted: Vec<String>,
    known: std::collections::HashSet<String>,
    unavailable: bool,
    /// #### PR #40: a public pool's blocks pay their own miners; the test
    /// checks those payouts itself.
    public: bool,
    /// #### PR #42: the template's other transactions (none by default).
    pub(super) transactions: Vec<Value>,
    /// #### PR #42: transactions the node takes in a block but leaves out
    /// of its templates (another node's mempool had them first), and how
    /// often validateblocktemplate was called.
    pub(super) spendable: Vec<Value>,
    pub(super) validations: usize,
    /// #### PR #48: the tip's time; one second ago when unset. Past it plus
    /// 1200 s the templates carry the limit's bits, as BCHN's do on
    /// Chipnet (it recomputes the bits for its curtime).
    pub(super) parent_time: Option<u32>,
}

impl Node {
    /// The transactions a block of this node's may carry, as hex.
    fn known_transactions(&self) -> std::collections::HashSet<String> {
        self.transactions
            .iter()
            .chain(&self.spendable)
            .map(|tx| tx["data"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// #### PR #42: a node for blocks that pay someone else (a pool's, built by
/// a template client); the test checks those payouts itself.
pub(super) fn pool_node() -> Arc<Mutex<Node>> {
    Arc::new(Mutex::new(Node {
        height: 325908,
        tip: "ab".repeat(32),
        reject: false,
        submissions: 0,
        lose_replies: false,
        submitted: Vec::new(),
        known: std::collections::HashSet::new(),
        unavailable: false,
        public: true,
        transactions: Vec::new(),
        spendable: Vec::new(),
        validations: 0,
        parent_time: None,
    }))
}

struct Rpc(Arc<Mutex<Node>>);

impl NodeRpc for Rpc {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let mut node = self.0.lock().unwrap();
        if node.unavailable {
            return Err("simulated private RPC connection failure".into());
        }
        match method {
            "getblockchaininfo" => Ok(json!({"chain":"chip", "initialblockdownload":false,
                "blocks":node.height,"headers":node.height,"bestblockhash":node.tip})),
            // #### PR #42: Chipnet's fork block.
            "getblockhash" if params[0] == MiningNetwork::Chipnet.fork_block().0 => {
                Ok(json!(MiningNetwork::Chipnet.fork_block().1))
            }
            "getblocktemplate" => {
                assert!(params[0].get("rules").is_none());
                let mut template = rpc_template();
                template["height"] = json!(node.height + 1);
                template["previousblockhash"] = json!(node.tip);
                template["curtime"] = json!(now());
                template["mintime"] = json!(now() - 1);
                template["transactions"] = json!(node.transactions);
                if node.parent_time.is_some_and(|parent| now() > parent + 1200) {
                    template["bits"] = json!("1d00ffff");
                    template["target"] = json!(format!("00000000ffff{}", "00".repeat(26)));
                }
                Ok(template)
            }
            "submitblock" => {
                let bytes = params[0].as_str().unwrap().to_owned();
                let block: Block = consensus::deserialize(&hex::decode(&bytes).unwrap()).unwrap();
                assert!(block.check_merkle_root());
                assert!(block.header.validate_pow(block.header.target()).is_ok());
                let hash = block.block_hash().to_string();
                node.submitted.push(bytes);
                if node.known.contains(&hash) {
                    return if node.lose_replies {
                        Err("simulated lost reply".into())
                    } else {
                        Ok(json!("duplicate"))
                    };
                }
                assert_eq!(block.header.prev_blockhash.to_string(), node.tip);
                // #### PR #42: a declared block may carry transactions the
                // node knows but left out of its template.
                let known = node.known_transactions();
                assert!(block.txdata[1..]
                    .iter()
                    .all(|tx| known.contains(&hex::encode(consensus::serialize(tx)))));
                if node.spendable.is_empty() {
                    assert_eq!(block.txdata.len(), 1 + node.transactions.len());
                }
                let coinbase = &block.txdata[0];
                assert!(coinbase.is_coinbase());
                let mut expected = vec![0x76, 0xa9, 0x14];
                expected.extend([0x12; 20]);
                expected.extend([0x88, 0xac]);
                let donor = crate::tx::cashaddr_to_p2pkh_locking(crate::donation::bch::address(
                    MiningNetwork::Chipnet,
                ))
                .unwrap();
                // #### PR #42: a merge-mining coinbase adds a zero-value
                // commitment (output 0, OP_RETURN) and Case B tickets; the
                // payouts are the other outputs.
                let token_outputs = coinbase
                    .output
                    .iter()
                    .filter(|output| {
                        let script = output.script_pubkey.as_bytes();
                        output.value.to_sat() == 0
                            && (script.first() == Some(&0x6a)
                                || super::merge::registry::is_ticket_script(script))
                    })
                    .count();
                let payouts: Vec<_> = coinbase
                    .output
                    .iter()
                    .filter(|output| {
                        let script = output.script_pubkey.as_bytes();
                        !(output.value.to_sat() == 0
                            && (script.first() == Some(&0x6a)
                                || super::merge::registry::is_ticket_script(script)))
                    })
                    .collect();
                if node.public {
                } else if payouts.len() == 1 {
                    assert_eq!(payouts[0].value.to_sat(), 312_500_000);
                    assert!(payouts[0].script_pubkey.as_bytes() == donor);
                } else {
                    assert_eq!(payouts.len(), 2);
                    assert_eq!(payouts[0].value.to_sat(), 309_375_000);
                    assert_eq!(payouts[1].value.to_sat(), 3_125_000);
                    assert!(payouts[0].script_pubkey.as_bytes() == expected);
                    assert!(payouts[1].script_pubkey.as_bytes() == donor);
                }
                assert!(coinbase.input[0].witness.is_empty());
                let size = consensus::serialize(coinbase).len();
                if token_outputs == 0 {
                    assert!((100..=200).contains(&size));
                } else {
                    assert!((200..=400).contains(&size), "{size}");
                }
                node.submissions += 1;
                if node.reject {
                    Ok(json!("bad-cb-amount"))
                } else {
                    node.height += 1;
                    node.tip = hash.clone();
                    node.known.insert(hash);
                    if node.lose_replies {
                        Err("simulated lost reply".into())
                    } else {
                        Ok(Value::Null)
                    }
                }
            }
            // #### PR #42: BCHN's check of a declared template: the parent
            // must be the tip, the merkle root right and every transaction
            // known; refusals come as BCHN's "Invalid block: …" errors.
            "validateblocktemplate" => {
                let bytes = hex::decode(params[0].as_str().unwrap()).unwrap();
                let block: Block = consensus::deserialize(&bytes).unwrap();
                node.validations += 1;
                let refuse = |reason: &str| {
                    Err(format!(
                        "rpc error: {}",
                        json!({"code": -25, "message": format!("Invalid block: {reason}")})
                    ))
                };
                if block.header.prev_blockhash.to_string() != node.tip {
                    return refuse("does not build on chain tip");
                }
                if !block.check_merkle_root() {
                    return refuse("bad-txnmrklroot");
                }
                let known = node.known_transactions();
                if !block.txdata[1..]
                    .iter()
                    .all(|tx| known.contains(&hex::encode(consensus::serialize(tx))))
                {
                    return refuse("bad-txns-inputs-missingorspent");
                }
                Ok(json!(true))
            }
            "getblockheader" => {
                let hash = params[0].as_str().unwrap();
                if !node.lose_replies && node.known.contains(hash) {
                    Ok(json!({"hash":hash,"confirmations":1,"time":now() - 1}))
                } else if hash == node.tip {
                    // #### PR #48: the parent's time, for Chipnet's 20-minute
                    // rule; a lost reply still confirms no block.
                    Ok(json!({"hash":hash,"time":node.parent_time.unwrap_or(now() - 1)}))
                } else {
                    Err("header temporarily unavailable".into())
                }
            }
            _ => panic!("unexpected RPC method"),
        }
    }
}

pub(super) struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), String>>>,
    pub(super) stats: Arc<Mutex<ServerStats>>,
    pub(super) node: Arc<Mutex<Node>>,
    pub(super) address: SocketAddr,
    pub(super) authority: [u8; 32],
    pub(super) state_directory: Arc<TestDirectory>,
    /// #### PR #42: the template listener, when it serves templates.
    pub(super) templates: Option<SocketAddr>,
    /// #### PR #42: a Job Declaration client's uplink thread.
    uplink: Option<thread::JoinHandle<()>>,
    /// #### PR #56: a pool's Job Declaration rules, when it accepts them.
    pub(super) declarator: Option<Arc<super::jd::server::Declarator>>,
}

impl Running {
    fn new(reject: bool) -> Self {
        let node = Arc::new(Mutex::new(Node {
            height: 325908,
            tip: "ab".repeat(32),
            reject,
            submissions: 0,
            lose_replies: false,
            submitted: Vec::new(),
            known: std::collections::HashSet::new(),
            unavailable: false,
            public: false,
            transactions: Vec::new(),
            spendable: Vec::new(),
            validations: 0,
            parent_time: None,
        }));
        Self::start(node, Arc::new(TestDirectory::new()))
    }

    /// #### PR #40: a public pool, where each miner's blocks pay them.
    fn public(public: super::payout::PublicPool) -> Self {
        let node = Arc::new(Mutex::new(Node {
            height: 325908,
            tip: "ab".repeat(32),
            reject: false,
            submissions: 0,
            lose_replies: false,
            submitted: Vec::new(),
            known: std::collections::HashSet::new(),
            unavailable: false,
            public: true,
            transactions: Vec::new(),
            spendable: Vec::new(),
            validations: 0,
            parent_time: None,
        }));
        Self::start_with(node, Arc::new(TestDirectory::new()), Some(public))
    }

    fn start(node: Arc<Mutex<Node>>, state_directory: Arc<TestDirectory>) -> Self {
        Self::start_with(node, state_directory, None)
    }

    fn start_with(
        node: Arc<Mutex<Node>>,
        state_directory: Arc<TestDirectory>,
        public: Option<super::payout::PublicPool>,
    ) -> Self {
        Self::start_nodes(vec![node], state_directory, public)
    }

    /// #### PR #40: a server with its nodes in failover order; `node` is the
    /// first.
    fn start_nodes(
        nodes: Vec<Arc<Mutex<Node>>>,
        state_directory: Arc<TestDirectory>,
        public: Option<super::payout::PublicPool>,
    ) -> Self {
        Self::start_full(nodes, state_directory, public, None)
    }

    /// #### PR #42: a server that merge-mines `tokens`.
    fn start_full(
        nodes: Vec<Arc<Mutex<Node>>>,
        state_directory: Arc<TestDirectory>,
        public: Option<super::payout::PublicPool>,
        tokens: Option<Arc<super::merge::hub::TokenHub>>,
    ) -> Self {
        Self::start_config(
            nodes,
            state_directory,
            public,
            tokens,
            None,
            None,
            None,
            Vec::new(),
        )
    }

    /// #### PR #42: a Job Declaration client's local server on `node`,
    /// declaring its templates to `pool` in `mode`.
    pub(super) fn jd_client(
        node: Arc<Mutex<Node>>,
        pool: &Running,
        mode: super::jd::JdMode,
    ) -> Self {
        Self::start_config(
            vec![node],
            Arc::new(TestDirectory::new()),
            None,
            None,
            None,
            None,
            Some(super::jd::client::JdTarget {
                p2pool_prepare_api: None,
                required: false,
                address: pool.address.to_string(),
                authority: pool.authority,
                identity: payout(),
                retry: Duration::from_millis(500),
                mode,
            }),
            Vec::new(),
        )
    }

    /// #### PR #42: a Job Declaration client merge-mining `hub`'s tokens,
    /// with its proofs in `state_directory`.
    pub(super) fn jd_client_merged(
        node: Arc<Mutex<Node>>,
        pool: &Running,
        mode: super::jd::JdMode,
        hub: Arc<super::merge::hub::TokenHub>,
        state_directory: Arc<TestDirectory>,
    ) -> Self {
        Self::start_config(
            vec![node],
            state_directory,
            None,
            Some(hub),
            None,
            None,
            Some(super::jd::client::JdTarget {
                p2pool_prepare_api: None,
                required: false,
                address: pool.address.to_string(),
                authority: pool.authority,
                identity: payout(),
                retry: Duration::from_millis(500),
                mode,
            }),
            Vec::new(),
        )
    }

    /// #### PR #42: a server whose templates come first from `provider`'s
    /// template server, then from its own `node`.
    pub(super) fn tdp_client(
        node: Arc<Mutex<Node>>,
        provider: super::tdp::client::TdpAddress,
    ) -> Self {
        let source = super::tdp::client::TdpSource::new(
            provider,
            super::tdp::PAYOUT_RESERVE,
            Box::new(|_, _| Ok(None)),
        );
        Self::start_config(
            vec![node],
            Arc::new(TestDirectory::new()),
            None,
            None,
            None,
            None,
            None,
            vec![Box::new(source)],
        )
    }

    /// #### PR #42: a public pool that accepts Coinbase-only Job
    /// Declaration.
    pub(super) fn jd_pool(public: super::payout::PublicPool) -> Self {
        Self::start_config(
            vec![pool_node()],
            Arc::new(TestDirectory::new()),
            Some(public),
            None,
            None,
            Some(super::jd::AcceptJd::CoinbaseOnly),
            None,
            Vec::new(),
        )
    }

    /// #### PR #42: a public pool on `node` that accepts both Job
    /// Declaration modes, whose node checks declared templates.
    pub(super) fn jd_pool_full(node: Arc<Mutex<Node>>, public: super::payout::PublicPool) -> Self {
        Self::start_config(
            vec![node],
            Arc::new(TestDirectory::new()),
            Some(public),
            None,
            None,
            Some(super::jd::AcceptJd::Both),
            None,
            Vec::new(),
        )
    }

    /// #### PR #42: a server that serves templates on a second listener,
    /// with its relay journal in `state_directory`.
    pub(super) fn templates(node: Arc<Mutex<Node>>, state_directory: Arc<TestDirectory>) -> Self {
        Self::start_config(
            vec![node],
            state_directory,
            None,
            None,
            Some(true),
            None,
            None,
            Vec::new(),
        )
    }

    /// #### PR #42: a server whose relay journal is configured but which
    /// serves no templates.
    pub(super) fn relay_only(node: Arc<Mutex<Node>>, state_directory: Arc<TestDirectory>) -> Self {
        Self::start_config(
            vec![node],
            state_directory,
            None,
            None,
            Some(false),
            None,
            None,
            Vec::new(),
        )
    }

    /// #### PR #42: where a test server keeps its relay journal.
    pub(super) fn relay_path(&self) -> std::path::PathBuf {
        self.state_directory.0.join("relay-blocks.json")
    }

    /// #### PR #42: a solo server on `node` that marks `preferred` while it
    /// has work.
    pub(super) fn solo_preferred(
        node: Arc<Mutex<Node>>,
        preferred: Arc<super::sv1::Preferred>,
    ) -> Self {
        Self::start_preferred(
            vec![node],
            Arc::new(TestDirectory::new()),
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
            Some(preferred),
            None,
        )
    }

    /// #### PR #42: a server mining `work`'s ASIC-exclusive token instead
    /// of BCH.
    pub(super) fn token(
        work: Arc<super::merge::source::HeaderWork>,
        state_directory: Arc<TestDirectory>,
    ) -> Self {
        Self::start_preferred(
            vec![pool_node()],
            state_directory,
            None,
            None,
            None,
            None,
            None,
            vec![Box::new(super::merge::source::TokenSource::new(
                work.clone(),
            ))],
            None,
            Some(work),
        )
    }

    /// `templates`: none, a relay journal only, or a relay journal and a
    /// template listener.
    #[allow(clippy::too_many_arguments)]
    fn start_config(
        nodes: Vec<Arc<Mutex<Node>>>,
        state_directory: Arc<TestDirectory>,
        public: Option<super::payout::PublicPool>,
        tokens: Option<Arc<super::merge::hub::TokenHub>>,
        templates: Option<bool>,
        job_declaration: Option<super::jd::AcceptJd>,
        uplink: Option<super::jd::client::JdTarget>,
        sources: Vec<Box<dyn super::provider::TemplateSource>>,
    ) -> Self {
        Self::start_preferred(
            nodes,
            state_directory,
            public,
            tokens,
            templates,
            job_declaration,
            uplink,
            sources,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_preferred(
        nodes: Vec<Arc<Mutex<Node>>>,
        state_directory: Arc<TestDirectory>,
        public: Option<super::payout::PublicPool>,
        tokens: Option<Arc<super::merge::hub::TokenHub>>,
        templates: Option<bool>,
        job_declaration: Option<super::jd::AcceptJd>,
        uplink: Option<super::jd::client::JdTarget>,
        mut sources: Vec<Box<dyn super::provider::TemplateSource>>,
        preferred: Option<Arc<super::sv1::Preferred>>,
        header_work: Option<Arc<super::merge::source::HeaderWork>>,
    ) -> Self {
        let node = nodes[0].clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let template_listener = templates
            .filter(|listen| *listen)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap());
        let template_address = template_listener
            .as_ref()
            .map(|listener| listener.local_addr().unwrap());
        let relay_journal_path = templates.map(|_| state_directory.0.join("relay-blocks.json"));
        let donation = Arc::new(std::sync::RwLock::new(Default::default()));
        let declarator = public.clone().zip(job_declaration).map(|(public, accept)| {
            let declarator = super::jd::server::Declarator::new(
                accept,
                MiningNetwork::Chipnet,
                public,
                donation.clone(),
            );
            Arc::new(if accept.allows(true) {
                declarator.with_validator(Arc::new(super::jd::server::NodeValidator::new(Rpc(
                    node.clone(),
                ))))
            } else {
                declarator
            })
        });
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(ServerStats::default()));
        let uplink = uplink.map(|target| super::jd::client::spawn(vec![target], stop.clone()));
        let secret = [17; 32];
        let authority = server::authority_public(&secret).unwrap();
        let config = ServerConfig {
            public,
            donation,
            allocation_phase: Some(300_000_000_000),
            network: MiningNetwork::Chipnet,
            payout: payout(),
            authority_secret: secret,
            share_target: [255; 32],
            journal_path: state_directory.journal(),
            legacy_sources: vec![[42; 32]],
            pool_tag: Vec::new(),
            tokens,
            relay_journal_path,
            declared_journal_path: declarator
                .as_ref()
                .map(|_| state_directory.0.join("jd-blocks.json")),
            declarator: declarator.clone(),
            uplink: uplink.as_ref().map(|(handle, _)| handle.clone()),
            preferred,
            token_donation: header_work
                .as_ref()
                .map(|work| work.token().donation_minimum),
            header_work,
            node_zmq: None,
        };
        let thread = {
            let stop = stop.clone();
            let stats = stats.clone();
            let rpcs: Vec<Rpc> = nodes.into_iter().map(Rpc).collect();
            thread::spawn(move || {
                server::run_with(
                    server::Listeners {
                        devices: listener,
                        templates: template_listener,
                    },
                    {
                        sources.extend(server::rpc_sources(rpcs, MiningNetwork::Chipnet));
                        sources
                    },
                    config,
                    stop,
                    stats,
                )
            })
        };
        let running = Self {
            stop,
            thread: Some(thread),
            stats,
            node,
            address,
            authority,
            state_directory,
            templates: template_address,
            uplink: uplink.map(|(_, thread)| thread),
            declarator,
        };
        running.wait(|stats| stats.template_ready);
        running
    }

    pub(super) fn wait(&self, condition: impl Fn(&ServerStats) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if condition(&self.stats.lock().unwrap()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server progress deadline exceeded"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let result = handle.join();
            if !thread::panicking() {
                assert!(matches!(result, Ok(Ok(()))));
            }
        }
        if let Some(uplink) = self.uplink.take() {
            let _ = uplink.join();
        }
    }
}

pub(super) fn now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32
}

struct Device {
    sender: Sender,
    receiver: Receiver,
    id: u32,
    prefix: Vec<u8>,
    extended: bool,
    /// #### PR #56: the extranonce bytes the device rolls.
    rolled: usize,
}

impl Device {
    fn connect(server: &Running, extended: bool) -> Self {
        Self::connect_as(server, extended, "cpu")
    }

    fn connect_as(server: &Running, extended: bool, identity: &str) -> Self {
        let device = Self::connect_rolling(server, extended, identity, 8);
        assert_eq!(device.rolled, 8);
        device
    }

    /// #### PR #56: a device asking for at least `rolling` extranonce bytes.
    fn connect_rolling(server: &Running, extended: bool, identity: &str, rolling: u16) -> Self {
        let (mut sender, mut receiver) = Session::initiate(
            TcpStream::connect(server.address).unwrap(),
            server.authority,
        )
        .unwrap()
        .split();
        sender
            .send(
                encoded(
                    SetupConnection {
                        protocol: Protocol::MiningProtocol,
                        min_version: 2,
                        max_version: 2,
                        flags: if extended { 4 } else { 5 },
                        endpoint_host: "localhost".try_into().unwrap(),
                        endpoint_port: server.address.port(),
                        vendor: "CPU experiment".try_into().unwrap(),
                        hardware_version: "".try_into().unwrap(),
                        firmware: "".try_into().unwrap(),
                        device_id: "".try_into().unwrap(),
                    },
                    0,
                    false,
                )
                .unwrap(),
            )
            .unwrap();
        let response = receiver.receive(Duration::from_secs(3)).unwrap().unwrap();
        assert_eq!(response.header().msg_type(), 1);
        let maximum = [255; 32];
        let open = if extended {
            Mining::OpenExtendedMiningChannel(OpenExtendedMiningChannel {
                request_id: 1,
                user_identity: identity.try_into().unwrap(),
                nominal_hash_rate: 1000.0,
                max_target: (&maximum).into(),
                min_extranonce_size: rolling,
            })
        } else {
            Mining::OpenStandardMiningChannel(OpenStandardMiningChannel {
                request_id: 1,
                user_identity: identity.try_into().unwrap(),
                nominal_hash_rate: 1000.0,
                max_target: (&maximum).into(),
            })
        };
        sender.send(mining(open).unwrap()).unwrap();
        let mut response = receiver.receive(Duration::from_secs(3)).unwrap().unwrap();
        let (id, prefix, rolled) = if extended {
            assert_eq!(
                response.header().msg_type(),
                MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS
            );
            let opened: OpenExtendedMiningChannelSuccess =
                binary_sv2::from_bytes(response.payload()).unwrap();
            (
                opened.channel_id,
                opened.extranonce_prefix.as_ref().to_vec(),
                usize::from(opened.extranonce_size),
            )
        } else {
            assert_eq!(
                response.header().msg_type(),
                MESSAGE_TYPE_OPEN_STANDARD_MINING_CHANNEL_SUCCESS
            );
            let opened: OpenStandardMiningChannelSuccess =
                binary_sv2::from_bytes(response.payload()).unwrap();
            (
                opened.channel_id,
                opened.extranonce_prefix.as_ref().to_vec(),
                8,
            )
        };
        Self {
            sender,
            receiver,
            id,
            prefix,
            extended,
            rolled,
        }
    }

    /// #### PR #48: the bits of each SetNewPrevHash until one carries
    /// `bits`; fails when none does by `deadline`.
    fn bits_until(&mut self, bits: u32, deadline: Instant) -> Vec<u32> {
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            let Some(mut frame) = self.receiver.receive(Duration::from_millis(100)).unwrap() else {
                continue;
            };
            if frame.header().msg_type() == MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH {
                let previous: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
                seen.push(previous.nbits);
                if previous.nbits == bits {
                    return seen;
                }
            }
        }
        panic!("no job on bits {bits:#x} in time; saw {seen:x?}");
    }

    fn receive(&mut self) -> SerializedFrame {
        self.receiver
            .receive(Duration::from_secs(3))
            .unwrap()
            .unwrap()
    }

    fn solve_and_submit(&mut self, sequence: u32) -> String {
        let mut frame = self.receive();
        let extra = vec![sequence as u8; self.rolled];
        let (job_id, version, root) = if self.extended {
            assert_eq!(
                frame.header().msg_type(),
                MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB
            );
            let job: NewExtendedMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
            assert!(job.version_rolling_allowed);
            let mut coinbase = job.coinbase_tx_prefix.as_ref().to_vec();
            coinbase.extend(&self.prefix);
            coinbase.extend(&extra);
            coinbase.extend(job.coinbase_tx_suffix.as_ref());
            let mut root = sha256d::Hash::hash(&coinbase).to_byte_array();
            for sibling in job.merkle_path.iter() {
                let mut pair = root.to_vec();
                pair.extend(sibling.as_ref());
                root = sha256d::Hash::hash(&pair).to_byte_array();
            }
            (job.job_id, job.version, root)
        } else {
            assert_eq!(frame.header().msg_type(), MESSAGE_TYPE_NEW_MINING_JOB);
            let job: NewMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
            (
                job.job_id,
                job.version,
                <[u8; 32]>::try_from(job.merkle_root.as_ref()).unwrap(),
            )
        };
        let mut frame = self.receive();
        assert_eq!(
            frame.header().msg_type(),
            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH
        );
        let previous: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(previous.channel_id, self.id);
        assert_eq!(previous.job_id, job_id);
        // Flip an allowed bit: actual ASICs depend on negotiated version rolling.
        let version = version ^ 0x2000;
        let mut bytes = version.to_le_bytes().to_vec();
        bytes.extend(previous.prev_hash.as_ref());
        bytes.extend(root);
        bytes.extend(previous.min_ntime.to_le_bytes());
        bytes.extend(previous.nbits.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        let mut header: Header = consensus::deserialize(&bytes).unwrap();
        let nonce = (0..10_000)
            .find(|nonce| {
                header.nonce = *nonce;
                header.validate_pow(header.target()).is_ok()
            })
            .expect("synthetic easy target must yield a CPU solution");
        let submit = if self.extended {
            Mining::SubmitSharesExtended(SubmitSharesExtended {
                channel_id: self.id,
                sequence_number: sequence,
                job_id,
                nonce,
                ntime: previous.min_ntime,
                version,
                extranonce: extra.as_slice().try_into().unwrap(),
            })
        } else {
            Mining::SubmitSharesStandard(SubmitSharesStandard {
                channel_id: self.id,
                sequence_number: sequence,
                job_id,
                nonce,
                ntime: previous.min_ntime,
                version,
            })
        };
        self.sender.send(mining(submit).unwrap()).unwrap();
        let mut accepted = self.receive();
        assert_eq!(
            accepted.header().msg_type(),
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS
        );
        let accepted: SubmitSharesSuccess = binary_sv2::from_bytes(accepted.payload()).unwrap();
        assert_eq!(accepted.last_sequence_number, sequence);
        assert_eq!(accepted.new_submits_accepted_count, 1);
        header.block_hash().to_string()
    }
}

// #### PR #42
// What: a device mines the Chipnet test token through the real server: its
// share wins Case A (the token's target) and, being a block, Case B; the
// claim worker proves both, saves them owner-only, lists them on the
// dashboard and moves the test token's baton, which re-issues jobs; a win is
// never proven twice; the acknowledgement does not wait for any of it.
// Look here if: the claim worker, the token hand-off or the re-publish on a
// token change changes.
#[test]
fn a_device_mines_the_test_token_and_each_state_is_proven_once() {
    use super::merge::hub::TokenHub;
    for extended in [false, true] {
        let directory = Arc::new(TestDirectory::new());
        let proofs = directory.0.join("token-proofs.json");
        // The node's own target, so every share wins the Case A state.
        let hub = Arc::new(
            TokenHub::test_token(MiningNetwork::Chipnet, proofs.clone(), 0x207f_ffff).unwrap(),
        );
        let node = Arc::new(Mutex::new(Node {
            height: 325908,
            tip: "ab".repeat(32),
            reject: false,
            submissions: 0,
            lose_replies: false,
            submitted: Vec::new(),
            known: std::collections::HashSet::new(),
            unavailable: false,
            public: false,
            transactions: Vec::new(),
            spendable: Vec::new(),
            validations: 0,
            parent_time: None,
        }));
        let server = Running::start_full(vec![node], directory, None, Some(hub.clone()));
        let mut device = Device::connect(&server, extended);
        device.solve_and_submit(0);
        server.wait(|stats| stats.recent_token_wins.len() >= 2);
        let stats = server.stats.lock().unwrap().clone();
        assert_eq!(stats.token_wins, 1, "one share, one hand-off");
        assert_eq!(stats.token_wins_dropped, 0);
        assert!(stats.tokens_off.is_none(), "{:?}", stats.tokens_off);
        let mut modes: Vec<char> = stats.recent_token_wins.iter().map(|win| win.mode).collect();
        modes.sort_unstable();
        assert_eq!(modes, ['A', 'B']);
        assert!(stats
            .recent_token_wins
            .iter()
            .all(|win| win.token == "Pickaxe test token" && win.height == 325909));
        let saved: Value = serde_json::from_slice(&std::fs::read(&proofs).unwrap()).unwrap();
        assert_eq!(saved["network"], "chipnet");
        let entries = saved["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|entry| entry["status"] == "proven"));
        assert!(!String::from_utf8(std::fs::read(&proofs).unwrap())
            .unwrap()
            .contains(&payout()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&proofs).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The baton moved to the winning header: a new set, new jobs.
        assert_eq!(hub.current().unwrap().serial(), 2);
        device.sender.close();
    }
}

#[test]
fn encrypted_cpu_devices_submit_blocks_and_receive_successor_without_reconnect() {
    for extended in [false, true] {
        let server = Running::new(false);
        let mut device = Device::connect(&server, extended);
        for sequence in 0..2 {
            let hash = device.solve_and_submit(sequence);
            // The peer has received the ACK, so its validated share must
            // already be visible even if node submission is still in flight.
            assert_eq!(
                server.stats.lock().unwrap().shares_accepted,
                u64::from(sequence) + 1
            );
            server.wait(|stats| stats.blocks_accepted == u64::from(sequence) + 1);
            assert_eq!(server.node.lock().unwrap().tip, hash);
        }
        let stats = server.stats.lock().unwrap().clone();
        assert_eq!(stats.shares_accepted, 2);
        assert_eq!(stats.blocks_pending, 0);
        assert_eq!(stats.connection_errors, 0);
        assert_eq!(stats.connections, 1);
        device.sender.close();
    }
}

/// A Chipnet payout address of its own for each seed.
fn address(seed: u8) -> String {
    let public = secp256k1::PublicKey::from_secret_key(
        &secp256k1::SecretKey::from_secret_bytes([seed; 32]).unwrap(),
    )
    .serialize();
    crate::config::reprefix_p2pkh_payout(
        &crate::reward::p2pkh_cashaddr_from_public_key(&public).unwrap(),
        MiningNetwork::Chipnet,
    )
    .unwrap()
}

// #### PR #40
#[test]
fn a_public_pool_pays_each_miner_their_own_address_after_the_donation_and_fee() {
    use crate::donation::bch::{FeeMode, PoolFee};
    use crate::tx::cashaddr_to_p2pkh_locking;
    let operator = address(2);
    let server = Running::public(super::payout::PublicPool {
        fee: Some(PoolFee {
            rate: "2".parse().unwrap(),
            mode: FeeMode::Coinbase,
        }),
        address: operator.clone(),
    });
    let donation = crate::donation::bch::address(MiningNetwork::Chipnet);
    // Two miners, one with a bare address and a worker name.
    let alice = address(3);
    let bob = address(4);
    let bare_bob = format!("{}.rig-2", bob.split_once(':').unwrap().1);
    for (round, (extended, identity, miner)) in [
        (false, alice.clone(), alice.clone()),
        (true, bare_bob, bob.clone()),
    ]
    .into_iter()
    .enumerate()
    {
        let mut device = Device::connect_as(&server, extended, &identity);
        let hash = device.solve_and_submit(0);
        server.wait(|stats| stats.blocks_accepted == round as u64 + 1);
        let node = server.node.lock().unwrap();
        assert_eq!(node.tip, hash);
        let block: Block =
            consensus::deserialize(&hex::decode(node.submitted.last().unwrap()).unwrap()).unwrap();
        let outputs: Vec<(u64, Vec<u8>)> = block.txdata[0]
            .output
            .iter()
            .map(|output| (output.value.to_sat(), output.script_pubkey.to_bytes()))
            .collect();
        let total: u64 = outputs.iter().map(|(value, _)| value).sum();
        // The donation comes off first (1% of the reward at 1.5%), then the
        // operator's 2% of what is left; the miner keeps the rest.
        let given = total / 100;
        let taken = (total - given) * 200 / 10_000;
        assert_eq!(
            outputs,
            vec![
                (
                    total - given - taken,
                    cashaddr_to_p2pkh_locking(&miner).unwrap()
                ),
                (given, cashaddr_to_p2pkh_locking(donation).unwrap()),
                (taken, cashaddr_to_p2pkh_locking(&operator).unwrap()),
            ],
            "round {round}"
        );
        drop(node);
        device.sender.close();
    }
    // #### PR #48: both blocks came from personal jobs (the fee here is a
    // coinbase split, not work).
    let work = server.stats.lock().unwrap().work;
    assert!(work.personal > 0.0);
    assert_eq!(work.shares(), Some((0.0, 0.0)));
}

/// An external reference process started by one of the tests below.
struct Reference(std::process::Child);
impl Drop for Reference {
    fn drop(&mut self) {
        // Only this test's child process; never a running user miner.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The pinned authority key as SRI's configuration and CLI spell it.
fn reference_authority(public: [u8; 32]) -> String {
    let mut encoded = vec![1, 0];
    encoded.extend(public);
    stratum_core::bitcoin::base58::encode_check(&encoded)
}

// #### PR #38
// Run an independently built, unmodified SRI mining_device against synthetic
// BCH templates. No live node, user payout or GPU is accessed by this test.
#[test]
#[ignore = "requires PICKAXE_SV2_REFERENCE_DEVICE built from the pinned SRI source"]
fn upstream_reference_device_authenticates_and_mines_successor_blocks() {
    use std::process::{Command, Stdio};

    let executable = std::env::var_os("PICKAXE_SV2_REFERENCE_DEVICE")
        .expect("set PICKAXE_SV2_REFERENCE_DEVICE to the unmodified SRI mining_device binary");
    let server = Running::new(false);
    let start = |public: [u8; 32], name: &str| {
        let authority = reference_authority(public);
        let log = std::fs::File::create(server.state_directory.0.join(name)).unwrap();
        Reference(
            Command::new(&executable)
                .args([
                    "--address-pool",
                    &server.address.to_string(),
                    "--pubkey-pool",
                    &authority,
                    "--id-device",
                    "reference-cpu",
                    "--id-user",
                    "interop-test",
                    "--cores",
                    "1",
                    "--nonces-per-call",
                    "1",
                    "--handicap",
                    "1000000",
                ])
                .stdin(Stdio::null())
                .stderr(log.try_clone().unwrap())
                .stdout(log)
                .spawn()
                .expect("cannot start the external reference device"),
        )
    };

    // The independent client must reject the wrong pinned authority rather
    // than silently accepting an unauthenticated mining endpoint.
    let mut wrong = start(
        server::authority_public(&[18; 32]).unwrap(),
        "wrong-key.log",
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = wrong.0.try_wait().unwrap() {
            assert!(!status.success(), "wrong authority unexpectedly accepted");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reference client did not reject wrong authority"
        );
        thread::sleep(Duration::from_millis(50));
    }
    drop(wrong);
    server.wait(|stats| stats.connections == 0 && stats.connection_errors == 1);
    assert_eq!(server.stats.lock().unwrap().shares_accepted, 0);

    let mut valid = start(server.authority, "reference-device.log");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snapshot = server.stats.lock().unwrap().clone();
        if snapshot.blocks_accepted >= 2 {
            assert_eq!(snapshot.sessions_started, 2); // one rejected key, one mining session
            assert_eq!(snapshot.connection_errors, 1);
            assert_eq!(snapshot.connections, 1);
            assert!(snapshot.shares_accepted >= 2);
            break;
        }
        let exited = valid.0.try_wait().unwrap();
        if exited.is_some() || Instant::now() >= deadline {
            let log =
                std::fs::read_to_string(server.state_directory.0.join("reference-device.log"))
                    .unwrap_or_default();
            // Keep diagnostic output free of any protocol payloads/identities.
            panic!(
                "reference experiment failed: exited={exited:?}, setup={}, channel={}, ntime={}, stats={snapshot:?}",
                log.contains("SetupConnectionSuccess"),
                log.contains("channel opened"),
                log.contains("invalid-ntime"),
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    // The RPC fixture independently deserializes and checks PoW, merkle, BCH
    // coinbase serialization, configured payout and successor parent linkage.
    assert!(server.node.lock().unwrap().known.len() >= 2);
    drop(valid);
}

// #### PR #48
// What: an independently built, unmodified SRI translator opens one extended
// channel per SV1 miner on one connection to this server (a group), and two
// SV1 miners take turns mining successor blocks through it.
// Why: the reference device above opens a standard channel; this is the
// external extended-channel client (S3 in implementation-status.md) and the
// group-channel path of PR #42 with SRI's translator.
// Look here if: the translator gets no job from the server, or the blocks it
// forwards are rejected.
#[test]
#[ignore = "requires PICKAXE_SV2_REFERENCE_TRANSLATOR built from the pinned SRI source"]
fn upstream_reference_translator_mines_successor_blocks_on_an_extended_channel() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    let executable = std::env::var_os("PICKAXE_SV2_REFERENCE_TRANSLATOR")
        .expect("set PICKAXE_SV2_REFERENCE_TRANSLATOR to the unmodified SRI translator_sv2 binary");
    let server = Running::new(false);
    // A free port for the translator's SV1 listener.
    let downstream = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = server.state_directory.0.join("translator.toml");
    // One extended channel per SV1 connection, the translator's vardiff off:
    // it sets each miner's difficulty from the configured minimum hashrate
    // (10 H/s, about 100 hashes a share).
    std::fs::write(
        &config,
        format!(
            r#"downstream_address = "127.0.0.1"
downstream_port = {downstream}
max_supported_version = 2
min_supported_version = 2
downstream_extranonce2_size = 4
aggregate_channels = false
supported_extensions = []

[downstream_difficulty_config]
min_individual_miner_hashrate = 10.0
shares_per_minute = 6.0
enable_vardiff = false
job_keepalive_interval_secs = 0

[[upstreams]]
address = "{host}"
port = {port}
authority_pubkey = "{authority}"
user_identity = "interop-test"
"#,
            downstream = downstream.port(),
            host = server.address.ip(),
            port = server.address.port(),
            authority = reference_authority(server.authority),
        ),
    )
    .unwrap();
    let log_path = server.state_directory.0.join("translator.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut translator = Reference(
        Command::new(&executable)
            .arg("--config")
            .arg(&config)
            // Channel opening is logged at debug level.
            .env("RUST_LOG", "debug")
            .stdin(Stdio::null())
            .stderr(log.try_clone().unwrap())
            .stdout(log)
            .spawn()
            .expect("cannot start the external reference translator"),
    );
    let failed = |translator: &mut Reference, why: &str| -> ! {
        let exited = translator.0.try_wait().unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let snapshot = server.stats.lock().unwrap().clone();
        // Keep diagnostic output free of any protocol payloads/identities.
        panic!(
            "reference translator experiment failed ({why}): exited={exited:?}, upstream={}, extended={}, stats={snapshot:?}",
            log.contains("SetupConnectionSuccess"),
            log.contains("Received OpenExtendedMiningChannelSuccess"),
        );
    };

    // Two SV1 miners behind the translator: two extended channels on one
    // connection, which the server groups (one job frame per template).
    struct Miner {
        device: FirmwareDevice,
        authorized: bool,
        /// The latest job and its parent's place in the chain: 0 for the
        /// first parent seen, one more for each new one.
        job: Option<(Value, u64)>,
        parent: Option<String>,
        generation: u64,
        submitted: u32,
        accepted: u32,
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut miners: Vec<Miner> = Vec::new();
    while miners.len() < 2 {
        let write = match TcpStream::connect(downstream) {
            Ok(stream) => stream,
            Err(_) if Instant::now() < deadline => {
                if translator.0.try_wait().unwrap().is_some() {
                    failed(&mut translator, "exited before listening");
                }
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(_) => failed(&mut translator, "never listened"),
        };
        write
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut device = FirmwareDevice {
            read: std::io::BufReader::new(write.try_clone().unwrap()),
            write,
            prefix: Vec::new(),
            extra_size: 0,
            name: format!("interop-test.{}", miners.len()),
            clean: false,
            difficulty: 0.0,
        };
        device.send(json!({"id":1,"method":"mining.subscribe","params":["Pickaxe test firmware"]}));
        miners.push(Miner {
            device,
            authorized: false,
            job: None,
            parent: None,
            generation: 0,
            submitted: 0,
            accepted: 0,
        });
    }

    // Answers and notifications arrive in any order. The miners take turns,
    // one block each, so two blocks never compete for one height. A job's
    // generation comes from its parent, never from when it was read, and no
    // one mines until both miners have their first job.
    let mut id = 0u32;
    loop {
        if server.stats.lock().unwrap().blocks_accepted >= 4 {
            break;
        }
        let both = miners.iter().all(|miner| miner.parent.is_some());
        if Instant::now() >= deadline || translator.0.try_wait().unwrap().is_some() {
            failed(&mut translator, "fewer than four blocks");
        }
        for (index, miner) in miners.iter_mut().enumerate() {
            let device = &mut miner.device;
            let mut line = String::new();
            match device.read.read_line(&mut line) {
                Ok(0) => failed(&mut translator, "SV1 connection closed"),
                Ok(_) => {
                    let message: Value = serde_json::from_str(&line).unwrap();
                    match (message["id"].as_u64(), message["method"].as_str()) {
                        (Some(1), None) => {
                            device.prefix =
                                hex::decode(message["result"][1].as_str().unwrap()).unwrap();
                            device.extra_size = message["result"][2].as_u64().unwrap() as usize;
                            assert!(device.extra_size >= 1, "{message}");
                            let name = device.name.clone();
                            device.send(
                                json!({"id":2,"method":"mining.authorize","params":[name,"x"]}),
                            );
                        }
                        (Some(2), None) => {
                            assert_eq!(message["result"], true, "{message}");
                            miner.authorized = true;
                        }
                        (Some(_), None) => {
                            assert_eq!(message["result"], true, "{message}");
                            miner.accepted += 1;
                        }
                        (_, Some("mining.set_difficulty")) => {
                            device.difficulty = message["params"][0].as_f64().unwrap();
                        }
                        (_, Some("mining.notify")) => {
                            let parent = message["params"][1].as_str().map(str::to_owned);
                            if miner.parent.is_some() && miner.parent != parent {
                                miner.generation += 1;
                            }
                            miner.parent = parent;
                            miner.job = Some((message, miner.generation));
                        }
                        _ => (),
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => failed(&mut translator, "SV1 read failed"),
            }
            let blocks = server.stats.lock().unwrap().blocks_accepted;
            let turn = both && blocks < 4 && usize::try_from(blocks % 2).unwrap() == index;
            let current = miner.job.as_ref().is_some_and(|(_, at)| *at == blocks);
            if miner.authorized && !miner.device.prefix.is_empty() && turn && current {
                let (notify, _) = miner.job.take().unwrap();
                id += 1;
                let (_, submit) = miner.device.solve_notify(id, false, &notify);
                miner.device.send(submit);
                miner.submitted += 1;
            }
        }
    }
    let snapshot = server.stats.lock().unwrap().clone();
    assert_eq!(snapshot.connections, 1);
    // Only the miner whose turn it is submits, so each block came from the
    // channel that mined it, with no competing or rejected share.
    assert_eq!((snapshot.blocks_accepted, snapshot.shares_rejected), (4, 0));
    assert!(snapshot.shares_accepted >= 4);
    assert!(miners.iter().all(|miner| miner.submitted == 2));
    assert!(miners.iter().all(|miner| miner.accepted >= 1));
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(
        log.matches("Received OpenExtendedMiningChannelSuccess")
            .count(),
        2
    );
    // The RPC fixture independently deserializes and checks PoW, merkle, BCH
    // coinbase serialization, configured payout and successor parent linkage.
    assert!(server.node.lock().unwrap().known.len() >= 4);
    drop(translator);
}

// #### PR #40
#[test]
fn the_dashboard_records_the_best_share_and_each_block_found() {
    let server = Running::new(false);
    let mut device = Device::connect(&server, false);
    device.solve_and_submit(0);
    server.wait(|stats| stats.blocks_accepted == 1);
    server.wait(|stats| {
        stats
            .recent_blocks
            .back()
            .is_some_and(|found| found.result == Some("accepted"))
    });
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!(stats.recent_blocks.len(), 1);
    let found = &stats.recent_blocks[0];
    assert_eq!(found.height, 325909);
    assert_eq!(found.hash.len(), 64);
    let (best, worker) = stats.best_share.clone().expect("a best share");
    assert!(best > 0.0);
    assert_eq!(worker, found.worker);
    let devices = stats.device_stats.snapshots(Instant::now());
    assert_eq!(devices[0].best_share, Some(best));
    device.sender.close();
}

#[test]
fn share_acknowledgement_does_not_claim_node_block_acceptance() {
    let server = Running::new(true);
    let mut device = Device::connect(&server, false);
    device.solve_and_submit(0);
    server.wait(|stats| stats.blocks_rejected == 1);
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!(stats.shares_accepted, 1);
    assert_eq!(stats.blocks_accepted, 0);
    assert_eq!(server.node.lock().unwrap().submissions, 1);
    assert_eq!(server.node.lock().unwrap().height, 325908);
    device.sender.close();
}

#[test]
fn acknowledged_block_survives_lost_replies_and_server_restart_exactly_once() {
    let server = Running::new(false);
    server.node.lock().unwrap().lose_replies = true;
    let mut device = Device::connect(&server, true);
    let hash = device.solve_and_submit(0);
    server.wait(|stats| stats.last_block_result == Some("node-response-unavailable"));
    assert_eq!(server.stats.lock().unwrap().blocks_accepted, 0);
    assert_eq!(server.stats.lock().unwrap().blocks_pending, 1);
    let disk: Value =
        serde_json::from_slice(&std::fs::read(server.state_directory.journal()).unwrap()).unwrap();
    assert_eq!(disk["pending"][0]["hash"], hash);
    let exact = disk["pending"][0]["block"].as_str().unwrap().to_owned();
    let node = server.node.clone();
    let directory = server.state_directory.clone();
    device.sender.close();
    drop(device);
    drop(server);
    node.lock().unwrap().lose_replies = false;
    let recovered = Running::start(node.clone(), directory.clone());
    recovered.wait(|stats| stats.blocks_accepted == 1 && stats.blocks_pending == 0);
    assert_eq!(
        node.lock().unwrap().submissions,
        1,
        "retry must not create a second block"
    );
    assert!(node
        .lock()
        .unwrap()
        .submitted
        .iter()
        .all(|bytes| bytes == &exact));
    assert!(node.lock().unwrap().submitted.len() >= 2);
    let attempts = node.lock().unwrap().submitted.len();
    drop(recovered);
    let reopened = Running::start(node.clone(), directory);
    assert_eq!(reopened.stats.lock().unwrap().blocks_accepted, 1);
    assert_eq!(
        node.lock().unwrap().submitted.len(),
        attempts,
        "receipt must suppress another submission"
    );
}

#[test]
fn disk_failure_stops_server_without_acknowledging_solved_work() {
    use std::io::BufRead;
    let mut server = Running::new(false);
    let adapter = FirmwareAdapter::new(&server);
    let mut device = FirmwareDevice::connect(&adapter, false, false);
    let (_, submit) = device.solve(0, false);
    let path = server.state_directory.journal();
    let original = std::fs::read(&path).unwrap();
    let saved = server.state_directory.0.join("before-failure.json");
    std::fs::rename(&path, &saved).unwrap();
    std::fs::create_dir(&path).unwrap();
    device.send(submit);
    let mut line = String::new();
    let received = device.read.read_line(&mut line);
    assert!(
        matches!(received, Ok(0)) || received.is_err(),
        "must not acknowledge undurable work"
    );
    let result = server.thread.take().unwrap().join().unwrap();
    assert_eq!(
        result.unwrap_err(),
        "cannot persist solved block; mining stopped"
    );
    assert_eq!(server.node.lock().unwrap().submissions, 0);
    assert_eq!(std::fs::read(&saved).unwrap(), original);
}

struct FirmwareAdapter {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), String>>>,
    address: SocketAddr,
}
impl FirmwareAdapter {
    fn new(server: &Running) -> Self {
        Self::with(
            server,
            super::sv1::Upstream::local(server.address, server.authority),
        )
    }

    /// #### PR #40: this server's adapter for a public pool.
    fn public(server: &Running) -> Self {
        Self::with(
            server,
            super::sv1::Upstream::local_public(server.address, server.authority),
        )
    }

    fn with(server: &Running, upstream: super::sv1::Upstream) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = server.stop.clone();
        let thread = {
            let stop = stop.clone();
            let stats = server.stats.clone();
            thread::spawn(move || super::sv1::run(listener, vec![upstream], stop, stats))
        };
        Self {
            stop,
            thread: Some(thread),
            address,
        }
    }
}
impl Drop for FirmwareAdapter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !thread::panicking() {
                assert!(matches!(result, Ok(Ok(()))));
            }
        }
    }
}

struct FirmwareDevice {
    write: TcpStream,
    read: std::io::BufReader<TcpStream>,
    prefix: Vec<u8>,
    /// The extranonce2 size the adapter gave: 8 from this server's own
    /// listener, 4 at a remote pool (#### PR #40).
    extra_size: usize,
    /// The worker name it authorized with and submits under.
    name: String,
    clean: bool,
    /// #### PR #48: the last `mining.set_difficulty` a proxy asked for.
    /// Solutions meet it as well as the block target; 0 is the block alone.
    difficulty: f64,
}
impl FirmwareDevice {
    fn connect(adapter: &FirmwareAdapter, rolling: bool, authorize_first: bool) -> Self {
        Self::connect_named(adapter, rolling, authorize_first, &payout())
    }

    fn connect_named(
        adapter: &FirmwareAdapter,
        rolling: bool,
        authorize_first: bool,
        name: &str,
    ) -> Self {
        let write = TcpStream::connect(adapter.address).unwrap();
        write
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let read = std::io::BufReader::new(write.try_clone().unwrap());
        let mut device = Self {
            write,
            read,
            prefix: Vec::new(),
            extra_size: 0,
            name: name.to_owned(),
            clean: false,
            difficulty: 0.0,
        };
        if rolling {
            device.send(json!({"id":1,"method":"mining.configure","params":[["version-rolling"],{"version-rolling.mask":"1fffe000","version-rolling.min-bit-count":2}]}));
            let configured = device.receive();
            assert_eq!(configured["result"]["version-rolling"], true);
            assert_eq!(configured["result"]["version-rolling.mask"], "1fffe000");
        }
        let authorize = json!({"id":3,"method":"mining.authorize","params":[name]});
        if authorize_first {
            device.send(authorize.clone());
            assert_eq!(device.receive()["result"], true);
        }
        device
            .send(json!({"id":2,"method":"mining.subscribe","params":["CPU firmware experiment"]}));
        let subscribed = device.receive();
        device.extra_size = subscribed["result"][2].as_u64().unwrap() as usize;
        device.prefix = hex::decode(subscribed["result"][1].as_str().unwrap()).unwrap();
        // This server's prefix and eight bytes, or at a remote pool the
        // adapter's four and four.
        assert!(
            matches!((device.prefix.len(), device.extra_size), (16, 8) | (4, 4)),
            "{subscribed}"
        );
        if !authorize_first {
            device.send(authorize);
            assert_eq!(device.receive()["result"], true);
        }
        device
    }
    /// #### PR #42: waits until the adapter ends this connection.
    fn closed(mut self) {
        use std::io::BufRead;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < deadline, "the connection stayed open");
            let mut line = String::new();
            match self.read.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => (),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return,
            }
        }
    }

    fn send(&mut self, value: Value) {
        use std::io::Write;
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        self.write.write_all(&bytes).unwrap();
    }
    fn receive(&mut self) -> Value {
        use std::io::BufRead;
        let mut line = String::new();
        self.read.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "firmware connection closed");
        serde_json::from_str(&line).unwrap()
    }
    fn solve(&mut self, id: u32, rolling: bool) -> (String, Value) {
        let difficulty = self.receive();
        assert_eq!(difficulty["method"], "mining.set_difficulty");
        assert!(difficulty["params"][0].as_f64().unwrap() > 0.0);
        let notify = self.receive();
        self.solve_notify(id, rolling, &notify)
    }
    fn solve_notify(&mut self, id: u32, rolling: bool, notify: &Value) -> (String, Value) {
        assert_eq!(notify["method"], "mining.notify");
        let fields = notify["params"].as_array().unwrap();
        self.clean = fields[8].as_bool().unwrap();
        let extra = vec![id as u8; self.extra_size];
        let mut coinbase = hex::decode(fields[2].as_str().unwrap()).unwrap();
        coinbase.extend(&self.prefix);
        coinbase.extend(&extra);
        coinbase.extend(hex::decode(fields[3].as_str().unwrap()).unwrap());
        let mut root = sha256d::Hash::hash(&coinbase).to_byte_array();
        for sibling in fields[4].as_array().unwrap() {
            let mut joined = root.to_vec();
            joined.extend(hex::decode(sibling.as_str().unwrap()).unwrap());
            root = sha256d::Hash::hash(&joined).to_byte_array();
        }
        let version = u32::from_str_radix(fields[5].as_str().unwrap(), 16).unwrap();
        let version = if rolling { version ^ 0x2000 } else { version };
        let mut header = version.to_le_bytes().to_vec();
        let mut previous = hex::decode(fields[1].as_str().unwrap()).unwrap();
        for word in previous.as_chunks_mut::<4>().0 {
            word.reverse();
        }
        header.extend(previous);
        header.extend(root);
        let time = u32::from_str_radix(fields[7].as_str().unwrap(), 16).unwrap();
        header.extend(time.to_le_bytes());
        let bits = u32::from_str_radix(fields[6].as_str().unwrap(), 16).unwrap();
        header.extend(bits.to_le_bytes());
        header.extend(0u32.to_le_bytes());
        let mut header: Header = consensus::deserialize(&header).unwrap();
        let nonce = (0..10_000)
            .find(|n| {
                header.nonce = *n;
                header.validate_pow(header.target()).is_ok()
                    && super::telemetry::share_difficulty(&header.block_hash().to_byte_array())
                        >= self.difficulty
            })
            .unwrap();
        let mut params = vec![
            json!(self.name),
            fields[0].clone(),
            json!(hex::encode(extra)),
            json!(format!("{time:08x}")),
            json!(format!("{nonce:08x}")),
        ];
        if rolling {
            params.push(json!(format!("{:08x}", version & 0x1fffe000)));
        }
        let submit = json!({"id":id+10,"method":"mining.submit","params":params});
        (header.block_hash().to_string(), submit)
    }
}

#[test]
fn sv1_transient_node_failure_revokes_work_and_recovers_without_reconnect() {
    let server = Running::new(false);
    let adapter = FirmwareAdapter::new(&server);
    let mut device = FirmwareDevice::connect(&adapter, true, false);
    let (_, revoked) = device.solve(0, true);
    server.node.lock().unwrap().unavailable = true;
    server.wait(|stats| !stats.template_ready && stats.template_failures > 0);
    device.send(revoked.clone());
    let response = device.receive();
    assert_eq!(response["id"], 10);
    assert!(response["error"].is_array());
    assert_eq!(server.node.lock().unwrap().submissions, 0);
    assert_eq!(server.stats.lock().unwrap().shares_accepted, 0);

    server.node.lock().unwrap().unavailable = false;
    let (hash, recovered) = device.solve(1, true);
    assert!(device.clean, "recovery must invalidate firmware's old jobs");
    assert_ne!(revoked["params"][1], recovered["params"][1]);
    device.send(revoked);
    assert_eq!(device.receive()["error"][0], 21);
    device.send(recovered);
    let response = device.receive();
    assert_eq!(response["id"], 11);
    assert_eq!(response["result"], true);
    server.wait(|stats| stats.blocks_accepted == 1);
    assert_eq!(server.node.lock().unwrap().tip, hash);
    let stats = server.stats.lock().unwrap();
    assert_eq!(stats.sessions_started, 1);
    assert_eq!(
        (stats.connection_errors, stats.sv1_connection_errors),
        (0, 0)
    );
    assert_eq!(stats.last_template_error, Some("node RPC unavailable"));
}

// #### PR #40
#[test]
fn the_server_moves_to_its_next_node_when_its_node_stops_answering() {
    let node = || {
        Arc::new(Mutex::new(Node {
            height: 325908,
            tip: "ab".repeat(32),
            reject: false,
            submissions: 0,
            lose_replies: false,
            submitted: Vec::new(),
            known: std::collections::HashSet::new(),
            unavailable: false,
            public: false,
            transactions: Vec::new(),
            spendable: Vec::new(),
            validations: 0,
            parent_time: None,
        }))
    };
    let (first, second) = (node(), node());
    let server = Running::start_nodes(
        vec![first.clone(), second.clone()],
        Arc::new(TestDirectory::new()),
        None,
    );
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!((stats.nodes, stats.active_node), (2, 0));
    first.lock().unwrap().unavailable = true;
    server.wait(|stats| stats.node_switches >= 1 && stats.active_node == 1 && stats.template_ready);
    // A block found now goes to the second node.
    let mut device = Device::connect(&server, false);
    device.solve_and_submit(0);
    server.wait(|stats| stats.blocks_accepted == 1);
    assert_eq!(second.lock().unwrap().submissions, 1);
    assert_eq!(first.lock().unwrap().submissions, 0);
    device.sender.close();
    // When the second fails too, the first (back again) takes over.
    first.lock().unwrap().unavailable = false;
    second.lock().unwrap().unavailable = true;
    server.wait(|stats| stats.node_switches >= 2 && stats.active_node == 0 && stats.template_ready);
}

#[test]
fn persistent_node_failure_still_closes_device_after_bounded_grace() {
    let server = Running::new(false);
    let adapter = FirmwareAdapter::new(&server);
    let mut device = FirmwareDevice::connect(&adapter, false, false);
    let _ = device.solve(0, false);
    server.node.lock().unwrap().unavailable = true;
    server.wait(|stats| !stats.template_ready && stats.template_failures > 0);
    server.wait(|stats| stats.connections == 0);
    let stats = server.stats.lock().unwrap();
    assert_eq!(stats.sessions_started, 1);
    assert_eq!(stats.connection_errors, 1);
    assert_eq!(stats.shares_accepted, 0);
    assert_eq!(
        stats.device_stats.snapshots(Instant::now())[0].connection_error,
        Some("template unavailable")
    );
}

#[test]
fn sv1_same_tip_refresh_accepts_inflight_block_and_new_tip_rejects_it() {
    let server = Running::new(false);
    let adapter = FirmwareAdapter::new(&server);
    let mut first = FirmwareDevice::connect(&adapter, true, false);
    let second = FirmwareDevice::connect(&adapter, true, true);
    // Both devices identify with the same address; their hashing spaces differ.
    assert_ne!(first.prefix, second.prefix);
    let (hash, submit) = first.solve(0, true);
    assert!(first.clean);
    // Exercise the real production 15-second template refresh over TCP/Noise.
    first
        .read
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let (_, new_submit) = first.solve(1, true);
    assert!(
        !first.clean,
        "same-tip updates must retain firmware pipeline work"
    );
    assert_ne!(submit["params"][1], new_submit["params"][1]);
    first.send(submit.clone());
    let ack = first.receive();
    assert_eq!(ack["id"], 10);
    assert_eq!(ack["result"], true);
    server.wait(|stats| stats.blocks_accepted == 1);
    assert_eq!(server.node.lock().unwrap().tip, hash);
    first.solve(2, true);
    assert!(first.clean, "new parent must revoke preceding jobs");
    first.send(submit);
    let stale = first.receive();
    assert_eq!(stale["error"][0], 21);
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!(
        stats.shares_rejected, 1,
        "SV1-local stale shares must reach the dashboard"
    );
    assert_eq!(stats.sv1_local_rejected, 1);
    let devices = stats.device_stats.snapshots(Instant::now());
    assert_eq!(
        devices.len(),
        2,
        "adapter and native socket must not become two devices"
    );
    assert_ne!(devices[0].label, devices[1].label);
    assert!(devices.iter().all(|row| row.protocol == "SV1"));
    assert_eq!(devices.iter().map(|row| row.accepted).sum::<u64>(), 1);
    assert_eq!(devices.iter().map(|row| row.rejected).sum::<u64>(), 1);
    first.write.shutdown(std::net::Shutdown::Both).unwrap();
    second.write.shutdown(std::net::Shutdown::Both).unwrap();
}

#[test]
fn sv1_cpu_firmware_mines_through_noise_and_receives_successor_jobs() {
    for (rolling, authorize_first) in [(false, true), (true, false)] {
        let server = Running::new(false);
        let adapter = FirmwareAdapter::new(&server);
        let mut device = FirmwareDevice::connect(&adapter, rolling, authorize_first);
        for round in 0..2 {
            let (hash, submit) = device.solve(round, rolling);
            device.send(submit);
            let ack = device.receive();
            assert_eq!(ack["id"], round + 10);
            assert_eq!(ack["result"], true);
            assert!(ack["error"].is_null());
            server.wait(|stats| stats.blocks_accepted == u64::from(round) + 1);
            assert_eq!(server.node.lock().unwrap().tip, hash);
        }
        assert_eq!(server.stats.lock().unwrap().shares_rejected, 0);
        assert_eq!(server.stats.lock().unwrap().connections, 1);
        let devices = server
            .stats
            .lock()
            .unwrap()
            .device_stats
            .snapshots(Instant::now());
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].accepted, 2);
        assert_eq!(devices[0].rejected, 0);
        device.write.shutdown(std::net::Shutdown::Both).unwrap();
    }
}

#[test]
fn sv1_firmware_mines_at_a_remote_sv2_pool_and_the_adapter_counts_its_verdicts() {
    // Pickaxe's own server stands in for the remote pool; the adapter keeps
    // its own statistics, as a separate process would.
    let pool = Running::new(false);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(ServerStats::default()));
    // The first pool pins a key the pool does not hold, so its handshake
    // fails and the device falls back to the second, which is correct.
    let pool_with = |authority| super::sv1::Upstream {
        // A host name, resolved at each connection.
        address: format!("localhost:{}", pool.address.port()),
        authority,
        identity: payout(),
        remote: true,
        donation: None,
        public: false,
        prefer: None,
        fixed_version: false,
    };
    let upstreams = vec![pool_with([3; 32]), pool_with(pool.authority)];
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || super::sv1::run(listener, upstreams, stop, stats))
    };
    let adapter = FirmwareAdapter {
        stop,
        thread: Some(thread),
        address,
    };
    let mut device = FirmwareDevice::connect(&adapter, true, false);
    for round in 0..2 {
        let (hash, submit) = device.solve(round, true);
        device.send(submit);
        let ack = device.receive();
        assert_eq!(ack["id"], round + 10);
        assert_eq!(ack["result"], true);
        pool.wait(|stats| stats.blocks_accepted == u64::from(round) + 1);
        assert_eq!(pool.node.lock().unwrap().tip, hash);
    }
    // The pool's verdicts reach the adapter's own workers page.
    let deadline = Instant::now() + Duration::from_secs(5);
    while stats.lock().unwrap().shares_accepted < 2 {
        assert!(Instant::now() < deadline, "pool verdicts not counted");
        thread::sleep(Duration::from_millis(10));
    }
    let counted = stats.lock().unwrap().clone();
    assert_eq!((counted.connections, counted.sessions_started), (1, 1));
    assert_eq!(counted.shares_rejected, 0);
    let rows = counted.device_stats.snapshots(Instant::now());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].accepted, rows[0].rejected, rows[0].protocol),
        (2, 0, "SV1")
    );
    // #### PR #48: both acknowledged shares count as personal work at the
    // pool, apart from this server's own jobs.
    assert!(counted.pool_work.personal > 0.0);
    assert_eq!(counted.pool_work.shares(), Some((0.0, 0.0)));
    assert_eq!(counted.work, super::server::WorkSplit::default());
    device.write.shutdown(std::net::Shutdown::Both).unwrap();
}

// #### PR #42
// What: the Chipnet ASIC test token on the real server, with no node: a
// device's share on its job is a win, the token worker proves and saves it,
// the simulated thread moves to the winning header, and the device's next
// job follows it; nothing is a block.
// Look here if: the token worker, its queue or the re-issued jobs change.
#[test]
fn the_simulated_test_token_advances_after_each_proven_win() {
    let directory = Arc::new(TestDirectory::new());
    let work = Arc::new(
        super::merge::source::HeaderWork::test_token(
            MiningNetwork::Chipnet,
            directory.0.join("token-proofs.json"),
            0x207f_ffff,
            &payout(),
        )
        .unwrap(),
    );
    let server = Running::token(work.clone(), directory.clone());
    assert_eq!(
        server.stats.lock().unwrap().template_source,
        Some(super::provider::SourceKind::Token)
    );
    let mut device = Device::connect(&server, false);
    let first = device.solve_and_submit(1);
    server.wait(|stats| {
        stats
            .header_token
            .as_ref()
            .is_some_and(|token| token.proven == 1)
    });
    let next = loop {
        let mut frame = device.receive();
        if frame.header().msg_type() == MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH {
            let previous: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
            let mut display = previous.prev_hash.as_ref().to_vec();
            display.reverse();
            break hex::encode(display);
        }
    };
    assert_eq!(next, first, "the next job follows the win");
    let saved = std::fs::read(directory.0.join("token-proofs.json")).unwrap();
    assert!(String::from_utf8(saved).unwrap().contains("\"mode\":\"H\""));
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!((stats.blocks_pending, stats.blocks_accepted), (0, 0));
    assert_eq!(server.node.lock().unwrap().submissions, 0);
    device.sender.close();
}

// #### PR #42
// What: an SV2 standard device, an SV2 extended device and SV1 firmware
// through the adapter mine a SAFA token job (from a test source) on the real
// server: every share is accepted, none is a block (no journal entry,
// nothing reaches the node), and the SV1 notify's coinbase part starts with
// the forwarder's push and tag (2050584831…).
// Look here if: token work in the share path changes.
#[test]
fn sv2_standard_extended_and_sv1_cpu_devices_mine_a_token_job() {
    struct TokenJobs(super::template::BchTemplate);
    impl super::provider::TemplateSource for TokenJobs {
        fn kind(&self) -> super::provider::SourceKind {
            super::provider::SourceKind::NodeRpc
        }
        fn current(&self) -> Option<(u64, &super::template::BchTemplate)> {
            Some((1, &self.0))
        }
        fn tip_is_current(&mut self) -> Result<bool, String> {
            Ok(true)
        }
        fn refresh(&mut self) -> Result<(u64, &super::template::BchTemplate), String> {
            Ok((1, &self.0))
        }
        fn submit_saved(
            &mut self,
            _: &super::journal::PendingBlock,
        ) -> super::provider::SubmissionOutcome {
            super::provider::SubmissionOutcome::Pending("token work makes no blocks")
        }
        fn reset(&mut self) {}
    }
    let template = super::template::BchTemplate::token_only(
        Arc::new(super::template::TokenJob {
            token: "test",
            layout: super::template::Layout::Safa,
            version_mask: 0x1fff_e000,
            anchor: [5; 32],
            thread: super::merge::OutPoint {
                txid: [6; 32],
                vout: 0,
            },
            age: 71,
            mtp: now(),
        }),
        [0xab; 32],
        0x2000_0000,
        0x207f_ffff,
        super::template::compact_target(0x207f_ffff).unwrap(),
        now() - 10,
        1,
    );
    let node = pool_node();
    let server = Running::start_config(
        vec![node.clone()],
        Arc::new(TestDirectory::new()),
        None,
        None,
        None,
        None,
        None,
        vec![Box::new(TokenJobs(template))],
    );
    let mut standard = Device::connect(&server, false);
    standard.solve_and_submit(1);
    let mut extended = Device::connect(&server, true);
    extended.solve_and_submit(1);
    let adapter = FirmwareAdapter::new(&server);
    let mut firmware = FirmwareDevice::connect(&adapter, true, false);
    let difficulty = firmware.receive();
    assert_eq!(difficulty["method"], "mining.set_difficulty");
    let notify = firmware.receive();
    assert!(
        notify["params"][2]
            .as_str()
            .unwrap()
            .starts_with("2050584831"),
        "{notify}"
    );
    let (_, submit) = firmware.solve_notify(1, true, &notify);
    firmware.send(submit);
    assert_eq!(firmware.receive()["result"], true);
    server.wait(|stats| stats.shares_accepted >= 3);
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!((stats.blocks_pending, stats.blocks_accepted), (0, 0));
    assert!(stats.recent_blocks.is_empty());
    assert_eq!(node.lock().unwrap().submissions, 0);
    firmware.write.shutdown(std::net::Shutdown::Both).unwrap();
    standard.sender.close();
    extended.sender.close();
}

// #### PR #42
// What: a proxy (one SV2 connection, two extended channels of one identity,
// as SRI's translator opens them) mines through group jobs on a real
// server: both channels are announced in one group and get their own first
// jobs; after a block the next template comes as one job addressed to the
// group, and a block on it from the second channel is accepted.
// Look here if: group channels change on the server.
#[test]
fn a_proxy_mines_through_a_grouped_connection() {
    struct Work {
        job: u32,
        version: u32,
        head: Vec<u8>,
        tail: Vec<u8>,
        path: Vec<[u8; 32]>,
    }
    /// One frame: its type and the channel or group it addresses, with
    /// the jobs and the parent recorded.
    fn read(
        receiver: &mut Receiver,
        work: &mut std::collections::HashMap<u32, Work>,
        parent: &mut Option<([u8; 32], u32, u32)>,
    ) -> (u8, u32) {
        let mut frame = receiver
            .receive(Duration::from_secs(3))
            .unwrap()
            .expect("a frame from the server");
        let kind = frame.header().msg_type();
        match kind {
            MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB => {
                let job: NewExtendedMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
                work.insert(
                    job.channel_id,
                    Work {
                        job: job.job_id,
                        version: job.version,
                        head: job.coinbase_tx_prefix.as_ref().to_vec(),
                        tail: job.coinbase_tx_suffix.as_ref().to_vec(),
                        path: job
                            .merkle_path
                            .iter()
                            .map(|hash| hash.as_ref().try_into().unwrap())
                            .collect(),
                    },
                );
                (kind, job.channel_id)
            }
            MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH => {
                let prev: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
                *parent = Some((
                    prev.prev_hash.as_ref().try_into().unwrap(),
                    prev.min_ntime,
                    prev.nbits,
                ));
                (kind, prev.channel_id)
            }
            MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS => {
                let opened: OpenExtendedMiningChannelSuccess =
                    binary_sv2::from_bytes(frame.payload()).unwrap();
                (kind, opened.channel_id)
            }
            MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS => {
                let success: SubmitSharesSuccess = binary_sv2::from_bytes(frame.payload()).unwrap();
                (kind, success.channel_id)
            }
            _ => (kind, 0),
        }
    }
    /// A nonce that makes a block of `work` on `channel`'s prefix.
    fn solve(work: &Work, prefix: &[u8], parent: ([u8; 32], u32, u32)) -> u32 {
        let mut coinbase = work.head.clone();
        coinbase.extend(prefix);
        coinbase.extend([0x42; 8]);
        coinbase.extend(&work.tail);
        let root = super::template::fold(super::template::double_sha256(&coinbase), &work.path);
        let mut header = work.version.to_le_bytes().to_vec();
        header.extend(parent.0);
        header.extend(root);
        header.extend(parent.1.to_le_bytes());
        header.extend(parent.2.to_le_bytes());
        let target = super::template::compact_target(parent.2).unwrap();
        (0..10_000u32)
            .find(|nonce| {
                header.truncate(76);
                header.extend(nonce.to_le_bytes());
                super::template::meets_target(&super::template::double_sha256(&header), &target)
            })
            .unwrap()
    }
    let server = Running::new(false);
    let (mut sender, mut receiver) = Session::initiate(
        TcpStream::connect(server.address).unwrap(),
        server.authority,
    )
    .unwrap()
    .split();
    sender
        .send(
            encoded(
                SetupConnection {
                    protocol: Protocol::MiningProtocol,
                    min_version: 2,
                    max_version: 2,
                    flags: 4,
                    endpoint_host: "localhost".try_into().unwrap(),
                    endpoint_port: server.address.port(),
                    vendor: "SRI translator".try_into().unwrap(),
                    hardware_version: "".try_into().unwrap(),
                    firmware: "1.0".try_into().unwrap(),
                    device_id: "".try_into().unwrap(),
                },
                0,
                false,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(
        receiver
            .receive(Duration::from_secs(3))
            .unwrap()
            .unwrap()
            .header()
            .msg_type(),
        1
    );
    let mut work = std::collections::HashMap::new();
    let mut parent = None;
    let mut channels = Vec::new();
    for request in 1..=2u32 {
        sender
            .send(
                mining(Mining::OpenExtendedMiningChannel(
                    OpenExtendedMiningChannel {
                        request_id: request,
                        user_identity: "cpu".try_into().unwrap(),
                        nominal_hash_rate: 1000.0,
                        max_target: (&[255; 32]).into(),
                        min_extranonce_size: 8,
                    },
                ))
                .unwrap(),
            )
            .unwrap();
        let mut frame = loop {
            let frame = receiver.receive(Duration::from_secs(3)).unwrap().unwrap();
            if frame.header().msg_type() == MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS {
                break frame;
            }
        };
        let opened: OpenExtendedMiningChannelSuccess =
            binary_sv2::from_bytes(frame.payload()).unwrap();
        channels.push((
            opened.channel_id,
            opened.group_channel_id,
            opened.extranonce_prefix.as_ref().to_vec(),
        ));
        // The channel's own first job and its activation.
        while read(&mut receiver, &mut work, &mut parent)
            != (MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, opened.channel_id)
        {}
    }
    let ((a, group, prefix_a), (b, also, prefix_b)) = (channels[0].clone(), channels[1].clone());
    assert_ne!(group, 0);
    assert_eq!(group, also, "one identity, one group");
    let first = parent.unwrap();
    let submit = |sender: &mut Sender, channel: u32, sequence: u32, work: &Work, nonce, ntime| {
        sender
            .send(
                mining(Mining::SubmitSharesExtended(SubmitSharesExtended {
                    channel_id: channel,
                    sequence_number: sequence,
                    job_id: work.job,
                    nonce,
                    ntime,
                    version: work.version,
                    extranonce: [0x42u8; 8].as_slice().try_into().unwrap(),
                }))
                .unwrap(),
            )
            .unwrap();
    };
    let nonce = solve(&work[&a], &prefix_a, first);
    submit(&mut sender, a, 1, &work[&a], nonce, first.1);
    while read(&mut receiver, &mut work, &mut parent) != (MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS, a) {}
    server.wait(|stats| stats.blocks_accepted == 1);
    // The next parent's job comes once, to the group.
    while read(&mut receiver, &mut work, &mut parent)
        != (MESSAGE_TYPE_MINING_SET_NEW_PREV_HASH, group)
    {}
    let next = parent.unwrap();
    assert_ne!(next.0, first.0);
    let nonce = solve(&work[&group], &prefix_b, next);
    submit(&mut sender, b, 2, &work[&group], nonce, next.1);
    while read(&mut receiver, &mut work, &mut parent) != (MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS, b) {}
    server.wait(|stats| stats.blocks_accepted == 2);
    sender.close();
}

// #### PR #42
// What: solo mining with a fallback pool, on loopback: an SV1 device mines
// on the miner's node; when the node stops answering, the local server ends
// its session and the adapter takes the device to the fallback pool
// (another server standing in for a pool); once the node answers again and
// has served for the return time (100 ms here, 30 s in use), the session at
// the pool ends and the device comes back to the node.
// Look here if: sv1::Preferred, the adapter's return or publish's marking
// change.
#[test]
fn solo_devices_move_to_the_fallback_pool_when_the_node_stops_and_return_when_it_answers() {
    let preferred = Arc::new(super::sv1::Preferred::with_return_after(
        Duration::from_millis(100),
    ));
    let node = pool_node();
    let local = Running::solo_preferred(node.clone(), preferred.clone());
    let pool = Running::new(false);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let upstreams = vec![
        super::sv1::Upstream::local(local.address, local.authority),
        super::sv1::Upstream {
            address: pool.address.to_string(),
            authority: pool.authority,
            identity: payout(),
            remote: true,
            donation: None,
            public: false,
            prefer: Some(preferred),
            fixed_version: false,
        },
    ];
    let thread = {
        let stop = stop.clone();
        let stats = Arc::new(Mutex::new(ServerStats::default()));
        thread::spawn(move || super::sv1::run(listener, upstreams, stop, stats))
    };
    let adapter = FirmwareAdapter {
        stop,
        thread: Some(thread),
        address,
    };
    // The node's server gives 16 prefix bytes and 8 to roll; a remote
    // pool, through the adapter, 4 and 4.
    let device = FirmwareDevice::connect(&adapter, true, false);
    assert_eq!(device.prefix.len(), 16, "on the node");
    node.lock().unwrap().unavailable = true;
    device.closed();
    let device = FirmwareDevice::connect(&adapter, true, false);
    assert_eq!(device.prefix.len(), 4, "at the fallback pool");
    node.lock().unwrap().unavailable = false;
    device.closed();
    let device = FirmwareDevice::connect(&adapter, true, false);
    assert_eq!(device.prefix.len(), 16, "back on the node");
    drop(device);
    drop(adapter);
    drop(local);
}

// #### PR #40
#[test]
fn at_a_remote_pool_the_donation_mines_on_its_own_channel_and_the_pool_accepts_it() {
    // Pickaxe's own server stands in for the pool. At 100% the device mines
    // only the donation channel's jobs once that channel has work.
    let pool = Running::new(false);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(ServerStats::default()));
    let upstreams = vec![super::sv1::Upstream {
        address: pool.address.to_string(),
        authority: pool.authority,
        identity: payout(),
        remote: true,
        donation: Some(super::sv1::DonationRoute {
            identity: crate::donation::bch::address(crate::config::MiningNetwork::Chipnet)
                .to_owned(),
            rate: Arc::new(std::sync::RwLock::new("100".parse().unwrap())),
            fallback: None,
        }),
        public: false,
        prefer: None,
        fixed_version: false,
    }];
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || super::sv1::run(listener, upstreams, stop, stats))
    };
    let adapter = FirmwareAdapter {
        stop,
        thread: Some(thread),
        address,
    };
    let mut device = FirmwareDevice::connect(&adapter, true, false);
    assert_eq!(device.extra_size, 4);
    // Wait for the switch: a clean job numbered with the donation bit.
    let deadline = Instant::now() + Duration::from_secs(10);
    let notify = loop {
        assert!(
            Instant::now() < deadline,
            "no donation job reached the device"
        );
        let message = device.receive();
        if message["method"] == "mining.notify"
            && message["params"][0]
                .as_str()
                .and_then(|id| id.parse::<u32>().ok())
                .is_some_and(|id| id >= 0x8000_0000)
        {
            assert_eq!(message["params"][8], true);
            break message;
        }
    };
    let (hash, submit) = device.solve_notify(0, true, &notify);
    device.send(submit);
    let ack = device.receive();
    assert_eq!(ack["result"], true);
    // The pool validated the share on the donation channel: the coinbase the
    // device built with the adapter's extranonce is the pool's own.
    pool.wait(|stats| stats.blocks_accepted == 1);
    assert_eq!(pool.node.lock().unwrap().tip, hash);
    let deadline = Instant::now() + Duration::from_secs(5);
    while stats.lock().unwrap().shares_accepted < 1 {
        assert!(Instant::now() < deadline, "pool verdict not counted");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(stats.lock().unwrap().shares_rejected, 0);
    // #### PR #48: the pool's acknowledgement counts as donation work, apart
    // from this server's own jobs.
    let work = stats.lock().unwrap().pool_work;
    assert!(work.donation > 0.0 && work.personal == 0.0 && work.fee == 0.0);
    assert_eq!(work.shares(), Some((1.0, 0.0)));
    device.write.shutdown(std::net::Shutdown::Both).unwrap();
}

// #### PR #40
#[test]
fn sv1_firmware_at_a_public_pool_is_paid_at_the_address_it_names() {
    use crate::donation::bch::{FeeMode, PoolFee};
    use crate::tx::cashaddr_to_p2pkh_locking;
    let operator = address(6);
    let server = Running::public(super::payout::PublicPool {
        fee: Some(PoolFee {
            rate: "1".parse().unwrap(),
            mode: FeeMode::Coinbase,
        }),
        address: operator.clone(),
    });
    let adapter = FirmwareAdapter::public(&server);
    // A username that is not a payout address is refused, and the device
    // may try again.
    {
        let write = TcpStream::connect(adapter.address).unwrap();
        write
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut read = std::io::BufReader::new(write.try_clone().unwrap());
        let mut write = write;
        let mut ask = |value: Value| {
            use std::io::{BufRead, Write};
            let mut bytes = serde_json::to_vec(&value).unwrap();
            bytes.push(b'\n');
            write.write_all(&bytes).unwrap();
            let mut line = String::new();
            read.read_line(&mut line).unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        };
        let subscribed = ask(json!({"id":1,"method":"mining.subscribe","params":[]}));
        assert_eq!(subscribed["result"][2], 4);
        let refused = ask(json!({"id":2,"method":"mining.authorize","params":["worker1"]}));
        assert_eq!(refused["error"][0], 24, "{refused}");
    }
    for (round, (miner, name)) in [
        (address(7), format!("{}.rig-1", address(7))),
        (address(8), address(8).split_once(':').unwrap().1.to_owned()),
    ]
    .into_iter()
    .enumerate()
    {
        let mut device = FirmwareDevice::connect_named(&adapter, true, false, &name);
        let (hash, submit) = device.solve(round as u32, true);
        device.send(submit);
        let ack = device.receive();
        assert_eq!(ack["result"], true, "{ack}");
        server.wait(|stats| stats.blocks_accepted == round as u64 + 1);
        let node = server.node.lock().unwrap();
        assert_eq!(node.tip, hash);
        let block: Block =
            consensus::deserialize(&hex::decode(node.submitted.last().unwrap()).unwrap()).unwrap();
        let outputs: Vec<Vec<u8>> = block.txdata[0]
            .output
            .iter()
            .map(|output| output.script_pubkey.to_bytes())
            .collect();
        assert_eq!(
            outputs,
            vec![
                cashaddr_to_p2pkh_locking(&miner).unwrap(),
                cashaddr_to_p2pkh_locking(crate::donation::bch::address(MiningNetwork::Chipnet))
                    .unwrap(),
                cashaddr_to_p2pkh_locking(&operator).unwrap(),
            ],
            "round {round}"
        );
        drop(node);
        device.write.shutdown(std::net::Shutdown::Both).unwrap();
    }
}

/// Opt-in and read-only: SV1 firmware reaches a real SV2 pool through the
/// adapter and receives work; no share is submitted, and the identity is the
/// address of a random, never-funded key. For example:
/// `PICKAXE_TEST_SV2_POOL=HOST:PORT PICKAXE_TEST_SV2_POOL_KEY=KEY cargo test
/// --features stratum-v2 --lib real_sv2_pool_sends_work -- --ignored --nocapture`.
/// On 2026-10-08 SoloFury's BCH SV2 endpoints (`eu-bch.solofury.com:7333`,
/// key `9c5s3n4RzRrDhzMBr3iSJsUfreSLPGiHkQyyzJjYAVWK9YWaZf7`) sent a Noise
/// certificate with version 1, which the SV2 spec requires refusing (it must
/// be 0), so this test reports "upstream certificate version is not SV2's"
/// there; their BTC endpoint's handshake succeeds.
#[test]
#[ignore = "needs a real SV2 pool"]
fn real_sv2_pool_sends_work_to_sv1_firmware() {
    use std::io::{BufRead, Write};
    let address = std::env::var("PICKAXE_TEST_SV2_POOL").expect("set PICKAXE_TEST_SV2_POOL");
    let key = std::env::var("PICKAXE_TEST_SV2_POOL_KEY").expect("set PICKAXE_TEST_SV2_POOL_KEY");
    let decoded = stratum_core::bitcoin::base58::decode_check(key.trim()).unwrap();
    assert_eq!(&decoded[..2], &[1, 0], "SV2 authority key version");
    let authority: [u8; 32] = decoded[2..].try_into().unwrap();
    let secret = secp256k1::SecretKey::from_secret_bytes(rand::random()).unwrap();
    let public = secp256k1::PublicKey::from_secret_key(&secret).serialize();
    // PICKAXE_TEST_SV2_POOL_USER overrides it, for a pool of another chain.
    let identity = std::env::var("PICKAXE_TEST_SV2_POOL_USER").unwrap_or_else(|_| {
        format!(
            "{}.pickaxe-test",
            crate::reward::p2pkh_cashaddr_from_public_key(&public).unwrap()
        )
    });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(ServerStats::default()));
    let upstream = super::sv1::Upstream {
        address,
        authority,
        identity,
        remote: true,
        donation: None,
        public: false,
        prefer: None,
        fixed_version: false,
    };
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || super::sv1::run(listener, vec![upstream], stop, stats))
    };
    let _adapter = FirmwareAdapter {
        stop,
        thread: Some(thread),
        address: local,
    };
    let mut write = TcpStream::connect(local).unwrap();
    write
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut read = std::io::BufReader::new(write.try_clone().unwrap());
    for request in [
        json!({"id":1,"method":"mining.configure","params":[["version-rolling"],{"version-rolling.mask":"1fffe000","version-rolling.min-bit-count":2}]}),
        json!({"id":2,"method":"mining.subscribe","params":["Pickaxe test firmware"]}),
        json!({"id":3,"method":"mining.authorize","params":["pickaxe-test", "x"]}),
    ] {
        let mut bytes = serde_json::to_vec(&request).unwrap();
        bytes.push(b'\n');
        write.write_all(&bytes).unwrap();
    }
    let (mut subscribed, mut authorized, mut difficulty, mut notify) = (None, None, None, None);
    while notify.is_none() {
        let mut line = String::new();
        let closed = read.read_line(&mut line).map_or(true, |read| read == 0);
        if closed {
            thread::sleep(Duration::from_millis(200));
            let reasons: Vec<_> = stats
                .lock()
                .unwrap()
                .device_stats
                .snapshots(Instant::now())
                .iter()
                .map(|row| (row.adapter_error, row.connection_error))
                .collect();
            panic!("the adapter closed the device: {reasons:?}");
        }
        let message: Value = serde_json::from_str(&line).unwrap();
        match (message["id"].as_u64(), message["method"].as_str()) {
            (Some(2), _) => subscribed = Some(message),
            (Some(3), _) => authorized = Some(message),
            (_, Some("mining.set_difficulty")) => difficulty = Some(message),
            (_, Some("mining.notify")) => notify = Some(message),
            _ => (),
        }
    }
    let subscribed = subscribed.expect("subscribe answered");
    assert_eq!(authorized.expect("authorize answered")["result"], true);
    let difficulty = difficulty.expect("difficulty before work")["params"][0]
        .as_f64()
        .unwrap();
    let notify = notify.unwrap();
    println!(
        "extranonce1 {} bytes, extranonce2 {} bytes, difficulty {difficulty}, job {}, clean {}",
        subscribed["result"][1].as_str().unwrap().len() / 2,
        subscribed["result"][2],
        notify["params"][0],
        notify["params"][8]
    );
    assert!(difficulty > 0.0);
    write.shutdown(std::net::Shutdown::Both).unwrap();
}

#[test]
fn a_device_whose_pools_all_fail_still_shows_the_reason() {
    let pool = Running::new(false);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(ServerStats::default()));
    // The only pool pins a valid key the pool does not hold, then a value
    // that is no key at all.
    let upstreams = vec![
        super::sv1::Upstream {
            address: pool.address.to_string(),
            authority: [3; 32],
            identity: payout(),
            remote: true,
            donation: None,
            public: false,
            prefer: None,
            fixed_version: false,
        },
        super::sv1::Upstream {
            address: pool.address.to_string(),
            authority: server::authority_public(&[18; 32]).unwrap(),
            identity: payout(),
            remote: true,
            donation: None,
            public: false,
            prefer: None,
            fixed_version: false,
        },
    ];
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || super::sv1::run(listener, upstreams, stop, stats))
    };
    let _adapter = FirmwareAdapter {
        stop,
        thread: Some(thread),
        address,
    };
    let device = TcpStream::connect(address).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rows = stats.lock().unwrap().device_stats.snapshots(Instant::now());
        if let Some(row) = rows.first() {
            assert!(!row.connected);
            assert_eq!(row.adapter_error, Some("authentication or framing failed"));
            break;
        }
        assert!(Instant::now() < deadline, "no row for the device");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(stats.lock().unwrap().sv1_connection_errors, 1);
    drop(device);
}

// #### PR #42
// What: a scripted Job Declaration client on loopback opens a JD session and
// a mining session on the pool's one SV2 port and key, allocates a token,
// sets a custom job paying [miner 304,734,375, fee 3,078,125, donation
// 4,687,500] and mines it: the share is accepted, the block is listed as
// submitted by the miner's node (the pool's node gets nothing), and the JD
// session is no device on the workers page.
// Look here if: serve_declarator, the SetCustomMiningJob arm or custom
// blocks change.
#[test]
fn a_scripted_jd_client_sets_a_custom_job_and_mines_it() {
    use stratum_core::job_declaration_sv2::{
        AllocateMiningJobToken, AllocateMiningJobTokenSuccess,
        MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
    };
    let fee_address = address(0x34);
    let server = Running::jd_pool(super::payout::PublicPool {
        fee: Some(crate::donation::bch::PoolFee {
            rate: "1".parse().unwrap(),
            mode: crate::donation::bch::FeeMode::Work,
        }),
        address: fee_address,
    });
    let miner = payout();
    let setup = |protocol, flags| {
        encoded(
            SetupConnection {
                protocol,
                min_version: 2,
                max_version: 2,
                flags,
                endpoint_host: "localhost".try_into().unwrap(),
                endpoint_port: server.address.port(),
                vendor: "scripted JD client".try_into().unwrap(),
                hardware_version: "".try_into().unwrap(),
                firmware: "".try_into().unwrap(),
                device_id: "".try_into().unwrap(),
            },
            0,
            false,
        )
        .unwrap()
    };
    let connect = || {
        Session::initiate(
            TcpStream::connect(server.address).unwrap(),
            server.authority,
        )
        .unwrap()
        .split()
    };
    let (mut jd, mut jd_replies) = connect();
    jd.send(setup(Protocol::JobDeclarationProtocol, 0)).unwrap();
    let reply = jd_replies.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(reply.header().msg_type(), 1);
    jd.send(
        encoded(
            AllocateMiningJobToken {
                user_identifier: miner.as_str().try_into().unwrap(),
                request_id: 1,
            },
            MESSAGE_TYPE_ALLOCATE_MINING_JOB_TOKEN,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let mut reply = jd_replies.receive(Duration::from_secs(3)).unwrap().unwrap();
    let allocated: AllocateMiningJobTokenSuccess = binary_sv2::from_bytes(reply.payload()).unwrap();
    let token = allocated.mining_job_token.as_ref().to_vec();
    let scripts: Vec<Vec<u8>> =
        super::jd::codec::parse_outputs(allocated.coinbase_outputs.as_ref())
            .unwrap()
            .into_iter()
            .map(|(_, script)| script)
            .collect();
    let outputs = super::jd::codec::serialize_outputs(
        &[304_734_375, 3_078_125, 4_687_500]
            .into_iter()
            .zip(scripts.clone())
            .collect::<Vec<_>>(),
    );
    let (mut mining_link, mut replies) = connect();
    mining_link
        .send(setup(Protocol::MiningProtocol, 0b110))
        .unwrap();
    let reply = replies.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(reply.header().msg_type(), 1);
    mining_link
        .send(
            mining(Mining::OpenExtendedMiningChannel(
                OpenExtendedMiningChannel {
                    request_id: 1,
                    user_identity: miner.as_str().try_into().unwrap(),
                    nominal_hash_rate: 1000.0,
                    max_target: (&[255; 32]).into(),
                    min_extranonce_size: 16,
                },
            ))
            .unwrap(),
        )
        .unwrap();
    let mut reply = replies.receive(Duration::from_secs(3)).unwrap().unwrap();
    let opened: OpenExtendedMiningChannelSuccess = binary_sv2::from_bytes(reply.payload()).unwrap();
    assert_eq!(opened.extranonce_size, 16);
    let (channel, channel_prefix) = (
        opened.channel_id,
        opened.extranonce_prefix.as_ref().to_vec(),
    );
    let time = now();
    let mut prefix = vec![0x03, 0x15, 0xf9, 0x04];
    prefix.extend(b"jd");
    mining_link
        .send(
            mining(Mining::SetCustomMiningJob(SetCustomMiningJob {
                channel_id: channel,
                request_id: 2,
                token: token.as_slice().try_into().unwrap(),
                version: 0x2000_0000,
                prev_hash: (&[0xab; 32]).into(),
                min_ntime: time,
                nbits: 0x207f_ffff,
                coinbase_tx_version: 2,
                coinbase_prefix: prefix.as_slice().try_into().unwrap(),
                coinbase_tx_input_n_sequence: u32::MAX,
                coinbase_tx_outputs: outputs.as_slice().try_into().unwrap(),
                coinbase_tx_locktime: 0,
                merkle_path: Vec::<binary_sv2::U256>::new().try_into().unwrap(),
            }))
            .unwrap(),
        )
        .unwrap();
    // Any frame before the answer (a SetTarget) is skipped.
    let job_id = loop {
        let mut reply = replies.receive(Duration::from_secs(3)).unwrap().unwrap();
        if reply.header().msg_type() == MESSAGE_TYPE_SET_CUSTOM_MINING_JOB_SUCCESS {
            let success: SetCustomMiningJobSuccess =
                binary_sv2::from_bytes(reply.payload()).unwrap();
            break success.job_id;
        }
        assert_ne!(
            reply.header().msg_type(),
            MESSAGE_TYPE_SET_CUSTOM_MINING_JOB_ERROR,
            "the custom job was refused"
        );
    };
    assert_eq!(job_id, 0x8000_0001);
    let extranonce = [0x42u8; 16];
    let mut coinbase = 2u32.to_le_bytes().to_vec();
    coinbase.push(1);
    coinbase.extend([0; 32]);
    coinbase.extend(u32::MAX.to_le_bytes());
    coinbase.push((prefix.len() + 32) as u8);
    coinbase.extend(&prefix);
    coinbase.extend(&channel_prefix);
    coinbase.extend(extranonce);
    coinbase.extend(u32::MAX.to_le_bytes());
    coinbase.extend(&outputs);
    coinbase.extend(0u32.to_le_bytes());
    let mut bytes = 0x2000_0000u32.to_le_bytes().to_vec();
    bytes.extend([0xab; 32]);
    bytes.extend(sha256d::Hash::hash(&coinbase).to_byte_array());
    bytes.extend(time.to_le_bytes());
    bytes.extend(0x207f_ffffu32.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    let mut header: Header = consensus::deserialize(&bytes).unwrap();
    let nonce = (0..10_000)
        .find(|nonce| {
            header.nonce = *nonce;
            header.validate_pow(header.target()).is_ok()
        })
        .unwrap();
    mining_link
        .send(
            mining(Mining::SubmitSharesExtended(SubmitSharesExtended {
                channel_id: channel,
                sequence_number: 1,
                job_id,
                nonce,
                ntime: time,
                version: 0x2000_0000,
                extranonce: extranonce.as_slice().try_into().unwrap(),
            }))
            .unwrap(),
        )
        .unwrap();
    let accepted = loop {
        let reply = replies.receive(Duration::from_secs(3)).unwrap().unwrap();
        let kind = reply.header().msg_type();
        if kind == MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS || kind == MESSAGE_TYPE_SUBMIT_SHARES_ERROR {
            break kind;
        }
    };
    assert_eq!(accepted, MESSAGE_TYPE_SUBMIT_SHARES_SUCCESS);
    server.wait(|stats| {
        stats.jd_server.as_ref().is_some_and(|jd| {
            jd.clients == 1 && jd.tokens == 1 && jd.custom_jobs == 1 && jd.blocks == 1
        })
    });
    let stats = server.stats.lock().unwrap().clone();
    let found = stats.recent_blocks.back().unwrap();
    assert_eq!(found.result, Some("submitted by the miner's node"));
    assert_eq!(found.hash, header.block_hash().to_string());
    assert_eq!(stats.connections, 1, "the JD session is not a device");
    assert_eq!(stats.device_stats.snapshots(Instant::now()).len(), 1);
    assert_eq!(server.node.lock().unwrap().submissions, 0);
    // #### PR #48: a custom job's share is no one's work here.
    assert_eq!(stats.work, super::server::WorkSplit::default());
    drop(jd);
    drop(jd_replies);
    server.wait(|stats| stats.jd_server.as_ref().is_some_and(|jd| jd.clients == 0));
}

// #### PR #42
// What: a pool that does not accept Job Declaration answers a JD
// SetupConnection with unsupported-protocol, as before.
// Look here if: the JD dispatch in serve_device changes.
#[test]
fn jd_is_refused_when_the_pool_does_not_accept_it() {
    let server = Running::new(false);
    let (mut link, mut replies) = Session::initiate(
        TcpStream::connect(server.address).unwrap(),
        server.authority,
    )
    .unwrap()
    .split();
    link.send(
        encoded(
            SetupConnection {
                protocol: Protocol::JobDeclarationProtocol,
                min_version: 2,
                max_version: 2,
                flags: 0,
                endpoint_host: "localhost".try_into().unwrap(),
                endpoint_port: server.address.port(),
                vendor: "scripted JD client".try_into().unwrap(),
                hardware_version: "".try_into().unwrap(),
                firmware: "".try_into().unwrap(),
                device_id: "".try_into().unwrap(),
            },
            0,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let mut reply = replies.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(reply.header().msg_type(), 2);
    let error: stratum_core::common_messages_sv2::SetupConnectionError =
        binary_sv2::from_bytes(reply.payload()).unwrap();
    assert_eq!(error.error_code.as_ref(), b"unsupported-protocol");
}

// #### PR #42
// What: a Pickaxe Job Declaration client (a local server on the miner's node
// and an uplink) mines at a Pickaxe pool that accepts Coinbase-only Job
// Declaration, on loopback: an SV2 device mines the local server's job,
// whose coinbase pays [miner 304,734,375, fee 3,078,125, donation
// 4,687,500]; the share reaches the pool on the declared custom job and is
// accepted there; the block goes to the miner's node only, and the pool
// lists it as submitted by the miner's node.
// Look here if: the uplink, the nested layout or the plan-gated publishing
// change.
#[test]
fn coinbase_only_pickaxe_jdc_mines_at_a_pickaxe_jds_on_loopback() {
    let pool = Running::jd_pool(super::payout::PublicPool {
        fee: Some(crate::donation::bch::PoolFee {
            rate: "1".parse().unwrap(),
            mode: crate::donation::bch::FeeMode::Work,
        }),
        address: address(0x34),
    });
    let miner_node = pool_node();
    let client = Running::jd_client(miner_node.clone(), &pool, super::jd::JdMode::CoinbaseOnly);
    client.wait(|stats| {
        stats
            .jd_client
            .as_ref()
            .is_some_and(|jd| jd.state == "active")
    });
    let mut device = Device::connect(&client, true);
    assert_eq!(device.prefix.len(), 4, "the local channel's lane");
    device.solve_and_submit(0);
    client.wait(|stats| stats.blocks_accepted == 1);
    let submitted = miner_node.lock().unwrap().submitted.clone();
    assert_eq!(submitted.len(), 1);
    assert_eq!(pool.node.lock().unwrap().submissions, 0);
    let block: Block = consensus::deserialize(&hex::decode(&submitted[0]).unwrap()).unwrap();
    let outputs: Vec<(u64, Vec<u8>)> = block.txdata[0]
        .output
        .iter()
        .map(|output| (output.value.to_sat(), output.script_pubkey.to_bytes()))
        .collect();
    let p2pkh = |hash: [u8; 20]| {
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend(hash);
        script.extend([0x88, 0xac]);
        script
    };
    let donation =
        crate::tx::cashaddr_to_p2pkh_locking(crate::donation::bch::address(MiningNetwork::Chipnet))
            .unwrap();
    assert_eq!(
        outputs,
        vec![
            (304_734_375, p2pkh([0x12; 20])),
            (
                3_078_125,
                crate::tx::cashaddr_to_coinbase_locking(&address(0x34)).unwrap()
            ),
            (4_687_500, donation),
        ]
    );
    pool.wait(|stats| {
        stats
            .jd_server
            .as_ref()
            .is_some_and(|jd| jd.custom_jobs >= 1 && jd.blocks >= 1)
            && stats.shares_accepted >= 1
    });
    client.wait(|stats| {
        stats
            .jd_client
            .as_ref()
            .is_some_and(|jd| jd.forwarded >= 1 && jd.accepted >= 1)
    });
    let found = pool
        .stats
        .lock()
        .unwrap()
        .recent_blocks
        .back()
        .cloned()
        .unwrap();
    assert_eq!(found.result, Some("submitted by the miner's node"));
    let summary = format!("{:?}", client.stats.lock().unwrap().jd_client);
    assert!(!summary.contains("bchtest"), "{summary}");
    device.sender.close();
}

// #### PR #56
// What: a pool granting only 8 rollable bytes, as BCH P2Pool does, refuses
// the client's 16; the client asks again for 8 and mines in the compact
// layout. Its local device rolls 4 bytes on a 2-byte lane, the device's
// block reaches the miner's node, and the forwarded share is accepted on
// the pool's custom job.
// Look here if: the compact layout or the uplink's channel request changes.
#[test]
fn a_pool_granting_8_bytes_gets_the_compact_layout() {
    let pool = Running::jd_pool(super::payout::PublicPool {
        fee: Some(crate::donation::bch::PoolFee {
            rate: "1".parse().unwrap(),
            mode: crate::donation::bch::FeeMode::Work,
        }),
        address: address(0x34),
    });
    pool.declarator.as_ref().unwrap().set_grant(8);
    let miner_node = pool_node();
    let client = Running::jd_client(miner_node.clone(), &pool, super::jd::JdMode::CoinbaseOnly);
    client.wait(|stats| {
        stats
            .jd_client
            .as_ref()
            .is_some_and(|jd| jd.state == "active" && jd.custom_jobs >= 1)
    });
    let mut device = Device::connect_rolling(&client, true, "cpu", 4);
    assert_eq!((device.prefix.len(), device.rolled), (2, 4));
    device.solve_and_submit(0);
    client.wait(|stats| stats.blocks_accepted == 1);
    assert_eq!(miner_node.lock().unwrap().submitted.len(), 1);
    pool.wait(|stats| {
        stats
            .jd_server
            .as_ref()
            .is_some_and(|jd| jd.custom_jobs >= 1 && jd.blocks >= 1)
            && stats.shares_accepted >= 1
    });
    client.wait(|stats| {
        stats
            .jd_client
            .as_ref()
            .is_some_and(|jd| jd.forwarded >= 1 && jd.accepted >= 1 && jd.rejected == 0)
    });
    device.sender.close();
}

// #### PR #42
// What: a Pickaxe Job Declaration client (Full-Template) mines at a Pickaxe
// pool that accepts both modes, on loopback. The pool's node has 2 of the
// client's 3 transactions: exactly one ProvideMissingTransactions round
// runs, validateblocktemplate is called once, the custom job is set on the
// declared token, and the device's block reaches both nodes with the same
// hash: the client's node from its own journal, the pool's from the JD
// journal (the forwarded share or PushSolution).
// Look here if: the Full-Template flow on either side changes.
#[test]
fn full_template_declares_provides_a_missing_transaction_and_both_nodes_get_the_block() {
    use super::template_tests::transaction;
    let mut txs: Vec<Value> = (1..=3).map(transaction).collect();
    txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
    let third = transaction(3);
    let pool_node = pool_node();
    {
        let mut node = pool_node.lock().unwrap();
        node.transactions = txs
            .iter()
            .filter(|tx| tx["txid"] != third["txid"])
            .cloned()
            .collect();
        node.spendable = vec![third];
    }
    let pool = Running::jd_pool_full(
        pool_node.clone(),
        super::payout::PublicPool {
            fee: Some(crate::donation::bch::PoolFee {
                rate: "1".parse().unwrap(),
                mode: crate::donation::bch::FeeMode::Work,
            }),
            address: address(0x34),
        },
    );
    let miner_node = super::server_tests::pool_node();
    miner_node.lock().unwrap().transactions = txs;
    let client = Running::jd_client(miner_node.clone(), &pool, super::jd::JdMode::FullTemplate);
    pool.wait(|stats| {
        stats
            .jd_server
            .as_ref()
            .is_some_and(|jd| jd.custom_jobs >= 1)
    });
    let jd = pool.stats.lock().unwrap().jd_server.clone().unwrap();
    assert_eq!((jd.missing_rounds, jd.validations), (1, 1));
    assert_eq!(jd.validator, Some("validateblocktemplate"));
    assert_eq!(pool_node.lock().unwrap().validations, 1);
    let mut device = Device::connect(&client, true);
    device.solve_and_submit(0);
    client.wait(|stats| stats.blocks_accepted == 1);
    pool.wait(|stats| {
        stats
            .jd_server
            .as_ref()
            .is_some_and(|jd| jd.declared_accepted == 1)
    });
    let hash = |node: &Arc<Mutex<Node>>| {
        let submitted = node.lock().unwrap().submitted[0].clone();
        let block: Block = consensus::deserialize(&hex::decode(submitted).unwrap()).unwrap();
        assert_eq!(block.txdata.len(), 4);
        block.block_hash()
    };
    assert_eq!(hash(&miner_node), hash(&pool_node));
    client.wait(|stats| {
        stats.jd_client.as_ref().is_some_and(|jd| {
            jd.mode == "full-template" && jd.provided == 1 && jd.declared >= 1 && jd.pushed == 1
        })
    });
    let found = pool
        .stats
        .lock()
        .unwrap()
        .recent_blocks
        .back()
        .cloned()
        .unwrap();
    assert_eq!(found.result, Some("accepted"));
    device.sender.close();
}

// #### PR #42
// What: a Job Declaration client merge-mines the Chipnet test token at a
// Pickaxe pool, in both modes: the pool accepts the custom job (and the
// declaration) with the commitment as output 0 and the ticket after its
// outputs; the device's share wins the token in both cases at the client,
// which proves them; the block carries the commitment and reaches the
// client's node and, Full-Template, the pool's.
// Look here if: declared_aux, the custom job's outputs or the pool's
// commitment rule change.
#[test]
fn merge_mined_tokens_ride_job_declaration_to_a_pickaxe_pool() {
    use super::merge::{commitment::AuxCommitment, hub::TokenHub, registry::is_ticket_script};
    for mode in [
        super::jd::JdMode::CoinbaseOnly,
        super::jd::JdMode::FullTemplate,
    ] {
        let public = super::payout::PublicPool {
            fee: Some(crate::donation::bch::PoolFee {
                rate: "1".parse().unwrap(),
                mode: crate::donation::bch::FeeMode::Work,
            }),
            address: address(0x34),
        };
        let pool_side = pool_node();
        let pool = match mode {
            super::jd::JdMode::CoinbaseOnly => Running::jd_pool(public),
            super::jd::JdMode::FullTemplate => Running::jd_pool_full(pool_side.clone(), public),
        };
        let directory = Arc::new(TestDirectory::new());
        // The node's own target, so every share wins the Case A state.
        let hub = Arc::new(
            TokenHub::test_token(
                MiningNetwork::Chipnet,
                directory.0.join("token-proofs.json"),
                0x207f_ffff,
            )
            .unwrap(),
        );
        let miner_node = pool_node();
        let client = Running::jd_client_merged(miner_node.clone(), &pool, mode, hub, directory);
        pool.wait(|stats| {
            stats
                .jd_server
                .as_ref()
                .is_some_and(|jd| jd.custom_jobs >= 1)
        });
        let mut device = Device::connect(&client, true);
        device.solve_and_submit(0);
        client.wait(|stats| stats.blocks_accepted == 1);
        client.wait(|stats| stats.recent_token_wins.len() >= 2);
        let stats = client.stats.lock().unwrap().clone();
        assert!(
            stats.tokens_off.is_none(),
            "{mode:?}: {:?}",
            stats.tokens_off
        );
        let mut modes: Vec<char> = stats.recent_token_wins.iter().map(|win| win.mode).collect();
        modes.sort_unstable();
        assert_eq!(modes, ['A', 'B'], "{mode:?}");
        let submitted = miner_node.lock().unwrap().submitted[0].clone();
        let block: Block = consensus::deserialize(&hex::decode(&submitted).unwrap()).unwrap();
        let outputs: Vec<(u64, Vec<u8>)> = block.txdata[0]
            .output
            .iter()
            .map(|output| (output.value.to_sat(), output.script_pubkey.to_bytes()))
            .collect();
        assert_eq!(outputs.len(), 5, "{mode:?}");
        assert_eq!(outputs[0].0, 0);
        assert!(AuxCommitment::parse_script(&outputs[0].1).is_some());
        assert_eq!(
            outputs[1..4]
                .iter()
                .map(|(value, _)| *value)
                .collect::<Vec<_>>(),
            [304_734_375, 3_078_125, 4_687_500]
        );
        assert_eq!(outputs[4].0, 0);
        assert!(is_ticket_script(&outputs[4].1));
        pool.wait(|stats| {
            stats.shares_accepted >= 1 && stats.jd_server.as_ref().is_some_and(|jd| jd.blocks >= 1)
        });
        if mode == super::jd::JdMode::FullTemplate {
            pool.wait(|stats| {
                stats
                    .jd_server
                    .as_ref()
                    .is_some_and(|jd| jd.declared_accepted == 1)
            });
            assert_eq!(pool_side.lock().unwrap().submitted[0], submitted);
        }
        device.sender.close();
    }
}

// #### PR #42
// What: when the pool refuses Job Declaration (here it does not accept it),
// the client's local server publishes no work and the uplink counts a
// fallback with the pool's reason, so devices go to the pool's own jobs.
// Look here if: the uplink's fallback or the plan gate changes.
#[test]
fn a_pool_without_job_declaration_leaves_the_local_server_without_work() {
    let pool = Running::new(false);
    let miner_node = pool_node();
    let stop = Arc::new(AtomicBool::new(false));
    let (handle, thread) = super::jd::client::spawn(
        vec![super::jd::client::JdTarget {
            p2pool_prepare_api: None,
            required: false,
            address: pool.address.to_string(),
            authority: pool.authority,
            identity: payout(),
            retry: Duration::from_millis(200),
            mode: super::jd::JdMode::FullTemplate,
        }],
        stop.clone(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let summary = handle.status.summary.lock().unwrap().clone();
        if summary.fallbacks >= 1 {
            assert!(
                summary
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("unsupported-protocol")),
                "{summary:?}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "no fallback");
        thread::sleep(Duration::from_millis(20));
    }
    assert!(handle.status.plan.read().unwrap().is_none());
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap();
    drop(miner_node);
}

// #### PR #42
// What: Pickaxe serves Pickaxe: a server takes its templates from another
// Pickaxe's template server (key pinned), a CPU device mines a block on
// them, and the block reaches the provider's node (relayed from
// SubmitSolution) and the client's own node (the whole-block fallback); the
// dashboard names the template provider.
// Look here if: TdpSource, the source failover or the whole-block fallback
// change.
#[test]
fn pickaxe_serves_pickaxe_and_a_block_reaches_both_nodes() {
    let provider = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let key = {
        let mut encoded = vec![1, 0];
        encoded.extend(provider.authority);
        stratum_core::bitcoin::base58::encode_check(&encoded)
    };
    let address = super::tdp::client::TdpAddress::parse(&format!(
        "sv2tp://{}/{key}",
        provider.templates.unwrap()
    ))
    .unwrap();
    let client_node = pool_node();
    let client = Running::tdp_client(client_node.clone(), address);
    let stats = client.stats.lock().unwrap().clone();
    assert_eq!(
        stats.template_source,
        Some(super::provider::SourceKind::TemplateProvider)
    );
    assert_eq!((stats.active_node, stats.nodes), (0, 2));
    let mut device = Device::connect(&client, true);
    device.solve_and_submit(0);
    client.wait(|stats| stats.blocks_accepted == 1);
    provider.wait(|stats| {
        stats
            .template_server
            .as_ref()
            .is_some_and(|templates| templates.relay_accepted == 1)
    });
    assert_eq!(client_node.lock().unwrap().submissions, 1);
    assert_eq!(provider.node.lock().unwrap().submissions, 1);
    device.sender.close();
}

// #### PR #42
// What: a provider on this computer may be used without its key (the
// session is refused for any other address), and its templates mine as a
// pinned one's do.
// Look here if: TdpAddress::parse or Session::initiate_unpinned changes.
#[test]
fn an_unpinned_provider_on_this_computer_serves_templates() {
    let provider = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let address =
        super::tdp::client::TdpAddress::parse(&format!("sv2tp://{}", provider.templates.unwrap()))
            .unwrap();
    assert!(address.authority.is_none());
    let client = Running::tdp_client(pool_node(), address);
    let mut device = Device::connect(&client, false);
    device.solve_and_submit(0);
    client.wait(|stats| stats.blocks_accepted == 1);
    device.sender.close();
}

// #### PR #48
// What: on Chipnet, a device gets the job on the limit's bits a few seconds
// after the parent's time plus 1200 s, not at the next 15-second refresh:
// from this server's node (the minimum-difficulty refresh), and through
// another Pickaxe's template server (which announces the new bits with a
// SetNewPrevHash on the same parent). Its first job is on normal bits.
// Why: until then every block a device finds is refused by the node
// (bad-diffbits), so Pickaxe sends none (see `BchTemplate::stale_bits`).
// Look here if: min_difficulty_due, the template server's same-parent bits
// or the fixture node's bits change.
#[test]
fn devices_get_the_minimum_difficulty_job_at_the_twenty_minute_point() {
    let node = pool_node();
    let start = Instant::now();
    // The point (the parent's time plus 1201 s) comes 4 to 5 s from now.
    node.lock().unwrap().parent_time = Some(now() - 1196);
    let provider = Running::templates(node, Arc::new(TestDirectory::new()));
    let key = {
        let mut encoded = vec![1, 0];
        encoded.extend(provider.authority);
        stratum_core::bitcoin::base58::encode_check(&encoded)
    };
    let address = super::tdp::client::TdpAddress::parse(&format!(
        "sv2tp://{}/{key}",
        provider.templates.unwrap()
    ))
    .unwrap();
    let client = Running::tdp_client(pool_node(), address);
    let mut direct = Device::connect(&provider, false);
    let mut relayed = Device::connect(&client, true);
    // Before the next 15-second refresh (the first was at `start`).
    let deadline = start + Duration::from_secs(12);
    let limit = super::template::MIN_DIFFICULTY_BITS;
    for device in [&mut direct, &mut relayed] {
        let seen = device.bits_until(limit, deadline);
        assert_eq!(seen.first(), Some(&0x207f_ffff), "{seen:x?}");
    }
    direct.sender.close();
    relayed.sender.close();
}
