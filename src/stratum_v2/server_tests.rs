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

struct Node {
    height: u32,
    tip: String,
    reject: bool,
    submissions: usize,
    lose_replies: bool,
    submitted: Vec<String>,
    known: std::collections::HashSet<String>,
    unavailable: bool,
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
            "getblocktemplate" => {
                assert!(params[0].get("rules").is_none());
                let mut template = rpc_template();
                template["height"] = json!(node.height + 1);
                template["previousblockhash"] = json!(node.tip);
                template["curtime"] = json!(now());
                template["mintime"] = json!(now() - 1);
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
                assert_eq!(block.txdata.len(), 1);
                let coinbase = &block.txdata[0];
                assert!(coinbase.is_coinbase());
                let mut expected = vec![0x76, 0xa9, 0x14];
                expected.extend([0x12; 20]);
                expected.extend([0x88, 0xac]);
                let donor = crate::tx::cashaddr_to_p2pkh_locking(crate::donation::bch::address(
                    MiningNetwork::Chipnet,
                ))
                .unwrap();
                if coinbase.output.len() == 1 {
                    assert_eq!(coinbase.output[0].value.to_sat(), 312_500_000);
                    assert!(coinbase.output[0].script_pubkey.as_bytes() == donor);
                } else {
                    assert_eq!(coinbase.output.len(), 2);
                    assert_eq!(coinbase.output[0].value.to_sat(), 309_375_000);
                    assert_eq!(coinbase.output[1].value.to_sat(), 3_125_000);
                    assert!(coinbase.output[0].script_pubkey.as_bytes() == expected);
                    assert!(coinbase.output[1].script_pubkey.as_bytes() == donor);
                }
                assert!(coinbase.input[0].witness.is_empty());
                assert!((100..=200).contains(&consensus::serialize(coinbase).len()));
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
            "getblockheader" => {
                let hash = params[0].as_str().unwrap();
                if !node.lose_replies && node.known.contains(hash) {
                    Ok(json!({"hash":hash,"confirmations":1}))
                } else {
                    Err("header temporarily unavailable".into())
                }
            }
            _ => panic!("unexpected RPC method"),
        }
    }
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), String>>>,
    stats: Arc<Mutex<ServerStats>>,
    node: Arc<Mutex<Node>>,
    address: SocketAddr,
    authority: [u8; 32],
    state_directory: Arc<TestDirectory>,
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
        }));
        Self::start(node, Arc::new(TestDirectory::new()))
    }

    fn start(node: Arc<Mutex<Node>>, state_directory: Arc<TestDirectory>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(ServerStats::default()));
        let secret = [17; 32];
        let authority = server::authority_public(&secret).unwrap();
        let config = ServerConfig {
            donation: Arc::new(std::sync::RwLock::new(Default::default())),
            allocation_phase: Some(300_000_000_000),
            network: MiningNetwork::Chipnet,
            payout: payout(),
            authority_secret: secret,
            share_target: [255; 32],
            journal_path: state_directory.journal(),
            source_identity: [42; 32],
        };
        let thread = {
            let stop = stop.clone();
            let stats = stats.clone();
            let rpc = Rpc(node.clone());
            thread::spawn(move || server::run(listener, rpc, config, stop, stats))
        };
        let running = Self {
            stop,
            thread: Some(thread),
            stats,
            node,
            address,
            authority,
            state_directory,
        };
        running.wait(|stats| stats.template_ready);
        running
    }

    fn wait(&self, condition: impl Fn(&ServerStats) -> bool) {
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
    }
}

fn now() -> u32 {
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
}

