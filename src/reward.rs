//! Protocol-valid Pickaxe 98/2 reward settlement.
//!
//! Production settlement spends the newly-created PHOTON baton and reward
//! together, preserves the baton at output 0, and splits the exact winning
//! reward 98/2 without any external funding input.

use crate::config::{
    RuntimeConfig, CHIPNET_DONATION_ADDRESS, DONATION_ADDRESS, DONATION_BPS, SHREC_DONATION_ADDRESS,
};
use crate::crypto;
use crate::protocol::{
    PhotonDeployment, CHIPNET_CATEGORY_HEX, COVENANT_LOCKING_BYTECODE_HEX, MAINNET_CATEGORY_HEX,
};
use crate::tx;
use ripemd::Ripemd160;
use secp256k1::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};

pub const TOKEN_OUTPUT_SATS: u64 = 700;
#[cfg(test)]
pub const PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS: u64 = 8_000;
#[cfg_attr(not(test), allow(dead_code))]
pub const MIN_RELAY_FEE_SATS_PER_KB: u64 = 1_000;
const SIGHASH_ALL_FORKID: u8 = 0x41;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSelfFundedSettlement {
    pub parent_txid: String,
    pub settlement_txid: String,
    pub raw_settlement: Vec<u8>,
    pub baton_input_value_sats: u64,
    pub baton_output_value_sats: u64,
    pub miner_output_value_sats: u64,
    pub donation_output_value_sats: u64,
    pub shrec_output_value_sats: u64,
    pub miner_token_amount: u128,
    pub donation_token_amount: u128,
    pub original_donation_token_amount: u128,
    pub shrec_donation_token_amount: u128,
    pub required_relay_fee_sats: u64,
    pub fee_sats: u64,
}

/// An externally verified confirmed BCH UTXO. The raw transaction lets the
/// splitter independently check the selected output and its token status.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct ConfirmedFundingUtxo {
    pub txid: String,
    pub raw_transaction: Vec<u8>,
    pub vout: u32,
    pub value_sats: u64,
    pub confirmations: u32,
}

/// A confirmed, independently verifiable PHOTON reward owned by the batch key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedRewardUtxo {
    pub txid: String,
    pub raw_transaction: Vec<u8>,
    pub vout: u32,
    pub value_sats: u64,
    pub token_amount: u128,
    pub confirmations: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedBatchedRewardSplit {
    pub settlement_txid: String,
    pub raw_settlement: Vec<u8>,
    pub input_outpoints: Vec<(String, u32)>,
    pub miner_token_amount: u128,
    pub original_donation_token_amount: u128,
    pub shrec_donation_token_amount: u128,
    pub reward_change_value_sats: Option<u64>,
    pub required_relay_fee_sats: u64,
    pub fee_sats: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct PreparedFundedRewardSplit {
    pub parent_txid: String,
    pub settlement_txid: String,
    pub raw_settlement: Vec<u8>,
    pub funding_txid: String,
    pub funding_vout: u32,
    pub funding_input_value_sats: u64,
    pub funding_change_value_sats: Option<u64>,
    pub miner_token_amount: u128,
    pub original_donation_token_amount: u128,
    pub shrec_donation_token_amount: u128,
    pub required_relay_fee_sats: u64,
    pub fee_sats: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedOutput {
    value_sats: u64,
    token_and_locking_bytecode: Vec<u8>,
}

/// Computes a double SHA-256 transaction hash.
fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// Returns the authorized maximum baton decrease for multi-input settlements.
pub fn photon_multi_input_max_baton_decrease_sats() -> Result<u64, String> {
    crate::protocol::photon_multi_input_max_baton_decrease_sats()
}

/// Reverses transaction hash bytes between display and wire order.
fn reverse(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().rev().copied().collect()
}

/// Decodes a displayed transaction ID into 32 bytes.
fn parse_txid_display(txid: &str) -> Result<[u8; 32], String> {
    let txid = txid.trim();
    if txid.len() != 64 || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("transaction id must be exactly 64 hexadecimal characters".into());
    }
    let bytes = hex::decode(txid).map_err(|error| error.to_string())?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Computes the displayed ID of a serialized transaction.
pub fn transaction_id(raw: &[u8]) -> String {
    hex::encode(reverse(&hash256(raw)))
}

/// Encodes a length as a Bitcoin compact-size integer.
fn compact_uint(value: u64) -> Vec<u8> {
    if value < 0xfd {
        vec![value as u8]
    } else if value <= u16::MAX as u64 {
        let mut out = vec![0xfd];
        out.extend_from_slice(&(value as u16).to_le_bytes());
        out
    } else if value <= u32::MAX as u64 {
        let mut out = vec![0xfe];
        out.extend_from_slice(&(value as u32).to_le_bytes());
        out
    } else {
        let mut out = vec![0xff];
        out.extend_from_slice(&value.to_le_bytes());
        out
    }
}

/// Encodes a CashToken amount as a compact integer.
fn compact_token_amount(value: u128) -> Result<Vec<u8>, String> {
    let value = u64::try_from(value).map_err(|_| "token amount exceeds u64")?;
    Ok(compact_uint(value))
}

/// Encodes data as a script push operation.
fn push_data(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(data.len() + 3);
    match data.len() {
        0..=75 => out.push(data.len() as u8),
        76..=255 => {
            out.push(0x4c);
            out.push(data.len() as u8);
        }
        256..=65_535 => {
            out.push(0x4d);
            out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        }
        _ => return Err("script push exceeds OP_PUSHDATA2 limit".into()),
    }
    out.extend_from_slice(data);
    Ok(out)
}

/// Serializes a CashToken prefix for a transaction output.
fn token_prefix(amount: u128) -> Result<Vec<u8>, String> {
    let category = hex::decode(MAINNET_CATEGORY_HEX).map_err(|error| error.to_string())?;
    if category.len() != 32 {
        return Err("PHOTON category must be 32 bytes".into());
    }
    let mut out = Vec::new();
    out.push(0xef);
    out.extend(category.into_iter().rev());
    out.push(0x10); // fungible amount only
    out.extend_from_slice(&compact_token_amount(amount)?);
    Ok(out)
}

/// Serializes a transaction output with CashToken data.
fn encode_output(value_sats: u64, token: Option<u128>, locking: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytecode = Vec::new();
    if let Some(amount) = token {
        bytecode.extend_from_slice(&token_prefix(amount)?);
    }
    bytecode.extend_from_slice(locking);

    let mut out = Vec::new();
    out.extend_from_slice(&value_sats.to_le_bytes());
    out.extend_from_slice(&compact_uint(bytecode.len() as u64));
    out.extend_from_slice(&bytecode);
    Ok(out)
}

#[cfg_attr(not(test), allow(dead_code))]
/// Serializes a transaction output from raw locking bytecode.
fn encode_raw_output(value_sats: u64, bytecode: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&value_sats.to_le_bytes());
    out.extend_from_slice(&compact_uint(bytecode.len() as u64));
    out.extend_from_slice(bytecode);
    out
}

#[cfg_attr(not(test), allow(dead_code))]
/// Decodes a compact-size integer from transaction bytes.
fn read_compact_uint(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let first = *bytes.get(*cursor).ok_or("truncated CompactSize prefix")?;
    *cursor += 1;
    let count = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        value => return Ok(u64::from(value)),
    };
    let end = cursor
        .checked_add(count)
        .ok_or("CompactSize cursor overflow")?;
    let slice = bytes
        .get(*cursor..end)
        .ok_or("truncated CompactSize value")?;
    *cursor = end;
    let mut padded = [0u8; 8];
    padded[..count].copy_from_slice(slice);
    Ok(u64::from_le_bytes(padded))
}

/// Rejects noncanonical compact-size encodings.
fn read_canonical_compact_uint(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let start = *cursor;
    let value = read_compact_uint(bytes, cursor)?;
    if bytes.get(start..*cursor) != Some(compact_uint(value).as_slice()) {
        return Err("non-canonical CompactSize encoding in PHOTON baton".into());
    }
    Ok(value)
}

