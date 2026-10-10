//! #### PR #42: the token worker
//! What: an ASIC-exclusive token's job source and its wins. It builds each
//! job from the token's thread (the header-shaped commitment the next win
//! must follow), takes the wins devices find through a bounded queue the
//! device threads never wait on, checks each win as the covenant would,
//! saves it to the owner-only proof journal, and, for the Chipnet test
//! token, moves the simulated thread to the winning header, which re-issues
//! jobs.
//! Why: ASICs mine a token instead of BCH without a node: the job comes from
//! the token's thread, not a block template.
//! Look here if: token jobs stop changing after a win, or wins are dropped.

use super::{
    header::{forwarder_body, HeaderWin, SAFA_V1},
    registry::{HeaderToken, HEADER_TEST_TOKEN},
    safa::{decode_compact, encode_compact, next_target, verify, Commitment},
    Hash, OutPoint,
};
use crate::{
    config::MiningNetwork,
    stratum_v2::{
        journal::PendingBlock,
        payout,
        provider::{SourceKind, SubmissionOutcome, TemplateSource},
        template::{double_sha256, BchTemplate, Layout, TokenJob},
    },
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// Wins waiting for the worker before more are dropped.
const QUEUE: usize = 64;
/// Thread states recently won, so each is proven once.
const RECENT: usize = 256;
/// Proofs the journal keeps, the oldest dropped first.
const KEPT_PROOFS: usize = 256;
/// The simulated thread's age, at which its target stays its own.
const TEST_AGE: u16 = 71;
/// How far behind now the simulated thread's jobs start, so their time is
/// below the chain's median time and a claim would be final at once.
const START_BEHIND: u32 = 7_200;

/// A win a device found, as the share path hands it on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderShare {
    /// The device's header: the token's next commitment.
    pub header: [u8; 80],
    /// The job's forwarder: the payout locking bytecode the header names.
    pub script: Vec<u8>,
}

/// What the dashboard shows of the token; never a script or an address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeaderSummary {
    pub token: &'static str,
    /// The thread's compact target.
    pub bits: u32,
    /// Wins handed on, proven, found on a thread already moved on, and
    /// dropped because the queue was full.
    pub wins: u64,
    pub proven: u64,
    pub stale: u64,
    pub dropped: u64,
    /// Why wins are no longer proven, if they are not.
    pub off: Option<String>,
}

struct Thread {
    commitment: [u8; 80],
    outpoint: OutPoint,
    /// The jobs' start time.
    start: u32,
}

pub struct HeaderWork {
    network: MiningNetwork,
    token: &'static HeaderToken,
    journal_path: PathBuf,
    /// The destinations a win may pay: the miner's and the donation's.
    destinations: [Vec<u8>; 2],
    thread: Mutex<Thread>,
    queue: Mutex<VecDeque<HeaderShare>>,
    won: Mutex<VecDeque<Hash>>,
    summary: Mutex<HeaderSummary>,
}

impl std::fmt::Debug for HeaderWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HeaderWork({})", self.token.name)
    }
}

impl HeaderWork {
    // #### PR #42: the ASIC test token
    // What: the Chipnet test token's simulated thread at a compact target:
    // its first commitment names no predecessor, its age stays 71 so the
    // target stays put, and each proven win becomes the next commitment.
    // Why: no ASIC-exclusive token is deployed; this runs jobs, devices,
    // wins, checks and re-issued jobs end to end, with no chain and no claim.
    // Look here if: the flag is accepted on mainnet, or test-token jobs never
    // change after a win.
    /// The Chipnet test token for miner `payout` at `target_bits`; refused
    /// on any network its registry row is not on.
    pub fn test_token(
        network: MiningNetwork,
        journal_path: PathBuf,
        target_bits: u32,
        payout: &str,
    ) -> Result<Self, String> {
        let token: &'static HeaderToken = &HEADER_TEST_TOKEN;
        if token.network != network {
            return Err("the ASIC test token exists only on Chipnet".into());
        }
        decode_compact(target_bits.to_le_bytes(), token.params.compact)?;
        let scripts = payout::scripts(network, payout, None)?;
        let start = unix_now().saturating_sub(START_BEHIND);
        let commitment = Commitment {
            virtual_slot: 0x2000_0001,
            prev: [0; 32],
            payout: [0; 32],
            time: start,
            bits: target_bits.to_le_bytes(),
            nonce: 0,
        }
        .encode();
        Ok(Self {
            network,
            token,
            journal_path,
            destinations: [scripts[0].clone(), scripts[1].clone()],
            thread: Mutex::new(Thread {
                commitment,
                outpoint: OutPoint {
                    txid: double_sha256(&commitment),
                    vout: 0,
                },
                start,
            }),
            queue: Mutex::new(VecDeque::new()),
            won: Mutex::new(VecDeque::new()),
            summary: Mutex::new(HeaderSummary {
                token: token.name,
                bits: target_bits,
                ..HeaderSummary::default()
            }),
        })
    }

