//! #### PR #38
//! Validated BCH full templates. Preserve the node's complete CTOR transaction
//! list and target; do not inherit Bitcoin witness or fixed block-size rules.

use crate::config::{validate_payout_address, MiningNetwork};
use crate::tx::cashaddr_to_p2pkh_locking;
use serde_json::Value;
use sha2::{Digest, Sha256};
use stratum_core::bitcoin::{consensus, Transaction};

/// Hash bytes in serialized/internal order, not RPC display order.
pub type Hash = [u8; 32];

#[derive(Clone, Debug)]
pub struct BchTemplate {
    pub previous_hash: Hash,
    pub version: u32,
    pub bits: u32,
    pub target: Hash,
    pub min_time: u32,
    pub current_time: u32,
    pub height: u32,
    pub size_limit: u64,
    pub coinbase_value: u64,
    coinbase_flags: Vec<u8>,
    transactions: Vec<Vec<u8>>,
    transaction_hashes: Vec<Hash>,
}

#[derive(Clone, Debug)]
pub struct Coinbase {
    pub bytes: Vec<u8>,
    pub merkle_root: Hash,
}

/// Payout and transaction order stay server-owned. Only the fixed-length
/// extranonce hole is delegated to an extended channel.
#[derive(Clone, Debug)]
pub struct CoinbaseParts {
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub merkle_path: Vec<Hash>,
}

impl BchTemplate {
    /// Accept a full BCHN getblocktemplate response. Light templates require a
    /// distinct source-bound job and must not enter the full-block serializer.
    pub fn from_rpc(value: &Value) -> Result<Self, String> {
        if value.get("default_witness_commitment").is_some()
            || value.get("job_id").is_some()
            || value.get("merkle").is_some()
        {
            return Err("expected a full BCH template without witness commitment".into());
        }
        if let Some(rules) = value.get("rules") {
            let rules = rules.as_array().ok_or("invalid template rules")?;
            if rules.iter().any(|rule| {
                rule.as_str()
                    .is_none_or(|rule| rule.trim_start_matches('!') == "segwit")
            }) {
                return Err("BCH templates must not require segwit".into());
            }
        }
        let bits_text = text(value, "bits")?;
        if bits_text.len() != 8 {
            return Err("bits must contain four bytes".into());
        }
        let bits = u32::from_str_radix(bits_text, 16).map_err(|_| "invalid bits")?;
        let target = compact_target(bits)?;
        if display_hash(text(value, "target")?)? != target {
            return Err("template target disagrees with bits".into());
        }
        let size_limit = number(value, "sizelimit")?;
        if size_limit < 180 {
            return Err("template block size limit is too small".into());
        }
        let mut transactions = Vec::new();
        let mut transaction_hashes = Vec::new();
        let mut previous_display = None;
        let mut total_size = 80u64;
        let entries = value
            .get("transactions")
            .and_then(Value::as_array)
            .ok_or("full template omitted transactions")?;
        for entry in entries {
            let encoded = text(entry, "data")?;
            if encoded.len() as u64 > size_limit.saturating_mul(2) {
                return Err("template transaction exceeds block size limit".into());
            }
            let bytes = hex::decode(encoded).map_err(|_| "invalid transaction hex")?;
            total_size = total_size
                .checked_add(bytes.len() as u64)
                .ok_or("template size overflow")?;
            if total_size > size_limit {
                return Err("template transactions exceed block size limit".into());
            }
            // Bitcoin's decoder treats CashTokens prefixes as opaque output
            // script bytes. Reject witness serialization explicitly: BCH txids
            // commit to all bytes. No Bitcoin consensus validation is used.
            let tx: Transaction =
                consensus::deserialize(&bytes).map_err(|_| "malformed template transaction")?;
            if tx.is_coinbase()
                || tx.input.is_empty()
                || tx.input.iter().any(|input| !input.witness.is_empty())
                || bytes.get(4) == Some(&0)
            {
                return Err("template contains coinbase, empty inputs or witness encoding".into());
            }
            let hash = double_sha256(&bytes);
            if display_hash(text(entry, "txid")?)? != hash {
                return Err("transaction bytes disagree with template txid".into());
            }
            let mut display = hash;
            display.reverse();
            if previous_display.is_some_and(|previous| previous >= display) {
                return Err("template transactions are duplicated or not in CTOR order".into());
            }
            previous_display = Some(display);
            transactions.push(bytes);
            transaction_hashes.push(hash);
        }
        let min_time = word(value, "mintime")?;
        let current_time = word(value, "curtime")?;
        if current_time < min_time {
            return Err("template time is below mintime".into());
        }
        let coinbase_value = number(value, "coinbasevalue")?;
        if coinbase_value > 21_000_000 * 100_000_000 {
            return Err("coinbase value exceeds BCH monetary range".into());
        }
        let mut coinbase_flags = Vec::new();
        if let Some(aux) = value.get("coinbaseaux") {
            for flag in aux.as_object().ok_or("invalid coinbaseaux")?.values() {
                let encoded = flag.as_str().ok_or("invalid coinbase flag")?;
                if encoded.len() > 200 {
                    return Err("coinbase flags exceed script limit".into());
                }
                coinbase_flags.extend(hex::decode(encoded).map_err(|_| "invalid coinbase flags")?);
            }
        }
        if coinbase_flags.len() > 100 {
            return Err("coinbase flags exceed script limit".into());
        }
        Ok(Self {
            previous_hash: display_hash(text(value, "previousblockhash")?)?,
            version: word(value, "version")?,
            bits,
            target,
            min_time,
            current_time,
            height: word(value, "height")?,
            size_limit,
            coinbase_value,
            coinbase_flags,
            transactions,
            transaction_hashes,
        })
    }