impl Device {
    fn connect(server: &Running, extended: bool) -> Self {
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
                user_identity: "cpu".try_into().unwrap(),
                nominal_hash_rate: 1000.0,
                max_target: (&maximum).into(),
                min_extranonce_size: 8,
            })
        } else {
            Mining::OpenStandardMiningChannel(OpenStandardMiningChannel {
                request_id: 1,
                user_identity: "cpu".try_into().unwrap(),
                nominal_hash_rate: 1000.0,
                max_target: (&maximum).into(),
            })
        };
        sender.send(mining(open).unwrap()).unwrap();
        let mut response = receiver.receive(Duration::from_secs(3)).unwrap().unwrap();
        let (id, prefix) = if extended {
            assert_eq!(
                response.header().msg_type(),
                MESSAGE_TYPE_OPEN_EXTENDED_MINING_CHANNEL_SUCCESS
            );
            let opened: OpenExtendedMiningChannelSuccess =
                binary_sv2::from_bytes(response.payload()).unwrap();
            assert_eq!(opened.extranonce_size, 8);
            (
                opened.channel_id,
                opened.extranonce_prefix.as_ref().to_vec(),
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
            )
        };
        Self {
            sender,
            receiver,
            id,
            prefix,
            extended,
        }
    }

    fn receive(&mut self) -> SerializedFrame {
        self.receiver
            .receive(Duration::from_secs(3))
            .unwrap()
            .unwrap()
    }

    fn solve_and_submit(&mut self, sequence: u32) -> String {
        let mut frame = self.receive();
        let extra = [sequence as u8; 8];
        let (job_id, version, root) = if self.extended {
            assert_eq!(
                frame.header().msg_type(),
                MESSAGE_TYPE_NEW_EXTENDED_MINING_JOB
            );
            let job: NewExtendedMiningJob = binary_sv2::from_bytes(frame.payload()).unwrap();
            assert!(job.version_rolling_allowed);
            let mut coinbase = job.coinbase_tx_prefix.as_ref().to_vec();
            coinbase.extend(&self.prefix);
            coinbase.extend(extra);
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

// #### PR #38
// Run an independently built, unmodified SRI mining_device against synthetic
// BCH templates. No live node, user payout or GPU is accessed by this test.
#[test]
#[ignore = "requires PICKAXE_SV2_REFERENCE_DEVICE built from the pinned SRI source"]
fn upstream_reference_device_authenticates_and_mines_successor_blocks() {
    use std::process::{Child, Command, Stdio};

    struct Reference(Child);
    impl Drop for Reference {
        fn drop(&mut self) {
            // Only this test's child process; never a running user miner.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let executable = std::env::var_os("PICKAXE_SV2_REFERENCE_DEVICE")
        .expect("set PICKAXE_SV2_REFERENCE_DEVICE to the unmodified SRI mining_device binary");
    let server = Running::new(false);
    let start = |public: [u8; 32], name: &str| {
        let mut encoded = vec![1, 0];
        encoded.extend(public);
        let authority = stratum_core::bitcoin::base58::encode_check(&encoded);
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
                "reference experiment failed: exited={exited:?}, setup={}, channel={}, stats={snapshot:?}",
                log.contains("SetupConnectionSuccess"),
                log.contains("channel opened"),
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    // The RPC fixture independently deserializes and checks PoW, merkle, BCH
    // coinbase serialization, configured payout and successor parent linkage.
    assert!(server.node.lock().unwrap().known.len() >= 2);
    drop(valid);
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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = server.stop.clone();
        let upstream = server.address;
        let authority = server.authority;
        let thread = {
            let stop = stop.clone();
            let stats = server.stats.clone();
            thread::spawn(move || {
                super::sv1::run(
                    listener,
                    super::sv1::Upstream::local(upstream, authority),
                    stop,
                    stats,
                )
            })
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
    clean: bool,
}
impl FirmwareDevice {
    fn connect(adapter: &FirmwareAdapter, rolling: bool, authorize_first: bool) -> Self {
        let write = TcpStream::connect(adapter.address).unwrap();
        write
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let read = std::io::BufReader::new(write.try_clone().unwrap());
        let mut device = Self {
            write,
            read,
            prefix: Vec::new(),
            clean: false,
        };
        if rolling {
            device.send(json!({"id":1,"method":"mining.configure","params":[["version-rolling"],{"version-rolling.mask":"1fffe000","version-rolling.min-bit-count":2}]}));
            let configured = device.receive();
            assert_eq!(configured["result"]["version-rolling"], true);
            assert_eq!(configured["result"]["version-rolling.mask"], "1fffe000");
        }
        let authorize = json!({"id":3,"method":"mining.authorize","params":[payout()]});
        if authorize_first {
            device.send(authorize.clone());
            assert_eq!(device.receive()["result"], true);
        }
        device
            .send(json!({"id":2,"method":"mining.subscribe","params":["CPU firmware experiment"]}));
        let subscribed = device.receive();
        assert_eq!(subscribed["result"][2], 8);
        device.prefix = hex::decode(subscribed["result"][1].as_str().unwrap()).unwrap();
        assert_eq!(device.prefix.len(), 16);
        if !authorize_first {
            device.send(authorize);
            assert_eq!(device.receive()["result"], true);
        }
        device
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
        assert_eq!(notify["method"], "mining.notify");
        let fields = notify["params"].as_array().unwrap();
        self.clean = fields[8].as_bool().unwrap();
        let extra = [id as u8; 8];
        let mut coinbase = hex::decode(fields[2].as_str().unwrap()).unwrap();
        coinbase.extend(&self.prefix);
        coinbase.extend(extra);
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
            })
            .unwrap();
        let mut params = vec![
            json!(payout()),
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
    let upstream = super::sv1::Upstream {
        // A host name, resolved at each connection.
        address: format!("localhost:{}", pool.address.port()),
        authority: pool.authority,
        identity: payout(),
        remote: true,
    };
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || super::sv1::run(listener, upstream, stop, stats))
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
    device.write.shutdown(std::net::Shutdown::Both).unwrap();
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
    };
    let thread = {
        let stop = stop.clone();
        let stats = stats.clone();
        thread::spawn(move || super::sv1::run(listener, upstream, stop, stats))
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
