use super::{
    channel::{Channel, ChannelKind, Share, ValidatedShare},
    journal::{Journal, TestDirectory},
    template::BchTemplate,
    template_tests::{payout, rpc_template},
};
use crate::config::MiningNetwork;
use std::{fs, sync::Arc};
use stratum_core::bitcoin::{block::Header, consensus};

pub(crate) fn solved_share(salt: u8) -> ValidatedShare {
    solved_share_with_payout(salt, Default::default())
}

fn solved_share_with_payout(
    salt: u8,
    payout_policy: crate::donation::bch::BchPayout,
) -> ValidatedShare {
    solved_share_for(salt, payout_policy, &payout(), None)
}

/// #### PR #40: a block solved for any miner, with any pool fee address.
fn solved_share_for(
    salt: u8,
    payout_policy: crate::donation::bch::BchPayout,
    miner: &str,
    operator: Option<&str>,
) -> ValidatedShare {
    let template = BchTemplate::from_rpc(&rpc_template()).unwrap();
    solved_share_on(template, salt, payout_policy, miner, operator)
}

/// #### PR #42: the same on any template (one with merge-mined tokens too),
/// through a channel's share path.
fn solved_share_on(
    template: BchTemplate,
    salt: u8,
    payout_policy: crate::donation::bch::BchPayout,
    miner: &str,
    operator: Option<&str>,
) -> ValidatedShare {
    let template = Arc::new(template);
    let mut channel = Channel::new(
        1,
        ChannelKind::Standard,
        [255; 32],
        [salt; 12],
        MiningNetwork::Chipnet,
        miner,
    )
    .unwrap();
    channel.set_operator(operator).unwrap();
    let job = channel
        .install_with_payout(1, 1, template.clone(), payout_policy)
        .unwrap();
    let nonce = (0..10_000)
        .find(|nonce| {
            let bytes = template
                .header(
                    &job.standard_coinbase,
                    template.version,
                    template.current_time,
                    *nonce,
                )
                .unwrap();
            let header: Header = consensus::deserialize(&bytes).unwrap();
            header.validate_pow(header.target()).is_ok()
        })
        .unwrap();
    channel
        .check(
            Share {
                channel_id: 1,
                job_id: 1,
                sequence: 0,
                version: template.version,
                time: template.current_time,
                nonce,
                extranonce: &[],
            },
            template.current_time,
        )
        .unwrap()
}

fn open(dir: &TestDirectory) -> Journal {
    Journal::open(
        &dir.journal(),
        MiningNetwork::Chipnet,
        &payout(),
        &[[42; 32]],
    )
    .unwrap()
}

/// #### PR #42: a block solved with the test token merge-mined in both
/// modes: a commitment as output 0 and a ticket after the payouts. It comes
/// from a channel's share path, which names the ticket's entry as won.
fn solved_token_share(salt: u8) -> ValidatedShare {
    use super::merge::{leaf::Mode, set::tests::test_set};
    let mut template = BchTemplate::from_rpc(&rpc_template()).unwrap();
    template.commit(Arc::new(test_set(&[
        Mode::ShareTarget,
        Mode::BlockRequired,
    ])));
    let share = solved_share_on(template, salt, Default::default(), &payout(), None);
    assert!(share.block && share.token_wins.contains(&1));
    share
}

/// #### PR #42: journals a token block, rewrites its coinbase with
/// `change` (keeping the proof of work and merkle root valid), and reports
/// whether the journal still opens.
fn opens_after(change: impl FnOnce(&mut Vec<stratum_core::bitcoin::TxOut>)) -> bool {
    use stratum_core::bitcoin::Block;
    let dir = TestDirectory::new();
    let mut journal = open(&dir);
    assert!(journal.enqueue(&solved_token_share(3)).unwrap());
    drop(journal);
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.journal()).unwrap()).unwrap();
    let raw = hex::decode(state["pending"][0]["block"].as_str().unwrap()).unwrap();
    let mut block: Block = consensus::deserialize(&raw).unwrap();
    change(&mut block.txdata[0].output);
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    block.header.nonce = (0..10_000)
        .find(|nonce| {
            block.header.nonce = *nonce;
            block.header.validate_pow(block.header.target()).is_ok()
        })
        .unwrap();
    state["pending"][0]["hash"] = serde_json::json!(block.block_hash().to_string());
    state["pending"][0]["block"] = serde_json::json!(hex::encode(consensus::serialize(&block)));
    fs::write(dir.journal(), serde_json::to_vec(&state).unwrap()).unwrap();
    Journal::open(
        &dir.journal(),
        MiningNetwork::Chipnet,
        &payout(),
        &[[42; 32]],
    )
    .is_ok()
}

