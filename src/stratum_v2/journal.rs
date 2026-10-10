//! #### PR #38
//! Persist complete solved blocks before acknowledging a device. Recovery is
//! bound to the same node, network and payout, and retries never rebuild work.

use super::{channel::ValidatedShare, template::double_sha256};
use crate::{
    config::{self, MiningNetwork},
    donation::bch::BchPayout,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use stratum_core::bitcoin::{consensus, Block};

// Operational storage bounds, not BCH consensus block-size limits. Fail closed
// before ACK if storage is exhausted; never drop an acknowledged pending block.
const MAX_PENDING: usize = 64;
const MAX_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = (MAX_BLOCK_BYTES as u64) * 2 + 1024 * 1024;
const MAX_RECEIPTS: usize = 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingBlock {
    pub hash: String,
    pub block: String,
    // Missing only in pre-donation journals. Recovery submits those exact
    // bytes; it must never silently rebuild an already solved coinbase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payout: Option<BchPayout>,
    /// #### PR #40: in a public pool, the miner this block pays when it is
    /// not the configured payout, and the pool's fee address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    context: String,
    pending: Vec<PendingBlock>,
    completed: Vec<String>,
    accepted: u64,
    rejected: u64,
}

// No Debug: blocks contain the configured payout script.
pub struct Journal {
    path: PathBuf,
    _lock: File,
    network: MiningNetwork,
    binding: JournalBinding,
    state: State,
}

/// #### PR #42: what a journal's blocks are checked against.
pub enum JournalBinding {
    /// This server's own blocks: they pay its payout (with the donation and
    /// any pool fee).
    Payout {
        payout: String,
        scripts: Vec<Vec<u8>>,
    },
    /// Blocks relayed for a Template Distribution client (a pool): they pay
    /// the client's coinbase, which this server neither builds nor knows.
    Relay,
    /// #### PR #42: a Job Declaration client's blocks: they pay the pool's
    /// outputs (the miner, the pool's fee and the donation).
    Declared,
}

/// #### PR #40
/// What a journal belongs to: its network and payout script, and before
/// PR #40 also the node it was written with (`source`).
fn binding(source: Option<&[u8; 32]>, network: MiningNetwork, script: &[u8]) -> String {
    let mut context = match source {
        Some(source) => source.to_vec(),
        None => b"pickaxe block journal: network and payout".to_vec(),
    };
    context.extend(network.as_str().as_bytes());
    context.extend(script);
    hex::encode(double_sha256(&context))
}

#[cfg(test)]
pub(super) fn legacy_binding(source: &[u8; 32], network: MiningNetwork, script: &[u8]) -> String {
    binding(Some(source), network, script)
}

impl Journal {
    pub fn open(
        path: &Path,
        network: MiningNetwork,
        payout: &str,
        legacy_sources: &[[u8; 32]],
    ) -> Result<Self, String> {
        let payout = config::validate_coinbase_address(network, payout)?;
        let scripts = super::payout::scripts(network, &payout, None)?;
        // #### PR #40
        // What: the journal belongs to its network and payout, no longer to
        // the one node it was first written with.
        // Why: with several nodes the server moves to the next when one stops
        // answering, and it may start on its second node when the first is
        // down; a journal bound to the first node refused to open then. Its
        // blocks are whole blocks, valid at any node of the same network.
        // A journal written under the old binding opens when its node is
        // still configured (`legacy_sources`), and is rebound.
        // Look here if: a journal is refused as another network's or
        // payout's, or "a node that is no longer configured".
        let context = binding(None, network, &scripts[0]);
        let legacy: Vec<String> = legacy_sources
            .iter()
            .map(|source| binding(Some(source), network, &scripts[0]))
            .collect();
        Self::open_bound(
            path,
            network,
            JournalBinding::Payout { payout, scripts },
            context,
            &legacy,
        )
    }

    /// #### PR #42: the relay journal
    /// What: `<config>.sv2-relay-blocks.json`, owner-only like the block
    /// journal, holds the blocks Template Distribution clients (pools) find
    /// on this server's templates, until the node answers. Its binding names
    /// the network alone, and its blocks are checked for size, merkle root,
    /// proof of work and no witness, never for a payout.
    /// Why: an SRI pool sends its blocks only to its template provider, so
    /// this file is the block's only way to the chain, and the PR #38 rule
    /// applies: persist first, then submit, retried until the node answers.
    /// These blocks pay the pool, so they never enter the payout-bound
    /// journal.
    /// Look here if: a pool's block is missing on chain, or the relay
    /// journal is refused as another network's.
    pub fn open_relay(path: &Path, network: MiningNetwork) -> Result<Self, String> {
        let mut context = b"pickaxe relay journal: ".to_vec();
        context.extend(network.as_str().as_bytes());
        let context = hex::encode(double_sha256(&context));
        Self::open_bound(path, network, JournalBinding::Relay, context, &[])
    }

    /// #### PR #42: the JD journal
    /// What: `<config>.sv2-jd-blocks.json`, owner-only, holds a Job
    /// Declaration client's own blocks until its node answers. They pay the
    /// pool's outputs, so the journal checks size, merkle root, proof of work
    /// and no witness, never this server's payout; it opens apart from the
    /// solo block journal, which older binaries read.
    /// Why: the PR #38 rule: a found block is saved before anything else, and
    /// Coinbase-only blocks reach the chain only through the miner's node.
    /// Look here if: a Job Declaration block is missing on chain.
    pub fn open_declared(path: &Path, network: MiningNetwork) -> Result<Self, String> {
        let mut context = b"pickaxe declared journal: ".to_vec();
        context.extend(network.as_str().as_bytes());
        let context = hex::encode(double_sha256(&context));
        Self::open_bound(path, network, JournalBinding::Declared, context, &[])
    }

    fn open_bound(
        path: &Path,
        network: MiningNetwork,
        binding: JournalBinding,
        context: String,
        legacy: &[String],
    ) -> Result<Self, String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|_| "cannot create block journal directory")?;
        let lock_path = path.with_extension("blocks-lock");
        regular_if_present(&lock_path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|_| "cannot open block journal lock")?;
        lock.try_lock()
            .map_err(|_| "block journal is already in use or cannot be locked")?;
        config::restrict_private_config(&lock_path)
            .map_err(|_| "cannot protect block journal lock")?;
        regular_if_present(path)?;
        let mut rebound = false;
        let state = if path.exists() {
            config::restrict_private_config(path).map_err(|_| "cannot protect block journal")?;
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(|_| "cannot open block journal")?
                .take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "cannot read block journal")?;
            if bytes.len() as u64 > MAX_FILE_BYTES {
                return Err("block journal exceeds storage budget".into());
            }
            let mut state: State = serde_json::from_slice(&bytes)
                .map_err(|_| "invalid block journal; refusing to replace it")?;
            let legacy = legacy.contains(&state.context);
            if state.version != 1 || (state.context != context && !legacy) {
                return Err(
                    "block journal belongs to another network or payout, or to a node that is \
                     no longer configured"
                        .into(),
                );
            }
            rebound = state.context != context;
            state.context = context.clone();
            state
        } else {
            State {
                version: 1,
                context,
                pending: Vec::new(),
                completed: Vec::new(),
                accepted: 0,
                rejected: 0,
            }
        };
        validate_state(&state, network, &binding)?;
        let journal = Self {
            path: path.to_owned(),
            _lock: lock,
            network,
            binding,
            state,
        };
        if !path.exists() || rebound {
            journal.persist(&journal.state)?;
        }
        Ok(journal)
    }

    pub fn pending_hashes(&self) -> Vec<String> {
        self.state.pending.iter().map(|b| b.hash.clone()).collect()
    }
    pub fn pending(&self, hash: &str) -> Option<PendingBlock> {
        self.state.pending.iter().find(|b| b.hash == hash).cloned()
    }
    pub fn counts(&self) -> (usize, u64, u64) {
        (
            self.state.pending.len(),
            self.state.accepted,
            self.state.rejected,
        )
    }

    pub fn enqueue(&mut self, share: &ValidatedShare) -> Result<bool, String> {
        if !share.block {
            return Err("cannot journal a non-block share".into());
        }
        let bytes = share.template.block(&share.coinbase, share.header)?;
        let (payout, scripts) = match &self.binding {
            JournalBinding::Payout { payout, scripts } => (payout, scripts),
            JournalBinding::Declared => {
                let hash = validate_block(&bytes, None)?;
                return self.push(PendingBlock {
                    hash,
                    block: hex::encode(bytes),
                    payout: None,
                    miner: None,
                    operator: None,
                });
            }
            JournalBinding::Relay => {
                return Err("the relay journal takes relayed blocks only".into())
            }
        };
        let miner = (&share.miner != payout).then(|| share.miner.clone());
        let scripts = block_scripts(
            self.network,
            scripts,
            miner.as_deref(),
            share.operator.as_deref(),
        )?;
        let hash = validate_block(&bytes, Some((scripts.as_slice(), Some(share.payout))))?;
        self.push(PendingBlock {
            hash,
            block: hex::encode(bytes),
            payout: Some(share.payout),
            miner,
            operator: share.operator.clone(),
        })
    }

    /// #### PR #42: saves a block found on a Full-Template declared job
    /// (a client's share, or its PushSolution); false when it is already
    /// saved. Only a JD journal takes them.
    pub fn enqueue_declared(&mut self, bytes: &[u8]) -> Result<bool, String> {
        if !matches!(self.binding, JournalBinding::Declared) {
            return Err("declared blocks go to the JD journal".into());
        }
        let hash = validate_block(bytes, None)?;
        self.push(PendingBlock {
            hash,
            block: hex::encode(bytes),
            payout: None,
            miner: None,
            operator: None,
        })
    }

    /// #### PR #42: saves a block a Template Distribution client found;
    /// false when it is already saved. Only the relay journal takes them.
    pub fn enqueue_relayed(&mut self, bytes: &[u8]) -> Result<bool, String> {
        if !matches!(self.binding, JournalBinding::Relay) {
            return Err("relayed blocks go to the relay journal".into());
        }
        let hash = validate_block(bytes, None)?;
        self.push(PendingBlock {
            hash,
            block: hex::encode(bytes),
            payout: None,
            miner: None,
            operator: None,
        })
    }

    fn push(&mut self, block: PendingBlock) -> Result<bool, String> {
        if self.state.completed.contains(&block.hash)
            || self.state.pending.iter().any(|b| b.hash == block.hash)
        {
            return Ok(false);
        }
        let mut next = self.state.clone();
        next.pending.push(block);
        // Existing entries were checked at load/enqueue. Do not hash every
        // stored full block again while another device waits to journal work.
        validate_limits(&next)?;
        self.persist(&next)?;
        self.state = next;
        Ok(true)
    }

    pub fn finish(&mut self, hash: &str, accepted: bool) -> Result<(), String> {
        let Some(index) = self.state.pending.iter().position(|b| b.hash == hash) else {
            return Ok(());
        };
        let mut next = self.state.clone();
        next.pending.remove(index);
        next.completed.push(hash.to_owned());
        if next.completed.len() > MAX_RECEIPTS {
            next.completed.remove(0);
        }
        if accepted {
            next.accepted = next.accepted.saturating_add(1);
        } else {
            next.rejected = next.rejected.saturating_add(1);
        }
        // Persist receipt and pending removal in one atomic snapshot. A crash
        // before this point replays identical bytes, never a new coinbase.
        self.persist(&next)?;
        self.state = next;
        Ok(())
    }

    fn persist(&self, state: &State) -> Result<(), String> {
        let bytes = serde_json::to_vec(state).map_err(|_| "cannot encode block journal")?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("block journal exceeds storage budget".into());
        }
        let temp = self
            .path
            .with_extension(format!("blocks-tmp-{:016x}", rand::random::<u64>()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options
                .open(&temp)
                .map_err(|_| "cannot create block journal update")?;
            config::restrict_private_config(&temp)
                .map_err(|_| "cannot protect block journal update")?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| "cannot flush block journal update")?;
            drop(file);
            fs::rename(&temp, &self.path).map_err(|_| "cannot install block journal update")?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .and_then(|f| f.sync_all())
                .map_err(|_| "cannot sync block journal")?;
            #[cfg(unix)]
            {
                let parent = self
                    .path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                File::open(parent)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| "cannot sync block journal directory")?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

fn regular_if_present(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err("block journal path is not a regular file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("cannot inspect block journal path".into()),
    }
}

/// #### PR #40
/// The recipients a recorded block pays: the configured payout and the
/// donation, or in a public pool the block's own miner, plus any fee address.
fn block_scripts(
    network: MiningNetwork,
    configured: &[Vec<u8>],
    miner: Option<&str>,
    operator: Option<&str>,
) -> Result<Vec<Vec<u8>>, String> {
    match (miner, operator) {
        (None, None) => Ok(configured.to_vec()),
        (Some(miner), operator) => super::payout::scripts(network, miner, operator),
        (None, Some(operator)) => {
            let mut scripts = configured.to_vec();
            scripts.extend(
                super::payout::scripts(network, operator, None)?
                    .into_iter()
                    .take(1),
            );
            Ok(scripts)
        }
    }
}

fn validate_state(
    state: &State,
    network: MiningNetwork,
    binding: &JournalBinding,
) -> Result<(), String> {
    validate_limits(state)?;
    let mut seen = std::collections::HashSet::new();
    for hash in &state.completed {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !seen.insert(hash.clone())
        {
            return Err("invalid block journal receipt".into());
        }
    }
    let mut total = 0usize;
    for pending in &state.pending {
        total = total
            .checked_add(pending.block.len())
            .ok_or("block journal size overflow")?;
        if total > MAX_BLOCK_BYTES * 2 {
            return Err("pending blocks exceed journal storage budget".into());
        }
        let bytes = hex::decode(&pending.block).map_err(|_| "invalid journal block encoding")?;
        let hash = match binding {
            JournalBinding::Payout { scripts, .. } => {
                let scripts = block_scripts(
                    network,
                    scripts,
                    pending.miner.as_deref(),
                    pending.operator.as_deref(),
                )?;
                validate_block(&bytes, Some((scripts.as_slice(), pending.payout)))?
            }
            // #### PR #42: a relayed or declared block names no payout.
            JournalBinding::Relay | JournalBinding::Declared => {
                if pending.payout.is_some() || pending.miner.is_some() || pending.operator.is_some()
                {
                    return Err("invalid relayed journal block".into());
                }
                validate_block(&bytes, None)?
            }
        };
        if hash != pending.hash || !seen.insert(pending.hash.clone()) {
            return Err("invalid or duplicate journal block".into());
        }
    }
    Ok(())
}

fn validate_limits(state: &State) -> Result<(), String> {
    if state.pending.len() > MAX_PENDING || state.completed.len() > MAX_RECEIPTS {
        return Err("block journal entry limit exceeded".into());
    }
    let total = state
        .pending
        .iter()
        .try_fold(0usize, |sum, block| sum.checked_add(block.block.len()))
        .ok_or("block journal size overflow")?;
    if total > MAX_BLOCK_BYTES * 2 {
        return Err("pending blocks exceed journal storage budget".into());
    }
    Ok(())
}

/// The block's hash after its checks. `expected` holds the payout scripts
/// and policy a block of this server must pay; a relayed block (#### PR #42)
/// has none, and its coinbase's outputs are the client's.
fn validate_block(
    bytes: &[u8],
    expected: Option<(&[Vec<u8>], Option<BchPayout>)>,
) -> Result<String, String> {
    if bytes.len() > MAX_BLOCK_BYTES {
        return Err("block exceeds journal storage budget".into());
    }
    let block: Block =
        consensus::deserialize(bytes).map_err(|_| "invalid journal block serialization")?;
    if !block.check_merkle_root() || block.header.validate_pow(block.header.target()).is_err() {
        return Err("journal block fails merkle root or proof of work".into());
    }
    let coinbase = block
        .txdata
        .first()
        .ok_or("journal block has no coinbase")?;
    if block
        .txdata
        .iter()
        .any(|tx| tx.input.iter().any(|input| !input.witness.is_empty()))
        || !coinbase.is_coinbase()
    {
        return Err("journal block payout or transaction encoding does not match".into());
    }
    let Some((scripts, payout)) = expected else {
        return Ok(block.block_hash().to_string());
    };
    let total = coinbase
        .output
        .iter()
        .try_fold(0u64, |sum, output| sum.checked_add(output.value.to_sat()))
        .filter(|sum| *sum <= 21_000_000 * 100_000_000)
        .ok_or("invalid journal coinbase value")?;
    let expected = match payout {
        Some(policy) => super::payout::outputs(total, scripts, policy),
        None => vec![(total, scripts[0].clone())],
    };
    // #### PR #42: merge-mining outputs in a journaled block
    // What: a zero-value commitment as output 0 and, after it, zero-value
    // ticket outputs at the end are set aside before the payouts are
    // compared; anything else (a valued or malformed output 0, a valued or
    // unknown trailing output, tickets without a commitment) still refuses
    // the block. The coinbase total is unchanged, since both are worth 0.
    // Why: merge-mined tokens add these outputs to the coinbase; the proof
    // of work and the merkle root already bind their bytes.
    // Look here if: a block with tokens is refused by the journal, or the
    // server stops with "cannot persist solved block".
    let mut outputs = coinbase.output.as_slice();
    if let Some((first, rest)) = outputs.split_first() {
        if first.value.to_sat() == 0
            && super::merge::commitment::AuxCommitment::parse_script(first.script_pubkey.as_bytes())
                .is_some()
        {
            outputs = rest;
            while let Some((last, rest)) = outputs.split_last() {
                if last.value.to_sat() != 0
                    || !super::merge::registry::is_ticket_script(last.script_pubkey.as_bytes())
                {
                    break;
                }
                outputs = rest;
            }
        }
    }
    let actual = outputs
        .iter()
        .map(|o| (o.value.to_sat(), o.script_pubkey.to_bytes()))
        .collect::<Vec<_>>();
    if actual != expected {
        return Err("journal block payout or transaction encoding does not match".into());
    }
    Ok(block.block_hash().to_string())
}

#[cfg(test)]
pub(crate) struct TestDirectory(pub PathBuf);

#[cfg(test)]
impl TestDirectory {
    pub fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pickaxe-sv2-test-{:032x}", rand::random::<u128>()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    pub fn journal(&self) -> PathBuf {
        self.0.join("blocks.json")
    }
}

#[cfg(test)]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(self
            .0
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pickaxe-sv2-test-"));
        let _ = fs::remove_dir_all(&self.0);
    }
}