/// Checks a parent token against the authoritative PHOTON baton.
fn validate_authoritative_baton_token(token_and_locking_bytecode: &[u8]) -> Result<(), String> {
    const TOKEN_PREFIX_MARKER: u8 = 0xef;
    const MUTABLE_NFT_WITH_COMMITMENT_AND_AMOUNT: u8 = 0x71;
    const MIN_REFERENCE_COMMITMENT_BYTES: usize = 36;

    let category = hex::decode(MAINNET_CATEGORY_HEX).map_err(|error| error.to_string())?;
    if category.len() != 32 {
        return Err("PHOTON category must be 32 bytes".into());
    }
    let category_le = reverse(&category);
    let covenant_lock =
        hex::decode(COVENANT_LOCKING_BYTECODE_HEX).map_err(|error| error.to_string())?;

    let marker = token_and_locking_bytecode
        .first()
        .copied()
        .ok_or("parent output 0 is missing its CashToken prefix")?;
    if marker != TOKEN_PREFIX_MARKER {
        return Err("parent output 0 is missing the CashToken prefix marker".into());
    }

    let category_end = 1usize
        .checked_add(category_le.len())
        .ok_or("PHOTON baton category cursor overflow")?;
    if token_and_locking_bytecode.get(1..category_end) != Some(category_le.as_slice()) {
        return Err("parent output 0 has the wrong PHOTON token category".into());
    }

    let capability = token_and_locking_bytecode
        .get(category_end)
        .copied()
        .ok_or("parent output 0 is missing its CashToken capability byte")?;
    if capability != MUTABLE_NFT_WITH_COMMITMENT_AND_AMOUNT {
        return Err(
            "parent output 0 is not a mutable PHOTON NFT with commitment and amount".into(),
        );
    }

    let mut cursor = category_end + 1;
    let commitment_len = usize::try_from(read_canonical_compact_uint(
        token_and_locking_bytecode,
        &mut cursor,
    )?)
    .map_err(|_| "PHOTON baton commitment length exceeds usize")?;
    if commitment_len < MIN_REFERENCE_COMMITMENT_BYTES {
        return Err("parent output 0 PHOTON commitment is missing or too short".into());
    }
    let commitment_end = cursor
        .checked_add(commitment_len)
        .ok_or("PHOTON baton commitment cursor overflow")?;
    token_and_locking_bytecode
        .get(cursor..commitment_end)
        .ok_or("parent output 0 PHOTON commitment is truncated")?;
    cursor = commitment_end;

    read_canonical_compact_uint(token_and_locking_bytecode, &mut cursor)?;
    if token_and_locking_bytecode.get(cursor..) != Some(covenant_lock.as_slice()) {
        return Err("parent output 0 does not end in the authoritative PHOTON covenant".into());
    }

    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
/// Extracts and validates the parent transaction outputs.
fn parse_parent_outputs(parent_raw: &[u8]) -> Result<[ParsedOutput; 2], String> {
    if parent_raw.len() < 10 {
        return Err("winning PHOTON parent transaction is truncated".into());
    }
    let mut cursor = 4usize;
    let input_count = read_compact_uint(parent_raw, &mut cursor)?;
    if input_count != 1 {
        return Err(format!(
            "winning PHOTON parent must have exactly one covenant input (got {input_count})"
        ));
    }
    cursor = cursor
        .checked_add(36)
        .ok_or("parent input cursor overflow")?;
    if cursor > parent_raw.len() {
        return Err("winning PHOTON parent input outpoint is truncated".into());
    }
    let script_len = usize::try_from(read_compact_uint(parent_raw, &mut cursor)?)
        .map_err(|_| "parent input script length exceeds usize")?;
    cursor = cursor
        .checked_add(script_len)
        .and_then(|value| value.checked_add(4))
        .ok_or("parent input cursor overflow")?;
    if cursor > parent_raw.len() {
        return Err("winning PHOTON parent input is truncated".into());
    }
    let output_count = read_compact_uint(parent_raw, &mut cursor)?;
    if output_count != 2 {
        return Err(format!(
            "winning PHOTON parent must have exactly two outputs (got {output_count})"
        ));
    }

    let mut parsed = Vec::with_capacity(2);
    for _ in 0..2 {
        let value_end = cursor
            .checked_add(8)
            .ok_or("parent output value cursor overflow")?;
        let value_bytes: [u8; 8] = parent_raw
            .get(cursor..value_end)
            .ok_or("winning PHOTON parent output value is truncated")?
            .try_into()
            .map_err(|_| "parent output value length error")?;
        cursor = value_end;
        let bytecode_len = usize::try_from(read_compact_uint(parent_raw, &mut cursor)?)
            .map_err(|_| "parent output bytecode length exceeds usize")?;
        let bytecode_end = cursor
            .checked_add(bytecode_len)
            .ok_or("parent output bytecode cursor overflow")?;
        let bytecode = parent_raw
            .get(cursor..bytecode_end)
            .ok_or("winning PHOTON parent output bytecode is truncated")?
            .to_vec();
        cursor = bytecode_end;
        parsed.push(ParsedOutput {
            value_sats: u64::from_le_bytes(value_bytes),
            token_and_locking_bytecode: bytecode,
        });
    }
    if cursor.checked_add(4) != Some(parent_raw.len()) {
        return Err("winning PHOTON parent has trailing or truncated bytes".into());
    }
    parsed
        .try_into()
        .map_err(|_| "internal parent output count error".into())
}

/// Encodes a transaction outpoint in wire byte order.
fn serialized_outpoint(txid: &str, vout: u32) -> Result<Vec<u8>, String> {
    let hash = parse_txid_display(txid)?;
    let mut out = reverse(&hash);
    out.extend_from_slice(&vout.to_le_bytes());
    Ok(out)
}

/// Serializes a signed transaction input.
fn encode_input(txid: &str, vout: u32, unlocking: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = serialized_outpoint(txid, vout)?;
    out.extend_from_slice(&compact_uint(unlocking.len() as u64));
    out.extend_from_slice(unlocking);
    out.extend_from_slice(&0u32.to_le_bytes());
    Ok(out)
}

/// Reads a conventional BCH transaction output from an untrusted raw transaction.
fn transaction_output(raw: &[u8], selected_vout: u32) -> Result<ParsedOutput, String> {
    if raw.len() < 10 {
        return Err("funding transaction is truncated".into());
    }
    let mut cursor = 4usize;
    let input_count = read_canonical_compact_uint(raw, &mut cursor)?;
    if input_count == 0 || input_count > 100_000 {
        return Err("funding transaction has an invalid input count".into());
    }
    for _ in 0..input_count {
        cursor = cursor
            .checked_add(36)
            .ok_or("funding input cursor overflow")?;
        if cursor > raw.len() {
            return Err("funding input outpoint is truncated".into());
        }
        let script_len = usize::try_from(read_canonical_compact_uint(raw, &mut cursor)?)
            .map_err(|_| "funding input script exceeds usize")?;
        cursor = cursor
            .checked_add(script_len)
            .and_then(|value| value.checked_add(4))
            .ok_or("funding input cursor overflow")?;
        if cursor > raw.len() {
            return Err("funding input is truncated".into());
        }
    }
    let output_count = read_canonical_compact_uint(raw, &mut cursor)?;
    if output_count == 0 || output_count > 100_000 || u64::from(selected_vout) >= output_count {
        return Err("funding vout is outside transaction outputs".into());
    }
    let mut selected = None;
    for index in 0..output_count {
        let value_end = cursor
            .checked_add(8)
            .ok_or("funding output cursor overflow")?;
        let value_bytes: [u8; 8] = raw
            .get(cursor..value_end)
            .ok_or("funding output value is truncated")?
            .try_into()
            .map_err(|_| "funding output value length error")?;
        cursor = value_end;
        let script_len = usize::try_from(read_canonical_compact_uint(raw, &mut cursor)?)
            .map_err(|_| "funding output script exceeds usize")?;
        let script_end = cursor
            .checked_add(script_len)
            .ok_or("funding output cursor overflow")?;
        let bytecode = raw
            .get(cursor..script_end)
            .ok_or("funding output bytecode is truncated")?;
        cursor = script_end;
        if index == u64::from(selected_vout) {
            selected = Some(ParsedOutput {
                value_sats: u64::from_le_bytes(value_bytes),
                token_and_locking_bytecode: bytecode.to_vec(),
            });
        }
    }
    if cursor.checked_add(4) != Some(raw.len()) {
        return Err("funding transaction has trailing or truncated bytes".into());
    }
    selected.ok_or_else(|| "funding output was not found".into())
}

fn deployment_token_prefix(deployment: &PhotonDeployment, amount: u128) -> Result<Vec<u8>, String> {
    let category = hex::decode(deployment.category_hex).map_err(|error| error.to_string())?;
    if category.len() != 32 {
        return Err("PHOTON category must be 32 bytes".into());
    }
    let mut out = Vec::with_capacity(43);
    out.push(0xef);
    out.extend(category.into_iter().rev());
    out.push(0x10);
    out.extend_from_slice(&compact_token_amount(amount)?);
    Ok(out)
}

fn validate_deployment_baton_token(
    deployment: &PhotonDeployment,
    token_and_locking_bytecode: &[u8],
) -> Result<(), String> {
    let category = hex::decode(deployment.category_hex).map_err(|error| error.to_string())?;
    let covenant_lock =
        hex::decode(deployment.covenant_lock_hex).map_err(|error| error.to_string())?;
    if token_and_locking_bytecode.first() != Some(&0xef)
        || token_and_locking_bytecode.get(1..33) != Some(reverse(&category).as_slice())
    {
        return Err("parent baton has the wrong PHOTON token category".into());
    }
    if token_and_locking_bytecode.get(33) != Some(&0x71) {
        return Err("parent baton is not a mutable PHOTON NFT with amount".into());
    }
    let mut cursor = 34usize;
    let commitment_len = usize::try_from(read_canonical_compact_uint(
        token_and_locking_bytecode,
        &mut cursor,
    )?)
    .map_err(|_| "PHOTON baton commitment exceeds usize")?;
    if commitment_len < 36 {
        return Err("parent baton commitment is too short".into());
    }
    cursor = cursor
        .checked_add(commitment_len)
        .ok_or("PHOTON baton commitment cursor overflow")?;
    token_and_locking_bytecode
        .get(..cursor)
        .ok_or("parent baton commitment is truncated")?;
    read_canonical_compact_uint(token_and_locking_bytecode, &mut cursor)?;
    if token_and_locking_bytecode.get(cursor..) != Some(covenant_lock.as_slice()) {
        return Err("parent baton does not end in the selected PHOTON covenant".into());
    }
    Ok(())
}

/// Bind direct-reward recovery metadata to the immutable transaction bytes.
pub fn validate_direct_reward_record(
    raw: &[u8],
    deployment: &PhotonDeployment,
    baton_value: u64,
    reward_amount: u128,
) -> Result<(), String> {
    let [baton, reward] = parse_parent_outputs(raw)?;
    validate_deployment_baton_token(deployment, &baton.token_and_locking_bytecode)?;
    let prefix = deployment_token_prefix(deployment, reward_amount)?;
    let locking = reward
        .token_and_locking_bytecode
        .strip_prefix(prefix.as_slice())
        .ok_or("direct reward journal token amount or category disagrees with transaction")?;
    if baton.value_sats != baton_value
        || reward.value_sats != TOKEN_OUTPUT_SATS
        || locking.len() != 25
        || locking[..3] != [0x76, 0xa9, 0x14]
        || locking[23..] != [0x88, 0xac]
    {
        return Err(
            "direct reward journal value or payout shape disagrees with transaction".into(),
        );
    }
    Ok(())
}

/// The PHOTON baton a winning parent creates at output 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessorBaton {
    pub value_sats: u64,
    pub commitment_hex: String,
    pub token_amount: u128,
}

/// Reads the successor baton from a signed winning parent so mining can
/// continue on it before an indexer lists the unconfirmed parent.
pub fn successor_baton(
    raw: &[u8],
    deployment: &PhotonDeployment,
) -> Result<SuccessorBaton, String> {
    let [baton, _reward] = parse_parent_outputs(raw)?;
    let bytes = &baton.token_and_locking_bytecode;
    validate_deployment_baton_token(deployment, bytes)?;
    let mut cursor = 34usize;
    let commitment_len = usize::try_from(read_canonical_compact_uint(bytes, &mut cursor)?)
        .map_err(|_| "PHOTON baton commitment exceeds usize")?;
    let commitment_end = cursor
        .checked_add(commitment_len)
        .ok_or("PHOTON baton commitment cursor overflow")?;
    let commitment = bytes
        .get(cursor..commitment_end)
        .ok_or("parent baton commitment is truncated")?;
    cursor = commitment_end;
    let token_amount = read_canonical_compact_uint(bytes, &mut cursor)?;
    Ok(SuccessorBaton {
        value_sats: baton.value_sats,
        commitment_hex: hex::encode(commitment),
        token_amount: u128::from(token_amount),
    })
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn funded_p2pkh_sighash(
    parent_txid: &str,
    funding_txid: &str,
    funding_vout: u32,
    input_index: u32,
    value_sats: u64,
    token_prefix: Option<&[u8]>,
    locking_bytecode: &[u8],
    outputs: &[u8],
) -> Result<[u8; 32], String> {
    let reward_outpoint = serialized_outpoint(parent_txid, 1)?;
    let funding_outpoint = serialized_outpoint(funding_txid, funding_vout)?;
    let mut outpoints = reward_outpoint.clone();
    outpoints.extend_from_slice(&funding_outpoint);
    let mut preimage = Vec::new();
    preimage.extend_from_slice(&2u32.to_le_bytes());
    preimage.extend_from_slice(&hash256(&outpoints));
    preimage.extend_from_slice(&hash256(&[0u8; 8]));
    preimage.extend_from_slice(if input_index == 0 {
        &reward_outpoint
    } else {
        &funding_outpoint
    });
    if let Some(prefix) = token_prefix {
        preimage.extend_from_slice(prefix);
    }
    preimage.extend_from_slice(&compact_uint(locking_bytecode.len() as u64));
    preimage.extend_from_slice(locking_bytecode);
    preimage.extend_from_slice(&value_sats.to_le_bytes());
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&hash256(outputs));
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&(u32::from(SIGHASH_ALL_FORKID)).to_le_bytes());
    Ok(hash256(&preimage))
}

