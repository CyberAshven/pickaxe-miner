use super::*;
use crate::config::MiningNetwork;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

struct Node {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Node {
    fn start(network: MiningNetwork, scan: Value, wrong_chain: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://fixture-user:fixture-secret@{}",
            listener.local_addr().unwrap()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (done, seen) = (stop.clone(), requests.clone());
        let handle = thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut data = Vec::new();
                let request: Value = loop {
                    let mut buffer = [0; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    assert_ne!(count, 0);
                    data.extend_from_slice(&buffer[..count]);
                    let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&data[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("Content-Length: ")
                                .and_then(|s| s.parse().ok())
                        })
                        .unwrap();
                    if data.len() >= end + 4 + length {
                        break serde_json::from_slice(&data[end + 4..end + 4 + length]).unwrap();
                    }
                };
                let method = request["method"].as_str().unwrap();
                seen.lock().unwrap().push(method.into());
                let raw = "02000000000000000000";
                let txid = crate::reward::transaction_id(&hex::decode(raw).unwrap());
                let result = match method {
                    "getblockcount" => json!(network.fork_block().0 + 100),
                    "getblockhash" if wrong_chain => json!("00".repeat(32)),
                    "getblockhash" => json!(network.fork_block().1),
                    "getblockchaininfo" => json!({"blocks":network.fork_block().0+100,
                        "bestblockhash":network.fork_block().1,"initialblockdownload":false}),
                    "scantxoutset" => scan.clone(),
                    "getrawmempool" => json!([]),
                    "getnetworkinfo" => json!({"relayfee":"0.00001000"}),
                    "getmempoolinfo" => json!({"mempoolminfee":"0.00002000"}),
                    "getrawtransaction" if request["params"][0] == txid => json!(raw),
                    "getrawtransaction" => Value::Null,
                    "gettxout" => Value::Null,
                    "sendrawtransaction" => json!(txid),
                    other => panic!("unexpected RPC {other}"),
                };
                let error = (method == "getrawtransaction" && result.is_null()).then(
                    || json!({"code":-5,"message":"No such mempool or blockchain transaction"}),
                );
                let body = json!({"result":result,"error":error,"id":"pickaxe"}).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        Self {
            url,
            stop,
            requests,
            handle: Some(handle),
        }
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.handle.take().unwrap().join().unwrap();
    }
}

#[test]
fn node_only_adapter_reads_both_networks_and_shares_recovery_and_broadcast_transport() {
    for network in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
        let token = json!({"category":"42".repeat(32),"amount":"9007199254740993",
            "nft":{"capability":"mutable","commitment":"00".repeat(16)}});
        let scan = json!({"success":true,"unspents":[
            {"txid":"11".repeat(32),"vout":1,"scriptPubKey":"51","amount":"0.00001000","tokenData":token},
            {"txid":"22".repeat(32),"vout":0,"scriptPubKey":"52","amount":"0.00010000"}]});
        let node = Node::start(network, scan, false);
        let cfg = RuntimeConfig {
            network,
            node_url: Some(node.url.clone()),
            source: JobSource::Node,
            fulcrum_url: Some("wss://unreachable.invalid".into()),
            ..Default::default()
        };
        let mut session = Session::connect(&cfg, &[vec![0x51], vec![0x52]]).unwrap();
        assert!(!session.url.contains("fixture-secret") && !session.url.contains("fixture-user"));
        assert!(session.fallback_reason.is_none());
        assert_eq!(session.tip().unwrap(), network.fork_block().0 + 100);
        let baton = session.unspent(&[0x51]).unwrap();
        assert_eq!(baton[0]["token_data"], token);
        assert_eq!(baton[0]["value"], 1000);
        let coins = session.unspent(&[0x52]).unwrap();
        assert_eq!(coins[0]["value"], 10_000);
        assert!(coins[0]["token_data"].is_null());
        assert_eq!(session.relay_fee().unwrap(), 2000);
        assert!(session.unspent(&[0x53]).is_err());
        assert!(!node
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "sendrawtransaction"));
        let raw = "02000000000000000000";
        let txid = crate::reward::transaction_id(&hex::decode(raw).unwrap());
        assert_eq!(session.transaction(&txid).unwrap(), raw);
        // Only the fixture receives this broadcast, with no external node or GPU.
        assert_eq!(session.broadcast_raw(raw).unwrap(), txid);
        assert!(session.transaction(&"ff".repeat(32)).is_err());
        assert_eq!(
            node.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.as_str() == "scantxoutset")
                .count(),
            1
        );
    }
}

#[test]
fn explicit_node_source_requires_endpoint_and_rejects_foreign_chain_without_fulcrum() {
    let cfg = RuntimeConfig {
        source: JobSource::Node,
        ..Default::default()
    };
    assert!(Session::connect(&cfg, &[])
        .err()
        .unwrap()
        .contains("needs a BCH node"));
    let node = Node::start(
        MiningNetwork::Mainnet,
        json!({"success":true,"unspents":[]}),
        true,
    );
    let cfg = RuntimeConfig {
        node_url: Some(node.url.clone()),
        ..cfg
    };
    let error = Session::connect(&cfg, &[vec![0x51]]).err().unwrap();
    assert!(!error.contains("fixture-secret") && !error.contains("fixture-user"));
    assert!(!node
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|s| s == "scantxoutset"));
}

#[test]
fn electric_status_command_reads_actual_deployment_baton_without_keys_or_devices() {
    for network in [MiningNetwork::Mainnet, MiningNetwork::Chipnet] {
        let deployment =
            crate::electric::Deployment::select(network, None, None, None, None).unwrap();
        let state = crate::electric::State {
            difficulty: 3,
            last_height: network.fork_block().0,
            mine_count: 7,
        };
        let scan = json!({"success":true,"unspents":[{"txid":"11".repeat(32),"vout":1,
            "scriptPubKey":hex::encode(deployment.locking_bytecode()),"amount":"0.00001000",
            "tokenData":{"category":deployment.category_hex(),"amount":"9007199254740993",
                "nft":{"capability":"mutable","commitment":hex::encode(state.encode())}}}]});
        let node = Node::start(network, scan, false);
        let cfg = RuntimeConfig {
            network,
            node_url: Some(node.url.clone()),
            source: JobSource::Node,
            fulcrum_url: Some("wss://unreachable.invalid".into()),
            ..Default::default()
        };
        crate::electric::miner::run(
            &cfg,
            &crate::cli::ElectricCommand::Status {
                deployment: crate::cli::ElectricDeploymentArgs::default(),
            },
            std::path::Path::new("not-a-wallet-or-config"),
            crate::backend::BackendKind::Auto,
            &crate::backend::DeviceSelection::default(),
            true,
        )
        .unwrap();
        assert!(!node
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "sendrawtransaction"));
    }
}