    pub fn token(&self) -> &'static HeaderToken {
        self.token
    }

    fn update(&self, change: impl FnOnce(&mut HeaderSummary)) {
        if let Ok(mut summary) = self.summary.lock() {
            change(&mut summary);
        }
    }

    pub fn summary(&self) -> HeaderSummary {
        self.summary
            .lock()
            .map(|summary| summary.clone())
            .unwrap_or_default()
    }

    /// Hands on a win from a device thread without waiting; false when the
    /// queue is full or busy (the win is dropped and counted).
    pub fn submit(&self, win: HeaderShare) -> bool {
        let queued = match self.queue.try_lock() {
            Ok(mut queue) if queue.len() < QUEUE => {
                queue.push_back(win);
                true
            }
            _ => false,
        };
        self.update(|summary| {
            if queued {
                summary.wins += 1;
            } else {
                summary.dropped += 1;
            }
        });
        queued
    }

    /// Whether `script` is a forwarder to one of this server's destinations.
    fn pays_us(&self, script: &[u8]) -> bool {
        self.destinations.iter().any(|destination| {
            forwarder_body(destination).is_ok_and(|body| script.ends_with(&body))
        })
    }

    /// Checks the waiting wins: each thread state is proven once, saved,
    /// and moves the simulated thread. Returns whether the thread moved.
    pub fn check(&self) -> bool {
        let wins: Vec<HeaderShare> = self
            .queue
            .lock()
            .map(|mut queue| queue.drain(..).collect())
            .unwrap_or_default();
        let mut moved = false;
        for win in wins {
            if self.summary().off.is_some() {
                break;
            }
            let Ok(mut thread) = self.thread.lock() else {
                break;
            };
            let anchor = double_sha256(&thread.commitment);
            let fresh = win.header[4..36] == anchor
                && self.won.lock().is_ok_and(|won| !won.contains(&anchor));
            if !fresh {
                self.update(|summary| summary.stale += 1);
                continue;
            }
            // A share the server accepted as a win that fails the
            // covenant's check, or pays another script, is a Pickaxe bug: it
            // is never kept as claimable.
            if !self.pays_us(&win.script)
                || verify(
                    &self.token.params,
                    &thread.commitment,
                    &win.header,
                    TEST_AGE,
                    &win.script,
                )
                .is_err()
            {
                self.update(|summary| {
                    summary.off = Some("a token win failed its own check".into())
                });
                break;
            }
            let record = HeaderWin {
                layout: SAFA_V1,
                header: win.header,
                age: TEST_AGE,
                thread: thread.outpoint,
                script: win.script.clone(),
            };
            if let Err(error) = self.save(&record, &anchor) {
                self.update(|summary| {
                    summary.off = Some(format!("token proofs cannot be saved: {error}"))
                });
                break;
            }
            if let Ok(mut won) = self.won.lock() {
                if won.len() >= RECENT {
                    won.pop_front();
                }
                won.push_back(anchor);
            }
            self.update(|summary| summary.proven += 1);
            // The simulated thread moves to the winning header.
            thread.commitment = win.header;
            thread.outpoint = OutPoint {
                txid: double_sha256(&win.header),
                vout: 0,
            };
            thread.start = unix_now().saturating_sub(START_BEHIND);
            moved = true;
        }
        moved
    }

    /// The job the thread's next win answers.
    pub fn job(&self) -> Result<BchTemplate, String> {
        let thread = self.thread.lock().map_err(|_| "token thread unavailable")?;
        let current = Commitment::decode(&thread.commitment)?;
        let params = &self.token.params;
        let target = next_target(
            &decode_compact(current.bits, params.compact)?,
            TEST_AGE,
            params.daa,
        );
        let bits = encode_compact(&target, params.compact)?;
        let mut full = target.to_bytes_le();
        full.resize(32, 0);
        let target: Hash = full
            .try_into()
            .map_err(|_| "the token's target exceeds 32 bytes")?;
        let anchor = double_sha256(&thread.commitment);
        Ok(BchTemplate::token_only(
            Arc::new(TokenJob {
                token: self.token.name,
                layout: Layout::Safa,
                version_mask: params.version.mask(),
                anchor,
                thread: thread.outpoint,
                age: TEST_AGE,
                mtp: thread.start + START_BEHIND / 2,
            }),
            anchor,
            current.virtual_slot,
            u32::from_le_bytes(bits),
            target,
            thread.start,
            1,
        ))
    }

    /// Adds a proven win to the journal, keeping the newest `KEPT_PROOFS`.
    fn save(&self, win: &HeaderWin, anchor: &Hash) -> Result<(), String> {
        let mut entries = match std::fs::read(&self.journal_path) {
            Ok(bytes) => {
                let saved: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| "the token proof journal is unreadable")?;
                if saved["version"] != 1 || saved["network"] != self.network.as_str() {
                    return Err("the token proof journal is for another network or version".into());
                }
                saved["entries"].as_array().cloned().unwrap_or_default()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        entries.push(serde_json::json!({
            "token": self.token.name,
            "category": hex::encode(self.token.category),
            "mode": "H",
            "anchor": hex::encode(anchor),
            "found_at": unix_now(),
            "proof": hex::encode(win.encode()),
            "status": "proven",
        }));
        let skip = entries.len().saturating_sub(KEPT_PROOFS);
        let entries: Vec<_> = entries.into_iter().skip(skip).collect();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "network": self.network.as_str(),
            "entries": entries,
        }))
        .map_err(|error| error.to_string())?;
        crate::config::write_private_atomic(&self.journal_path, &bytes)
    }
}

