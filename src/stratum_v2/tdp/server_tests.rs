//! #### PR #42
//! A Template Distribution client on the reference crates against the real
//! server, over Noise, with CPU-solved headers. The node fixture decodes the
//! blocks it is sent with `bitcoin::Block`; this is not live chain proof.

use super::super::{
    journal::TestDirectory,
    server::{ServerStats, TemplateServerStats},
    server_tests::{pool_node, Running},
    template::{compact_target, double_sha256, fold, Hash},
    template_tests::transaction,
    transport::{Limits, Receiver, Sender, Session},
    wire::encoded,
};
use super::convert::tests::sri_coinbase;
use serde_json::{json, Value};
use std::{
    net::TcpStream,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use stratum_core::{
    binary_sv2::{self, GetSize, Serialize},
    bitcoin::{block::Header, consensus, Block},
    codec_sv2::SerializedFrame,
    common_messages_sv2::{self as common, Protocol, SetupConnection, SetupConnectionError},
    template_distribution_sv2::{
        CoinbaseOutputConstraints, NewTemplate, RequestTransactionData,
        RequestTransactionDataError, RequestTransactionDataSuccess, SetNewPrevHash, SubmitSolution,
        MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS, MESSAGE_TYPE_NEW_TEMPLATE,
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA, MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR,
        MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS, MESSAGE_TYPE_SET_NEW_PREV_HASH,
        MESSAGE_TYPE_SUBMIT_SOLUTION,
    },
};

/// What a client keeps of a NewTemplate.
#[derive(Clone)]
struct Template {
    id: u64,
    future: bool,
    version: u32,
    prefix: Vec<u8>,
    value: u64,
    path: Vec<Hash>,
}

/// What a client keeps of a SetNewPrevHash.
#[derive(Clone, Copy)]
struct Parent {
    previous: Hash,
    time: u32,
    bits: u32,
}

struct Solved {
    id: u64,
    /// The coinbase as sent, with BIP141 bytes when asked for.
    sent: Vec<u8>,
    /// The same coinbase without them, as the block must carry it.
    plain: Vec<u8>,
    version: u32,
    time: u32,
    nonce: u32,
}

struct Client {
    sender: Sender,
    receiver: Receiver,
}

impl Client {
    fn connect(server: &Running, limits: Limits) -> Self {
        let (sender, receiver) = Session::initiate_with(
            TcpStream::connect(server.templates.unwrap()).unwrap(),
            server.authority,
            limits,
        )
        .unwrap()
        .split();
        Self { sender, receiver }
    }

    /// A client whose setup the server accepted.
    fn setup(server: &Running) -> Self {
        let mut client = Self::connect(server, Limits::TDP_CLIENT);
        client.send_setup(Protocol::TemplateDistributionProtocol, 2, 2, 0);
        let frame = client.receive();
        assert_eq!(
            frame.header().msg_type(),
            common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS
        );
        client
    }

    fn send<T: Serialize + GetSize>(&mut self, message: T, kind: u8) {
        self.sender
            .send(encoded(message, kind, false).unwrap())
            .unwrap();
    }

    fn send_setup(&mut self, protocol: Protocol, min: u16, max: u16, flags: u32) {
        self.send(
            SetupConnection {
                protocol,
                min_version: min,
                max_version: max,
                flags,
                endpoint_host: "localhost".try_into().unwrap(),
                endpoint_port: 48442,
                vendor: "SRI-shaped test client".try_into().unwrap(),
                hardware_version: "".try_into().unwrap(),
                firmware: "".try_into().unwrap(),
                device_id: "".try_into().unwrap(),
            },
            common::MESSAGE_TYPE_SETUP_CONNECTION,
        );
    }

    fn constraints(&mut self, size: u32) {
        self.send(
            CoinbaseOutputConstraints {
                coinbase_output_max_additional_size: size,
                coinbase_output_max_additional_sigops: 0,
            },
            MESSAGE_TYPE_COINBASE_OUTPUT_CONSTRAINTS,
        );
    }

    fn receive(&mut self) -> SerializedFrame {
        self.receiver
            .receive(Duration::from_secs(5))
            .unwrap()
            .expect("a frame within 5 s")
    }

    /// True when nothing arrives for `wait`.
    fn quiet(&mut self, wait: Duration) -> bool {
        self.receiver.receive(wait).unwrap().is_none()
    }

    /// True once the server has closed the session, within `wait`.
    fn closed_within(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            match self.receiver.receive(Duration::from_millis(100)) {
                Err(_) => return true,
                Ok(Some(_)) => return false,
                Ok(None) => (),
            }
        }
        false
    }

    fn template(&mut self) -> Template {
        let mut frame = self.receive();
        assert_eq!(frame.header().msg_type(), MESSAGE_TYPE_NEW_TEMPLATE);
        let message: NewTemplate = binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(message.coinbase_tx_version, 2);
        assert_eq!(message.coinbase_tx_input_sequence, u32::MAX);
        assert_eq!(message.coinbase_tx_outputs_count, 0);
        assert!(message.coinbase_tx_outputs.as_ref().is_empty());
        assert_eq!(message.coinbase_tx_locktime, 0);
        Template {
            id: message.template_id,
            future: message.future_template,
            version: message.version,
            prefix: message.coinbase_prefix.as_ref().to_vec(),
            value: message.coinbase_tx_value_remaining,
            path: message
                .merkle_path
                .iter()
                .map(|hash| hash.as_ref().try_into().unwrap())
                .collect(),
        }
    }

    /// A future template and the SetNewPrevHash that activates it.
    fn future_template(&mut self) -> (Template, Parent) {
        let template = self.template();
        assert!(template.future);
        let mut frame = self.receive();
        assert_eq!(frame.header().msg_type(), MESSAGE_TYPE_SET_NEW_PREV_HASH);
        let message: SetNewPrevHash = binary_sv2::from_bytes(frame.payload()).unwrap();
        assert_eq!(message.template_id, template.id);
        let target: Hash = message.target.as_ref().try_into().unwrap();
        assert_eq!(target, compact_target(message.n_bits).unwrap());
        let parent = Parent {
            previous: message.prev_hash.as_ref().try_into().unwrap(),
            time: message.header_timestamp,
            bits: message.n_bits,
        };
        (template, parent)
    }

    fn submit(&mut self, solved: &Solved) {
        self.send(
            SubmitSolution {
                template_id: solved.id,
                version: solved.version,
                header_timestamp: solved.time,
                header_nonce: solved.nonce,
                coinbase_tx: solved.sent.as_slice().try_into().unwrap(),
            },
            MESSAGE_TYPE_SUBMIT_SOLUTION,
        );
    }

    /// A template's transactions, or the error code.
    fn request(&mut self, id: u64) -> Result<Vec<Vec<u8>>, String> {
        self.send(
            RequestTransactionData { template_id: id },
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA,
        );
        let mut frame = self.receive();
        match frame.header().msg_type() {
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_SUCCESS => {
                let data: RequestTransactionDataSuccess =
                    binary_sv2::from_bytes(frame.payload()).unwrap();
                assert_eq!(data.template_id, id);
                assert!(data.excess_data.as_ref().is_empty());
                Ok(data
                    .transaction_list
                    .iter()
                    .map(|tx| tx.as_ref().to_vec())
                    .collect())
            }
            MESSAGE_TYPE_REQUEST_TRANSACTION_DATA_ERROR => {
                let error: RequestTransactionDataError =
                    binary_sv2::from_bytes(frame.payload()).unwrap();
                assert_eq!(error.template_id, id);
                Err(String::from_utf8_lossy(error.error_code.as_ref()).into_owned())
            }
            other => panic!("unexpected message {other:#x}"),
        }
    }
}

