//! #### PR #38
//! Exercise the real TCP/Noise server with CPU-solved headers. The node fixture
//! uses an independent block decoder/hash oracle. This is not live chain proof.

use super::{
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
}

struct Rpc(Arc<Mutex<Node>>);

impl NodeRpc for Rpc {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let mut node = self.0.lock().unwrap();
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
                let block: Block =
                    consensus::deserialize(&hex::decode(params[0].as_str().unwrap()).unwrap())
                        .unwrap();
                assert!(block.check_merkle_root());
                assert!(block.header.validate_pow(block.header.target()).is_ok());
                assert_eq!(block.header.prev_blockhash.to_string(), node.tip);
                assert_eq!(block.txdata.len(), 1);
                let coinbase = &block.txdata[0];
                assert!(coinbase.is_coinbase());
                assert_eq!(coinbase.output.len(), 1);
                assert_eq!(coinbase.output[0].value.to_sat(), 312_500_000);
                let mut expected = vec![0x76, 0xa9, 0x14];
                expected.extend([0x12; 20]);
                expected.extend([0x88, 0xac]);
                assert_eq!(coinbase.output[0].script_pubkey.as_bytes(), expected);
                assert!(coinbase.input[0].witness.is_empty());
                assert!((100..=200).contains(&consensus::serialize(coinbase).len()));
                node.submissions += 1;
                if node.reject {
                    Ok(json!("fixture-rejection"))
                } else {
                    node.height += 1;
                    node.tip = block.block_hash().to_string();
                    Ok(Value::Null)
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
}

impl Running {
    fn new(reject: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(ServerStats::default()));
        let node = Arc::new(Mutex::new(Node {
            height: 325908,
            tip: "ab".repeat(32),
            reject,
            submissions: 0,
        }));
        let secret = [17; 32];
        let authority = server::authority_public(&secret).unwrap();
        let config = ServerConfig {
            network: MiningNetwork::Chipnet,
            payout: payout(),
            authority_secret: secret,
            share_target: [255; 32],
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
        assert_eq!(stats.blocks_unconfirmed, 0);
        assert_eq!(stats.connection_errors, 0);
        assert_eq!(stats.connections, 1);
        device.sender.close();
    }
}

#[test]
fn share_acknowledgement_does_not_claim_node_block_acceptance() {
    let server = Running::new(true);
    let mut device = Device::connect(&server, false);
    device.solve_and_submit(0);
    server.wait(|stats| stats.blocks_unconfirmed == 1);
    let stats = server.stats.lock().unwrap().clone();
    assert_eq!(stats.shares_accepted, 1);
    assert_eq!(stats.blocks_accepted, 0);
    assert_eq!(server.node.lock().unwrap().submissions, 1);
    assert_eq!(server.node.lock().unwrap().height, 325908);
    device.sender.close();
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
            thread::spawn(move || super::sv1::run(listener, upstream, authority, stop))
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
        device.write.shutdown(std::net::Shutdown::Both).unwrap();
    }
}
