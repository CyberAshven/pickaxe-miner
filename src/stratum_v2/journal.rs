//! #### PR #38
//! Persist complete solved blocks before acknowledging a device. Recovery is
//! bound to the same node, network and payout, and retries never rebuild work.

use super::{channel::ValidatedShare, template::double_sha256};
use crate::{
    config::{self, MiningNetwork},
    tx::cashaddr_to_p2pkh_locking,
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
    script: Vec<u8>,
    state: State,
}

impl Journal {
    pub fn open(
        path: &Path,
        network: MiningNetwork,
        payout: &str,
        source: [u8; 32],
    ) -> Result<Self, String> {
        let payout = config::validate_payout_address(network, payout)?;
        let script = cashaddr_to_p2pkh_locking(&payout)?;
        let mut context = source.to_vec();
        context.extend(network.as_str().as_bytes());
        context.extend(&script);
        let context = hex::encode(double_sha256(&context));
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
            let state: State = serde_json::from_slice(&bytes)
                .map_err(|_| "invalid block journal; refusing to replace it")?;
            if state.version != 1 || state.context != context {
                return Err("block journal belongs to another node, network or payout".into());
            }
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
        validate_state(&state, &script)?;
        let journal = Self {
            path: path.to_owned(),
            _lock: lock,
            script,
            state,
        };
        if !path.exists() {
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
        let hash = validate_block(&bytes, &self.script)?;
        if self.state.completed.contains(&hash) || self.state.pending.iter().any(|b| b.hash == hash)
        {
            return Ok(false);
        }
        let mut next = self.state.clone();
        next.pending.push(PendingBlock {
            hash,
            block: hex::encode(bytes),
        });
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

fn validate_state(state: &State, script: &[u8]) -> Result<(), String> {
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
        if validate_block(&bytes, script)? != pending.hash || !seen.insert(pending.hash.clone()) {
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

fn validate_block(bytes: &[u8], script: &[u8]) -> Result<String, String> {
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
    if !coinbase.is_coinbase()
        || coinbase.output.len() != 1
        || coinbase.output[0].script_pubkey.as_bytes() != script
        || block
            .txdata
            .iter()
            .any(|tx| tx.input.iter().any(|input| !input.witness.is_empty()))
    {
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