    /// Build one channel's coinbase. The caller assigns a unique extranonce;
    /// devices never control the payout or the transaction ordering.
    pub fn coinbase(
        &self,
        network: MiningNetwork,
        payout: &str,
        extranonce: &[u8],
    ) -> Result<Coinbase, String> {
        let canonical = validate_payout_address(network, payout)?;
        let locking = cashaddr_to_p2pkh_locking(&canonical)?;
        if extranonce.len() > 64 {
            return Err("extranonce exceeds coinbase budget".into());
        }
        let mut script = height_script(self.height);
        script.extend_from_slice(&self.coinbase_flags);
        script.extend_from_slice(extranonce);
        // Keep coinbases at least 100 bytes, valid before and after BCH's
        // minimum-transaction-size change. This is not a block-size cap.
        script.resize(script.len().max(15), 0);
        if script.len() > 100 {
            return Err("coinbase script exceeds 100 bytes".into());
        }
        let mut bytes = 2u32.to_le_bytes().to_vec();
        bytes.push(1);
        bytes.extend_from_slice(&[0; 32]);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        compact_size(script.len(), &mut bytes);
        bytes.extend(script);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&self.coinbase_value.to_le_bytes());
        compact_size(locking.len(), &mut bytes);
        bytes.extend(locking);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        self.check_block_size(bytes.len())?;
        let mut hashes = Vec::with_capacity(self.transaction_hashes.len() + 1);
        hashes.push(double_sha256(&bytes));
        hashes.extend_from_slice(&self.transaction_hashes);
        Ok(Coinbase {
            bytes,
            merkle_root: merkle_root(hashes),
        })
    }

    pub fn coinbase_parts(
        &self,
        network: MiningNetwork,
        payout: &str,
        extranonce_len: usize,
    ) -> Result<CoinbaseParts, String> {
        if extranonce_len > 64 {
            return Err("extranonce exceeds coinbase budget".into());
        }
        let coinbase = self.coinbase(network, payout, &vec![0; extranonce_len])?;
        // The coinbase script is at most 100 bytes, so its CompactSize is one byte.
        let offset =
            4 + 1 + 32 + 4 + 1 + height_script(self.height).len() + self.coinbase_flags.len();
        let mut hashes = vec![[0; 32]];
        hashes.extend_from_slice(&self.transaction_hashes);
        let mut path = Vec::new();
        while hashes.len() > 1 {
            if hashes.len() % 2 == 1 {
                hashes.push(*hashes.last().unwrap());
            }
            path.push(hashes[1]);
            hashes = hashes
                .chunks_exact(2)
                .map(|pair| {
                    let mut bytes = [0; 64];
                    bytes[..32].copy_from_slice(&pair[0]);
                    bytes[32..].copy_from_slice(&pair[1]);
                    double_sha256(&bytes)
                })
                .collect();
        }
        Ok(CoinbaseParts {
            prefix: coinbase.bytes[..offset].to_vec(),
            suffix: coinbase.bytes[offset + extranonce_len..].to_vec(),
            merkle_path: path,
        })
    }

    pub fn header(
        &self,
        coinbase: &Coinbase,
        version: u32,
        time: u32,
        nonce: u32,
    ) -> Result<[u8; 80], String> {
        if time < self.min_time {
            return Err("header time is below mintime".into());
        }
        let mut header = [0; 80];
        header[..4].copy_from_slice(&version.to_le_bytes());
        header[4..36].copy_from_slice(&self.previous_hash);
        header[36..68].copy_from_slice(&coinbase.merkle_root);
        header[68..72].copy_from_slice(&time.to_le_bytes());
        header[72..76].copy_from_slice(&self.bits.to_le_bytes());
        header[76..].copy_from_slice(&nonce.to_le_bytes());
        Ok(header)
    }

    /// Build only a full block; no light-job fallback can truncate its tx list.
    pub fn block(&self, coinbase: &Coinbase, header: [u8; 80]) -> Result<Vec<u8>, String> {
        self.check_block_size(coinbase.bytes.len())?;
        let mut hashes = vec![double_sha256(&coinbase.bytes)];
        hashes.extend_from_slice(&self.transaction_hashes);
        if header[4..36] != self.previous_hash
            || header[36..68] != merkle_root(hashes)
            || header[72..76] != self.bits.to_le_bytes()
        {
            return Err("block header does not belong to this template".into());
        }
        let mut block = header.to_vec();
        compact_size(self.transactions.len() + 1, &mut block);
        block.extend_from_slice(&coinbase.bytes);
        for tx in &self.transactions {
            block.extend_from_slice(tx);
        }
        Ok(block)
    }

    pub fn transaction_count(&self) -> usize {
        self.transactions.len() + 1
    }

    fn check_block_size(&self, coinbase_size: usize) -> Result<(), String> {
        let mut count = Vec::new();
        compact_size(self.transaction_count(), &mut count);
        let size = 80u64
            + count.len() as u64
            + coinbase_size as u64
            + self
                .transactions
                .iter()
                .map(|tx| tx.len() as u64)
                .sum::<u64>();
        if size > self.size_limit {
            return Err("coinbase exceeds template block size budget".into());
        }
        Ok(())
    }
}

