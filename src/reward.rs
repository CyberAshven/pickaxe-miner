//! Protocol-valid Pickaxe 98/2 reward settlement.
//!
//! Production settlement spends the newly-created PHOTON baton and reward
//! together, preserves the baton at output 0, and splits the exact winning
//! reward 98/2 without any external funding input.

use crate::config::{RuntimeConfig, DONATION_ADDRESS, DONATION_BPS};
use crate::crypto;
use crate::protocol::{COVENANT_LOCKING_BYTECODE_HEX, MAINNET_CATEGORY_HEX, REDEEM_SCRIPT_HEX};
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
    pub miner_token_amount: u128,
    pub donation_token_amount: u128,
    pub required_relay_fee_sats: u64,
    pub fee_sats: u64,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedOutput {
    value_sats: u64,
    token_and_locking_bytecode: Vec<u8>,
}

fn hash256(data: &[u8]) -> [u8; 32] {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

fn decode_positive_script_number(bytes: &[u8]) -> Result<u64, String> {
    if bytes.is_empty() {
        return Ok(0);
    }
    if bytes.len() > 8 {
        return Err("PHOTON covenant value rule exceeds u64 ScriptNum range".into());
    }
    if bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
        return Err("PHOTON covenant value rule must be non-negative".into());
    }
    if bytes == [0]
        || (bytes.len() > 1 && bytes.last() == Some(&0) && bytes[bytes.len() - 2] & 0x80 == 0)
    {
        return Err("PHOTON covenant value rule uses a non-minimal ScriptNum".into());
    }

    let mut value = 0u64;
    for (index, byte) in bytes.iter().enumerate() {
        value |= u64::from(*byte) << (index * 8);
    }
    Ok(value)
}

fn extract_multi_input_max_baton_decrease_sats(redeem_script: &[u8]) -> Result<u64, String> {
    // Authoritative multi-input branch value rule:
    //   output[active].value <push positive ScriptNum> ADD input[active].value GREATERTHANOREQUAL VERIFY
    // The introspection opcodes surrounding the push make this distinct from the
    // normal one-input branch's 1500-sat rule.
    const RULE_PREFIX: &[u8] = &[0xc0, 0xcc];
    const RULE_SUFFIX: &[u8] = &[0x93, 0xc0, 0xc6, 0xa2, 0x69];

    let mut found = None;
    for start in 0..redeem_script.len() {
        if redeem_script.get(start..start + RULE_PREFIX.len()) != Some(RULE_PREFIX) {
            continue;
        }
        let push_len = match redeem_script.get(start + RULE_PREFIX.len()) {
            Some(1..=8) => usize::from(redeem_script[start + RULE_PREFIX.len()]),
            _ => continue,
        };
        let data_start = start + RULE_PREFIX.len() + 1;
        let Some(data_end) = data_start.checked_add(push_len) else {
            continue;
        };
        let Some(suffix_end) = data_end.checked_add(RULE_SUFFIX.len()) else {
            continue;
        };
        if redeem_script.get(data_end..suffix_end) != Some(RULE_SUFFIX) {
            continue;
        }
        let data = redeem_script
            .get(data_start..data_end)
            .ok_or("truncated PHOTON covenant value rule")?;
        let value = decode_positive_script_number(data)?;
        if found.replace(value).is_some() {
            return Err("PHOTON redeem script contains multiple multi-input value rules".into());
        }
    }
    found.ok_or_else(|| "PHOTON redeem script multi-input value rule was not found".into())
}

fn authoritative_redeem_script() -> Result<(Vec<u8>, u64), String> {
    let redeem_script = hex::decode(REDEEM_SCRIPT_HEX.trim()).map_err(|error| error.to_string())?;
    if redeem_script.len() != 259 {
        return Err(format!(
            "PHOTON redeem script must be 259 bytes (got {})",
            redeem_script.len()
        ));
    }

    let covenant_lock =
        hex::decode(COVENANT_LOCKING_BYTECODE_HEX).map_err(|error| error.to_string())?;
    if covenant_lock.len() != 35
        || covenant_lock[0] != 0xaa
        || covenant_lock[1] != 0x20
        || covenant_lock[34] != 0x87
    {
        return Err("PHOTON covenant locking bytecode is not canonical P2SH32".into());
    }
    let redeem_hash = hash256(&redeem_script);
    if covenant_lock[2..34] != redeem_hash {
        return Err("PHOTON redeem script does not match the covenant locking bytecode".into());
    }

    let multi_input_max_baton_decrease_sats =
        extract_multi_input_max_baton_decrease_sats(&redeem_script)?;
    Ok((redeem_script, multi_input_max_baton_decrease_sats))
}