fn sats(value: u64) -> stratum_core::bitcoin::Amount {
    stratum_core::bitcoin::Amount::from_sat(value)
}

/// Moves one satoshi from the miner's output (output 1) to `index`.
fn move_a_satoshi(outputs: &mut [stratum_core::bitcoin::TxOut], index: usize) {
    outputs[1].value = sats(outputs[1].value.to_sat() - 1);
    outputs[index].value = sats(outputs[index].value.to_sat() + 1);
}

fn edit_script(output: &mut stratum_core::bitcoin::TxOut, change: impl FnOnce(&mut Vec<u8>)) {
    let mut script = output.script_pubkey.to_bytes();
    change(&mut script);
    output.script_pubkey = stratum_core::bitcoin::ScriptBuf::from_bytes(script);
}

// #### PR #42
#[test]
fn a_block_with_a_commitment_and_tickets_enqueues_and_survives_reopen() {
    let dir = TestDirectory::new();
    let share = solved_token_share(1);
    let mut journal = open(&dir);
    assert!(journal.enqueue(&share).unwrap());
    assert!(!journal.enqueue(&share).unwrap());
    let hash = journal.pending_hashes()[0].clone();
    let bytes = journal.pending(&hash).unwrap().block;
    drop(journal);
    let mut journal = open(&dir);
    assert_eq!(journal.counts(), (1, 0, 0));
    assert_eq!(journal.pending(&hash).unwrap().block, bytes);
    journal.finish(&hash, true).unwrap();
    drop(journal);
    assert_eq!(open(&dir).counts(), (0, 1, 0));
    // The rewrite helper itself keeps a valid block valid.
    assert!(opens_after(|_| {}));
}

// #### PR #42
#[test]
fn a_leading_output_that_is_not_an_exact_commitment_is_refused() {
    // A valued output 0 (the satoshi taken from the miner).
    assert!(!opens_after(|outputs| move_a_satoshi(outputs, 0)));
    // The wrong magic, version or height, and the wrong length.
    for (index, value) in [(2, b'X'), (6, 2), (39, 17)] {
        assert!(
            !opens_after(|outputs| edit_script(&mut outputs[0], |s| s[index] = value)),
            "byte {index}"
        );
    }
    assert!(!opens_after(
        |outputs| edit_script(&mut outputs[0], |s| s.push(0))
    ));
    assert!(!opens_after(|outputs| edit_script(&mut outputs[0], |s| {
        s.pop();
    })));
    // The commitment anywhere but output 0.
    assert!(!opens_after(|outputs| outputs.swap(0, 1)));
}

// #### PR #42
#[test]
fn a_valued_or_unknown_trailing_output_is_refused() {
    // A valued ticket (the satoshi taken from the miner).
    assert!(!opens_after(|outputs| move_a_satoshi(outputs, 3)));
    // A ticket with another script: no capability, another opcode, longer.
    for (index, value) in [(35, 0), (1, 0xcf), (36, 0x88)] {
        assert!(
            !opens_after(|outputs| edit_script(&mut outputs[3], |s| s[index] = value)),
            "byte {index}"
        );
    }
    assert!(!opens_after(
        |outputs| edit_script(&mut outputs[3], |s| s.push(0x87))
    ));
    // A zero-value output after the tickets that is not a ticket.
    assert!(!opens_after(|outputs| {
        let mut extra = outputs[3].clone();
        edit_script(&mut extra, |s| *s = vec![0x6a]);
        outputs.push(extra);
    }));
    // Tickets without a commitment.
    assert!(!opens_after(|outputs| {
        outputs.remove(0);
    }));
}

