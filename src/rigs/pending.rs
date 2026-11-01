//! #### PR #53: retain completed rig proofs until the coordinator advances the baton.
//! Sending a frame is not a claim receipt. Reconnect/restart retries keep the exact
//! transaction and payout, and only rebind the coordinator-local generation ID.

use super::{MiningJob, VerifiedWinner, WireJob, WireWinner};
use crate::{config, mining_job::verify_rig_winner};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_WINNERS: usize = 16;
const MAX_BYTES: u64 = 1024 * 1024;

fn same_work(previous: &WireJob, current: &WireJob) -> bool {
    let mut previous = previous.clone();
    previous.generation_id = current.generation_id;
    previous
        .source_identity
        .clone_from(&current.source_identity);
    previous == *current
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedWinner {
    job: WireJob,
    winner: WireWinner,
}

impl SavedWinner {
    /// The proof's claim transaction becomes the next baton, so a job on it
    /// shows the claim reached the chain. The digest is that transaction's
    /// hash, which `open`/`record` verified.
    fn claimed_by(&self, job: &MiningJob) -> bool {
        hex::decode(&self.winner.digest).is_ok_and(|mut digest| {
            digest.reverse();
            hex::encode(digest).eq_ignore_ascii_case(&job.baton_txid)
        })
    }
}

pub(super) struct PendingWinners {
    path: PathBuf,
    saved: Vec<SavedWinner>,
}

impl PendingWinners {
    pub(super) fn open(path: &Path) -> Result<Self, String> {
        let saved: Vec<SavedWinner> = match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_BYTES => {
                config::restrict_private_config(path)?;
                let mut bytes = Vec::new();
                fs::File::open(path)
                    .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
                    .map_err(|_| "cannot read pending rig winners")?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err("pending rig winners exceed storage limit".into());
                }
                serde_json::from_slice(&bytes)
                    .map_err(|_| "invalid pending rig winners; refusing to replace them")?
            }
            _ => {
                return Err(
                    "pending rig winners must be a regular file within storage limit".into(),
                )
            }
        };
        if saved.len() > MAX_WINNERS {
            return Err("too many pending rig winners".into());
        }
        for entry in &saved {
            verify_rig_winner(
                &VerifiedWinner::try_from(entry.winner.clone())?,
                &MiningJob::try_from(entry.job.clone())?,
            )?;
        }
        Ok(Self {
            path: path.to_owned(),
            saved,
        })
    }

    pub(super) fn record(
        &mut self,
        job: &MiningJob,
        winners: &[VerifiedWinner],
    ) -> Result<(), String> {
        let mut next = self.saved.clone();
        let mut added = false;
        let job_wire = WireJob::from(job);
        for winner in winners {
            // A prior generation finishing after a job change is already stale.
            if winner.generation_id != job.generation_id {
                continue;
            }
            // Only one proof can spend this baton; extra solutions for the same work earn no extra reward.
            if next.iter().any(|entry| same_work(&entry.job, &job_wire)) {
                continue;
            }
            verify_rig_winner(winner, job)?;
            next.push(SavedWinner {
                job: job_wire.clone(),
                winner: WireWinner::from(winner),
            });
            added = true;
        }
        // Batons advance many times per block, and lost races are never
        // claimed by this rig. Each newer proof spends a later baton, so the
        // oldest are the ones already spent; dropping one costs at most a
        // re-mined winner, while refusing the new one would stop mining.
        let excess = next.len().saturating_sub(MAX_WINNERS);
        next.drain(..excess);
        if added {
            self.commit(next)?;
        }
        Ok(())
    }

    pub(super) fn for_job(&mut self, job: &MiningJob) -> Result<Vec<VerifiedWinner>, String> {
        crate::mining_job::validate_job(job)?;
        let mut next = self.saved.clone();
        // A job on the proof's own claim, or a higher chain tip, retires a
        // proof. Another baton at the same height may come from an indexer
        // that has not yet seen the newest claim, so the proof stays; it is
        // never re-sent to different work.
        next.retain(|entry| {
            entry.job.network != job.network.as_str()
                || (!entry.claimed_by(job)
                    && (entry.job.height >= job.height
                        || (entry.job.baton_txid == job.baton_txid
                            && entry.job.baton_vout == job.baton_vout)))
        });
        if next.len() != self.saved.len() {
            self.commit(next)?;
        }
        let expected = WireJob::from(job);
        self.saved
            .iter()
            .filter_map(|entry| {
                if !same_work(&entry.job, &expected) {
                    return None;
                }
                Some((|| {
                    let mut winner = VerifiedWinner::try_from(entry.winner.clone())?;
                    winner.generation_id = job.generation_id;
                    verify_rig_winner(&winner, job)?;
                    Ok(winner)
                })())
            })
            .collect()
    }

    fn commit(&mut self, next: Vec<SavedWinner>) -> Result<(), String> {
        let bytes = serde_json::to_vec(&next).map_err(|_| "cannot encode pending rig winners")?;
        if next.len() > MAX_WINNERS || bytes.len() as u64 > MAX_BYTES {
            return Err("pending rig winners exceed storage limit; mining stopped".into());
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|_| "cannot create rig journal directory")?;
        config::write_private_atomic(&self.path, &bytes)?;
        fs::File::options()
            .read(true)
            .write(true)
            .open(&self.path)
            .and_then(|file| file.sync_all())
            .map_err(|_| "cannot sync installed rig journal")?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|_| "cannot sync rig journal directory")?;
        self.saved = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mining_job::tests::{easy_job, solved_winner};
    use crate::stratum_v2::journal::TestDirectory;

    #[test]
    fn unsent_winner_survives_restart_and_rebinds_only_local_generation() {
        let dir = TestDirectory::new();
        let path = dir.journal();
        let mut job = easy_job();
        let winner = solved_winner(&job, [3; 32]);
        let mut pending = PendingWinners::open(&path).unwrap();
        pending.record(&job, std::slice::from_ref(&winner)).unwrap();
        let disk = fs::read(&path).unwrap();
        drop(pending); // Disconnect before delivery, or after sending but before a claim receipt.
        let mut pending = PendingWinners::open(&path).unwrap();
        job.generation_id += 9;
        job.source_identity = "backup".into();
        let retry = pending.for_job(&job).unwrap();
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].generation_id, job.generation_id);
        assert_eq!(retry[0].transaction, winner.transaction);
        assert_eq!(retry[0].digest, winner.digest);
        verify_rig_winner(&retry[0], &job).unwrap();
        pending.record(&job, &retry).unwrap();
        pending
            .record(&job, &[solved_winner(&job, [4; 32])])
            .unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            disk,
            "a lost reply must not duplicate or rewrite a proof"
        );
    }

    #[test]
    fn retry_cannot_change_payout_target_network_or_transaction_material() {
        let dir = TestDirectory::new();
        let job = easy_job();
        let winner = solved_winner(&job, [3; 32]);
        let mut pending = PendingWinners::open(&dir.journal()).unwrap();
        pending.record(&job, &[winner]).unwrap();
        let mut changes = Vec::new();
        let mut changed = job.clone();
        changed.payout_address =
            crate::tx::p2pkh_hash_to_cashaddr_for_network(&[42; 20], job.network).unwrap();
        changes.push(changed);
        let mut changed = job.clone();
        changed.relay_fee_sats_per_kb += 1;
        changes.push(changed);
        let mut changed = job.clone();
        changed.target_le_hex = format!("{}01", "00".repeat(31));
        changes.push(changed);
        let mut changed = job.clone();
        changed.token_amount += 1;
        changes.push(changed);
        let mut changed = job.clone();
        changed.baton_value_sats += 1;
        changes.push(changed);
        let mut changed = job.clone();
        changed.network = crate::config::MiningNetwork::Chipnet;
        changes.push(changed);
        let mut changed = job.clone();
        changed.age += 1;
        changes.push(changed);
        for changed in changes {
            assert!(pending.for_job(&changed).unwrap().is_empty());
        }
        assert_eq!(pending.for_job(&job).unwrap().len(), 1);
        // Another baton at the same height (an indexer behind the newest
        // claim, or a claim already made) is not sent the proof but keeps it.
        let mut other = job.clone();
        other.baton_txid = "22".repeat(32);
        assert!(pending.for_job(&other).unwrap().is_empty());
        assert_eq!(pending.for_job(&job).unwrap().len(), 1);
        // A higher tip on another baton retires it.
        let mut advanced = other.clone();
        advanced.height += 1;
        assert!(pending.for_job(&advanced).unwrap().is_empty());
        assert!(PendingWinners::open(&dir.journal())
            .unwrap()
            .for_job(&job)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_proof_retires_once_its_claim_is_the_baton() {
        let dir = TestDirectory::new();
        let job = easy_job();
        let winner = solved_winner(&job, [3; 32]);
        let mut pending = PendingWinners::open(&dir.journal()).unwrap();
        pending.record(&job, std::slice::from_ref(&winner)).unwrap();
        // Claimed: the next job, at the same height, spends the claim's output.
        let mut next = job.clone();
        let mut claim = winner.digest;
        claim.reverse();
        next.baton_txid = hex::encode(claim);
        next.generation_id += 1;
        assert!(pending.for_job(&next).unwrap().is_empty());
        assert!(PendingWinners::open(&dir.journal())
            .unwrap()
            .for_job(&job)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn many_unclaimed_proofs_in_one_block_never_stop_mining() {
        let dir = TestDirectory::new();
        let mut pending = PendingWinners::open(&dir.journal()).unwrap();
        let mut jobs = Vec::new();
        for round in 0..MAX_WINNERS + 4 {
            // Lost races: every new baton at this height is another miner's claim.
            let mut job = easy_job();
            job.baton_txid = format!("{:064x}", round + 0x100);
            job.generation_id += round as u64;
            pending
                .record(&job, &[solved_winner(&job, [3; 32])])
                .unwrap();
            jobs.push(job);
        }
        let mut reopened = PendingWinners::open(&dir.journal()).unwrap();
        assert_eq!(reopened.saved.len(), MAX_WINNERS);
        assert!(
            reopened.for_job(&jobs[3]).unwrap().is_empty(),
            "the oldest proofs make room"
        );
        assert_eq!(reopened.for_job(jobs.last().unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn corrupt_journal_and_failed_save_do_not_replace_existing_proof() {
        let dir = TestDirectory::new();
        let path = dir.journal();
        let job = easy_job();
        let first = solved_winner(&job, [3; 32]);
        let mut pending = PendingWinners::open(&path).unwrap();
        pending.record(&job, std::slice::from_ref(&first)).unwrap();
        let original = fs::read(&path).unwrap();
        let saved = dir.0.join("saved.json");
        fs::rename(&path, &saved).unwrap();
        fs::create_dir(&path).unwrap();
        let mut next_job = job.clone();
        next_job.relay_fee_sats_per_kb += 1;
        assert!(pending
            .record(&next_job, &[solved_winner(&next_job, [4; 32])])
            .is_err());
        assert_eq!(pending.for_job(&job).unwrap(), vec![first]);
        assert_eq!(fs::read(&saved).unwrap(), original);
        fs::remove_dir(&path).unwrap();
        fs::write(&path, b"incomplete").unwrap();
        assert!(PendingWinners::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"incomplete");
    }

    #[test]
    fn saved_proof_survives_process_exit_without_destructors() {
        const CHILD: &str = "PICKAXE_RIG_PROOF_CRASH_TEST";
        let job = easy_job();
        let winner = solved_winner(&job, [3; 32]);
        if let Some(path) = std::env::var_os(CHILD) {
            PendingWinners::open(Path::new(&path))
                .unwrap()
                .record(&job, &[winner])
                .unwrap();
            std::process::exit(23);
        }
        let dir = TestDirectory::new();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rigs::pending::tests::saved_proof_survives_process_exit_without_destructors",
                "--test-threads=1",
            ])
            .env(CHILD, dir.journal())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        let mut pending = PendingWinners::open(&dir.journal()).unwrap();
        assert_eq!(pending.for_job(&job).unwrap(), vec![winner]);
    }

    #[test]
    fn encrypted_coordinator_restart_receives_the_saved_proof_with_unchanged_payout() {
        use crate::{
            rigs::{net, RigHub},
            stratum_v2::transport::Session,
        };
        use std::{
            net::TcpStream,
            time::{Duration, Instant},
        };
        let dir = TestDirectory::new();
        let path = dir.journal();
        let mut job = easy_job();
        let winner = solved_winner(&job, [3; 32]);
        PendingWinners::open(&path)
            .unwrap()
            .record(&job, std::slice::from_ref(&winner))
            .unwrap();
        for generation in [job.generation_id, job.generation_id + 10] {
            job.generation_id = generation;
            let hub =
                RigHub::start("127.0.0.1:0".parse().unwrap(), &dir.0.join("authority")).unwrap();
            hub.publish(job.clone());
            let (mut sender, mut receiver) = Session::initiate(
                TcpStream::connect(hub.listen()).unwrap(),
                net::decode_key(hub.key()).unwrap(),
            )
            .unwrap()
            .split();
            let frame = receiver.receive(Duration::from_secs(5)).unwrap().unwrap();
            let (kind, bytes) = net::read(frame).unwrap();
            assert_eq!(kind, 2);
            let sent_job =
                MiningJob::try_from(serde_json::from_slice::<WireJob>(&bytes).unwrap()).unwrap();
            let mut pending = PendingWinners::open(&path).unwrap();
            for retry in pending.for_job(&sent_job).unwrap() {
                sender
                    .send(net::frame(3, &WireWinner::from(&retry)).unwrap())
                    .unwrap();
            }
            let until = Instant::now() + Duration::from_secs(5);
            while !hub.has_winner() {
                assert!(Instant::now() < until, "saved proof was not delivered");
                std::thread::sleep(Duration::from_millis(10));
            }
            let received = hub.take_winners();
            assert_eq!(received.len(), 1);
            verify_rig_winner(&received[0], &job).unwrap();
            assert_eq!(received[0].transaction, winner.transaction);
            assert_eq!(received[0].digest, winner.digest);
            assert_eq!(hub.summary().rejected, 0);
            sender.close();
            receiver.close();
            // Simulate a coordinator exiting after receipt but before a journaled claim.
            drop(hub);
        }
    }
}