pub fn double_sha256(bytes: &[u8]) -> Hash {
    Sha256::digest(Sha256::digest(bytes)).into()
}

/// Decode nBits with Bitcoin/BCH's sign, zero and overflow checks. The node
/// computes ASERT; callers must not substitute Bitcoin's difficulty epochs.
pub fn compact_target(bits: u32) -> Result<Hash, String> {
    let size = bits >> 24;
    let word = bits & 0x007f_ffff;
    if word == 0
        || bits & 0x0080_0000 != 0
        || size > 34
        || (word > 0xff && size > 33)
        || (word > 0xffff && size > 32)
    {
        return Err("invalid compact target".into());
    }
    let value = if size <= 3 {
        num_bigint::BigUint::from(word >> (8 * (3 - size)))
    } else {
        num_bigint::BigUint::from(word) << (8 * (size - 3)) as usize
    };
    let bytes = value.to_bytes_le();
    if bytes.len() > 32 || bytes.iter().all(|&b| b == 0) {
        return Err("invalid compact target".into());
    }
    let mut result = [0; 32];
    result[..bytes.len()].copy_from_slice(&bytes);
    Ok(result)
}

pub fn meets_target(hash: &Hash, target: &Hash) -> bool {
    hash.iter().rev().cmp(target.iter().rev()).is_le()
}

fn merkle_root(mut hashes: Vec<Hash>) -> Hash {
    while hashes.len() > 1 {
        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }
        hashes = hashes
            .chunks_exact(2)
            .map(|pair| {
                let mut joined = [0; 64];
                joined[..32].copy_from_slice(&pair[0]);
                joined[32..].copy_from_slice(&pair[1]);
                double_sha256(&joined)
            })
            .collect();
    }
    hashes[0]
}

fn display_hash(value: &str) -> Result<Hash, String> {
    let mut bytes: Hash = hex::decode(value)
        .map_err(|_| "invalid hash hex")?
        .try_into()
        .map_err(|_| "hash must have 32 bytes")?;
    bytes.reverse();
    Ok(bytes)
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("template omitted {key}"))
}
fn number(value: &Value, key: &str) -> Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("invalid template {key}"))
}
fn word(value: &Value, key: &str) -> Result<u32, String> {
    u32::try_from(number(value, key)?).map_err(|_| format!("template {key} exceeds u32"))
}
fn compact_size(value: usize, out: &mut Vec<u8>) {
    match value {
        0..=252 => out.push(value as u8),
        253..=65535 => {
            out.push(253);
            out.extend_from_slice(&(value as u16).to_le_bytes());
        }
        65536..=4294967295 => {
            out.push(254);
            out.extend_from_slice(&(value as u32).to_le_bytes());
        }
        _ => {
            out.push(255);
            out.extend_from_slice(&(value as u64).to_le_bytes());
        }
    }
}
fn height_script(height: u32) -> Vec<u8> {
    if height == 0 {
        return vec![0];
    }
    if height <= 16 {
        return vec![0x50 + height as u8];
    }
    let mut bytes = height.to_le_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.last().unwrap() & 0x80 != 0 {
        bytes.push(0);
    }
    let mut script = vec![bytes.len() as u8];
    script.extend(bytes);
    script
}