// #### PR #42
#[test]
fn legacy_pending_blocks_still_open() {
    use stratum_core::bitcoin::Block;
    let dir = TestDirectory::new();
    let mut journal = open(&dir);
    journal.enqueue(&solved_share(50)).unwrap();
    journal.enqueue(&solved_share(51)).unwrap();
    drop(journal);
    // Turn the first block into a pre-donation one (one output, no policy),
    // as journals held before PR #38.
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.journal()).unwrap()).unwrap();
    let raw = hex::decode(state["pending"][0]["block"].as_str().unwrap()).unwrap();
    let mut block: Block = consensus::deserialize(&raw).unwrap();
    block.txdata[0].output.truncate(1);
    block.txdata[0].output[0].value = sats(312_500_000);
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    block.header.nonce = (0..10_000)
        .find(|nonce| {
            block.header.nonce = *nonce;
            block.header.validate_pow(block.header.target()).is_ok()
        })
        .unwrap();
    let legacy_hash = block.block_hash().to_string();
    state["pending"][0] = serde_json::json!({
        "hash": legacy_hash,
        "block": hex::encode(consensus::serialize(&block)),
    });
    fs::write(dir.journal(), serde_json::to_vec(&state).unwrap()).unwrap();
    // A token block joins the legacy and the current one.
    let mut journal = open(&dir);
    assert!(journal.enqueue(&solved_token_share(52)).unwrap());
    let snapshot = fs::read(dir.journal()).unwrap();
    drop(journal);
    let mut journal = open(&dir);
    assert_eq!(fs::read(dir.journal()).unwrap(), snapshot);
    assert_eq!(journal.counts(), (3, 0, 0));
    assert_eq!(journal.pending(&legacy_hash).unwrap().payout, None);
    for hash in journal.pending_hashes() {
        journal.finish(&hash, true).unwrap();
    }
    drop(journal);
    assert_eq!(open(&dir).counts(), (0, 3, 0));
}

// #### PR #40
#[test]
fn a_public_pool_block_for_another_miner_survives_a_restart() {
    use crate::donation::bch::{BchPayout, FeeMode, PoolFee};
    let miner = crate::config::reprefix_p2pkh_payout(
        &crate::reward::p2pkh_cashaddr_from_public_key(
            &secp256k1::PublicKey::from_secret_key(
                &secp256k1::SecretKey::from_secret_bytes([5; 32]).unwrap(),
            )
            .serialize(),
        )
        .unwrap(),
        MiningNetwork::Chipnet,
    )
    .unwrap();
    let operator = payout();
    let policy = BchPayout {
        fee: Some(PoolFee {
            rate: "2".parse().unwrap(),
            mode: FeeMode::Both,
        }),
        ..BchPayout::default()
    };
    let dir = TestDirectory::new();
    let share = solved_share_for(7, policy, &miner, Some(&operator));
    let hash = {
        let mut journal = open(&dir);
        assert!(journal.enqueue(&share).unwrap());
        journal.pending_hashes()[0].clone()
    };
    // Reopened, the pending block still checks against its own miner.
    let journal = open(&dir);
    let pending = journal.pending(&hash).unwrap();
    assert_eq!(pending.miner.as_deref(), Some(miner.as_str()));
    assert_eq!(pending.operator.as_deref(), Some(operator.as_str()));
    assert_eq!(pending.payout, Some(policy));
    // A tampered miner fails closed.
    drop(journal);
    let path = dir.journal();
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace(&miner, &operator)).unwrap();
    assert!(Journal::open(&path, MiningNetwork::Chipnet, &payout(), &[[42; 32]]).is_err());
}