#[cfg_attr(not(test), allow(dead_code))]
/// Builds the BCH signature hash for a self-funded input.
fn self_funded_p2pkh_sighash(
    parent_txid: &str,
    reward_token_amount: u128,
    reward_value_sats: u64,
    reward_lock: &[u8],
    outputs: &[u8],
) -> Result<[u8; 32], String> {
    let mut outpoints = serialized_outpoint(parent_txid, 0)?;
    outpoints.extend_from_slice(&serialized_outpoint(parent_txid, 1)?);
    let hash_prevouts = hash256(&outpoints);
    let hash_sequence = hash256(&[0u8; 8]);
    let hash_outputs = hash256(outputs);

    let mut preimage = Vec::new();
    preimage.extend_from_slice(&2u32.to_le_bytes());
    preimage.extend_from_slice(&hash_prevouts);
    preimage.extend_from_slice(&hash_sequence);
    preimage.extend_from_slice(&serialized_outpoint(parent_txid, 1)?);
    preimage.extend_from_slice(&token_prefix(reward_token_amount)?);
    preimage.extend_from_slice(&compact_uint(reward_lock.len() as u64));
    preimage.extend_from_slice(reward_lock);
    preimage.extend_from_slice(&reward_value_sats.to_le_bytes());
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&hash_outputs);
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&(SIGHASH_ALL_FORKID as u32).to_le_bytes());
    Ok(hash256(&preimage))
}

/// Builds P2PKH locking bytecode from a public key.
pub fn p2pkh_locking_from_public_key(public_key: &[u8; 33]) -> Vec<u8> {
    let sha = Sha256::digest(public_key);
    let hash = Ripemd160::digest(sha);
    let mut out = Vec::with_capacity(25);
    out.extend_from_slice(&[0x76, 0xa9, 0x14]);
    out.extend_from_slice(&hash);
    out.extend_from_slice(&[0x88, 0xac]);
    out
}

/// Derives a CashAddr from a public key.
pub fn p2pkh_cashaddr_from_public_key(public_key: &[u8; 33]) -> Result<String, String> {
    let locking = p2pkh_locking_from_public_key(public_key);
    let hash: [u8; 20] = locking[3..23]
        .try_into()
        .map_err(|_| "internal P2PKH hash length error")?;
    tx::p2pkh_hash_to_cashaddr(&hash)
}

/// Creates an ephemeral identity for the self-funded settlement.
fn required_relay_fee_sats(
    serialized_bytes: usize,
    relay_fee_sats_per_kb: u64,
) -> Result<u64, String> {
    let bytes = u64::try_from(serialized_bytes).map_err(|_| "transaction size exceeds u64")?;
    bytes
        .checked_mul(relay_fee_sats_per_kb)
        .and_then(|value| value.checked_add(999))
        .map(|value| value / 1_000)
        .ok_or_else(|| "relay-fee calculation overflow".into())
}

#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
/// Builds miner and donation outputs with the required token split.
fn build_self_funded_outputs(
    baton_output_value_sats: u64,
    baton_token_and_locking_bytecode: &[u8],
    reward_value_sats: u64,
    miner_lock: &[u8],
    donation_lock: &[u8],
    shrec_lock: &[u8],
    miner_token_amount: u128,
    original_donation_token_amount: u128,
    shrec_donation_token_amount: u128,
) -> Result<Vec<u8>, String> {
    let mut outputs = Vec::new();
    outputs.extend_from_slice(&encode_raw_output(
        baton_output_value_sats,
        baton_token_and_locking_bytecode,
    ));
    outputs.extend_from_slice(&encode_output(
        reward_value_sats,
        Some(miner_token_amount),
        miner_lock,
    )?);
    outputs.extend_from_slice(&encode_output(
        reward_value_sats,
        Some(original_donation_token_amount),
        donation_lock,
    )?);
    outputs.extend_from_slice(&encode_output(
        reward_value_sats,
        Some(shrec_donation_token_amount),
        shrec_lock,
    )?);
    Ok(outputs)
}

#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
/// Measures the size of a signed self-funded settlement.
fn self_funded_serialized_len(
    parent_txid: &str,
    baton_output_value_sats: u64,
    baton_token_and_locking_bytecode: &[u8],
    reward_value_sats: u64,
    reward_public_key: &[u8; 33],
    miner_lock: &[u8],
    donation_lock: &[u8],
    shrec_lock: &[u8],
    miner_token_amount: u128,
    original_donation_token_amount: u128,
    shrec_donation_token_amount: u128,
) -> Result<usize, String> {
    let redeem_script = crate::protocol::photon_authoritative_redeem_script()?;
    let baton_unlocking = push_data(&redeem_script)?;

    // BCH Schnorr is exactly 64 bytes, followed by the one-byte sighash type.
    // P2PKH then pushes that 65-byte value and the fixed 33-byte compressed key.
    // These placeholders are used only for serialization sizing; no provisional
    // signature is created with the live reward key.
    let mut reward_unlocking = push_data(&[0u8; 65])?;
    reward_unlocking.extend_from_slice(&push_data(reward_public_key)?);
    let outputs = build_self_funded_outputs(
        baton_output_value_sats,
        baton_token_and_locking_bytecode,
        reward_value_sats,
        miner_lock,
        donation_lock,
        shrec_lock,
        miner_token_amount,
        original_donation_token_amount,
        shrec_donation_token_amount,
    )?;

    let mut raw = Vec::new();
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.push(2);
    raw.extend_from_slice(&encode_input(parent_txid, 0, &baton_unlocking)?);
    raw.extend_from_slice(&encode_input(parent_txid, 1, &reward_unlocking)?);
    raw.push(4);
    raw.extend_from_slice(&outputs);
    raw.extend_from_slice(&0u32.to_le_bytes());
    Ok(raw.len())
}

#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
/// Signs and serializes the self-funded settlement transaction.
fn build_self_funded_raw(
    parent_txid: &str,
    baton_output_value_sats: u64,
    baton_token_and_locking_bytecode: &[u8],
    reward_value_sats: u64,
    reward_token_amount: u128,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    miner_lock: &[u8],
    donation_lock: &[u8],
    shrec_lock: &[u8],
    miner_token_amount: u128,
    original_donation_token_amount: u128,
    shrec_donation_token_amount: u128,
) -> Result<Vec<u8>, String> {
    let redeem_script = crate::protocol::photon_authoritative_redeem_script()?;
    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);
    let outputs = build_self_funded_outputs(
        baton_output_value_sats,
        baton_token_and_locking_bytecode,
        reward_value_sats,
        miner_lock,
        donation_lock,
        shrec_lock,
        miner_token_amount,
        original_donation_token_amount,
        shrec_donation_token_amount,
    )?;

    let sighash = self_funded_p2pkh_sighash(
        parent_txid,
        reward_token_amount,
        reward_value_sats,
        &reward_lock,
        &outputs,
    )?;
    let signature = crypto::bch_schnorr_sign(reward_secret, &sighash)?;
    if !crypto::bch_schnorr_verify(reward_public_key, &sighash, &signature)? {
        return Err("self-funded settlement Schnorr signature failed local verification".into());
    }
    let mut bitcoin_signature = signature.to_vec();
    bitcoin_signature.push(SIGHASH_ALL_FORKID);
    let mut reward_unlocking = push_data(&bitcoin_signature)?;
    reward_unlocking.extend_from_slice(&push_data(reward_public_key)?);
    let baton_unlocking = push_data(&redeem_script)?;

    let mut raw = Vec::new();
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.push(2);
    raw.extend_from_slice(&encode_input(parent_txid, 0, &baton_unlocking)?);
    raw.extend_from_slice(&encode_input(parent_txid, 1, &reward_unlocking)?);
    raw.push(4);
    raw.extend_from_slice(&outputs);
    raw.extend_from_slice(&0u32.to_le_bytes());
    Ok(raw)
}

#[cfg_attr(not(test), allow(dead_code))]
/// Builds a settlement with the default relay fee policy.
pub fn build_self_funded_settlement(
    parent_raw: &[u8],
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    miner_payout: &str,
    reward_token_amount: u128,
) -> Result<PreparedSelfFundedSettlement, String> {
    build_self_funded_settlement_with_relay_fee(
        parent_raw,
        reward_secret,
        reward_public_key,
        miner_payout,
        reward_token_amount,
        MIN_RELAY_FEE_SATS_PER_KB,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
/// Builds a settlement at the supplied live relay fee.
pub fn build_self_funded_settlement_with_relay_fee(
    parent_raw: &[u8],
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    miner_payout: &str,
    reward_token_amount: u128,
    relay_fee_sats_per_kb: u64,
) -> Result<PreparedSelfFundedSettlement, String> {
    let derived_public = PublicKey::from_secret_key(
        &SecretKey::from_secret_bytes(*reward_secret).map_err(|error| error.to_string())?,
    )
    .serialize();
    if &derived_public != reward_public_key {
        return Err("reward public key does not match the runtime reward secret".into());
    }

    let [baton, reward] = parse_parent_outputs(parent_raw)?;
    validate_authoritative_baton_token(&baton.token_and_locking_bytecode)?;
    if reward.value_sats != TOKEN_OUTPUT_SATS {
        return Err(format!(
            "parent reward BCH value must be the proven {TOKEN_OUTPUT_SATS} sats (got {})",
            reward.value_sats
        ));
    }
    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);
    let mut expected_reward = token_prefix(reward_token_amount)?;
    expected_reward.extend_from_slice(&reward_lock);
    if reward.token_and_locking_bytecode != expected_reward {
        return Err("parent reward output does not match the signed runtime reward state".into());
    }

    let miner_lock = tx::cashaddr_to_p2pkh_locking(miner_payout)?;
    let donation_lock = tx::cashaddr_to_p2pkh_locking(DONATION_ADDRESS)?;
    let shrec_lock = tx::cashaddr_to_p2pkh_locking(SHREC_DONATION_ADDRESS)?;
    let (miner_token_amount, donation_token_amount) =
        RuntimeConfig::split_reward(reward_token_amount);
    if donation_token_amount
        != reward_token_amount.saturating_mul(u128::from(DONATION_BPS)) / 10_000
        || miner_token_amount
            .checked_add(donation_token_amount)
            .ok_or("reward split overflow")?
            != reward_token_amount
    {
        return Err("reward split failed exact 98/2 conservation".into());
    }

    let (original_donation_token_amount, shrec_donation_token_amount) =
        RuntimeConfig::split_donation(donation_token_amount);
    if original_donation_token_amount == 0 || shrec_donation_token_amount == 0 {
        return Err("reward donation is too small to split into two token outputs".into());
    }

    let parent_txid = transaction_id(parent_raw);
    let multi_input_max_baton_decrease_sats = photon_multi_input_max_baton_decrease_sats()?;
    let sizing_baton_value = baton
        .value_sats
        .checked_sub(multi_input_max_baton_decrease_sats)
        .ok_or("baton BCH value is too small for settlement")?;
    let serialized_len = self_funded_serialized_len(
        &parent_txid,
        sizing_baton_value,
        &baton.token_and_locking_bytecode,
        reward.value_sats,
        reward_public_key,
        &miner_lock,
        &donation_lock,
        &shrec_lock,
        miner_token_amount,
        original_donation_token_amount,
        shrec_donation_token_amount,
    )?;
    let required_relay_fee_sats = required_relay_fee_sats(serialized_len, relay_fee_sats_per_kb)?;
    let baton_decrease = reward
        .value_sats
        .checked_mul(2)
        .and_then(|value| value.checked_add(required_relay_fee_sats))
        .ok_or("baton decrease overflow")?;
    if baton_decrease > multi_input_max_baton_decrease_sats {
        return Err(format!(
            "settlement needs {baton_decrease} baton sats but covenant permits at most {multi_input_max_baton_decrease_sats}"
        ));
    }
    let baton_output_value_sats = baton
        .value_sats
        .checked_sub(baton_decrease)
        .ok_or("baton output value underflow")?;
    let raw_settlement = build_self_funded_raw(
        &parent_txid,
        baton_output_value_sats,
        &baton.token_and_locking_bytecode,
        reward.value_sats,
        reward_token_amount,
        reward_secret,
        reward_public_key,
        &miner_lock,
        &donation_lock,
        &shrec_lock,
        miner_token_amount,
        original_donation_token_amount,
        shrec_donation_token_amount,
    )?;
    if raw_settlement.len() != serialized_len {
        return Err("settlement relay-fee sizing changed after finalization".into());
    }
    let input_value = baton
        .value_sats
        .checked_add(reward.value_sats)
        .ok_or("settlement input value overflow")?;
    let output_value = baton_output_value_sats
        .checked_add(reward.value_sats)
        .and_then(|value| value.checked_add(reward.value_sats))
        .and_then(|value| value.checked_add(reward.value_sats))
        .ok_or("settlement output value overflow")?;
    let fee_sats = input_value
        .checked_sub(output_value)
        .ok_or("settlement outputs exceed BCH inputs")?;
    if fee_sats != required_relay_fee_sats {
        return Err(format!(
            "settlement fee {fee_sats} does not equal required relay fee {required_relay_fee_sats}"
        ));
    }
    Ok(PreparedSelfFundedSettlement {
        parent_txid,
        settlement_txid: transaction_id(&raw_settlement),
        raw_settlement,
        baton_input_value_sats: baton.value_sats,
        baton_output_value_sats,
        miner_output_value_sats: reward.value_sats,
        donation_output_value_sats: reward.value_sats,
        shrec_output_value_sats: reward.value_sats,
        miner_token_amount,
        donation_token_amount,
        original_donation_token_amount,
        shrec_donation_token_amount,
        required_relay_fee_sats,
        fee_sats,
    })
}

const P2PKH_CHANGE_DUST_SATS: u64 = 546;

/// Preserve the established 2% donation and odd-token rounding policy.
#[cfg(test)]
fn funded_split_amounts(reward_token_amount: u128) -> Result<(u128, u128, u128), String> {
    let (miner, donation) = RuntimeConfig::split_reward(reward_token_amount);
    let (original, shrec) = RuntimeConfig::split_donation(donation);
    if original == 0 || shrec == 0 {
        return Err("reward donation is too small for two nonzero token outputs".into());
    }
    if miner
        .checked_add(original)
        .and_then(|amount| amount.checked_add(shrec))
        != Some(reward_token_amount)
    {
        return Err("funded reward split failed token conservation".into());
    }
    Ok((miner, original, shrec))
}

#[allow(clippy::too_many_arguments)]
fn funded_split_outputs(
    deployment: &PhotonDeployment,
    miner_lock: &[u8],
    original_lock: &[u8],
    shrec_lock: &[u8],
    funding_lock: &[u8],
    miner_amount: u128,
    original_amount: u128,
    shrec_amount: u128,
    change_sats: Option<u64>,
) -> Result<(Vec<u8>, u8), String> {
    let mut outputs = Vec::new();
    for (amount, lock) in [
        (miner_amount, miner_lock),
        (original_amount, original_lock),
        (shrec_amount, shrec_lock),
    ] {
        let mut bytecode = deployment_token_prefix(deployment, amount)?;
        bytecode.extend_from_slice(lock);
        outputs.extend_from_slice(&encode_raw_output(TOKEN_OUTPUT_SATS, &bytecode));
    }
    if let Some(change) = change_sats {
        outputs.extend_from_slice(&encode_raw_output(change, funding_lock));
    }
    Ok((outputs, if change_sats.is_some() { 4 } else { 3 }))
}

#[cfg(test)]
fn funded_split_raw(
    parent_txid: &str,
    funding_txid: &str,
    funding_vout: u32,
    reward_unlocking: &[u8],
    funding_unlocking: &[u8],
    outputs: &[u8],
    output_count: u8,
) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.push(2);
    raw.extend_from_slice(&encode_input(parent_txid, 1, reward_unlocking)?);
    raw.extend_from_slice(&encode_input(
        funding_txid,
        funding_vout,
        funding_unlocking,
    )?);
    raw.push(output_count);
    raw.extend_from_slice(outputs);
    raw.extend_from_slice(&0u32.to_le_bytes());
    Ok(raw)
}