/// A solution for `template` on `parent`, its coinbase built as SRI's job
/// factory builds one; `prefix` replaces the template's when given.
fn solve(
    template: &Template,
    parent: &Parent,
    extranonce: u8,
    witness: bool,
    prefix: Option<&[u8]>,
) -> Solved {
    let prefix = prefix.unwrap_or(&template.prefix);
    let plain = sri_coinbase(prefix, &[extranonce; 16], template.value, false);
    let sent = sri_coinbase(prefix, &[extranonce; 16], template.value, witness);
    let root = fold(double_sha256(&plain), &template.path);
    // Version rolling, as ASICs behind a pool do.
    let version = template.version ^ 0x2000;
    let mut bytes = version.to_le_bytes().to_vec();
    bytes.extend(parent.previous);
    bytes.extend(root);
    bytes.extend(parent.time.to_le_bytes());
    bytes.extend(parent.bits.to_le_bytes());
    bytes.extend(0u32.to_le_bytes());
    let mut header: Header = consensus::deserialize(&bytes).unwrap();
    let nonce = (0..10_000)
        .find(|nonce| {
            header.nonce = *nonce;
            header.validate_pow(header.target()).is_ok()
        })
        .expect("the test target yields a CPU solution");
    Solved {
        id: template.id,
        sent,
        plain,
        version,
        time: parent.time,
        nonce,
    }
}