/// #### PR #42: an ASIC-exclusive token's work as the server's template
/// source: a new job whenever the thread moves.
pub struct TokenSource {
    work: Arc<HeaderWork>,
    current: Option<BchTemplate>,
    generation: u64,
}

impl TokenSource {
    pub fn new(work: Arc<HeaderWork>) -> Self {
        Self {
            work,
            current: None,
            generation: 0,
        }
    }
}

impl TemplateSource for TokenSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Token
    }

    fn current(&self) -> Option<(u64, &BchTemplate)> {
        self.current
            .as_ref()
            .map(|template| (self.generation, template))
    }

    /// Checks the wins found since; a moved thread needs a new job.
    fn tip_is_current(&mut self) -> Result<bool, String> {
        Ok(!self.work.check() && self.current.is_some())
    }

    fn refresh(&mut self) -> Result<(u64, &BchTemplate), String> {
        let template = self.work.job()?;
        let current = match self.current.take() {
            Some(current) if current.previous_hash == template.previous_hash => current,
            _ => {
                self.generation += 1;
                template
            }
        };
        Ok((self.generation, self.current.insert(current)))
    }

    fn submit_saved(&mut self, _: &PendingBlock) -> SubmissionOutcome {
        SubmissionOutcome::Pending("token work makes no blocks")
    }

    fn reset(&mut self) {}
}