/// Builds the corrected-deployment 98/1/1 child without spending the baton.
/// The caller must supply a confirmed unspent funding output; this function
/// verifies its raw transaction, ownership, BCH value, and absence of tokens.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub fn build_funded_reward_split_with_relay_fee(
    deployment: &PhotonDeployment,
    parent_raw: &[u8],
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    miner_payout: &str,
    reward_token_amount: u128,
    funding: &ConfirmedFundingUtxo,
    funding_secret: &[u8; 32],
    relay_fee_sats_per_kb: u64,
) -> Result<PreparedFundedRewardSplit, String> {
    deployment.verify()?;
    if reward_token_amount == 0 {
        return Err("reward token amount must be positive".into());
    }
    if funding.confirmations == 0 {
        return Err("funding UTXO must have at least one confirmation".into());
    }
    if funding.txid != transaction_id(&funding.raw_transaction) {
        return Err("funding raw transaction does not match its txid".into());
    }
    let funding_output = transaction_output(&funding.raw_transaction, funding.vout)?;
    if funding_output.value_sats != funding.value_sats {
        return Err("funding UTXO value does not match its raw transaction".into());
    }
    if funding_output.token_and_locking_bytecode.first() == Some(&0xef) {
        return Err("funding UTXO must not contain a CashToken prefix".into());
    }

    let reward_public_derived = PublicKey::from_secret_key(
        &SecretKey::from_secret_bytes(*reward_secret).map_err(|error| error.to_string())?,
    )
    .serialize();
    if &reward_public_derived != reward_public_key {
        return Err("reward public key does not match reward secret".into());
    }
    let funding_public_key = PublicKey::from_secret_key(
        &SecretKey::from_secret_bytes(*funding_secret).map_err(|error| error.to_string())?,
    )
    .serialize();
    if &funding_public_key == reward_public_key {
        return Err("funding secret must be separate from the reward secret".into());
    }
    let funding_lock = p2pkh_locking_from_public_key(&funding_public_key);
    if funding_output.token_and_locking_bytecode != funding_lock {
        return Err("funding UTXO is not token-free P2PKH owned by funding key".into());
    }

    let [baton, reward] = parse_parent_outputs(parent_raw)?;
    validate_deployment_baton_token(deployment, &baton.token_and_locking_bytecode)?;
    if reward.value_sats != TOKEN_OUTPUT_SATS {
        return Err(format!(
            "parent reward BCH value must be the proven {TOKEN_OUTPUT_SATS} sats (got {})",
            reward.value_sats
        ));
    }
    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);
    let reward_token_prefix = deployment_token_prefix(deployment, reward_token_amount)?;
    let mut expected_reward = reward_token_prefix.clone();
    expected_reward.extend_from_slice(&reward_lock);
    if reward.token_and_locking_bytecode != expected_reward {
        return Err("parent reward output does not match the signed runtime reward state".into());
    }

    let original_address = if deployment.category_hex == CHIPNET_CATEGORY_HEX {
        CHIPNET_DONATION_ADDRESS
    } else {
        DONATION_ADDRESS
    };
    let miner_lock = tx::cashaddr_to_p2pkh_locking(miner_payout)?;
    let original_lock = tx::cashaddr_to_p2pkh_locking(original_address)?;
    let shrec_lock = tx::cashaddr_to_p2pkh_locking(SHREC_DONATION_ADDRESS)?;
    let (miner_amount, original_amount, shrec_amount) = funded_split_amounts(reward_token_amount)?;
    let parent_txid = transaction_id(parent_raw);
    if funding.txid == parent_txid {
        return Err("funding UTXO cannot come from the unconfirmed winning parent".into());
    }
    let mut placeholder_unlocking = push_data(&[0u8; 65])?;
    placeholder_unlocking.extend_from_slice(&push_data(reward_public_key)?);
    let mut placeholder_funding_unlocking = push_data(&[0u8; 65])?;
    placeholder_funding_unlocking.extend_from_slice(&push_data(&funding_public_key)?);

    let (no_change_outputs, no_change_count) = funded_split_outputs(
        deployment,
        &miner_lock,
        &original_lock,
        &shrec_lock,
        &funding_lock,
        miner_amount,
        original_amount,
        shrec_amount,
        None,
    )?;
    let no_change_len = funded_split_raw(
        &parent_txid,
        &funding.txid,
        funding.vout,
        &placeholder_unlocking,
        &placeholder_funding_unlocking,
        &no_change_outputs,
        no_change_count,
    )?
    .len();
    let no_change_fee = required_relay_fee_sats(no_change_len, relay_fee_sats_per_kb)?;
    let total_input_sats = funding
        .value_sats
        .checked_add(reward.value_sats)
        .ok_or("input BCH value overflow")?;
    let three_output_sats = TOKEN_OUTPUT_SATS
        .checked_mul(3)
        .ok_or("output BCH value overflow")?;
    let no_change_actual_fee = total_input_sats
        .checked_sub(three_output_sats)
        .ok_or("funding UTXO cannot cover three token outputs")?;
    if no_change_actual_fee < no_change_fee {
        return Err(format!(
            "funding UTXO cannot cover the required {no_change_fee}-sat relay fee"
        ));
    }

    let (change_sizing_outputs, change_sizing_count) = funded_split_outputs(
        deployment,
        &miner_lock,
        &original_lock,
        &shrec_lock,
        &funding_lock,
        miner_amount,
        original_amount,
        shrec_amount,
        Some(P2PKH_CHANGE_DUST_SATS),
    )?;
    let change_len = funded_split_raw(
        &parent_txid,
        &funding.txid,
        funding.vout,
        &placeholder_unlocking,
        &placeholder_funding_unlocking,
        &change_sizing_outputs,
        change_sizing_count,
    )?
    .len();
    let change_fee = required_relay_fee_sats(change_len, relay_fee_sats_per_kb)?;
    let change = no_change_actual_fee
        .checked_sub(change_fee)
        .filter(|amount| *amount >= P2PKH_CHANGE_DUST_SATS);
    let (outputs, output_count) = if let Some(change_sats) = change {
        funded_split_outputs(
            deployment,
            &miner_lock,
            &original_lock,
            &shrec_lock,
            &funding_lock,
            miner_amount,
            original_amount,
            shrec_amount,
            Some(change_sats),
        )?
    } else {
        (no_change_outputs, no_change_count)
    };

    let reward_sighash = funded_p2pkh_sighash(
        &parent_txid,
        &funding.txid,
        funding.vout,
        0,
        reward.value_sats,
        Some(&reward_token_prefix),
        &reward_lock,
        &outputs,
    )?;
    let funding_sighash = funded_p2pkh_sighash(
        &parent_txid,
        &funding.txid,
        funding.vout,
        1,
        funding.value_sats,
        None,
        &funding_lock,
        &outputs,
    )?;
    let mut reward_signature = crypto::bch_schnorr_sign(reward_secret, &reward_sighash)?.to_vec();
    let mut funding_signature =
        crypto::bch_schnorr_sign(funding_secret, &funding_sighash)?.to_vec();
    reward_signature.push(SIGHASH_ALL_FORKID);
    funding_signature.push(SIGHASH_ALL_FORKID);
    let mut reward_unlocking = push_data(&reward_signature)?;
    reward_unlocking.extend_from_slice(&push_data(reward_public_key)?);
    let mut funding_unlocking = push_data(&funding_signature)?;
    funding_unlocking.extend_from_slice(&push_data(&funding_public_key)?);
    let raw_settlement = funded_split_raw(
        &parent_txid,
        &funding.txid,
        funding.vout,
        &reward_unlocking,
        &funding_unlocking,
        &outputs,
        output_count,
    )?;
    let expected_len = if change.is_some() {
        change_len
    } else {
        no_change_len
    };
    if raw_settlement.len() != expected_len {
        return Err("funded split relay-fee sizing changed after signing".into());
    }
    let required_relay_fee_sats = if change.is_some() {
        change_fee
    } else {
        no_change_fee
    };
    let fee_sats = no_change_actual_fee
        .checked_sub(change.unwrap_or(0))
        .ok_or("funded split BCH accounting underflow")?;
    if fee_sats < required_relay_fee_sats {
        return Err("funded split fee is below required relay fee".into());
    }
    Ok(PreparedFundedRewardSplit {
        parent_txid,
        settlement_txid: transaction_id(&raw_settlement),
        raw_settlement,
        funding_txid: funding.txid.clone(),
        funding_vout: funding.vout,
        funding_input_value_sats: funding.value_sats,
        funding_change_value_sats: change,
        miner_token_amount: miner_amount,
        original_donation_token_amount: original_amount,
        shrec_donation_token_amount: shrec_amount,
        required_relay_fee_sats,
        fee_sats,
    })
}