fn counts(stats: &ServerStats) -> TemplateServerStats {
    stats.template_server.clone().unwrap_or_default()
}

fn relay_journal(server: &Running) -> Value {
    serde_json::from_slice(&std::fs::read(server.relay_path()).unwrap()).unwrap()
}

// #### PR #42
// What: SetupConnection for Template Distribution version 2 with no flags
// succeeds; another protocol, a version range without 2 or any flag is
// answered with its error (echoing the flags) and the session closes.
// Look here if: tdp::server::setup changes.
#[test]
fn setup_succeeds_for_version_2_and_refuses_other_protocols_versions_and_flags() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    for (protocol, min, max, flags, refusal) in [
        (Protocol::TemplateDistributionProtocol, 1, 3, 0, None),
        (
            Protocol::MiningProtocol,
            2,
            2,
            0,
            Some(("unsupported-protocol", 0)),
        ),
        (
            Protocol::TemplateDistributionProtocol,
            3,
            3,
            0,
            Some(("protocol-version-mismatch", 0)),
        ),
        (
            Protocol::TemplateDistributionProtocol,
            2,
            2,
            0b101,
            Some(("unsupported-feature-flags", 0b101)),
        ),
    ] {
        let mut client = Client::connect(&server, Limits::TDP_CLIENT);
        client.send_setup(protocol, min, max, flags);
        let mut reply = client.receive();
        match refusal {
            None => assert_eq!(
                reply.header().msg_type(),
                common::MESSAGE_TYPE_SETUP_CONNECTION_SUCCESS
            ),
            Some((code, echoed)) => {
                assert_eq!(
                    reply.header().msg_type(),
                    common::MESSAGE_TYPE_SETUP_CONNECTION_ERROR
                );
                let error: SetupConnectionError = binary_sv2::from_bytes(reply.payload()).unwrap();
                assert_eq!(error.error_code.as_ref(), code.as_bytes());
                assert_eq!(error.flags, echoed);
                assert!(client.closed_within(Duration::from_secs(3)));
            }
        }
    }
}

// #### PR #42
// What: no template goes out before the client's CoinbaseOutputConstraints;
// the first then follows at once; a client that never sends them is closed
// after the setup deadline (3 s in tests, 10 s otherwise).
// Look here if: the session's setup steps change.
#[test]
fn no_template_before_constraints_and_a_silent_client_is_closed() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let mut client = Client::setup(&server);
    assert!(client.quiet(Duration::from_millis(500)));
    client.constraints(0);
    let (template, parent) = client.future_template();
    assert_eq!(
        template.prefix,
        [3, 0x15, 0xf9, 0x04],
        "height 325,909 alone"
    );
    assert_eq!(template.value, 312_500_000);
    assert_eq!(parent.previous, [0xab; 32]);
    let mut silent = Client::setup(&server);
    assert!(silent.closed_within(Duration::from_secs(8)));
}

// #### PR #42
// What: the first template is a future one with its SetNewPrevHash; the
// same template's lease renewals send nothing; a constraints change on the
// same parent sends a current (not future) template only; a block moves the
// node to a new parent, which comes as a future template and its
// SetNewPrevHash; template ids always rise.
// Look here if: Active::offer or the session loop changes.
#[test]
fn templates_follow_the_node_future_on_a_new_parent_and_current_on_the_same_one() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let mut client = Client::setup(&server);
    client.constraints(122);
    let (first, parent) = client.future_template();
    assert!(client.quiet(Duration::from_millis(600)));
    client.constraints(221);
    let same = client.template();
    assert!(!same.future && same.id > first.id);
    assert!(client.quiet(Duration::from_millis(300)));
    client.submit(&solve(&same, &parent, 1, false, None));
    let (next, next_parent) = client.future_template();
    assert!(next.id > same.id);
    assert_ne!(next_parent.previous, parent.previous);
    server.wait(|stats| counts(stats).sent >= 3 && counts(stats).clients == 1);
}