#[test]
fn journal_recovers_each_jobs_rate_and_rejects_policy_tampering() {
    use crate::donation::bch::BchPayout;
    let dir = TestDirectory::new();
    let mut journal = open(&dir);
    for (index, policy) in [
        BchPayout::default(),
        BchPayout {
            donation: "2".parse().unwrap(),
            donation_work: false,
            ..BchPayout::default()
        },
        BchPayout {
            donation: "2.01".parse().unwrap(),
            donation_work: true,
            ..BchPayout::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        journal
            .enqueue(&solved_share_with_payout(index as u8, policy))
            .unwrap();
    }
    let original = fs::read(dir.journal()).unwrap();
    drop(journal);
    let journal = open(&dir);
    assert_eq!(journal.counts(), (3, 0, 0));
    assert_eq!(fs::read(dir.journal()).unwrap(), original);
    drop(journal);
    for replacement in [
        serde_json::json!(null),
        serde_json::json!({"donation":200,"donation_work":false}),
        serde_json::json!({"donation":150,"donation_work":true}),
        serde_json::json!({"donation":0,"donation_work":false}),
    ] {
        let mut state: serde_json::Value = serde_json::from_slice(&original).unwrap();
        state["pending"][0]["payout"] = replacement;
        fs::write(dir.journal(), serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(Journal::open(
            &dir.journal(),
            MiningNetwork::Chipnet,
            &payout(),
            &[[42; 32]]
        )
        .is_err());
    }
}

#[test]
fn pre_donation_solved_blocks_are_recovered_without_rewriting_the_coinbase() {
    use stratum_core::bitcoin::{Amount, Block};
    let dir = TestDirectory::new();
    let mut journal = open(&dir);
    journal.enqueue(&solved_share(42)).unwrap();
    drop(journal);
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.journal()).unwrap()).unwrap();
    let raw = hex::decode(state["pending"][0]["block"].as_str().unwrap()).unwrap();
    let mut block: Block = consensus::deserialize(&raw).unwrap();
    block.txdata[0].output.truncate(1);
    block.txdata[0].output[0].value = Amount::from_sat(312_500_000);
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    block.header.nonce = (0..10_000)
        .find(|nonce| {
            block.header.nonce = *nonce;
            block.header.validate_pow(block.header.target()).is_ok()
        })
        .unwrap();
    let hash = block.block_hash().to_string();
    let bytes = hex::encode(consensus::serialize(&block));
    state["pending"][0] = serde_json::json!({"hash":hash,"block":bytes});
    let legacy = serde_json::to_vec(&state).unwrap();
    fs::write(dir.journal(), &legacy).unwrap();
    let mut journal = open(&dir);
    assert_eq!(fs::read(dir.journal()).unwrap(), legacy);
    assert_eq!(journal.pending(&hash).unwrap().block, bytes);
    journal.enqueue(&solved_share(43)).unwrap();
    drop(journal);
    let mut journal = open(&dir);
    assert_eq!(journal.pending(&hash).unwrap().block, bytes);
    journal.finish(&hash, true).unwrap();
    assert_eq!(journal.counts(), (1, 1, 0));
}

#[test]
fn solved_block_and_completion_receipt_survive_reopen_without_double_counting() {
    let dir = TestDirectory::new();
    let share = solved_share(8);
    let mut journal = open(&dir);
    assert!(journal.enqueue(&share).unwrap());
    assert!(!journal.enqueue(&share).unwrap());
    let hash = journal.pending_hashes()[0].clone();
    let bytes = journal.pending(&hash).unwrap().block;
    assert_eq!(journal.counts(), (1, 0, 0));
    drop(journal);
    let mut journal = open(&dir);
    assert_eq!(journal.pending(&hash).unwrap().block, bytes);
    journal.finish(&hash, true).unwrap();
    journal.finish(&hash, true).unwrap();
    assert_eq!(journal.counts(), (0, 1, 0));
    drop(journal);
    let mut journal = open(&dir);
    assert!(!journal.enqueue(&share).unwrap());
    assert_eq!(journal.counts(), (0, 1, 0));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.journal()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

// #### PR #40
#[test]
fn a_journal_bound_to_one_node_opens_while_that_node_is_configured_and_is_rebound() {
    let dir = TestDirectory::new();
    drop(open(&dir));
    // Bind the journal to node [42; 32], as journals were before PR #40.
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.journal()).unwrap()).unwrap();
    let script = super::payout::scripts(MiningNetwork::Chipnet, &payout(), None)
        .unwrap()
        .remove(0);
    state["context"] = serde_json::json!(super::journal::legacy_binding(
        &[42; 32],
        MiningNetwork::Chipnet,
        &script
    ));
    fs::write(dir.journal(), state.to_string()).unwrap();
    // Without its node it cannot be told apart from another miner's journal.
    assert!(Journal::open(
        &dir.journal(),
        MiningNetwork::Chipnet,
        &payout(),
        &[[43; 32]]
    )
    .is_err());
    // With its node among the configured ones it opens and is rebound, so
    // it then opens with any node of the network.
    drop(
        Journal::open(
            &dir.journal(),
            MiningNetwork::Chipnet,
            &payout(),
            &[[43; 32], [42; 32]],
        )
        .unwrap(),
    );
    drop(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), &[]).unwrap());
    // Another payout still cannot open it.
    let other =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], MiningNetwork::Chipnet).unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &other, &[[42; 32]]).is_err());
}