/// Signs one CashToken P2PKH input in an N-input BCH transaction.
fn batched_reward_sighash(
    outpoints: &[u8],
    sequences: &[u8],
    outpoint: &[u8],
    token_prefix: &[u8],
    reward_lock: &[u8],
    value_sats: u64,
    outputs: &[u8],
) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(180);
    preimage.extend_from_slice(&2u32.to_le_bytes());
    preimage.extend_from_slice(&hash256(outpoints));
    preimage.extend_from_slice(&hash256(sequences));
    preimage.extend_from_slice(outpoint);
    preimage.extend_from_slice(token_prefix);
    preimage.extend_from_slice(&compact_uint(reward_lock.len() as u64));
    preimage.extend_from_slice(reward_lock);
    preimage.extend_from_slice(&value_sats.to_le_bytes());
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&hash256(outputs));
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&u32::from(SIGHASH_ALL_FORKID).to_le_bytes());
    hash256(&preimage)
}

fn batched_reward_raw(
    rewards: &[ConfirmedRewardUtxo],
    unlockings: &[Vec<u8>],
    outputs: &[u8],
    output_count: u8,
) -> Result<Vec<u8>, String> {
    if rewards.len() != unlockings.len() {
        return Err("batch input and unlocking counts differ".into());
    }
    let mut raw = Vec::new();
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.extend_from_slice(&compact_uint(rewards.len() as u64));
    for (reward, unlocking) in rewards.iter().zip(unlockings) {
        raw.extend_from_slice(&encode_input(&reward.txid, reward.vout, unlocking)?);
    }
    raw.extend_from_slice(&compact_uint(u64::from(output_count)));
    raw.extend_from_slice(outputs);
    raw.extend_from_slice(&0u32.to_le_bytes());
    Ok(raw)
}

