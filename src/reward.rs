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
#[cfg_attr(not(test), allow(dead_code))]
pub const PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS: u64 = 8_000;
#[cfg_attr(not(test), allow(dead_code))]
pub const MIN_RELAY_FEE_SATS_PER_KB: u64 = 1_000;
#[cfg(test)]
pub const SPONSOR_SUBSIDY_SATS: u64 = 2_000;
#[cfg(test)]
pub const SPONSOR_STANDARD_MAX_LOCKING_BYTES: usize = 201;
#[cfg(test)]
pub const SPONSOR_OUTPUT_DUST_SATS: u64 = 1_062;
#[cfg(test)]
pub const SPONSOR_MIN_RESERVE_SATS: u64 = SPONSOR_SUBSIDY_SATS + SPONSOR_OUTPUT_DUST_SATS;
const SIGHASH_ALL_FORKID: u8 = 0x41;

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SponsorReserve {
    pub txid: String,
    pub vout: u32,
    pub value_sats: u64,
    pub locking_script: Vec<u8>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRewardSplit {
    pub parent_txid: String,
    pub child_txid: String,
    pub raw_child: Vec<u8>,
    pub miner_token_amount: u128,
    pub donation_token_amount: u128,
    pub fee_sats: u64,
    pub next_sponsor_locking_script: Vec<u8>,
    pub next_sponsor_value_sats: u64,
}

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

#[cfg(test)]
fn script_number_bytes(value: u64) -> Vec<u8> {
    if value == 0 {
        return Vec::new();
    }
    let mut value = value;
    let mut out = Vec::new();
    while value != 0 {
        out.push((value & 0xff) as u8);
        value >>= 8;
    }
    if out.last().is_some_and(|byte| byte & 0x80 != 0) {
        out.push(0);
    }
    out
}

#[cfg(test)]
fn push_number(value: u64) -> Result<Vec<u8>, String> {
    match value {
        0 => Ok(vec![0x00]),
        1..=16 => Ok(vec![0x50 + value as u8]),
        _ => push_data(&script_number_bytes(value)),
    }
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

#[cfg(test)]
fn p2pkh_sighash(
    parent_txid: &str,
    reward_token_amount: u128,
    reward_lock: &[u8],
    sponsor: &SponsorReserve,
    outputs: &[u8],
) -> Result<[u8; 32], String> {
    let mut outpoints = serialized_outpoint(parent_txid, 1)?;
    outpoints.extend_from_slice(&serialized_outpoint(&sponsor.txid, sponsor.vout)?);
    let hash_prevouts = hash256(&outpoints);
    let mut sequences = Vec::with_capacity(8);
    sequences.extend_from_slice(&0u32.to_le_bytes());
    sequences.extend_from_slice(&0u32.to_le_bytes());
    let hash_sequence = hash256(&sequences);
    let hash_outputs = hash256(outputs);

    let mut preimage = Vec::new();
    preimage.extend_from_slice(&2u32.to_le_bytes());
    preimage.extend_from_slice(&hash_prevouts);
    preimage.extend_from_slice(&hash_sequence);
    preimage.extend_from_slice(&serialized_outpoint(parent_txid, 1)?);
    preimage.extend_from_slice(&token_prefix(reward_token_amount)?);
    preimage.extend_from_slice(&compact_uint(reward_lock.len() as u64));
    preimage.extend_from_slice(reward_lock);
    preimage.extend_from_slice(&TOKEN_OUTPUT_SATS.to_le_bytes());
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&hash_outputs);
    preimage.extend_from_slice(&0u32.to_le_bytes());
    preimage.extend_from_slice(&(SIGHASH_ALL_FORKID as u32).to_le_bytes());
    Ok(hash256(&preimage))
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
fn required_relay_fee_sats(serialized_bytes: usize) -> Result<u64, String> {
    let bytes = u64::try_from(serialized_bytes).map_err(|_| "transaction size exceeds u64")?;
    bytes
        .checked_mul(MIN_RELAY_FEE_SATS_PER_KB)
        .and_then(|value| value.checked_add(999))
        .map(|value| value / 1_000)
        .ok_or_else(|| "relay-fee calculation overflow".into())
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
    let redeem_script = hex::decode(REDEEM_SCRIPT_HEX.trim()).map_err(|error| error.to_string())?;
    if redeem_script.len() != 259 {
        return Err(format!(
            "PHOTON redeem script must be 259 bytes (got {})",
            redeem_script.len()
        ));
    }
    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);

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
    let derived_public = PublicKey::from_secret_key(
        &SecretKey::from_secret_bytes(*reward_secret).map_err(|error| error.to_string())?,
    )
    .serialize();
    if &derived_public != reward_public_key {
        return Err("reward public key does not match the runtime reward secret".into());
    }

    let [baton, reward] = parse_parent_outputs(parent_raw)?;
    let covenant_lock =
        hex::decode(COVENANT_LOCKING_BYTECODE_HEX).map_err(|error| error.to_string())?;
    if baton.token_and_locking_bytecode.first() != Some(&0xef)
        || !baton.token_and_locking_bytecode.ends_with(&covenant_lock)
    {
        return Err("parent output 0 is not the authoritative PHOTON baton".into());
    }
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
    let provisional_baton_value = baton
        .value_sats
        .checked_sub(PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS)
        .ok_or("baton BCH value is too small for settlement")?;
    let provisional = build_self_funded_raw(
        &parent_txid,
        provisional_baton_value,
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
    let required_relay_fee_sats = required_relay_fee_sats(provisional.len())?;
    let baton_decrease = reward
        .value_sats
        .checked_add(required_relay_fee_sats)
        .ok_or("baton decrease overflow")?;
    if baton_decrease > PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS {
        return Err(format!(
            "settlement needs {baton_decrease} baton sats but covenant permits at most {PHOTON_MULTI_INPUT_MAX_BATON_DECREASE_SATS}"
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
    if raw_settlement.len() != provisional.len() {
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

/// Legacy M54 sponsor-covenant research helper. Production mining does not use it.
#[cfg(test)]
pub fn build_sponsor_script(expected_baton_txid: &str) -> Result<Vec<u8>, String> {
    let baton = parse_txid_display(expected_baton_txid)?;
    let expected_baton_hash_le = reverse(&baton);
    let donation_lock = tx::cashaddr_to_p2pkh_locking(DONATION_ADDRESS)?;
    let owner_pkh = &donation_lock[3..23];
    let mut script = Vec::new();

    script.extend_from_slice(&push_data(&expected_baton_hash_le)?);
    script.extend_from_slice(&[0x6b, 0x63]); // TOALTSTACK IF
    script.extend_from_slice(&[0x6c, 0x75, 0x76, 0xa9]); // FROMALTSTACK DROP DUP HASH160
    script.extend_from_slice(&push_data(owner_pkh)?);
    script.extend_from_slice(&[0x88, 0xac, 0x67]); // EQUALVERIFY CHECKSIG ELSE

    script.push(0xc0); // INPUTINDEX
    script.extend_from_slice(&push_number(1)?);
    script.extend_from_slice(&[0x9d, 0xc3]); // NUMEQUALVERIFY TXINPUTCOUNT
    script.extend_from_slice(&push_number(2)?);
    script.extend_from_slice(&[0x9d, 0xc4]); // NUMEQUALVERIFY TXOUTPUTCOUNT
    script.extend_from_slice(&push_number(3)?);
    script.push(0x9d);

    script.extend_from_slice(&push_number(0)?);
    script.push(0xc9); // OUTPOINTINDEX
    script.extend_from_slice(&push_number(1)?);
    script.push(0x9d);

    script.extend_from_slice(&[0x76, 0xaa]); // DUP HASH256
    script.extend_from_slice(&push_number(0)?);
    script.extend_from_slice(&[0xc8, 0x88]); // OUTPOINTTXHASH EQUALVERIFY

    script.extend_from_slice(&push_number(5)?);
    script.extend_from_slice(&[0x7f, 0x7c]); // SPLIT SWAP
    script.extend_from_slice(&push_data(&[0x02, 0x00, 0x00, 0x00, 0x01])?);
    script.push(0x88);

    script.extend_from_slice(&push_number(32)?);
    script.extend_from_slice(&[0x7f, 0x7c, 0x6c, 0x88]); // SPLIT SWAP FROMALTSTACK EQUALVERIFY
    script.extend_from_slice(&push_number(4)?);
    script.extend_from_slice(&[0x7f, 0x75]); // SPLIT DROP
    script.extend_from_slice(&push_data(&[0, 0, 0, 0])?);
    script.push(0x88);

    script.extend_from_slice(&push_number(0)?);
    script.extend_from_slice(&[0xd0, 0x76]); // UTXOTOKENAMOUNT DUP
    script.extend_from_slice(&push_number(u64::from(DONATION_BPS))?);
    script.push(0x95); // MUL
    script.extend_from_slice(&push_number(10_000)?);
    script.extend_from_slice(&[0x96, 0x76]); // DIV DUP
    script.extend_from_slice(&push_number(1)?);
    script.extend_from_slice(&[0xd3, 0x9d, 0x94]); // OUTPUTTOKENAMOUNT NUMEQUALVERIFY SUB
    script.extend_from_slice(&push_number(0)?);
    script.extend_from_slice(&[0xd3, 0x9d]);

    script.extend_from_slice(&push_number(1)?);
    script.push(0xcd); // OUTPUTBYTECODE
    script.extend_from_slice(&push_data(&donation_lock)?);
    script.push(0x88);

    for output_index in [0u64, 1] {
        script.extend_from_slice(&push_number(output_index)?);
        script.push(0xcc); // OUTPUTVALUE
        script.extend_from_slice(&push_number(TOKEN_OUTPUT_SATS)?);
        script.push(0x9d);
    }

    script.extend_from_slice(&push_number(0)?);
    script.push(0xc6); // UTXOVALUE
    script.extend_from_slice(&push_number(TOKEN_OUTPUT_SATS)?);
    script.push(0x9d);

    script.extend_from_slice(&push_data(&[0x20])?);
    script.extend_from_slice(&push_number(0)?);
    script.extend_from_slice(&[0xc8, 0x7e, 0xc1]); // OUTPOINTTXHASH CAT ACTIVEBYTECODE
    script.extend_from_slice(&push_number(33)?);
    script.extend_from_slice(&[0x7f, 0x77, 0x7e]); // SPLIT NIP CAT
    script.extend_from_slice(&push_number(2)?);
    script.extend_from_slice(&[0xcd, 0x88]); // OUTPUTBYTECODE EQUALVERIFY

    script.extend_from_slice(&push_number(1)?);
    script.push(0xc6); // UTXOVALUE
    script.extend_from_slice(&push_number(SPONSOR_SUBSIDY_SATS)?);
    script.push(0x94); // SUB
    script.extend_from_slice(&push_number(2)?);
    script.extend_from_slice(&[0xcc, 0x9d]); // OUTPUTVALUE NUMEQUALVERIFY
    script.extend_from_slice(&push_number(1)?);
    script.push(0x68); // ENDIF

    if script.len() != 197 || script.len() > SPONSOR_STANDARD_MAX_LOCKING_BYTES {
        return Err(format!(
            "M54 sponsor script must be 197 standard bytes (got {})",
            script.len()
        ));
    }
    Ok(script)
}

#[cfg(test)]
pub fn sponsor_electrum_scripthash(expected_baton_txid: &str) -> Result<String, String> {
    let script = build_sponsor_script(expected_baton_txid)?;
    let digest = Sha256::digest(&script);
    Ok(hex::encode(
        digest.iter().rev().copied().collect::<Vec<_>>(),
    ))
}

#[cfg(test)]
pub fn next_sponsor_locking_script(
    parent_raw: &[u8],
    current_sponsor_script: &[u8],
) -> Result<Vec<u8>, String> {
    if current_sponsor_script.len() != 197 || current_sponsor_script.first() != Some(&0x20) {
        return Err("current sponsor script is not the canonical 197-byte M54 state".into());
    }
    let parent_hash = hash256(parent_raw);
    let mut next = Vec::with_capacity(current_sponsor_script.len());
    next.push(0x20);
    next.extend_from_slice(&parent_hash);
    next.extend_from_slice(&current_sponsor_script[33..]);
    Ok(next)
}

#[cfg(test)]
pub fn build_reward_split_child(
    parent_raw: &[u8],
    expected_baton_txid: &str,
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    miner_payout: &str,
    reward_token_amount: u128,
    sponsor: &SponsorReserve,
) -> Result<PreparedRewardSplit, String> {
    if parent_raw.is_empty() {
        return Err("winning PHOTON parent transaction is empty".into());
    }
    let expected_sponsor = build_sponsor_script(expected_baton_txid)?;
    if sponsor.locking_script != expected_sponsor {
        return Err("sponsor reserve does not match the winning PHOTON baton state".into());
    }
    let next_sponsor_value_sats = sponsor
        .value_sats
        .checked_sub(SPONSOR_SUBSIDY_SATS)
        .ok_or("sponsor reserve is smaller than the required subsidy")?;
    if next_sponsor_value_sats < SPONSOR_OUTPUT_DUST_SATS {
        return Err("sponsor continuation would fall below standard dust".into());
    }
    let derived_public = PublicKey::from_secret_key(
        &SecretKey::from_secret_bytes(*reward_secret).map_err(|error| error.to_string())?,
    )
    .serialize();
    if &derived_public != reward_public_key {
        return Err("reward public key does not match the runtime reward secret".into());
    }

    let reward_lock = p2pkh_locking_from_public_key(reward_public_key);
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
    let next_sponsor_locking_script =
        next_sponsor_locking_script(parent_raw, &sponsor.locking_script)?;

    let mut outputs = Vec::new();
    outputs.extend_from_slice(&encode_output(
        TOKEN_OUTPUT_SATS,
        Some(miner_token_amount),
        &miner_lock,
    )?);
    outputs.extend_from_slice(&encode_output(
        TOKEN_OUTPUT_SATS,
        Some(donation_token_amount),
        &donation_lock,
    )?);
    outputs.extend_from_slice(&encode_output(
        next_sponsor_value_sats,
        None,
        &next_sponsor_locking_script,
    )?);

    let sighash = p2pkh_sighash(
        &parent_txid,
        reward_token_amount,
        &reward_lock,
        sponsor,
        &outputs,
    )?;
    let signature = crypto::bch_schnorr_sign(reward_secret, &sighash)?;
    if !crypto::bch_schnorr_verify(reward_public_key, &sighash, &signature)? {
        return Err("reward-child Schnorr signature failed local verification".into());
    }
    let mut bitcoin_signature = signature.to_vec();
    bitcoin_signature.push(SIGHASH_ALL_FORKID);
    let mut reward_unlocking = push_data(&bitcoin_signature)?;
    reward_unlocking.extend_from_slice(&push_data(reward_public_key)?);

    let mut sponsor_unlocking = push_data(parent_raw)?;
    sponsor_unlocking.push(0x00); // public branch selector

    let mut raw_child = Vec::new();
    raw_child.extend_from_slice(&2u32.to_le_bytes());
    raw_child.push(2);
    raw_child.extend_from_slice(&encode_input(&parent_txid, 1, &reward_unlocking)?);
    raw_child.extend_from_slice(&encode_input(
        &sponsor.txid,
        sponsor.vout,
        &sponsor_unlocking,
    )?);
    raw_child.push(3);
    raw_child.extend_from_slice(&outputs);
    raw_child.extend_from_slice(&0u32.to_le_bytes());

    let input_value = TOKEN_OUTPUT_SATS
        .checked_add(sponsor.value_sats)
        .ok_or("reward child input value overflow")?;
    let output_value = TOKEN_OUTPUT_SATS
        .checked_mul(2)
        .and_then(|value| value.checked_add(next_sponsor_value_sats))
        .ok_or("reward child output value overflow")?;
    let fee_sats = input_value
        .checked_sub(output_value)
        .ok_or("reward child outputs exceed BCH inputs")?;

    Ok(PreparedRewardSplit {
        parent_txid,
        child_txid: transaction_id(&raw_child),
        raw_child,
        miner_token_amount,
        donation_token_amount,
        fee_sats,
        next_sponsor_locking_script,
        next_sponsor_value_sats,
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
    fn m54_sponsor_script_matches_reference_size_and_state_prefix() {
        let script = build_sponsor_script(VECTOR_BATON_TXID).unwrap();
        assert_eq!(script.len(), 197);
        assert_eq!(script[0], 0x20);
        assert_eq!(
            &script[1..33],
            reverse(&hex::decode(VECTOR_BATON_TXID).unwrap()).as_slice()
        );
    }

    #[test]
    fn reward_child_matches_libauth_bch_2026_vm_oracle() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let sponsor = SponsorReserve {
            txid: "55".repeat(32),
            vout: 0,
            value_sats: 100_000,
            locking_script: build_sponsor_script(VECTOR_BATON_TXID).unwrap(),
        };
        let split = build_reward_split_child(
            &parent,
            VECTOR_BATON_TXID,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &sponsor,
        )
        .unwrap();

        assert_eq!(split.raw_child.len(), 1169);
        assert_eq!(split.fee_sats, 1300);
        assert_eq!(split.miner_token_amount, 4_899_778_337);
        assert_eq!(split.donation_token_amount, 99_995_476);
        assert_eq!(split.next_sponsor_value_sats, 98_000);
        assert_eq!(
            hex::encode(hash256(&split.raw_child)),
            "56da88f1f5695a0edfc6057b06788ca91d37b926e4ee9b75e2d89bb315557166"
        );
    }

    #[test]
    fn reward_child_rejects_wrong_sponsor_state_and_dust_exhaustion() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let miner_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();
        let mut sponsor = SponsorReserve {
            txid: "55".repeat(32),
            vout: 0,
            value_sats: 100_000,
            locking_script: build_sponsor_script(&"11".repeat(32)).unwrap(),
        };
        assert!(build_reward_split_child(
            &parent,
            VECTOR_BATON_TXID,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &sponsor,
        )
        .is_err());

        sponsor.locking_script = build_sponsor_script(VECTOR_BATON_TXID).unwrap();
        sponsor.value_sats = SPONSOR_SUBSIDY_SATS + SPONSOR_OUTPUT_DUST_SATS - 1;
        assert!(build_reward_split_child(
            &parent,
            VECTOR_BATON_TXID,
            &reward_secret,
            &reward_public,
            &miner_payout,
            4_999_773_813,
            &sponsor,
        )
        .is_err());
    }

    #[test]
    fn payout_change_only_changes_miner_destination_not_donation_policy() {
        let mut reward_secret = [0u8; 32];
        reward_secret[31] = 1;
        let reward_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(reward_secret).unwrap())
                .serialize();
        let parent = vector_parent(p2pkh_locking_from_public_key(&reward_public));
        let first_payout = p2pkh_cashaddr_from_public_key(&reward_public).unwrap();

        let mut second_secret = [0u8; 32];
        second_secret[31] = 2;
        let second_public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(second_secret).unwrap())
                .serialize();
        let second_payout = p2pkh_cashaddr_from_public_key(&second_public).unwrap();
        assert_ne!(first_payout, second_payout);

        let sponsor = SponsorReserve {
            txid: "55".repeat(32),
            vout: 0,
            value_sats: 100_000,
            locking_script: build_sponsor_script(VECTOR_BATON_TXID).unwrap(),
        };
        let first = build_reward_split_child(
            &parent,
            VECTOR_BATON_TXID,
            &reward_secret,
            &reward_public,
            &first_payout,
            4_999_773_813,
            &sponsor,
        )
        .unwrap();
        let second = build_reward_split_child(
            &parent,
            VECTOR_BATON_TXID,
            &reward_secret,
            &reward_public,
            &second_payout,
            4_999_773_813,
            &sponsor,
        )
        .unwrap();

        assert_eq!(DONATION_BPS, 200);
        assert_eq!(first.donation_token_amount, second.donation_token_amount);
        assert_eq!(first.miner_token_amount, second.miner_token_amount);

        let donation_lock = tx::cashaddr_to_p2pkh_locking(DONATION_ADDRESS).unwrap();
        let first_miner_lock = tx::cashaddr_to_p2pkh_locking(&first_payout).unwrap();
        let second_miner_lock = tx::cashaddr_to_p2pkh_locking(&second_payout).unwrap();
        assert!(first
            .raw_child
            .windows(donation_lock.len())
            .any(|window| window == donation_lock));
        assert!(second
            .raw_child
            .windows(donation_lock.len())
            .any(|window| window == donation_lock));
        assert!(first
            .raw_child
            .windows(first_miner_lock.len())
            .any(|window| window == first_miner_lock));
        assert!(second
            .raw_child
            .windows(second_miner_lock.len())
            .any(|window| window == second_miner_lock));
    }
}