#[test]
fn journal_refuses_concurrent_writer_foreign_context_and_corrupt_state() {
    let dir = TestDirectory::new();
    let journal = open(&dir);
    assert!(Journal::open(
        &dir.journal(),
        MiningNetwork::Chipnet,
        &payout(),
        &[[42; 32]]
    )
    .is_err());
    drop(journal);
    let original = fs::read(dir.journal()).unwrap();
    // #### PR #40: another node of the same network opens it, unchanged.
    drop(
        Journal::open(
            &dir.journal(),
            MiningNetwork::Chipnet,
            &payout(),
            &[[43; 32]],
        )
        .unwrap(),
    );
    assert_eq!(fs::read(dir.journal()).unwrap(), original);
    let other =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], MiningNetwork::Chipnet).unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &other, &[[42; 32]]).is_err());
    let main =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x12; 20], MiningNetwork::Mainnet).unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Mainnet, &main, &[[42; 32]]).is_err());
    assert_eq!(fs::read(dir.journal()).unwrap(), original);
    fs::write(dir.journal(), b"partial journal").unwrap();
    assert!(Journal::open(
        &dir.journal(),
        MiningNetwork::Chipnet,
        &payout(),
        &[[42; 32]]
    )
    .is_err());
    assert_eq!(fs::read(dir.journal()).unwrap(), b"partial journal");
}

#[test]
fn corrupt_saved_block_and_failed_atomic_commit_cannot_be_acknowledged() {
    let dir = TestDirectory::new();
    let mut journal = open(&dir);
    assert!(journal.enqueue(&solved_share(1)).unwrap());
    let original = fs::read(dir.journal()).unwrap();
    let saved = dir.0.join("prior-state.json");
    fs::rename(dir.journal(), &saved).unwrap();
    fs::create_dir(dir.journal()).unwrap();
    assert!(journal.enqueue(&solved_share(2)).is_err());
    assert_eq!(journal.counts(), (1, 0, 0));
    assert_eq!(fs::read(&saved).unwrap(), original);
    drop(journal);
    fs::remove_dir(dir.journal()).unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let mut bytes = hex::decode(state["pending"][0]["block"].as_str().unwrap()).unwrap();
    bytes[36] ^= 1;
    state["pending"][0]["block"] = serde_json::json!(hex::encode(bytes));
    fs::write(dir.journal(), serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(Journal::open(
        &dir.journal(),
        MiningNetwork::Chipnet,
        &payout(),
        &[[42; 32]]
    )
    .is_err());
}

#[test]
fn source_identity_survives_credentials_rotation_but_not_node_changes() {
    let identity = crate::node::rpc_source_identity;
    assert_eq!(
        identity("https://alice:old@NODE.example/rpc").unwrap(),
        identity("https://bob:new@node.example:443/rpc").unwrap()
    );
    for other in [
        "http://node.example:443/rpc",
        "https://other.example/rpc",
        "https://node.example/other",
        "https://node.example:48332/rpc",
    ] {
        assert_ne!(
            identity("https://node.example/rpc").unwrap(),
            identity(other).unwrap()
        );
    }
}

#[test]
fn journal_capacity_refuses_new_work_without_losing_pending_blocks() {
    let dir = TestDirectory::new();
    let mut journal = open(&dir);
    for salt in 0..64 {
        assert!(journal.enqueue(&solved_share(salt)).unwrap());
    }
    let disk = fs::read(dir.journal()).unwrap();
    assert!(journal.enqueue(&solved_share(64)).is_err());
    assert_eq!(journal.counts(), (64, 0, 0));
    assert_eq!(fs::read(dir.journal()).unwrap(), disk);
    let first = journal.pending_hashes()[0].clone();
    journal.finish(&first, false).unwrap();
    assert!(journal.enqueue(&solved_share(64)).unwrap());
    drop(journal);
    assert_eq!(open(&dir).counts(), (64, 0, 1));
}

#[test]
fn pending_block_and_receipt_survive_process_exit_without_destructors() {
    const CHILD: &str = "PICKAXE_BLOCK_JOURNAL_CRASH_TEST";
    if let Some(path) = std::env::var_os(CHILD) {
        let mut journal = Journal::open(
            std::path::Path::new(&path),
            MiningNetwork::Chipnet,
            &payout(),
            &[[42; 32]],
        )
        .unwrap();
        if journal.counts().0 == 0 {
            assert!(journal.enqueue(&solved_share(77)).unwrap());
        } else {
            let hash = journal.pending_hashes()[0].clone();
            journal.finish(&hash, true).unwrap();
        }
        // Exit only this test subprocess, bypassing Rust destructors and the
        // normal server shutdown. The parent owns its synthetic scratch files.
        std::process::exit(23);
    }
    let dir = TestDirectory::new();
    for expected in [(1, 0, 0), (0, 1, 0)] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "stratum_v2::journal_tests::pending_block_and_receipt_survive_process_exit_without_destructors",
                "--test-threads=1",
            ])
            .env(CHILD, dir.journal())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        let mut journal = open(&dir);
        assert_eq!(journal.counts(), expected);
        assert!(!journal.enqueue(&solved_share(77)).unwrap());
    }
}

