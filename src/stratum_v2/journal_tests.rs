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
    let template = Arc::new(BchTemplate::from_rpc(&rpc_template()).unwrap());
    let mut channel = Channel::new(
        1,
        ChannelKind::Standard,
        [255; 32],
        [salt; 12],
        MiningNetwork::Chipnet,
        &payout(),
    )
    .unwrap();
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
    Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), [42; 32]).unwrap()
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
        },
        BchPayout {
            donation: "2.01".parse().unwrap(),
            donation_work: true,
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
        assert!(
            Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), [42; 32]).is_err()
        );
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

#[test]
fn journal_refuses_concurrent_writer_foreign_context_and_corrupt_state() {
    let dir = TestDirectory::new();
    let journal = open(&dir);
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), [42; 32]).is_err());
    drop(journal);
    let original = fs::read(dir.journal()).unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), [43; 32]).is_err());
    let other =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x34; 20], MiningNetwork::Chipnet).unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &other, [42; 32]).is_err());
    let main =
        crate::tx::p2pkh_hash_to_cashaddr_for_network(&[0x12; 20], MiningNetwork::Mainnet).unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Mainnet, &main, [42; 32]).is_err());
    assert_eq!(fs::read(dir.journal()).unwrap(), original);
    fs::write(dir.journal(), b"partial journal").unwrap();
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), [42; 32]).is_err());
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
    assert!(Journal::open(&dir.journal(), MiningNetwork::Chipnet, &payout(), [42; 32]).is_err());
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
            [42; 32],
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