fn unix_now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u32::try_from(elapsed.as_secs()).unwrap_or(u32::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stratum_v2::{
        journal::TestDirectory,
        merge::safa::{proof_passes, ProofRule},
        template_tests::payout as miner,
    };

    /// A win on `work`'s current job from its miner's forwarder: the first
    /// nonce whose header passes.
    fn win(work: &HeaderWork) -> HeaderShare {
        let job = work.job().unwrap();
        let (prefix, suffix) =
            super::super::header::extended_parts(1, &work.destinations[0]).unwrap();
        let mut script = prefix;
        script.extend([0x11; 24]);
        script.extend(suffix);
        let target = num_bigint::BigUint::from_bytes_le(&job.target);
        let mut header = [0; 80];
        header[..4].copy_from_slice(&job.version.to_le_bytes());
        header[4..36].copy_from_slice(&job.previous_hash);
        header[36..68].copy_from_slice(&double_sha256(&script));
        header[68..72].copy_from_slice(&(job.current_time + 1).to_le_bytes());
        header[72..76].copy_from_slice(&job.bits.to_le_bytes());
        let nonce = (0..10_000u32)
            .find(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                proof_passes(&double_sha256(&header), &target, ProofRule::Positive)
            })
            .unwrap();
        header[76..].copy_from_slice(&nonce.to_le_bytes());
        HeaderShare { header, script }
    }

    fn work(directory: &TestDirectory) -> HeaderWork {
        HeaderWork::test_token(
            MiningNetwork::Chipnet,
            directory.0.join("token-proofs.json"),
            0x207f_ffff,
            &miner(),
        )
        .unwrap()
    }

    // #### PR #42
    // What: a proven win moves the simulated thread: the next job's
    // previous hash is the win's HASH256 and the source gives a new
    // generation; the win is saved once (mode H, a HeaderWin), a second win
    // on the old thread is stale, and the test token is refused on mainnet.
    // Look here if: HeaderWork::check or TokenSource changes.
    #[test]
    fn the_simulated_thread_advances_after_each_proven_win() {
        let directory = TestDirectory::new();
        let work = Arc::new(work(&directory));
        let mut source = TokenSource::new(work.clone());
        let (first, template) = source.refresh().unwrap();
        let first_parent = template.previous_hash;
        assert!(template.token_job().is_some());
        assert!(source.tip_is_current().unwrap());
        let won = win(&work);
        assert!(work.submit(won.clone()));
        assert!(work.submit(won.clone()), "the same win again");
        assert!(!source.tip_is_current().unwrap(), "the thread moved");
        let (second, template) = source.refresh().unwrap();
        assert_eq!(second, first + 1);
        assert_eq!(template.previous_hash, double_sha256(&won.header));
        assert_ne!(template.previous_hash, first_parent);
        let summary = work.summary();
        assert_eq!((summary.wins, summary.proven, summary.stale), (2, 1, 1));
        assert!(summary.off.is_none());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.0.join("token-proofs.json")).unwrap())
                .unwrap();
        let entry = &saved["entries"][0];
        assert_eq!(entry["mode"], "H");
        let record =
            HeaderWin::decode(&hex::decode(entry["proof"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(record.header, won.header);
        let (same, _) = source.refresh().unwrap();
        assert_eq!(same, second, "no new job without a move");
        assert!(HeaderWork::test_token(
            MiningNetwork::Mainnet,
            directory.0.join("other.json"),
            0x207f_ffff,
            &miner()
        )
        .is_err());
    }

    // #### PR #42
    // What: a win that pays another script than this server's forwarders,
    // or fails the covenant's check, turns proofs off (a Pickaxe bug) and is
    // never saved; a full queue drops and counts wins.
    // Look here if: the win checks or the queue change.
    #[test]
    fn a_win_paying_another_script_is_never_proven() {
        let directory = TestDirectory::new();
        let work = work(&directory);
        let mut other = win(&work);
        other.script = super::super::header::standard_script(1, &[0; 16], &[0x51]).unwrap();
        work.submit(other);
        assert!(!work.check());
        assert!(work.summary().off.is_some());
        assert!(!directory.0.join("token-proofs.json").exists());
        let full = self::work(&TestDirectory::new());
        for _ in 0..QUEUE {
            assert!(full.submit(win(&full)));
        }
        assert!(!full.submit(win(&full)));
        assert_eq!(full.summary().dropped, 1);
    }
}