// #### PR #42
// What: the relay journal keeps a block that pays someone else (a pool's
// coinbase), once, across a reopen, owner-only; it refuses blocks without a
// valid merkle root, takes no payout-bound share, and never opens as (or
// in place of) the block journal or another network's relay journal.
// Look here if: open_relay, enqueue_relayed or the relay binding changes.
#[test]
fn relay_journal_is_owner_only_and_never_mixes_with_the_block_journal() {
    let dir = TestDirectory::new();
    let path = dir.0.join("relay-blocks.json");
    let pool =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x56; 20], MiningNetwork::Chipnet).unwrap();
    let share = solved_share_for(5, Default::default(), &pool, None);
    let bytes = share.template.block(&share.coinbase, share.header).unwrap();
    let mut relay = Journal::open_relay(&path, MiningNetwork::Chipnet).unwrap();
    assert!(relay.enqueue(&share).is_err(), "no payout-bound shares");
    let mut broken = bytes.clone();
    // A byte of the coinbase's script: the merkle root no longer matches.
    broken[80 + 1 + 4 + 1 + 32 + 4 + 1 + 2] ^= 1;
    assert!(relay.enqueue_relayed(&broken).is_err());
    assert!(relay.enqueue_relayed(&bytes).unwrap());
    assert!(!relay.enqueue_relayed(&bytes).unwrap(), "saved once");
    assert_eq!(relay.counts(), (1, 0, 0));
    drop(relay);
    let relay = Journal::open_relay(&path, MiningNetwork::Chipnet).unwrap();
    assert_eq!(relay.counts(), (1, 0, 0));
    let pending = relay.pending(&relay.pending_hashes()[0]).unwrap();
    assert_eq!(hex::decode(&pending.block).unwrap(), bytes);
    assert!(pending.payout.is_none() && pending.miner.is_none() && pending.operator.is_none());
    drop(relay);
    assert!(Journal::open(&path, MiningNetwork::Chipnet, &payout(), &[]).is_err());
    assert!(Journal::open_relay(&path, MiningNetwork::Mainnet).is_err());
    let mut own = open(&dir);
    assert!(own.enqueue_relayed(&bytes).is_err(), "no relayed blocks");
    drop(own);
    assert!(Journal::open_relay(&dir.journal(), MiningNetwork::Chipnet).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

// #### PR #42
// What: the JD journal keeps a Job Declaration client's block, which pays
// the pool's outputs rather than this server's payout, across a reopen; it
// never opens as the block or relay journal, or they as it.
// Look here if: open_declared or the Declared binding changes.
#[test]
fn declared_blocks_enqueue_and_reopen_and_never_mix_with_other_journals() {
    let dir = TestDirectory::new();
    let path = dir.0.join("jd-blocks.json");
    let pool =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x56; 20], MiningNetwork::Chipnet).unwrap();
    let share = solved_share_for(6, Default::default(), &pool, None);
    let mut declared = Journal::open_declared(&path, MiningNetwork::Chipnet).unwrap();
    assert!(declared.enqueue(&share).unwrap());
    assert!(!declared.enqueue(&share).unwrap());
    drop(declared);
    let declared = Journal::open_declared(&path, MiningNetwork::Chipnet).unwrap();
    assert_eq!(declared.counts(), (1, 0, 0));
    drop(declared);
    assert!(Journal::open_relay(&path, MiningNetwork::Chipnet).is_err());
    assert!(Journal::open(&path, MiningNetwork::Chipnet, &payout(), &[]).is_err());
    drop(open(&dir));
    assert!(Journal::open_declared(&dir.journal(), MiningNetwork::Chipnet).is_err());
}