/// Spends confirmed reward outputs together so their BCH value pays the
/// three 700-sat token outputs and relay fee, without a separate BCH UTXO.
pub fn build_batched_reward_split_with_relay_fee(
    deployment: &PhotonDeployment,
    rewards: &[ConfirmedRewardUtxo],
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    miner_payout: &str,
    relay_fee_sats_per_kb: u64,
    change_payout: &str,
) -> Result<PreparedBatchedRewardSplit, String> {
    deployment.verify()?;
    if rewards.len() < 5 {
        return Err("batch split needs at least five confirmed rewards".into());
    }
    let derived_public = PublicKey::from_secret_key(
        &SecretKey::from_secret_bytes(*reward_secret).map_err(|error| error.to_string())?,
    )
    .serialize();
    if &derived_public != reward_public_key {
        return Err("reward public key does not match reward secret".into());
    }
    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);
    let mut seen = std::collections::HashSet::with_capacity(rewards.len());
    let mut input_outpoints = Vec::with_capacity(rewards.len());
    let mut outpoints = Vec::with_capacity(rewards.len() * 36);
    let mut sequences = Vec::with_capacity(rewards.len() * 4);
    let (mut miner_amount, mut original_amount, mut shrec_amount) = (0u128, 0u128, 0u128);
    let mut total_input_sats = 0u64;
    for reward in rewards {
        if reward.confirmations == 0 {
            return Err("batch reward must have at least one confirmation".into());
        }
        if !seen.insert((reward.txid.clone(), reward.vout)) {
            return Err("batch reward has a duplicate outpoint".into());
        }
        if transaction_id(&reward.raw_transaction) != reward.txid {
            return Err("batch reward raw transaction does not match its txid".into());
        }
        let output = transaction_output(&reward.raw_transaction, reward.vout)?;
        if reward.value_sats != TOKEN_OUTPUT_SATS || output.value_sats != TOKEN_OUTPUT_SATS {
            return Err("batch reward BCH value must be the proven 700 sats".into());
        }
        if reward.token_amount == 0 {
            return Err("batch reward token amount must be positive".into());
        }
        let mut expected = deployment_token_prefix(deployment, reward.token_amount)?;
        expected.extend_from_slice(&reward_lock);
        if output.token_and_locking_bytecode != expected {
            return Err("batch reward token category, amount, or owner does not match".into());
        }
        let (miner, donation) = RuntimeConfig::split_reward(reward.token_amount);
        let (original, shrec) = RuntimeConfig::split_donation(donation);
        miner_amount = miner_amount
            .checked_add(miner)
            .ok_or("miner token total overflow")?;
        original_amount = original_amount
            .checked_add(original)
            .ok_or("original donation token total overflow")?;
        shrec_amount = shrec_amount
            .checked_add(shrec)
            .ok_or("shrec donation token total overflow")?;
        total_input_sats = total_input_sats
            .checked_add(reward.value_sats)
            .ok_or("batch reward BCH input total overflow")?;
        input_outpoints.push((reward.txid.clone(), reward.vout));
        outpoints.extend_from_slice(&serialized_outpoint(&reward.txid, reward.vout)?);
        sequences.extend_from_slice(&0u32.to_le_bytes());
    }
    if miner_amount == 0 || original_amount == 0 || shrec_amount == 0 {
        return Err("batch reward donation is too small for three token outputs".into());
    }
    let miner_lock = tx::cashaddr_to_p2pkh_locking(miner_payout)?;
    let change_lock = tx::cashaddr_to_p2pkh_locking(change_payout)?;
    let original_address = if deployment.category_hex == CHIPNET_CATEGORY_HEX {
        CHIPNET_DONATION_ADDRESS
    } else {
        DONATION_ADDRESS
    };
    let original_lock = tx::cashaddr_to_p2pkh_locking(original_address)?;
    let shrec_lock = tx::cashaddr_to_p2pkh_locking(SHREC_DONATION_ADDRESS)?;
    let (no_change_outputs, no_change_count) = funded_split_outputs(
        deployment,
        &miner_lock,
        &original_lock,
        &shrec_lock,
        &change_lock,
        miner_amount,
        original_amount,
        shrec_amount,
        None,
    )?;
    let mut placeholder = push_data(&[0u8; 65])?;
    placeholder.extend_from_slice(&push_data(reward_public_key)?);
    let placeholders = vec![placeholder; rewards.len()];
    let no_change_len =
        batched_reward_raw(rewards, &placeholders, &no_change_outputs, no_change_count)?.len();
    let no_change_fee = required_relay_fee_sats(no_change_len, relay_fee_sats_per_kb)?;
    let token_output_sats = TOKEN_OUTPUT_SATS
        .checked_mul(3)
        .ok_or("batch output BCH overflow")?;
    let available_fee = total_input_sats
        .checked_sub(token_output_sats)
        .ok_or("batch rewards cannot fund three token outputs")?;
    if available_fee < no_change_fee {
        return Err(format!(
            "batch rewards cannot cover the required {no_change_fee}-sat relay fee"
        ));
    }
    let (change_sizing_outputs, change_sizing_count) = funded_split_outputs(
        deployment,
        &miner_lock,
        &original_lock,
        &shrec_lock,
        &change_lock,
        miner_amount,
        original_amount,
        shrec_amount,
        Some(P2PKH_CHANGE_DUST_SATS),
    )?;
    let change_len = batched_reward_raw(
        rewards,
        &placeholders,
        &change_sizing_outputs,
        change_sizing_count,
    )?
    .len();
    let change_fee = required_relay_fee_sats(change_len, relay_fee_sats_per_kb)?;
    let change = available_fee
        .checked_sub(change_fee)
        .filter(|amount| *amount >= P2PKH_CHANGE_DUST_SATS);
    let (outputs, output_count) = if let Some(change_sats) = change {
        funded_split_outputs(
            deployment,
            &miner_lock,
            &original_lock,
            &shrec_lock,
            &change_lock,
            miner_amount,
            original_amount,
            shrec_amount,
            Some(change_sats),
        )?
    } else {
        (no_change_outputs, no_change_count)
    };
    let mut unlockings = Vec::with_capacity(rewards.len());
    for (index, reward) in rewards.iter().enumerate() {
        let prefix = deployment_token_prefix(deployment, reward.token_amount)?;
        let signature_hash = batched_reward_sighash(
            &outpoints,
            &sequences,
            &outpoints[index * 36..(index + 1) * 36],
            &prefix,
            &reward_lock,
            reward.value_sats,
            &outputs,
        );
        let signature = crypto::bch_schnorr_sign(reward_secret, &signature_hash)?;
        if !crypto::bch_schnorr_verify(reward_public_key, &signature_hash, &signature)? {
            return Err("batch reward signature failed self-verification".into());
        }
        let mut signature_with_type = signature.to_vec();
        signature_with_type.push(SIGHASH_ALL_FORKID);
        let mut unlocking = push_data(&signature_with_type)?;
        unlocking.extend_from_slice(&push_data(reward_public_key)?);
        unlockings.push(unlocking);
    }
    let raw_settlement = batched_reward_raw(rewards, &unlockings, &outputs, output_count)?;
    let expected_len = if change.is_some() {
        change_len
    } else {
        no_change_len
    };
    if raw_settlement.len() != expected_len {
        return Err("batch relay-fee sizing changed after signing".into());
    }
    let required_relay_fee_sats = if change.is_some() {
        change_fee
    } else {
        no_change_fee
    };
    let fee_sats = available_fee
        .checked_sub(change.unwrap_or(0))
        .ok_or("batch BCH accounting underflow")?;
    if fee_sats < required_relay_fee_sats {
        return Err("batch fee is below required relay fee".into());
    }
    Ok(PreparedBatchedRewardSplit {
        settlement_txid: transaction_id(&raw_settlement),
        raw_settlement,
        input_outpoints,
        miner_token_amount: miner_amount,
        original_donation_token_amount: original_amount,
        shrec_donation_token_amount: shrec_amount,
        reward_change_value_sats: change,
        required_relay_fee_sats,
        fee_sats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CHIPNET_PHOTON;

    const VECTOR_BATON_TXID: &str =
        "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042";
    /// Builds the reference parent transaction with the given reward lock.
    fn vector_parent(reward_lock: Vec<u8>) -> Vec<u8> {
        tx::build_photon_template_bytes(&tx::TemplateParams {
            prev_tx_hash_hex: VECTOR_BATON_TXID.into(),
            prev_index: 0,
            age: 10,
            public_key_hex:
                "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
                    .into(),
            target_hex:
                "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                    .into(),
            signature_hex:
                "5b73543b21b74bd47b0dfc4565780e4ed2f0e5c4bb85f2c6dd3546727f84604fc6e8cc2b6b38de1c5630da8356e2e07a403ddeba8835caba0b80d75a5ac471e4"
                    .into(),
            nonce: 0x1234_5678,
            contract_value_sats: 15_971_500,
            contract_token_amount: 2_099_905_002_035_715,
            reward_amount: 4_999_773_813,
            payout_locking: reward_lock,
        })
        .unwrap()
    }

    fn chipnet_parent(reward_lock: Vec<u8>) -> Vec<u8> {
        tx::build_photon_template_bytes_for_deployment(
            &tx::TemplateParams {
                prev_tx_hash_hex: VECTOR_BATON_TXID.into(),
                prev_index: 0,
                age: 10,
                public_key_hex:
                    "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
                        .into(),
                target_hex:
                    "ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000"
                        .into(),
                signature_hex:
                    "5b73543b21b74bd47b0dfc4565780e4ed2f0e5c4bb85f2c6dd3546727f84604fc6e8cc2b6b38de1c5630da8356e2e07a403ddeba8835caba0b80d75a5ac471e4"
                        .into(),
                nonce: 0x1234_5678,
                contract_value_sats: 49_079_000,
                contract_token_amount: 2_099_905_002_035_715,
                reward_amount: 4_999_773_813,
                payout_locking: reward_lock,
            },
            &CHIPNET_PHOTON,
        )
        .unwrap()
    }

    fn funding_fixture(secret: [u8; 32], value_sats: u64) -> ConfirmedFundingUtxo {
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        let locking = p2pkh_locking_from_public_key(&public);
        let mut raw = Vec::new();
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&[0x42; 32]);
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.push(0);
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&encode_raw_output(value_sats, &locking));
        raw.extend_from_slice(&0u32.to_le_bytes());
        ConfirmedFundingUtxo {
            txid: transaction_id(&raw),
            raw_transaction: raw,
            vout: 0,
            value_sats,
            confirmations: 3,
        }
    }

    fn funded_split_fixture(
        value_sats: u64,
    ) -> (
        Vec<u8>,
        [u8; 32],
        [u8; 33],
        [u8; 32],
        ConfirmedFundingUtxo,
        String,
    ) {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let mut funding_secret = [0u8; 32];
        funding_secret[31] = 2;
        let parent = chipnet_parent(p2pkh_locking_from_public_key(&reward_public));
        let funding = funding_fixture(funding_secret, value_sats);
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        (
            parent,
            reward_secret,
            reward_public,
            funding_secret,
            funding,
            miner_payout,
        )
    }

    fn confirmed_reward_fixture(
        secret: [u8; 32],
        input_marker: u8,
        amount: u128,
    ) -> ConfirmedRewardUtxo {
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        let lock = p2pkh_locking_from_public_key(&public);
        let mut bytecode = deployment_token_prefix(&CHIPNET_PHOTON, amount).unwrap();
        bytecode.extend_from_slice(&lock);
        let mut raw = Vec::new();
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&[input_marker; 32]);
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.push(0);
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.push(1);
        raw.extend_from_slice(&encode_raw_output(TOKEN_OUTPUT_SATS, &bytecode));
        raw.extend_from_slice(&0u32.to_le_bytes());
        ConfirmedRewardUtxo {
            txid: transaction_id(&raw),
            raw_transaction: raw,
            vout: 0,
            value_sats: TOKEN_OUTPUT_SATS,
            token_amount: amount,
            confirmations: 2,
        }
    }

    #[test]
    fn chipnet_batch_five_rewards_fund_exact_split_and_fee() {
        let mut secret = [0u8; 32];
        secret[31] = 7;
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        let amounts = [100u128, 149, 150, 199, 200];
        let rewards: Vec<_> = (1..=5)
            .zip(amounts)
            .map(|(marker, amount)| confirmed_reward_fixture(secret, marker, amount))
            .collect();
        let payout = p2pkh_cashaddr_from_public_key(&public).unwrap();
        let batch = build_batched_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &rewards,
            &secret,
            &public,
            &payout,
            1_000,
            &payout,
        )
        .unwrap();
        assert_eq!(batch.input_outpoints.len(), 5);
        assert_eq!(batch.reward_change_value_sats, None);
        assert_eq!(batch.fee_sats, 1_400);
        assert!(batch.fee_sats >= batch.required_relay_fee_sats);
        assert_eq!(batch.miner_token_amount, 784);
        assert_eq!(batch.original_donation_token_amount, 8);
        assert_eq!(batch.shrec_donation_token_amount, 6);
        assert_eq!(batch.settlement_txid, transaction_id(&batch.raw_settlement));
        let raw = &batch.raw_settlement;
        let mut cursor = 4usize;
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 5);
        let mut outpoints = Vec::new();
        let mut signatures = Vec::new();
        for reward in &rewards {
            let expected = serialized_outpoint(&reward.txid, reward.vout).unwrap();
            assert_eq!(&raw[cursor..cursor + 36], expected);
            outpoints.extend_from_slice(&expected);
            cursor += 36;
            let script_len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            let script = &raw[cursor..cursor + script_len];
            assert_eq!(script[0], 65);
            assert_eq!(script[65], SIGHASH_ALL_FORKID);
            assert_eq!(script[66], 33);
            assert_eq!(&script[67..100], &public);
            signatures.push(script[1..65].to_vec());
            cursor += script_len;
            assert_eq!(&raw[cursor..cursor + 4], &[0u8; 4]);
            cursor += 4;
        }
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 3);
        let outputs_start = cursor;
        let locks = [
            tx::cashaddr_to_p2pkh_locking(&payout).unwrap(),
            tx::cashaddr_to_p2pkh_locking(CHIPNET_DONATION_ADDRESS).unwrap(),
            tx::cashaddr_to_p2pkh_locking(SHREC_DONATION_ADDRESS).unwrap(),
        ];
        for (index, amount) in [
            batch.miner_token_amount,
            batch.original_donation_token_amount,
            batch.shrec_donation_token_amount,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                u64::from_le_bytes(raw[cursor..cursor + 8].try_into().unwrap()),
                TOKEN_OUTPUT_SATS
            );
            cursor += 8;
            let len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            let mut expected = deployment_token_prefix(&CHIPNET_PHOTON, amount).unwrap();
            expected.extend_from_slice(&locks[index]);
            assert_eq!(&raw[cursor..cursor + len], expected);
            cursor += len;
        }
        assert_eq!(cursor + 4, raw.len());
        let outputs = &raw[outputs_start..cursor];
        let reward_lock = p2pkh_locking_from_public_key(&public);
        for (index, reward) in rewards.iter().enumerate() {
            let mut preimage = Vec::new();
            preimage.extend_from_slice(&2u32.to_le_bytes());
            preimage.extend_from_slice(&hash256(&outpoints));
            preimage.extend_from_slice(&hash256(&[0u8; 20]));
            preimage.extend_from_slice(&outpoints[index * 36..(index + 1) * 36]);
            preimage.extend_from_slice(
                &deployment_token_prefix(&CHIPNET_PHOTON, reward.token_amount).unwrap(),
            );
            preimage.push(reward_lock.len() as u8);
            preimage.extend_from_slice(&reward_lock);
            preimage.extend_from_slice(&TOKEN_OUTPUT_SATS.to_le_bytes());
            preimage.extend_from_slice(&0u32.to_le_bytes());
            preimage.extend_from_slice(&hash256(outputs));
            preimage.extend_from_slice(&0u32.to_le_bytes());
            preimage.extend_from_slice(&u32::from(SIGHASH_ALL_FORKID).to_le_bytes());
            let signature: [u8; 64] = signatures[index].as_slice().try_into().unwrap();
            assert!(crypto::bch_schnorr_verify(&public, &hash256(&preimage), &signature).unwrap());
        }
        if let Ok(path) = std::env::var("PICKAXE_BATCH_FIXTURE_OUT") {
            let parents: Vec<_> = rewards
                .iter()
                .map(|reward| {
                    serde_json::json!({
                        "txid": reward.txid,
                        "raw_transaction": hex::encode(&reward.raw_transaction),
                        "vout": reward.vout,
                        "value_sats": reward.value_sats,
                        "token_amount": reward.token_amount.to_string(),
                    })
                })
                .collect();
            let fixture = serde_json::json!({
                "network": "chipnet",
                "category_hex": CHIPNET_CATEGORY_HEX,
                "parent_rewards": parents,
                "child_txid": batch.settlement_txid,
                "child_raw": hex::encode(&batch.raw_settlement),
                "public_key_hex": hex::encode(public),
                "expected_output_values_sats": [700, 700, 700],
                "expected_output_token_amounts": [
                    batch.miner_token_amount.to_string(),
                    batch.original_donation_token_amount.to_string(),
                    batch.shrec_donation_token_amount.to_string(),
                ],
                "expected_output_locks_hex": locks.iter().map(hex::encode).collect::<Vec<_>>(),
                "fee_sats": batch.fee_sats,
            });
            std::fs::write(path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
        }
    }

    #[test]
    fn chipnet_batch_rejects_four_rewards_and_duplicate_outpoints() {
        let mut secret = [0u8; 32];
        secret[31] = 7;
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        let mut rewards: Vec<_> = (1..=4)
            .map(|marker| confirmed_reward_fixture(secret, marker, 1000))
            .collect();
        let payout = p2pkh_cashaddr_from_public_key(&public).unwrap();
        let build = |rewards: &[ConfirmedRewardUtxo]| {
            build_batched_reward_split_with_relay_fee(
                &CHIPNET_PHOTON,
                rewards,
                &secret,
                &public,
                &payout,
                1_000,
                &payout,
            )
        };
        assert!(build(&rewards).unwrap_err().contains("at least five"));
        rewards.push(rewards[0].clone());
        assert!(build(&rewards).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn chipnet_batch_rejects_wrong_key_category_amount_and_unconfirmed_rewards() {
        let mut secret = [0u8; 32];
        secret[31] = 7;
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        let payout = p2pkh_cashaddr_from_public_key(&public).unwrap();
        let rewards: Vec<_> = (1..=5)
            .map(|marker| confirmed_reward_fixture(secret, marker, 1000))
            .collect();
        let build = |rewards: &[ConfirmedRewardUtxo], key: &[u8; 32]| {
            build_batched_reward_split_with_relay_fee(
                &CHIPNET_PHOTON,
                rewards,
                key,
                &public,
                &payout,
                1_000,
                &payout,
            )
        };
        let mut wrong_secret = [0u8; 32];
        wrong_secret[31] = 8;
        assert!(build(&rewards, &wrong_secret)
            .unwrap_err()
            .contains("does not match reward secret"));
        let mut changed = rewards.clone();
        changed[0].confirmations = 0;
        assert!(build(&changed, &secret)
            .unwrap_err()
            .contains("confirmation"));
        changed = rewards.clone();
        changed[0].token_amount += 1;
        assert!(build(&changed, &secret)
            .unwrap_err()
            .contains("category, amount, or owner"));
        changed = rewards.clone();
        let wrong_category =
            deployment_token_prefix(&crate::protocol::MAINNET_PHOTON, 1000).unwrap();
        let right_category = deployment_token_prefix(&CHIPNET_PHOTON, 1000).unwrap();
        let offset = changed[0]
            .raw_transaction
            .windows(right_category.len())
            .position(|window| window == right_category)
            .unwrap();
        changed[0].raw_transaction[offset..offset + wrong_category.len()]
            .copy_from_slice(&wrong_category);
        changed[0].txid = transaction_id(&changed[0].raw_transaction);
        assert!(build(&changed, &secret)
            .unwrap_err()
            .contains("category, amount, or owner"));
    }

    #[test]
    fn chipnet_batch_uses_six_rewards_for_high_fee_and_returns_change() {
        let mut secret = [0u8; 32];
        secret[31] = 7;
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        let payout = p2pkh_cashaddr_from_public_key(&public).unwrap();
        let mut funding_secret = [0u8; 32];
        funding_secret[31] = 8;
        let funding_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(funding_secret).unwrap())
                .serialize();
        let change_payout = p2pkh_cashaddr_from_public_key(&funding_public).unwrap();
        let mut rewards: Vec<_> = (1..=5)
            .map(|marker| confirmed_reward_fixture(secret, marker, 1000))
            .collect();
        assert!(build_batched_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &rewards,
            &secret,
            &public,
            &payout,
            2_000,
            &change_payout,
        )
        .unwrap_err()
        .contains("relay fee"));
        rewards.push(confirmed_reward_fixture(secret, 6, 1000));
        assert!(build_batched_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &rewards,
            &secret,
            &public,
            &payout,
            1_000,
            "not-a-cashaddr",
        )
        .is_err());
        let batch = build_batched_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &rewards,
            &secret,
            &public,
            &payout,
            1_000,
            &change_payout,
        )
        .unwrap();
        assert!(batch.reward_change_value_sats.unwrap() >= P2PKH_CHANGE_DUST_SATS);
        assert_eq!(batch.fee_sats, batch.required_relay_fee_sats);
        let raw = &batch.raw_settlement;
        let mut cursor = 4usize;
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 6);
        for _ in 0..6 {
            cursor += 36;
            let script_len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            cursor += script_len + 4;
        }
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 4);
        for _ in 0..3 {
            cursor += 8;
            let len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            cursor += len;
        }
        assert_eq!(
            u64::from_le_bytes(raw[cursor..cursor + 8].try_into().unwrap()),
            batch.reward_change_value_sats.unwrap()
        );
        cursor += 8;
        let len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
        assert_eq!(
            &raw[cursor..cursor + len],
            p2pkh_locking_from_public_key(&funding_public)
        );
        cursor += len;
        assert_eq!(cursor + 4, raw.len());
        if let Ok(path) = std::env::var("PICKAXE_BATCH_CHANGE_FIXTURE_OUT") {
            let fixture = serde_json::json!({
                "network": "chipnet",
                "category_hex": CHIPNET_CATEGORY_HEX,
                "parent_rewards": rewards.iter().map(|reward| serde_json::json!({
                    "txid": reward.txid,
                    "raw_transaction": hex::encode(&reward.raw_transaction),
                    "vout": reward.vout,
                    "value_sats": reward.value_sats,
                    "token_amount": reward.token_amount.to_string(),
                })).collect::<Vec<_>>(),
                "child_txid": batch.settlement_txid,
                "child_raw": hex::encode(&batch.raw_settlement),
                "public_key_hex": hex::encode(public),
                "expected_output_values_sats": [700, 700, 700, batch.reward_change_value_sats.unwrap()],
                "expected_output_token_amounts": [
                    batch.miner_token_amount.to_string(),
                    batch.original_donation_token_amount.to_string(),
                    batch.shrec_donation_token_amount.to_string(),
                ],
                "expected_change_lock_hex": hex::encode(p2pkh_locking_from_public_key(&funding_public)),
                "fee_sats": batch.fee_sats,
            });
            std::fs::write(path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();
        }
    }

    #[test]
    fn chipnet_funded_split_spends_only_reward_and_funding_with_valid_signatures() {
        let (parent, reward_secret, reward_public, funding_secret, funding, miner_payout) =
            funded_split_fixture(10_000);
        let amount = 4_999_773_813u128;
        let split = build_funded_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            amount,
            &funding,
            &funding_secret,
            1_000,
        )
        .unwrap();
        assert_eq!(split.original_donation_token_amount, amount / 100);
        assert_eq!(split.shrec_donation_token_amount, amount / 100);
        assert_eq!(
            split.miner_token_amount
                + split.original_donation_token_amount
                + split.shrec_donation_token_amount,
            amount
        );
        assert!(split.funding_change_value_sats.unwrap() >= P2PKH_CHANGE_DUST_SATS);
        assert_eq!(split.fee_sats, split.required_relay_fee_sats);
        assert_eq!(
            split.required_relay_fee_sats,
            split.raw_settlement.len() as u64
        );

        let raw = &split.raw_settlement;
        let mut cursor = 4usize;
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 2);
        let mut outpoints = Vec::new();
        let mut signatures = Vec::new();
        for (expected_txid, expected_vout, expected_pubkey) in [
            (&split.parent_txid, 1u32, reward_public),
            (
                &split.funding_txid,
                split.funding_vout,
                PublicKey::from_secret_key(&SecretKey::from_secret_bytes(funding_secret).unwrap())
                    .serialize(),
            ),
        ] {
            let outpoint = raw[cursor..cursor + 36].to_vec();
            assert_eq!(
                outpoint,
                serialized_outpoint(expected_txid, expected_vout).unwrap()
            );
            outpoints.extend_from_slice(&outpoint);
            cursor += 36;
            let script_len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            let script = &raw[cursor..cursor + script_len];
            assert_eq!(script[0], 65);
            assert_eq!(script[65], SIGHASH_ALL_FORKID);
            assert_eq!(script[66], 33);
            assert_eq!(&script[67..100], &expected_pubkey);
            signatures.push(script[1..65].to_vec());
            cursor += script_len;
            assert_eq!(&raw[cursor..cursor + 4], &[0u8; 4]);
            cursor += 4;
        }
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 4);
        let outputs_start = cursor;
        let expected_locks = [
            tx::cashaddr_to_p2pkh_locking(&miner_payout).unwrap(),
            tx::cashaddr_to_p2pkh_locking(CHIPNET_DONATION_ADDRESS).unwrap(),
            tx::cashaddr_to_p2pkh_locking(SHREC_DONATION_ADDRESS).unwrap(),
        ];
        for (index, amount) in [
            split.miner_token_amount,
            split.original_donation_token_amount,
            split.shrec_donation_token_amount,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                u64::from_le_bytes(raw[cursor..cursor + 8].try_into().unwrap()),
                TOKEN_OUTPUT_SATS
            );
            cursor += 8;
            let len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            let mut expected = deployment_token_prefix(&CHIPNET_PHOTON, amount).unwrap();
            expected.extend_from_slice(&expected_locks[index]);
            assert_eq!(&raw[cursor..cursor + len], expected);
            cursor += len;
        }
        assert_eq!(
            u64::from_le_bytes(raw[cursor..cursor + 8].try_into().unwrap()),
            split.funding_change_value_sats.unwrap()
        );
        cursor += 8;
        let change_len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
        let funding_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(funding_secret).unwrap())
                .serialize();
        assert_eq!(
            &raw[cursor..cursor + change_len],
            p2pkh_locking_from_public_key(&funding_public)
        );
        cursor += change_len;
        assert_eq!(cursor + 4, raw.len());
        let outputs = &raw[outputs_start..cursor];

        // Reconstruct the BCH signing preimage from parsed wire bytes, without
        // calling the production sighash helper used to create either signature.
        for (index, (value, token, lock, public)) in [
            (
                TOKEN_OUTPUT_SATS,
                Some(deployment_token_prefix(&CHIPNET_PHOTON, amount).unwrap()),
                p2pkh_locking_from_public_key(&reward_public),
                reward_public,
            ),
            (
                funding.value_sats,
                None,
                p2pkh_locking_from_public_key(&funding_public),
                funding_public,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let mut preimage = Vec::new();
            preimage.extend_from_slice(&2u32.to_le_bytes());
            preimage.extend_from_slice(&hash256(&outpoints));
            preimage.extend_from_slice(&hash256(&[0u8; 8]));
            preimage.extend_from_slice(&outpoints[index * 36..index * 36 + 36]);
            if let Some(token) = token {
                preimage.extend_from_slice(&token);
            }
            preimage.push(lock.len() as u8);
            preimage.extend_from_slice(&lock);
            preimage.extend_from_slice(&value.to_le_bytes());
            preimage.extend_from_slice(&0u32.to_le_bytes());
            preimage.extend_from_slice(&hash256(outputs));
            preimage.extend_from_slice(&0u32.to_le_bytes());
            preimage.extend_from_slice(&0x41u32.to_le_bytes());
            let signature: [u8; 64] = signatures[index].as_slice().try_into().unwrap();
            assert!(crypto::bch_schnorr_verify(&public, &hash256(&preimage), &signature).unwrap());
        }
    }

    #[test]
    fn chipnet_funded_split_rejects_unconfirmed_tokenized_or_wrong_funding() {
        let (parent, reward_secret, reward_public, funding_secret, funding, miner_payout) =
            funded_split_fixture(10_000);
        let build = |funding: &ConfirmedFundingUtxo, funding_secret: &[u8; 32]| {
            build_funded_reward_split_with_relay_fee(
                &CHIPNET_PHOTON,
                &parent,
                &reward_secret,
                &reward_public,
                &miner_payout,
                4_999_773_813,
                funding,
                funding_secret,
                1_000,
            )
        };
        let mut unconfirmed = funding.clone();
        unconfirmed.confirmations = 0;
        assert!(build(&unconfirmed, &funding_secret)
            .unwrap_err()
            .contains("confirmation"));
        let mut wrong_txid = funding.clone();
        wrong_txid.txid.replace_range(0..1, "0");
        assert!(build(&wrong_txid, &funding_secret)
            .unwrap_err()
            .contains("does not match its txid"));
        let mut wrong_value = funding.clone();
        wrong_value.value_sats += 1;
        assert!(build(&wrong_value, &funding_secret)
            .unwrap_err()
            .contains("value does not match"));
        let mut tokenized = funding.clone();
        let locking = p2pkh_locking_from_public_key(
            &PublicKey::from_secret_key(&SecretKey::from_secret_bytes(funding_secret).unwrap())
                .serialize(),
        );
        let mut raw = tokenized.raw_transaction[..47].to_vec();
        raw.extend_from_slice(&encode_output(10_000, Some(1), &locking).unwrap());
        raw.extend_from_slice(&0u32.to_le_bytes());
        tokenized.raw_transaction = raw;
        tokenized.txid = transaction_id(&tokenized.raw_transaction);
        assert!(build(&tokenized, &funding_secret)
            .unwrap_err()
            .contains("CashToken prefix"));
        let mut other_secret = [0u8; 32];
        other_secret[31] = 3;
        assert!(build(&funding, &other_secret)
            .unwrap_err()
            .contains("owned by funding key"));
    }

    #[test]
    fn chipnet_funded_split_rejects_old_parent_and_insufficient_funding() {
        let (parent, reward_secret, reward_public, funding_secret, funding, miner_payout) =
            funded_split_fixture(1_800);
        let error = build_funded_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &funding,
            &funding_secret,
            1_000,
        )
        .unwrap_err();
        assert!(error.contains("cannot cover"));
        let mainnet_parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let rich_funding = funding_fixture(funding_secret, 10_000);
        let error = build_funded_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &mainnet_parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &rich_funding,
            &funding_secret,
            1_000,
        )
        .unwrap_err();
        assert!(error.contains("wrong PHOTON token category"));
    }

    #[test]
    fn chipnet_funded_split_omits_dust_change_and_meets_relay_fee() {
        let (parent, reward_secret, reward_public, funding_secret, funding, miner_payout) =
            funded_split_fixture(2_300);
        let split = build_funded_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &funding,
            &funding_secret,
            1_000,
        )
        .unwrap();
        assert_eq!(split.funding_change_value_sats, None);
        assert!(split.fee_sats >= split.required_relay_fee_sats);
        assert_eq!(split.fee_sats, 900);
        let raw = &split.raw_settlement;
        let mut cursor = 4usize;
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 2);
        for _ in 0..2 {
            cursor += 36;
            let script_len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            cursor += script_len + 4;
        }
        assert_eq!(read_compact_uint(raw, &mut cursor).unwrap(), 3);
    }

    #[test]
    fn chipnet_funded_split_rejects_non_700_parent_reward() {
        let (mut parent, reward_secret, reward_public, funding_secret, funding, miner_payout) =
            funded_split_fixture(10_000);
        let [_, reward] = parse_parent_outputs(&parent).unwrap();
        let mut reward_bytes =
            encode_raw_output(reward.value_sats, &reward.token_and_locking_bytecode);
        reward_bytes[..8].copy_from_slice(&707u64.to_le_bytes());
        let original = encode_raw_output(reward.value_sats, &reward.token_and_locking_bytecode);
        let offset = parent
            .windows(original.len())
            .position(|window| window == original)
            .unwrap();
        parent[offset..offset + reward_bytes.len()].copy_from_slice(&reward_bytes);
        let error = build_funded_reward_split_with_relay_fee(
            &CHIPNET_PHOTON,
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &funding,
            &funding_secret,
            1_000,
        )
        .unwrap_err();
        assert!(error.contains("proven 700 sats"));
    }

    #[test]
    /// Checks that runtime intermediate key matches standard p2pkh vector.
    fn runtime_intermediate_key_matches_standard_p2pkh_vector() {
        let mut secret = [0u8; 32];
        secret[31] = 1;
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(secret).unwrap()).serialize();
        assert_eq!(
            hex::encode(p2pkh_locking_from_public_key(&public)),
            "76a914751e76e8199196d454941c45d1b3a323f1433bd688ac"
        );
        let address = p2pkh_cashaddr_from_public_key(&public).unwrap();
        assert_eq!(
            tx::cashaddr_to_p2pkh_locking(&address).unwrap(),
            p2pkh_locking_from_public_key(&public)
        );
    }

    #[test]
    /// Checks reward rounding at token amounts near the donation split boundary.
    fn reward_split_rounding_regression() {
        assert_eq!(RuntimeConfig::split_reward(0), (0, 0));
        assert_eq!(RuntimeConfig::split_reward(49), (49, 0));
        assert_eq!(RuntimeConfig::split_reward(50), (49, 1));
        assert_eq!(RuntimeConfig::split_reward(100), (98, 2));
    }

    #[test]
    fn funded_split_preserves_existing_odd_donation_rounding() {
        for amount in [100u128, 199, 4_999_773_850] {
            let (miner, original, shrec) = funded_split_amounts(amount).unwrap();
            let (expected_miner, donation) = RuntimeConfig::split_reward(amount);
            let (expected_original, expected_shrec) = RuntimeConfig::split_donation(donation);
            assert_eq!(
                (miner, original, shrec),
                (expected_miner, expected_original, expected_shrec)
            );
            assert_eq!(miner + original + shrec, amount);
        }
        assert_eq!(funded_split_amounts(199).unwrap(), (196, 2, 1));
        assert_eq!(
            funded_split_amounts(99).unwrap_err(),
            "reward donation is too small for two nonzero token outputs"
        );
    }

    #[test]
    /// Checks that self funded settlement derives budget from authoritative redeem script.
    fn self_funded_settlement_derives_budget_from_authoritative_redeem_script() {
        assert_eq!(
            photon_multi_input_max_baton_decrease_sats().unwrap(),
            PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS
        );
    }

    #[test]
    /// Checks that self funded settlement matches vm fixture accounting.
    fn self_funded_settlement_matches_vm_fixture_accounting() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let settlement = build_self_funded_settlement(
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
        )
        .unwrap();

        assert_eq!(settlement.raw_settlement.len(), 867);
        assert_eq!(settlement.required_relay_fee_sats, 867);
        assert_eq!(settlement.fee_sats, 867);
        assert_eq!(settlement.baton_input_value_sats, 15_970_000);
        assert_eq!(settlement.baton_output_value_sats, 15_967_733);
        assert_eq!(settlement.miner_token_amount, 4_899_778_337);
        assert_eq!(settlement.donation_token_amount, 99_995_476);
        assert_eq!(settlement.original_donation_token_amount, 49_997_738);
        assert_eq!(settlement.shrec_donation_token_amount, 49_997_738);
        assert_eq!(
            hex::encode(hash256(&settlement.raw_settlement)),
            "271815f7527c379a72f71ba216272750f48ff2fb0c7333dc537b2b8d3114b0c3"
        );
        assert_eq!(
            settlement.miner_token_amount + settlement.donation_token_amount,
            4_999_773_813
        );
        assert_eq!(
            settlement.baton_input_value_sats - settlement.baton_output_value_sats,
            2 * TOKEN_OUTPUT_SATS + settlement.fee_sats
        );
        assert!(
            settlement.baton_input_value_sats - settlement.baton_output_value_sats
                <= PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS
        );

        let parent_outpoint = reverse(&hex::decode(&settlement.parent_txid).unwrap());
        let raw = &settlement.raw_settlement;
        assert_eq!(raw[4], 2);
        let mut cursor = 5usize;
        for expected_vout in [0u32, 1] {
            assert_eq!(&raw[cursor..cursor + 32], parent_outpoint.as_slice());
            cursor += 32;
            assert_eq!(&raw[cursor..cursor + 4], &expected_vout.to_le_bytes());
            cursor += 4;
            let script_len = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            cursor += script_len + 4;
        }
        assert_eq!(raw[cursor], 4);
        cursor += 1;
        let expected = [
            (settlement.baton_output_value_sats, None),
            (
                TOKEN_OUTPUT_SATS,
                Some((
                    settlement.miner_token_amount,
                    tx::cashaddr_to_p2pkh_locking(&miner_payout).unwrap(),
                )),
            ),
            (
                TOKEN_OUTPUT_SATS,
                Some((
                    settlement.original_donation_token_amount,
                    tx::cashaddr_to_p2pkh_locking(DONATION_ADDRESS).unwrap(),
                )),
            ),
            (
                TOKEN_OUTPUT_SATS,
                Some((
                    settlement.shrec_donation_token_amount,
                    tx::cashaddr_to_p2pkh_locking(SHREC_DONATION_ADDRESS).unwrap(),
                )),
            ),
        ];
        for (value, token) in expected {
            let output_value = u64::from_le_bytes(raw[cursor..cursor + 8].try_into().unwrap());
            assert_eq!(output_value, value);
            cursor += 8;
            let size = read_compact_uint(raw, &mut cursor).unwrap() as usize;
            let bytecode = &raw[cursor..cursor + size];
            if let Some((amount, lock)) = token {
                let mut expected_bytecode = token_prefix(amount).unwrap();
                expected_bytecode.extend_from_slice(&lock);
                assert_eq!(bytecode, expected_bytecode);
            }
            cursor += size;
        }
        assert_eq!(cursor + 4, raw.len());
    }

    #[test]
    /// Checks that self funded settlement uses supplied relay fee floor.
    fn self_funded_settlement_uses_supplied_relay_fee_floor() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();

        let settlement = build_self_funded_settlement_with_relay_fee(
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            2_000,
        )
        .unwrap();

        assert_eq!(settlement.raw_settlement.len(), 867);
        assert_eq!(settlement.required_relay_fee_sats, 1_734);
        assert_eq!(settlement.fee_sats, 1_734);
        assert_eq!(settlement.baton_output_value_sats, 15_966_866);
        assert_eq!(
            settlement.baton_input_value_sats - settlement.baton_output_value_sats,
            2 * TOKEN_OUTPUT_SATS + settlement.fee_sats
        );
    }

    #[test]
    /// Checks that self funded settlement rejects relay floor above covenant budget.
    fn self_funded_settlement_rejects_relay_floor_above_covenant_budget() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();

        let error = build_self_funded_settlement_with_relay_fee(
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            10_000,
        )
        .unwrap_err();
        assert!(error.contains("covenant permits at most"));
    }

    #[test]
    /// Checks that self funded settlement rejects reward state mismatch.
    fn self_funded_settlement_rejects_reward_state_mismatch() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let error = build_self_funded_settlement(
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_812,
        )
        .unwrap_err();
        assert!(error.contains("reward output does not match"));
    }

    #[test]
    /// Checks that self funded settlement rejects wrong baton category.
    fn self_funded_settlement_rejects_wrong_baton_category() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let mut parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let category_le = reverse(&hex::decode(MAINNET_CATEGORY_HEX).unwrap());
        let prefix = [vec![0xef], category_le.clone(), vec![0x71]].concat();
        let offset = parent
            .windows(prefix.len())
            .position(|window| window == prefix)
            .expect("authoritative baton token prefix");
        parent[offset + 1] ^= 1;
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();

        let error = build_self_funded_settlement(
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
        )
        .unwrap_err();

        assert!(error.contains("wrong PHOTON token category"));
    }

    #[test]
    /// Checks that self funded settlement rejects non mutable baton capability.
    fn self_funded_settlement_rejects_non_mutable_baton_capability() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let mut parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let category_le = reverse(&hex::decode(MAINNET_CATEGORY_HEX).unwrap());
        let prefix = [vec![0xef], category_le, vec![0x71]].concat();
        let offset = parent
            .windows(prefix.len())
            .position(|window| window == prefix)
            .expect("authoritative baton token prefix");
        parent[offset + 33] = 0x70;
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();

        let error = build_self_funded_settlement(
            &parent,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
        )
        .unwrap_err();

        assert!(error.contains("not a mutable PHOTON NFT"));
    }

    #[test]
    /// Checks that authoritative baton parser rejects noncanonical commitment length.
    fn authoritative_baton_parser_rejects_noncanonical_commitment_length() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let [baton, _] = parse_parent_outputs(&parent).unwrap();
        validate_authoritative_baton_token(&baton.token_and_locking_bytecode).unwrap();

        let mut malformed = baton.token_and_locking_bytecode;
        let commitment_len_offset = 1 + 32 + 1;
        assert_eq!(malformed[commitment_len_offset], 100);
        malformed.splice(
            commitment_len_offset..=commitment_len_offset,
            [0xfd, 100, 0],
        );

        let error = validate_authoritative_baton_token(&malformed).unwrap_err();
        assert!(error.contains("non-canonical CompactSize"));
    }

    #[test]
    /// Checks that authoritative baton parser rejects short reference commitment.
    fn authoritative_baton_parser_rejects_short_reference_commitment() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let [baton, _] = parse_parent_outputs(&parent).unwrap();
        let mut malformed = baton.token_and_locking_bytecode;
        let commitment_len_offset = 1 + 32 + 1;
        malformed[commitment_len_offset] = 35;

        let error = validate_authoritative_baton_token(&malformed).unwrap_err();
        assert!(error.contains("commitment is missing or too short"));
    }
}
