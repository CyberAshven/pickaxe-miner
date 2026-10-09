//! #### PR #38
//! Validated BCH full templates. Preserve the node's complete CTOR transaction
//! list and target; do not inherit Bitcoin witness or fixed block-size rules.

use super::merge::set::{AuxJob, AuxOutputs, TokenSet};
use crate::config::MiningNetwork;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;
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
    /// #### PR #42: the coinbase's merkle branch (index 0), computed once
    /// from the transactions' hashes, which nothing else needs.
    merkle_path: Arc<[Hash]>,
    /// #### PR #42: the merge-mined tokens of jobs built from this template.
    tokens: Option<Arc<TokenSet>>,
    /// #### PR #42: a custom job's template (Job Declaration), which checks
    /// headers but has no transactions to build a block with.
    header_only: bool,
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
            merkle_path: coinbase_path(&transaction_hashes).into(),
            transactions,
            tokens: None,
            header_only: false,
        })
    }

    /// #### PR #42
    /// A Coinbase-only custom job's template: the pool's parent, bits,
    /// target, height and limits, with the client's version, start time and
    /// merkle path. The pool never sees the client's transactions, so it can
    /// check headers and shares against it but never build its block.
    pub fn custom(
        context: &BchTemplate,
        version: u32,
        min_ntime: u32,
        merkle_path: Vec<Hash>,
    ) -> Self {
        Self {
            previous_hash: context.previous_hash,
            version,
            bits: context.bits,
            target: context.target,
            min_time: min_ntime,
            current_time: min_ntime,
            height: context.height,
            size_limit: context.size_limit,
            coinbase_value: context.coinbase_value,
            coinbase_flags: Vec::new(),
            transactions: Vec::new(),
            merkle_path: merkle_path.into(),
            tokens: None,
            header_only: true,
        }
    }

    /// #### PR #42: a custom job's template, without transactions.
    pub fn is_header_only(&self) -> bool {
        self.header_only
    }

    /// #### PR #40
    /// Writes the pool's name (`--pool-tag`) into the coinbase script of every
    /// block built from this template, after the node's flags and before the
    /// extranonce; the coinbase script stays within 100 bytes, which
    /// `coinbase_with_payout` checks.
    pub fn tag(&mut self, tag: &[u8]) {
        self.coinbase_flags.extend_from_slice(tag);
    }

    /// #### PR #42
    /// Attaches the token set to merge-mine in jobs built from this template.
    /// It only feeds `aux_job`: a coinbase carries the commitment and tickets
    /// only when the caller passes that job's `outputs` to
    /// `coinbase_with_aux` or `coinbase_parts_with_aux`, which do not check
    /// them against this set. `coinbase_with_payout` and
    /// `coinbase_parts_with_payout` always build token-free coinbases.
    /// Without a call (no token registered) jobs and coinbases stay as they
    /// were.
    pub fn commit(&mut self, set: Arc<TokenSet>) {
        self.tokens = Some(set);
    }

    pub fn tokens(&self) -> Option<&Arc<TokenSet>> {
        self.tokens.as_ref()
    }

    /// #### PR #42
    /// One job's merge-mining for this template's token set, or `None`
    /// without one. Its leaves bind the job's beneficiary (the miner, or
    /// the donation or operator in their work jobs) and the donation's
    /// split, and its tickets follow the job's payout outputs.
    pub fn aux_job(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        policy: crate::donation::bch::BchPayout,
    ) -> Result<Option<AuxJob>, String> {
        let Some(set) = &self.tokens else {
            return Ok(None);
        };
        let scripts = super::payout::scripts(network, payout, operator)?;
        let outputs = super::payout::outputs(self.coinbase_value, &scripts, policy);
        let first_ticket_vout =
            u32::try_from(outputs.len() + 1).map_err(|_| "too many coinbase outputs")?;
        AuxJob::build(
            set,
            super::payout::beneficiary(&scripts, policy),
            super::payout::token_split(policy, &scripts[1]),
            first_ticket_vout,
        )
        .map(Some)
    }

    /// Build one channel's coinbase. The caller assigns a unique extranonce;
    /// devices never control the payout or the transaction ordering.
    pub fn coinbase(
        &self,
        network: MiningNetwork,
        payout: &str,
        extranonce: &[u8],
    ) -> Result<Coinbase, String> {
        self.coinbase_with_payout(network, payout, None, extranonce, Default::default())
    }

    pub fn coinbase_with_payout(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce: &[u8],
        policy: crate::donation::bch::BchPayout,
    ) -> Result<Coinbase, String> {
        self.coinbase_with_aux(network, payout, operator, extranonce, policy, None)
    }

    /// The coinbase with a job's merge-mining outputs, if any.
    pub fn coinbase_with_aux(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce: &[u8],
        policy: crate::donation::bch::BchPayout,
        aux: Option<&AuxOutputs>,
    ) -> Result<Coinbase, String> {
        let scripts = super::payout::scripts(network, payout, operator)?;
        let outputs = super::payout::outputs(self.coinbase_value, &scripts, policy);
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
        // #### PR #42: merge-mining commitment in output 0
        // What: with tokens, output 0 is the 53-byte commitment (value 0) and
        // each Case B token's 46-byte ticket (value 0) follows the payouts.
        // Without tokens (`aux` is `None`, the default on both networks) the
        // coinbase is byte for byte what it was.
        // Why: a covenant finds output 0 from the single input alone, with no
        // search, and output 0 does not compete with P2Pool's last-output
        // share commitment. Case B tickets do come last here; a P2Pool
        // coinbase would put its commitment after them, which the journal's
        // trailing-ticket strip does not accept yet.
        // The payouts, the script, the extranonce offset and the parts are
        // unchanged, so devices and SV1 firmware see only a longer suffix.
        // Look here if: a token-enabled block is refused by the node, the
        // B leaves name the wrong ticket vouts, or the output count reaches
        // 0xfd.
        let (commitment, tickets) = match aux {
            Some(aux) => {
                if !aux.tickets.is_empty()
                    && usize::try_from(aux.first_ticket_vout).ok() != Some(outputs.len() + 1)
                {
                    return Err("token tickets do not follow the payout outputs".into());
                }
                (Some(&aux.commitment), aux.tickets.as_slice())
            }
            None => (None, &[][..]),
        };
        let count = outputs.len() + usize::from(commitment.is_some()) + tickets.len();
        if commitment.is_some() && count >= 0xfd {
            return Err("a merge-mined coinbase needs fewer than 253 outputs".into());
        }
        compact_size(count, &mut bytes);
        if let Some(commitment) = commitment {
            bytes.extend_from_slice(commitment);
        }
        for (amount, script) in outputs {
            bytes.extend_from_slice(&amount.to_le_bytes());
            compact_size(script.len(), &mut bytes);
            bytes.extend(script);
        }
        for ticket in tickets {
            bytes.extend_from_slice(ticket);
        }
        // #### end PR #42 ####
        bytes.extend_from_slice(&0u32.to_le_bytes());
        self.check_block_size(bytes.len())?;
        // #### PR #42: the coinbase path is computed once per template
        // What: the merkle root folds the coinbase's hash up the branch
        // computed when the template arrived, instead of building the whole
        // tree for every coinbase.
        // Why: log2(n) hashes instead of n for each job and share.
        // Look here if: a block's merkle root mismatches.
        let merkle_root = fold(double_sha256(&bytes), &self.merkle_path);
        Ok(Coinbase { bytes, merkle_root })
    }

    pub fn coinbase_parts(
        &self,
        network: MiningNetwork,
        payout: &str,
        extranonce_len: usize,
    ) -> Result<CoinbaseParts, String> {
        self.coinbase_parts_with_payout(network, payout, None, extranonce_len, Default::default())
    }

    pub fn coinbase_parts_with_payout(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce_len: usize,
        policy: crate::donation::bch::BchPayout,
    ) -> Result<CoinbaseParts, String> {
        self.coinbase_parts_with_aux(network, payout, operator, extranonce_len, policy, None)
    }

    /// #### PR #42: the parts with a job's merge-mining outputs, which sit
    /// in the suffix; the prefix and the merkle path are unchanged.
    pub fn coinbase_parts_with_aux(
        &self,
        network: MiningNetwork,
        payout: &str,
        operator: Option<&str>,
        extranonce_len: usize,
        policy: crate::donation::bch::BchPayout,
        aux: Option<&AuxOutputs>,
    ) -> Result<CoinbaseParts, String> {
        if extranonce_len > 64 {
            return Err("extranonce exceeds coinbase budget".into());
        }
        let coinbase = self.coinbase_with_aux(
            network,
            payout,
            operator,
            &vec![0; extranonce_len],
            policy,
            aux,
        )?;
        // The coinbase script is at most 100 bytes, so its CompactSize is one byte.
        let offset =
            4 + 1 + 32 + 4 + 1 + height_script(self.height).len() + self.coinbase_flags.len();
        Ok(CoinbaseParts {
            prefix: coinbase.bytes[..offset].to_vec(),
            suffix: coinbase.bytes[offset + extranonce_len..].to_vec(),
            merkle_path: self.merkle_path.to_vec(),
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
        if self.header_only {
            return Err("a custom job's template has no transactions".into());
        }
        self.check_block_size(coinbase.bytes.len())?;
        if header[4..36] != self.previous_hash
            || header[36..68] != fold(double_sha256(&coinbase.bytes), &self.merkle_path)
            || header[72..76] != self.bits.to_le_bytes()
        {
            return Err("block header does not belong to this template".into());
        }
        Ok(self.assemble(&coinbase.bytes, &header))
    }

    /// #### PR #42: the block's bytes, unchecked: the header, the count,
    /// the coinbase and the template's transactions. A Template
    /// Distribution client's solution is assembled with it before its
    /// checks, so even a refused one can still reach the node (D24).
    pub fn assemble(&self, coinbase: &[u8], header: &[u8; 80]) -> Vec<u8> {
        let mut block = header.to_vec();
        compact_size(self.transactions.len() + 1, &mut block);
        block.extend_from_slice(coinbase);
        for tx in &self.transactions {
            block.extend_from_slice(tx);
        }
        block
    }

    pub fn transaction_count(&self) -> usize {
        self.transactions.len() + 1
    }

    /// #### PR #42: the BIP34 height push that begins every coinbase
    /// script; a Template Distribution client's coinbase prefix.
    pub fn height_push(&self) -> Vec<u8> {
        height_script(self.height)
    }

    /// #### PR #42: the coinbase's merkle branch, deepest sibling first.
    pub fn merkle_path(&self) -> &[Hash] {
        &self.merkle_path
    }

    /// #### PR #42: the block's other transactions, in block (CTOR) order.
    pub fn transactions(&self) -> &[Vec<u8>] {
        &self.transactions
    }

    /// #### PR #42: the bytes a coinbase may take in a block of this
    /// template: the size limit less the header, the transaction count and
    /// every other transaction.
    pub fn coinbase_budget(&self) -> u64 {
        let mut count = Vec::new();
        compact_size(self.transaction_count(), &mut count);
        let others = self
            .transactions
            .iter()
            .map(|tx| tx.len() as u64)
            .sum::<u64>();
        self.size_limit
            .saturating_sub(80 + count.len() as u64 + others)
    }

    /// #### PR #42: the template with other transactions, unchecked (its
    /// merkle path is not recomputed), for transaction-data size tests.
    #[cfg(test)]
    pub(super) fn with_transactions(mut self, transactions: Vec<Vec<u8>>) -> Self {
        self.transactions = transactions;
        self
    }

    fn check_block_size(&self, coinbase_size: usize) -> Result<(), String> {
        if coinbase_size as u64 > self.coinbase_budget() {
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

/// #### PR #42: the merkle branch of the coinbase (index 0) over the
/// block's other transactions: one sibling per level.
pub fn coinbase_path(txids: &[Hash]) -> Vec<Hash> {
    let mut hashes = vec![[0; 32]];
    hashes.extend_from_slice(txids);
    let mut path = Vec::new();
    while hashes.len() > 1 {
        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }
        path.push(hashes[1]);
        hashes = hashes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let mut bytes = [0; 64];
                bytes[..32].copy_from_slice(&pair[0]);
                bytes[32..].copy_from_slice(&pair[1]);
                double_sha256(&bytes)
            })
            .collect();
    }
    path
}

/// #### PR #42: folds the coinbase's hash up its branch at index 0 to the
/// merkle root.
pub fn fold(leaf: Hash, path: &[Hash]) -> Hash {
    path.iter().fold(leaf, |current, sibling| {
        let mut joined = [0; 64];
        joined[..32].copy_from_slice(&current);
        joined[32..].copy_from_slice(sibling);
        double_sha256(&joined)
    })
}

/// The whole tree's root, the reference the branch is checked against.
#[cfg(test)]
pub(super) fn merkle_root(mut hashes: Vec<Hash>) -> Hash {
    while hashes.len() > 1 {
        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }
        hashes = hashes
            .as_chunks::<2>()
            .0
            .iter()
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