pub fn photon_multi_input_max_baton_decrease_sats() -> Result<u64, String> {
    Ok(authoritative_redeem_script()?.1)
}

fn reverse(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().rev().copied().collect()
}

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

pub fn transaction_id(raw: &[u8]) -> String {
    hex::encode(reverse(&hash256(raw)))
}

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

fn compact_token_amount(value: u128) -> Result<Vec<u8>, String> {
    let value = u64::try_from(value).map_err(|_| "token amount exceeds u64")?;
    Ok(compact_uint(value))
}

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
fn encode_raw_output(value_sats: u64, bytecode: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&value_sats.to_le_bytes());
    out.extend_from_slice(&compact_uint(bytecode.len() as u64));
    out.extend_from_slice(bytecode);
    out
}

#[cfg_attr(not(test), allow(dead_code))]
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

fn read_canonical_compact_uint(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let start = *cursor;
    let value = read_compact_uint(bytes, cursor)?;
    if bytes.get(start..*cursor) != Some(compact_uint(value).as_slice()) {
        return Err("non-canonical CompactSize encoding in PHOTON baton".into());
    }
    Ok(value)
}

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

fn serialized_outpoint(txid: &str, vout: u32) -> Result<Vec<u8>, String> {
    let hash = parse_txid_display(txid)?;
    let mut out = reverse(&hash);
    out.extend_from_slice(&vout.to_le_bytes());
    Ok(out)
}

fn encode_input(txid: &str, vout: u32, unlocking: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = serialized_outpoint(txid, vout)?;
    out.extend_from_slice(&compact_uint(unlocking.len() as u64));
    out.extend_from_slice(unlocking);
    out.extend_from_slice(&0u32.to_le_bytes());
    Ok(out)
}

#[cfg_attr(not(test), allow(dead_code))]
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

pub fn p2pkh_locking_from_public_key(public_key: &[u8; 33]) -> Vec<u8> {
    let sha = Sha256::digest(public_key);
    let hash = Ripemd160::digest(sha);
    let mut out = Vec::with_capacity(25);
    out.extend_from_slice(&[0x76, 0xa9, 0x14]);
    out.extend_from_slice(&hash);
    out.extend_from_slice(&[0x88, 0xac]);
    out
}

pub fn p2pkh_cashaddr_from_public_key(public_key: &[u8; 33]) -> Result<String, String> {
    let locking = p2pkh_locking_from_public_key(public_key);
    let hash: [u8; 20] = locking[3..23]
        .try_into()
        .map_err(|_| "internal P2PKH hash length error")?;
    tx::p2pkh_hash_to_cashaddr(&hash)
}

pub fn new_intermediate_identity() -> Result<([u8; 32], [u8; 33], String), String> {
    let secret = SecretKey::new(&mut rand::rng());
    let secret_bytes = secret.to_secret_bytes();
    let public_key = PublicKey::from_secret_key(&secret).serialize();
    let address = p2pkh_cashaddr_from_public_key(&public_key)?;
    Ok((secret_bytes, public_key, address))
}

#[cfg_attr(not(test), allow(dead_code))]
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
fn build_self_funded_outputs(
    baton_output_value_sats: u64,
    baton_token_and_locking_bytecode: &[u8],
    reward_value_sats: u64,
    miner_lock: &[u8],
    donation_lock: &[u8],
    miner_token_amount: u128,
    donation_token_amount: u128,
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
        Some(donation_token_amount),
        donation_lock,
    )?);
    Ok(outputs)
}

#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
fn self_funded_serialized_len(
    parent_txid: &str,
    baton_output_value_sats: u64,
    baton_token_and_locking_bytecode: &[u8],
    reward_value_sats: u64,
    reward_public_key: &[u8; 33],
    miner_lock: &[u8],
    donation_lock: &[u8],
    miner_token_amount: u128,
    donation_token_amount: u128,
) -> Result<usize, String> {
    let redeem_script = hex::decode(REDEEM_SCRIPT_HEX.trim()).map_err(|error| error.to_string())?;
    if redeem_script.len() != 259 {
        return Err(format!(
            "PHOTON redeem script must be 259 bytes (got {})",
            redeem_script.len()
        ));
    }
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
        miner_token_amount,
        donation_token_amount,
    )?;

    let mut raw = Vec::new();
    raw.extend_from_slice(&2u32.to_le_bytes());
    raw.push(2);
    raw.extend_from_slice(&encode_input(parent_txid, 0, &baton_unlocking)?);
    raw.extend_from_slice(&encode_input(parent_txid, 1, &reward_unlocking)?);
    raw.push(3);
    raw.extend_from_slice(&outputs);
    raw.extend_from_slice(&0u32.to_le_bytes());
    Ok(raw.len())
}