// #### PR #42
// What: an SRI-shaped solution (version 2 coinbase with BIP141's marker,
// flag and one 32-byte zero item) is stripped, saved in the relay journal,
// submitted once and finished there: the node gets a block whose coinbase
// has no witness and whose merkle root checks, and this server's own block
// journal never holds it.
// Look here if: convert::assemble, the relay journal or the node worker's
// relay changes.
#[test]
fn an_sri_shaped_solution_with_a_bip141_witness_is_stripped_and_relayed() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let mut client = Client::setup(&server);
    client.constraints(122);
    let (template, parent) = client.future_template();
    let solved = solve(&template, &parent, 1, true, None);
    assert_eq!(solved.sent.len(), solved.plain.len() + 36);
    client.submit(&solved);
    server.wait(|stats| counts(stats).relay_accepted == 1);
    let node = server.node.lock().unwrap();
    assert_eq!(node.submissions, 1);
    let block: Block = consensus::deserialize(&hex::decode(&node.submitted[0]).unwrap()).unwrap();
    assert!(block.check_merkle_root());
    assert_eq!(consensus::serialize(&block.txdata[0]), solved.plain);
    assert!(block.txdata[0].input[0].witness.is_empty());
    drop(node);
    let stats = counts(&server.stats.lock().unwrap());
    assert_eq!(
        (
            stats.solutions,
            stats.invalid,
            stats.refused_locally,
            stats.unsaved
        ),
        (1, 0, 0, 0)
    );
    let saved = relay_journal(&server);
    assert_eq!(saved["accepted"], 1);
    assert_eq!(saved["pending"], json!([]));
    assert_eq!(saved["completed"].as_array().unwrap().len(), 1);
    let own: Value =
        serde_json::from_slice(&std::fs::read(server.state_directory.journal()).unwrap()).unwrap();
    assert_eq!(
        (own["accepted"].clone(), own["pending"].clone()),
        (json!(0), json!([]))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(server.relay_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

// #### PR #42
// What: two solutions that fail Pickaxe's own check (a coinbase that does
// not begin with the height push sent) within 10 s give one submitblock;
// both are counted, neither is saved.
// Look here if: the bounded unchecked relay changes.
#[test]
fn a_solution_failing_pickaxes_own_check_reaches_the_node_at_a_bounded_rate() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let mut client = Client::setup(&server);
    client.constraints(122);
    let (template, parent) = client.future_template();
    for extranonce in [1, 2] {
        client.submit(&solve(
            &template,
            &parent,
            extranonce,
            false,
            Some(&[3, 1, 2, 3]),
        ));
    }
    server.wait(|stats| counts(stats).refused_locally == 2 && counts(stats).once_sent == 1);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(server.node.lock().unwrap().submissions, 1);
    let stats = counts(&server.stats.lock().unwrap());
    assert_eq!(
        (stats.once_sent, stats.relay_pending, stats.relay_accepted),
        (1, 0, 0)
    );
    assert_eq!(relay_journal(&server)["completed"], json!([]));
}

// #### PR #42
// What: a template's transactions come in block order for a current
// template; an unknown id is template-id-not-found, and a template whose
// parent was replaced is stale-template-id.
// Look here if: RequestTransactionData handling changes.
#[test]
fn transaction_data_is_sent_for_current_templates_and_refused_for_stale_or_unknown_ones() {
    let node = pool_node();
    let mut txs: Vec<Value> = (1..=3).map(transaction).collect();
    txs.sort_by_key(|tx| tx["txid"].as_str().unwrap().to_owned());
    node.lock().unwrap().transactions = txs.clone();
    let server = Running::templates(node, Arc::new(TestDirectory::new()));
    let mut client = Client::setup(&server);
    client.constraints(122);
    let (template, parent) = client.future_template();
    assert_eq!(template.path.len(), 2);
    let expected: Vec<Vec<u8>> = txs
        .iter()
        .map(|tx| hex::decode(tx["data"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(client.request(template.id), Ok(expected.clone()));
    assert_eq!(
        client.request(template.id + 1_000),
        Err("template-id-not-found".into())
    );
    client.submit(&solve(&template, &parent, 1, true, None));
    let (next, _) = client.future_template();
    assert_eq!(client.request(template.id), Err("stale-template-id".into()));
    assert_eq!(client.request(next.id), Ok(expected));
}

// #### PR #42
// What: a template the client's reserve does not fit is withheld and
// counted, and the template follows once smaller constraints arrive.
// Look here if: the size budget in Active::offer changes.
#[test]
fn a_template_the_reserve_does_not_fit_is_withheld_until_it_fits() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let mut client = Client::setup(&server);
    client.constraints(u32::MAX);
    assert!(client.quiet(Duration::from_millis(600)));
    server.wait(|stats| counts(stats).withheld == 1 && counts(stats).sent == 0);
    client.constraints(122);
    client.future_template();
}

// #### PR #42
// What: a relayed block whose submission reply is lost stays pending in the
// relay journal across a restart; a server started without the template
// listener still opens that journal and finishes the block.
// Look here if: the relay journal's opening or the relay retries change.
#[test]
fn a_relayed_block_survives_lost_replies_and_a_restart_without_the_template_listener() {
    let node = pool_node();
    node.lock().unwrap().lose_replies = true;
    let directory = Arc::new(TestDirectory::new());
    let server = Running::templates(node.clone(), directory.clone());
    let mut client = Client::setup(&server);
    client.constraints(122);
    let (template, parent) = client.future_template();
    client.submit(&solve(&template, &parent, 1, false, None));
    server.wait(|stats| counts(stats).relay_pending == 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while node.lock().unwrap().submissions == 0 {
        assert!(Instant::now() < deadline, "the node never got the block");
        thread::sleep(Duration::from_millis(10));
    }
    drop(client);
    drop(server);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(directory.0.join("relay-blocks.json")).unwrap()
        )
        .unwrap()["pending"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    node.lock().unwrap().lose_replies = false;
    let server = Running::relay_only(node.clone(), directory);
    assert!(server.templates.is_none());
    server.wait(|stats| counts(stats).relay_accepted == 1 && counts(stats).relay_pending == 0);
    assert_eq!(
        node.lock().unwrap().submissions,
        1,
        "the retry found it known"
    );
}

// #### PR #42
// What: at most 8 template clients at once: a ninth is dropped before its
// handshake, and a place frees when a client leaves.
// Look here if: MAX_TP_CLIENTS or the template accept loop changes.
#[test]
fn template_clients_are_capped_at_eight() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let clients: Vec<Client> = (0..8).map(|_| Client::setup(&server)).collect();
    server.wait(|stats| counts(stats).clients == 8);
    assert!(Session::initiate_with(
        TcpStream::connect(server.templates.unwrap()).unwrap(),
        server.authority,
        Limits::TDP_CLIENT,
    )
    .is_err());
    drop(clients);
    server.wait(|stats| counts(stats).clients == 0);
    // The accept loop frees their places on its next turn.
    thread::sleep(Duration::from_millis(200));
    Client::setup(&server);
}

// #### PR #42
// What: a client frame over 64 KiB (more than the largest solution) closes
// the session.
// Look here if: Limits::TDP_SERVER changes.
#[test]
fn client_frames_over_64_kib_close_the_session() {
    let server = Running::templates(pool_node(), Arc::new(TestDirectory::new()));
    let mut client = Client::connect(&server, Limits::DEVICE);
    client.send_setup(Protocol::TemplateDistributionProtocol, 2, 2, 0);
    client.receive();
    let big = vec![0u8; 70_000];
    let payload: binary_sv2::B016M = big.as_slice().try_into().unwrap();
    client.send(payload, MESSAGE_TYPE_SUBMIT_SOLUTION);
    assert!(client.closed_within(Duration::from_secs(5)));
}