#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
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
    miner_token_amount: u128,
    donation_token_amount: u128,
) -> Result<Vec<u8>, String> {
    let (redeem_script, _) = authoritative_redeem_script()?;
    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);
    let outputs = build_self_funded_outputs(
        baton_output_value_sats,
        baton_token_and_locking_bytecode,
        reward_value_sats,
        miner_lock,
        donation_lock,
        miner_token_amount,
        donation_token_amount,
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
    raw.push(3);
    raw.extend_from_slice(&outputs);
    raw.extend_from_slice(&0u32.to_le_bytes());
    Ok(raw)
}

#[cfg_attr(not(test), allow(dead_code))]
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
        miner_token_amount,
        donation_token_amount,
    )?;
    let required_relay_fee_sats = required_relay_fee_sats(serialized_len, relay_fee_sats_per_kb)?;
    let baton_decrease = reward
        .value_sats
        .checked_add(required_relay_fee_sats)
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
        miner_token_amount,
        donation_token_amount,
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
        miner_token_amount,
        donation_token_amount,
        required_relay_fee_sats,
        fee_sats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTOR_BATON_TXID: &str =
        "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042";
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

    #[test]
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
    fn reward_split_rounding_regression() {
        assert_eq!(RuntimeConfig::split_reward(0), (0, 0));
        assert_eq!(RuntimeConfig::split_reward(49), (49, 0));
        assert_eq!(RuntimeConfig::split_reward(50), (49, 1));
        assert_eq!(RuntimeConfig::split_reward(100), (98, 2));
    }

    #[test]
    fn self_funded_settlement_derives_budget_from_authoritative_redeem_script() {
        let (redeem_script, max_baton_decrease_sats) = authoritative_redeem_script().unwrap();
        assert_eq!(
            max_baton_decrease_sats,
            PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS
        );

        let covenant_lock = hex::decode(COVENANT_LOCKING_BYTECODE_HEX).unwrap();
        assert_eq!(&covenant_lock[2..34], hash256(&redeem_script).as_slice());
    }

    #[test]
    fn self_funded_settlement_rejects_drifted_multi_input_value_rule() {
        let mut redeem_script = hex::decode(REDEEM_SCRIPT_HEX.trim()).unwrap();
        let rule = [0xc0, 0xcc, 0x02, 0x40, 0x1f, 0x93, 0xc0, 0xc6, 0xa2, 0x69];
        let start = redeem_script
            .windows(rule.len())
            .position(|window| window == rule)
            .expect("authoritative multi-input value rule");
        redeem_script[start + 5] = 0x94;

        assert!(extract_multi_input_max_baton_decrease_sats(&redeem_script).is_err());
    }

    #[test]
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

        assert_eq!(settlement.raw_settlement.len(), 794);
        assert_eq!(settlement.required_relay_fee_sats, 794);
        assert_eq!(settlement.fee_sats, 794);
        assert_eq!(settlement.baton_input_value_sats, 15_970_000);
        assert_eq!(settlement.baton_output_value_sats, 15_968_506);
        assert_eq!(settlement.miner_token_amount, 4_899_778_337);
        assert_eq!(settlement.donation_token_amount, 99_995_476);
        assert_eq!(
            hex::encode(hash256(&settlement.raw_settlement)),
            "dab595f2cf51f796a722f4fd75c2d31c1607ed42ed4bd1983d7344f050fee3bc"
        );
        assert_eq!(
            settlement.miner_token_amount + settlement.donation_token_amount,
            4_999_773_813
        );
        assert_eq!(
            settlement.baton_input_value_sats - settlement.baton_output_value_sats,
            TOKEN_OUTPUT_SATS + settlement.fee_sats
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
        assert_eq!(raw[cursor], 3);
    }

    #[test]
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

        assert_eq!(settlement.raw_settlement.len(), 794);
        assert_eq!(settlement.required_relay_fee_sats, 1_588);
        assert_eq!(settlement.fee_sats, 1_588);
        assert_eq!(settlement.baton_output_value_sats, 15_967_712);
        assert_eq!(
            settlement.baton_input_value_sats - settlement.baton_output_value_sats,
            TOKEN_OUTPUT_SATS + settlement.fee_sats
        );
    }

    #[test]
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
