//! Local BCH funding key and verified, confirmed token-free UTXO discovery.

use crate::config::MiningNetwork;
use crate::electrum::ElectrumSession;
use crate::protocol::{PhotonDeployment, CHIPNET_CATEGORY_HEX};
#[cfg(test)]
use crate::reward::ConfirmedFundingUtxo;
use crate::reward::ConfirmedRewardUtxo;
use ripemd::Ripemd160;
use secp256k1::{PublicKey, SecretKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Limits raw transaction retention to 32 MB even if a server advertises many rewards.
/// Once a batch is spent, discovery advances to the next unspent prefix.
const MAX_REWARD_UTXOS_PER_SWEEP: usize = 32;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// The secret is deliberately excluded from `Debug` and is never serialized.
pub struct FundingWallet {
    _key_lock: File,
    network: MiningNetwork,
    #[cfg(test)]
    secret_key: [u8; 32],
    #[cfg(test)]
    public_key: [u8; 33],
    address: String,
    #[cfg(test)]
    locking_bytecode: Vec<u8>,
    #[cfg(test)]
    electrum_scripthash: String,
    reward_secret_key: [u8; 32],
    reward_public_key: [u8; 33],
    #[cfg(test)]
    reward_token_address: String,
    reward_locking_bytecode: Vec<u8>,
    reward_electrum_scripthash: String,
}

impl FundingWallet {
    /// Recovery must never replace a missing legacy key with a new identity.
    pub fn load_existing(path: &Path, network: MiningNetwork) -> Result<Self, String> {
        Self::load(path, network, false)
    }

    /// Loads a 32-byte raw key or atomically creates one with private permissions.
    /// The parent directory must already exist; the key is never printed.
    #[cfg(test)]
    pub fn load_or_create(path: &Path, network: MiningNetwork) -> Result<Self, String> {
        Self::load(path, network, true)
    }

    fn load(path: &Path, network: MiningNetwork, create: bool) -> Result<Self, String> {
        if path.file_name().is_none() || path.parent().is_none_or(|p| !p.is_dir()) {
            return Err("funding key parent directory does not exist".into());
        }
        reject_symlink_components(path)?;
        let (secret_bytes, key_lock) = match fs::symlink_metadata(path) {
            Ok(_) => read_private_key(path)?,
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                let secret = SecretKey::new(&mut rand::rng()).to_secret_bytes();
                match create_private_key(path, &secret) {
                    Ok(key_lock) => (secret, key_lock),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        read_private_key(path)?
                    }
                    Err(error) => return Err(format!("create funding key: {error}")),
                }
            }
            Err(error) => return Err(format!("inspect funding key: {error}")),
        };
        let secret = SecretKey::from_secret_bytes(secret_bytes)
            .map_err(|error| format!("invalid funding key: {error}"))?;
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let sha = Sha256::digest(public_key);
        let hash = Ripemd160::digest(sha);
        let mut hash160 = [0u8; 20];
        hash160.copy_from_slice(&hash);
        let address = crate::tx::p2pkh_hash_to_cashaddr_for_network(&hash160, network)?;
        let mut locking_bytecode = Vec::with_capacity(25);
        locking_bytecode.extend_from_slice(&[0x76, 0xa9, 0x14]);
        locking_bytecode.extend_from_slice(&hash160);
        locking_bytecode.extend_from_slice(&[0x88, 0xac]);
        let mut script_hash: [u8; 32] = Sha256::digest(&locking_bytecode).into();
        script_hash.reverse();
        let reward_secret_key = derive_reward_key(&secret_bytes)?;
        let reward_public_key = PublicKey::from_secret_key(
            &SecretKey::from_secret_bytes(reward_secret_key)
                .map_err(|error| format!("invalid derived reward key: {error}"))?,
        )
        .serialize();
        let reward_sha = Sha256::digest(reward_public_key);
        let reward_hash = Ripemd160::digest(reward_sha);
        let mut reward_hash160 = [0u8; 20];
        reward_hash160.copy_from_slice(&reward_hash);
        #[cfg(test)]
        let reward_token_address =
            crate::tx::token_p2pkh_hash_to_cashaddr_for_network(&reward_hash160, network)?;
        let mut reward_locking_bytecode = Vec::with_capacity(25);
        reward_locking_bytecode.extend_from_slice(&[0x76, 0xa9, 0x14]);
        reward_locking_bytecode.extend_from_slice(&reward_hash160);
        reward_locking_bytecode.extend_from_slice(&[0x88, 0xac]);
        let mut reward_scripthash: [u8; 32] = Sha256::digest(&reward_locking_bytecode).into();
        reward_scripthash.reverse();
        Ok(Self {
            _key_lock: key_lock,
            network,
            #[cfg(test)]
            secret_key: secret_bytes,
            #[cfg(test)]
            public_key,
            address,
            #[cfg(test)]
            locking_bytecode,
            #[cfg(test)]
            electrum_scripthash: hex::encode(script_hash),
            reward_secret_key,
            reward_public_key,
            #[cfg(test)]
            reward_token_address,
            reward_locking_bytecode,
            reward_electrum_scripthash: hex::encode(reward_scripthash),
        })
    }

    pub fn address(&self) -> &str {
        &self.address
    }
    #[cfg(test)]
    pub fn public_key(&self) -> &[u8; 33] {
        &self.public_key
    }
    #[cfg(test)]
    pub fn secret_key(&self) -> &[u8; 32] {
        &self.secret_key
    }
    #[cfg(test)]
    pub fn locking_bytecode(&self) -> &[u8] {
        &self.locking_bytecode
    }
    #[cfg(test)]
    pub fn electrum_scripthash(&self) -> &str {
        &self.electrum_scripthash
    }

    pub fn reward_secret_key(&self) -> &[u8; 32] {
        &self.reward_secret_key
    }
    pub fn reward_public_key(&self) -> &[u8; 33] {
        &self.reward_public_key
    }
    #[cfg(test)]
    pub fn reward_token_address(&self) -> &str {
        &self.reward_token_address
    }

    pub fn confirmed_reward_utxos(
        &self,
        session: &mut ElectrumSession,
        deployment: &PhotonDeployment,
    ) -> Result<Vec<ConfirmedRewardUtxo>, String> {
        if self.network != MiningNetwork::Chipnet || deployment.category_hex != CHIPNET_CATEGORY_HEX
        {
            return Err("reward discovery requires the selected chipnet deployment".into());
        }
        deployment.verify()?;
        let before = session.rpc("blockchain.headers.subscribe", json!([]))?;
        let unspent = session.rpc(
            "blockchain.scripthash.listunspent",
            json!([self.reward_electrum_scripthash, "include_tokens"]),
        )?;
        let after = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if before != after {
            return Err("reward UTXO snapshot changed chain tip during read".into());
        }
        let tip_height = after
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or("reward chain tip has invalid height")?;
        let candidates: Vec<_> =
            parse_confirmed_reward_candidates(&unspent, tip_height, deployment)?
                .into_iter()
                .take(MAX_REWARD_UTXOS_PER_SWEEP)
                .collect();
        let mut verified = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            let raw_response =
                session.rpc("blockchain.transaction.get", json!([candidate.txid, false]))?;
            let raw_hex = raw_response
                .as_str()
                .ok_or("reward transaction.get did not return raw hex")?;
            let raw = hex::decode(raw_hex)
                .map_err(|error| format!("reward transaction.get returned invalid hex: {error}"))?;
            verified.push(verify_reward_transaction(
                candidate,
                &raw,
                &self.reward_locking_bytecode,
                deployment,
            )?);
        }
        let final_tip = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if after != final_tip {
            return Err(
                "reward UTXO snapshot changed chain tip while fetching transactions".into(),
            );
        }
        let current_unspent = session.rpc(
            "blockchain.scripthash.listunspent",
            json!([self.reward_electrum_scripthash, "include_tokens"]),
        )?;
        let current = parse_confirmed_reward_candidates(&current_unspent, tip_height, deployment)?;
        let end_tip = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if final_tip != end_tip {
            return Err(
                "reward UTXO snapshot changed chain tip while checking unspent status".into(),
            );
        }
        if candidates.iter().any(|candidate| {
            !current.iter().any(|item| {
                item.txid == candidate.txid
                    && item.vout == candidate.vout
                    && item.value_sats == candidate.value_sats
                    && item.token_amount == candidate.token_amount
            })
        }) {
            return Err("reward UTXO was spent or changed during discovery".into());
        }
        Ok(verified)
    }

    /// Verifies journaled reward inputs by exact outpoint, independent of the
    /// discovery prefix used for building new sweeps.
    pub fn verify_confirmed_reward_outpoints(
        &self,
        session: &mut ElectrumSession,
        deployment: &PhotonDeployment,
        outpoints: &[(String, u32)],
    ) -> Result<(), String> {
        if self.network != MiningNetwork::Chipnet || deployment.category_hex != CHIPNET_CATEGORY_HEX
        {
            return Err("reward recovery requires the selected chipnet deployment".into());
        }
        deployment.verify()?;
        if outpoints.is_empty() || outpoints.len() > MAX_REWARD_UTXOS_PER_SWEEP {
            return Err("pending reward input count must be between 1 and 32".into());
        }
        for (index, (txid, vout)) in outpoints.iter().enumerate() {
            if txid.len() != 64 || !txid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("pending reward transaction ID must be 64 hex characters".into());
            }
            if outpoints[..index]
                .iter()
                .any(|(previous_txid, previous_vout)| {
                    previous_txid.eq_ignore_ascii_case(txid) && previous_vout == vout
                })
            {
                return Err("duplicate pending reward input outpoint".into());
            }
        }
        let before = session.rpc("blockchain.headers.subscribe", json!([]))?;
        let unspent = session.rpc(
            "blockchain.scripthash.listunspent",
            json!([self.reward_electrum_scripthash, "include_tokens"]),
        )?;
        let after = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if before != after {
            return Err("pending reward input snapshot changed chain tip during read".into());
        }
        let tip_height = after
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or("pending reward chain tip has invalid height")?;
        let candidates = parse_confirmed_reward_candidates(&unspent, tip_height, deployment)?;
        let mut selected = Vec::with_capacity(outpoints.len());
        for (txid, vout) in outpoints {
            let candidate = candidates
                .iter()
                .find(|candidate| {
                    candidate.txid.eq_ignore_ascii_case(txid) && candidate.vout == *vout
                })
                .ok_or_else(|| {
                    format!("pending reward input is no longer unspent: {txid}:{vout}")
                })?;
            selected.push(candidate);
        }
        for candidate in &selected {
            let raw_response =
                session.rpc("blockchain.transaction.get", json!([candidate.txid, false]))?;
            let raw_hex = raw_response
                .as_str()
                .ok_or("pending reward transaction.get did not return raw hex")?;
            let raw = hex::decode(raw_hex).map_err(|error| {
                format!("pending reward transaction.get returned invalid hex: {error}")
            })?;
            verify_reward_transaction(candidate, &raw, &self.reward_locking_bytecode, deployment)?;
        }
        let final_tip = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if after != final_tip {
            return Err(
                "pending reward input snapshot changed chain tip while fetching transactions"
                    .into(),
            );
        }
        let current_unspent = session.rpc(
            "blockchain.scripthash.listunspent",
            json!([self.reward_electrum_scripthash, "include_tokens"]),
        )?;
        let current = parse_confirmed_reward_candidates(&current_unspent, tip_height, deployment)?;
        let end_tip = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if final_tip != end_tip {
            return Err(
                "pending reward input snapshot changed chain tip while checking unspent status"
                    .into(),
            );
        }
        for candidate in selected {
            if !current.iter().any(|item| {
                item.txid == candidate.txid
                    && item.vout == candidate.vout
                    && item.value_sats == candidate.value_sats
                    && item.token_amount == candidate.token_amount
            }) {
                return Err(format!(
                    "pending reward input is no longer unspent: {}:{}",
                    candidate.txid, candidate.vout
                ));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn select_optional_confirmed_utxo(
        &self,
        session: &mut ElectrumSession,
        min_sats: u64,
    ) -> Result<Option<ConfirmedFundingUtxo>, String> {
        let before = session.rpc("blockchain.headers.subscribe", json!([]))?;
        let unspent = session.rpc(
            "blockchain.scripthash.listunspent",
            json!([self.electrum_scripthash, "include_tokens"]),
        )?;
        let after = session.rpc("blockchain.headers.subscribe", json!([]))?;
        if before != after {
            return Err("funding UTXO snapshot changed chain tip during read".into());
        }
        let tip_height = after
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or("funding chain tip has invalid height")?;
        let candidate = parse_confirmed_candidates(&unspent, tip_height, min_sats)?
            .into_iter()
            .next();
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        let raw_response =
            session.rpc("blockchain.transaction.get", json!([candidate.txid, false]))?;
        let raw_hex = raw_response
            .as_str()
            .ok_or("funding transaction.get did not return raw hex")?;
        let raw = hex::decode(raw_hex)
            .map_err(|error| format!("funding transaction.get returned invalid hex: {error}"))?;
        verify_funding_transaction(&candidate, &raw, &self.locking_bytecode).map(Some)
    }
}

fn derive_reward_key(root: &[u8; 32]) -> Result<[u8; 32], String> {
    let mut digest = Sha256::new();
    digest.update(b"pickaxe-miner/photon-chipnet-reward/v1");
    digest.update(root);
    let candidate: [u8; 32] = digest.finalize().into();
    if candidate == *root {
        return Err("derived reward key unexpectedly equals funding root key".into());
    }
    SecretKey::from_secret_bytes(candidate)
        .map_err(|error| format!("derived reward key is invalid: {error}"))?;
    Ok(candidate)
}

#[cfg(test)]
struct FundingCandidate {
    txid: String,
    vout: u32,
    value_sats: u64,
    confirmations: u32,
}

#[cfg(test)]
fn parse_confirmed_candidates(
    unspent: &Value,
    tip_height: u32,
    min_sats: u64,
) -> Result<Vec<FundingCandidate>, String> {
    let entries = unspent
        .as_array()
        .ok_or("funding listunspent response is not an array")?;
    let mut candidates = Vec::new();
    for entry in entries {
        let height = entry
            .get("height")
            .and_then(Value::as_u64)
            .ok_or("funding UTXO has invalid height")?;
        if height == 0 {
            continue;
        }
        if height > u64::from(tip_height) {
            return Err("funding UTXO height exceeds chain tip".into());
        }
        let value_sats = entry
            .get("value")
            .and_then(Value::as_u64)
            .ok_or("funding UTXO has invalid BCH value")?;
        if value_sats < min_sats {
            continue;
        }
        if ["token_data", "token", "tokenData"]
            .iter()
            .any(|key| entry.get(*key).is_some_and(|value| !value.is_null()))
        {
            continue;
        }
        let txid = entry
            .get("tx_hash")
            .and_then(Value::as_str)
            .ok_or("funding UTXO has no transaction ID")?;
        if txid.len() != 64 || !txid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("funding UTXO transaction ID must be 64 hex characters".into());
        }
        let vout = entry
            .get("tx_pos")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or("funding UTXO has invalid output index")?;
        let confirmations = u32::try_from(u64::from(tip_height) - height + 1)
            .map_err(|_| "funding UTXO confirmation count overflow")?;
        candidates.push(FundingCandidate {
            txid: txid.to_ascii_lowercase(),
            vout,
            value_sats,
            confirmations,
        });
    }
    candidates
        .sort_by(|a, b| (a.value_sats, &a.txid, a.vout).cmp(&(b.value_sats, &b.txid, b.vout)));
    Ok(candidates)
}

#[derive(Clone)]
struct RewardCandidate {
    txid: String,
    vout: u32,
    value_sats: u64,
    token_amount: u128,
    confirmations: u32,
}

fn parse_confirmed_reward_candidates(
    unspent: &Value,
    tip_height: u32,
    deployment: &PhotonDeployment,
) -> Result<Vec<RewardCandidate>, String> {
    let entries = unspent
        .as_array()
        .ok_or("reward listunspent response is not an array")?;
    if entries.len() > 4096 {
        return Err("reward listunspent response exceeds 4096 outputs".into());
    }
    let mut candidates = Vec::new();
    for entry in entries {
        // Token-aware and plain P2PKH addresses use the same locking bytecode.
        // An ordinary BCH deposit to this key is not a PHOTON reward.
        let token = match entry.get("token_data") {
            Some(Value::Object(token)) => token,
            None | Some(Value::Null) => continue,
            _ => return Err("reward UTXO has malformed token data".into()),
        };
        let category = token
            .get("category")
            .and_then(Value::as_str)
            .ok_or("reward UTXO token category is missing")?;
        if category.len() != 64 || !category.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("reward UTXO token category is invalid".into());
        }
        if !category.eq_ignore_ascii_case(deployment.category_hex) {
            continue;
        }
        if token.get("nft").is_some_and(|nft| !nft.is_null()) {
            return Err("PHOTON reward UTXO unexpectedly carries an NFT".into());
        }
        let amount = token
            .get("amount")
            .ok_or("PHOTON reward UTXO amount is missing")?;
        let amount = match amount {
            Value::String(text)
                if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                text.parse::<u64>().ok()
            }
            Value::Number(number) => number.as_u64(),
            _ => None,
        }
        .filter(|amount| *amount > 0)
        .ok_or("PHOTON reward UTXO amount is invalid")?;
        let height = entry
            .get("height")
            .and_then(Value::as_u64)
            .ok_or("reward UTXO has invalid height")?;
        if height == 0 {
            continue;
        }
        if height > u64::from(tip_height) {
            return Err("reward UTXO height exceeds chain tip".into());
        }
        let value_sats = entry
            .get("value")
            .and_then(Value::as_u64)
            .ok_or("reward UTXO has invalid BCH value")?;
        if value_sats != 700 {
            return Err("PHOTON reward UTXO must carry exactly 700 sats".into());
        }
        let txid = entry
            .get("tx_hash")
            .and_then(Value::as_str)
            .ok_or("reward UTXO has no transaction ID")?;
        if txid.len() != 64 || !txid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("reward UTXO transaction ID must be 64 hex characters".into());
        }
        let vout = entry
            .get("tx_pos")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or("reward UTXO has invalid output index")?;
        let confirmations = u32::try_from(u64::from(tip_height) - height + 1)
            .map_err(|_| "reward UTXO confirmation count overflow")?;
        candidates.push(RewardCandidate {
            txid: txid.to_ascii_lowercase(),
            vout,
            value_sats,
            token_amount: u128::from(amount),
            confirmations,
        });
    }
    candidates.sort_by(|a, b| (&a.txid, a.vout).cmp(&(&b.txid, b.vout)));
    if candidates
        .windows(2)
        .any(|pair| pair[0].txid == pair[1].txid && pair[0].vout == pair[1].vout)
    {
        return Err("duplicate PHOTON reward UTXO".into());
    }
    Ok(candidates)
}

fn verify_reward_transaction(
    candidate: &RewardCandidate,
    raw: &[u8],
    expected_locking: &[u8],
    deployment: &PhotonDeployment,
) -> Result<ConfirmedRewardUtxo, String> {
    if raw.len() > 1_000_000 {
        return Err("reward transaction exceeds 1 MB verification limit".into());
    }
    if crate::reward::transaction_id(raw) != candidate.txid {
        return Err("reward transaction ID does not match raw transaction".into());
    }
    let mut cursor = TxCursor { raw, pos: 0 };
    cursor.take(4)?;
    let inputs = cursor.compact_size()?;
    if inputs == 0 {
        return Err("reward transaction has no inputs".into());
    }
    for _ in 0..inputs {
        cursor.take(36)?;
        let script_len = usize::try_from(cursor.compact_size()?)
            .map_err(|_| "reward input script length overflow")?;
        cursor.take(script_len)?;
        cursor.take(4)?;
    }
    let outputs = cursor.compact_size()?;
    if outputs == 0 || u64::from(candidate.vout) >= outputs {
        return Err("reward output index does not exist".into());
    }
    let mut selected = None;
    for index in 0..outputs {
        let value = u64::from_le_bytes(cursor.take(8)?.try_into().unwrap());
        let script_len = usize::try_from(cursor.compact_size()?)
            .map_err(|_| "reward output bytecode length overflow")?;
        let script = cursor.take(script_len)?;
        if index == u64::from(candidate.vout) {
            selected = Some((value, script));
        }
    }
    cursor.take(4)?;
    if cursor.pos != raw.len() {
        return Err("reward transaction has trailing bytes".into());
    }
    let (value, script) = selected.ok_or("reward output missing")?;
    if value != candidate.value_sats || value != 700 {
        return Err("PHOTON reward BCH value differs from verified UTXO".into());
    }
    let mut category = hex::decode(deployment.category_hex)
        .map_err(|error| format!("invalid PHOTON reward category: {error}"))?;
    category.reverse();
    let mut token_cursor = TxCursor {
        raw: script,
        pos: 0,
    };
    if token_cursor.take(1)? != [0xef] || token_cursor.take(32)? != category {
        return Err("PHOTON reward category differs from verified UTXO".into());
    }
    if token_cursor.take(1)? != [0x10] {
        return Err("PHOTON reward must have fungible tokens only".into());
    }
    let amount = token_cursor.compact_size()?;
    if amount == 0 || u128::from(amount) != candidate.token_amount {
        return Err("PHOTON reward token amount differs from verified UTXO".into());
    }
    if script.get(token_cursor.pos..) != Some(expected_locking) {
        return Err("PHOTON reward does not lock to local reward key".into());
    }
    Ok(ConfirmedRewardUtxo {
        txid: candidate.txid.clone(),
        raw_transaction: raw.to_vec(),
        vout: candidate.vout,
        value_sats: value,
        token_amount: candidate.token_amount,
        confirmations: candidate.confirmations,
    })
}

#[cfg(test)]
fn verify_funding_transaction(
    candidate: &FundingCandidate,
    raw: &[u8],
    expected_locking: &[u8],
) -> Result<ConfirmedFundingUtxo, String> {
    if candidate.confirmations == 0 {
        return Err("funding UTXO must be confirmed".into());
    }
    if crate::reward::transaction_id(raw) != candidate.txid {
        return Err("funding transaction ID does not match raw transaction".into());
    }
    let mut cursor = TxCursor { raw, pos: 0 };
    cursor.take(4)?; // version
    let inputs = cursor.compact_size()?;
    if inputs == 0 {
        return Err("funding transaction has no inputs".into());
    }
    for _ in 0..inputs {
        cursor.take(36)?; // previous outpoint
        let script_len = usize::try_from(cursor.compact_size()?)
            .map_err(|_| "funding input bytecode length overflow")?;
        cursor.take(script_len)?;
        cursor.take(4)?; // sequence
    }
    let outputs = cursor.compact_size()?;
    if outputs == 0 || u64::from(candidate.vout) >= outputs {
        return Err("funding output index does not exist".into());
    }
    let mut selected = None;
    for index in 0..outputs {
        let value = u64::from_le_bytes(cursor.take(8)?.try_into().unwrap());
        let script_len = usize::try_from(cursor.compact_size()?)
            .map_err(|_| "funding output bytecode length overflow")?;
        let script = cursor.take(script_len)?;
        if index == u64::from(candidate.vout) {
            selected = Some((value, script));
        }
    }
    cursor.take(4)?; // locktime
    if cursor.pos != raw.len() {
        return Err("funding transaction has trailing bytes".into());
    }
    let (value, script) = selected.ok_or("funding output missing")?;
    if value != candidate.value_sats {
        return Err("funding UTXO BCH value disagrees with raw transaction".into());
    }
    if script != expected_locking {
        return Err("funding output is not token-free P2PKH for this key".into());
    }
    Ok(ConfirmedFundingUtxo {
        txid: candidate.txid.clone(),
        raw_transaction: raw.to_vec(),
        vout: candidate.vout,
        value_sats: value,
        confirmations: candidate.confirmations,
    })
}

struct TxCursor<'a> {
    raw: &'a [u8],
    pos: usize,
}

impl<'a> TxCursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or("funding transaction offset overflow")?;
        let bytes = self
            .raw
            .get(self.pos..end)
            .ok_or("funding transaction is truncated")?;
        self.pos = end;
        Ok(bytes)
    }

    fn compact_size(&mut self) -> Result<u64, String> {
        let prefix = self.take(1)?[0];
        match prefix {
            0..=0xfc => Ok(u64::from(prefix)),
            0xfd => {
                let value = u16::from_le_bytes(self.take(2)?.try_into().unwrap());
                if value < 0xfd {
                    return Err("noncanonical funding compact size".into());
                }
                Ok(u64::from(value))
            }
            0xfe => {
                let value = u32::from_le_bytes(self.take(4)?.try_into().unwrap());
                if value <= u16::MAX.into() {
                    return Err("noncanonical funding compact size".into());
                }
                Ok(u64::from(value))
            }
            0xff => {
                let value = u64::from_le_bytes(self.take(8)?.try_into().unwrap());
                if value <= u32::MAX.into() {
                    return Err("noncanonical funding compact size".into());
                }
                Ok(value)
            }
        }
    }
}

fn reject_symlink_components(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("funding key path must not contain symlinks".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect funding key path: {error}")),
        }
    }
    Ok(())
}

fn create_private_key(path: &Path, secret: &[u8; 32]) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.try_lock()?;
    file.write_all(secret)?;
    file.sync_all()?;
    #[cfg(unix)]
    File::open(path.parent().expect("validated funding key parent"))?.sync_all()?;
    Ok(file)
}

fn read_private_key(path: &Path) -> Result<([u8; 32], File), String> {
    let before =
        fs::symlink_metadata(path).map_err(|error| format!("inspect funding key: {error}"))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err("funding key must be a regular file, not a symlink".into());
    }
    #[cfg(unix)]
    if before.permissions().mode() & 0o077 != 0 {
        return Err("funding key has group or world permissions".into());
    }
    let mut file = File::open(path).map_err(|error| format!("open funding key: {error}"))?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => "funding wallet already in use".to_string(),
        std::fs::TryLockError::Error(error) => format!("lock funding key: {error}"),
    })?;
    let opened = file
        .metadata()
        .map_err(|error| format!("inspect opened funding key: {error}"))?;
    if !opened.is_file() {
        return Err("funding key must be a regular file".into());
    }
    #[cfg(unix)]
    if before.dev() != opened.dev()
        || before.ino() != opened.ino()
        || opened.permissions().mode() & 0o077 != 0
    {
        return Err("funding key changed or has broad permissions".into());
    }
    let mut secret = [0u8; 32];
    file.read_exact(&mut secret)
        .map_err(|error| format!("read funding key: {error}"))?;
    let mut extra = [0u8; 1];
    if file
        .read(&mut extra)
        .map_err(|error| format!("read funding key: {error}"))?
        != 0
    {
        return Err("funding key must contain exactly 32 bytes".into());
    }
    Ok((secret, file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CHIPNET_PHOTON;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::net::TcpListener;
    use std::thread;
    use tungstenite::{accept, Message};

    fn synthetic_transaction(script: &[u8], value_sats: u64) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&2u32.to_le_bytes());
        raw.push(1); // one input
        raw.extend_from_slice(&[0u8; 32]);
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.push(0); // empty unlocking bytecode
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.push(1); // one output
        raw.extend_from_slice(&value_sats.to_le_bytes());
        raw.push(script.len() as u8);
        raw.extend_from_slice(script);
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw
    }

    fn reward_script(wallet: &FundingWallet, category_hex: &str, amount: u8) -> Vec<u8> {
        let mut script = vec![0xef];
        let mut category = hex::decode(category_hex).unwrap();
        category.reverse();
        script.extend_from_slice(&category);
        script.extend_from_slice(&[0x10, amount]);
        script.extend_from_slice(
            &crate::tx::cashaddr_to_p2pkh_locking(wallet.reward_token_address()).unwrap(),
        );
        script
    }

    fn unique_path() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pickaxe-funding-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn creates_stable_chipnet_identity_with_correct_electrum_hash() {
        let dir = unique_path();
        let path = dir.join("key");
        let first = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        assert!(first.address().starts_with("bchtest:q"));
        assert_eq!(first.locking_bytecode().len(), 25);
        let mut hash: [u8; 32] = Sha256::digest(first.locking_bytecode()).into();
        hash.reverse();
        assert_eq!(first.electrum_scripthash(), hex::encode(hash));
        let secret = *first.secret_key();
        let public = *first.public_key();
        let address = first.address().to_string();
        drop(first);
        let second = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        assert_eq!(second.secret_key(), &secret);
        assert_eq!(second.public_key(), &public);
        assert_eq!(second.address(), address);
        drop(second);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reward_key_is_stable_distinct_and_has_token_aware_address() {
        let dir = unique_path();
        let path = dir.join("key");
        let first = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        let reward_secret = *first.reward_secret_key();
        let reward_public = *first.reward_public_key();
        let reward_address = first.reward_token_address().to_string();
        assert_ne!(reward_secret, *first.secret_key());
        assert_ne!(reward_public, *first.public_key());
        assert!(reward_address.starts_with("bchtest:z"));
        let reward_lock = crate::tx::cashaddr_to_p2pkh_locking(&reward_address).unwrap();
        assert_ne!(reward_lock, first.locking_bytecode());
        let mut reward_scripthash: [u8; 32] = Sha256::digest(&reward_lock).into();
        reward_scripthash.reverse();
        assert_eq!(
            first.reward_electrum_scripthash,
            hex::encode(reward_scripthash)
        );
        drop(first);
        let second = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        assert_eq!(second.reward_secret_key(), &reward_secret);
        assert_eq!(second.reward_public_key(), &reward_public);
        assert_eq!(second.reward_token_address(), reward_address);
        drop(second);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reward_utxo_requires_confirmed_expected_category_and_raw_output() {
        let dir = unique_path();
        let wallet =
            FundingWallet::load_or_create(&dir.join("key"), MiningNetwork::Chipnet).unwrap();
        let script = reward_script(&wallet, CHIPNET_PHOTON.category_hex, 77);
        let raw = synthetic_transaction(&script, 700);
        let txid = crate::reward::transaction_id(&raw);
        let unspent = json!([{
            "tx_hash": txid, "tx_pos": 0, "value": 700, "height": 98,
            "token_data": {"category": CHIPNET_PHOTON.category_hex, "amount": "77", "nft": null}
        }]);
        let candidates = parse_confirmed_reward_candidates(&unspent, 100, &CHIPNET_PHOTON).unwrap();
        assert_eq!(candidates.len(), 1);
        let reward = verify_reward_transaction(
            &candidates[0],
            &raw,
            &wallet.reward_locking_bytecode,
            &CHIPNET_PHOTON,
        )
        .unwrap();
        assert_eq!(reward.value_sats, 700);
        assert_eq!(reward.token_amount, 77);
        assert_eq!(reward.confirmations, 3);
        let unconfirmed = json!([{
            "tx_hash": txid, "tx_pos": 0, "value": 700, "height": 0,
            "token_data": {"category": CHIPNET_PHOTON.category_hex, "amount": "77", "nft": null}
        }]);
        assert!(
            parse_confirmed_reward_candidates(&unconfirmed, 100, &CHIPNET_PHOTON)
                .unwrap()
                .is_empty()
        );
        let wrong_category = json!([{
            "tx_hash": txid, "tx_pos": 0, "value": 700, "height": 98,
            "token_data": {"category": "aa".repeat(32), "amount": "77", "nft": null}
        }]);
        assert!(
            parse_confirmed_reward_candidates(&wrong_category, 100, &CHIPNET_PHOTON)
                .unwrap()
                .is_empty()
        );
        let mut mismatched = candidates[0].clone();
        mismatched.txid = "00".repeat(32);
        assert!(verify_reward_transaction(
            &mismatched,
            &raw,
            &wallet.reward_locking_bytecode,
            &CHIPNET_PHOTON
        )
        .is_err());
        let wrong_lock = synthetic_transaction(
            &reward_script(&wallet, CHIPNET_PHOTON.category_hex, 78),
            700,
        );
        let mut wrong_amount = candidates[0].clone();
        wrong_amount.txid = crate::reward::transaction_id(&wrong_lock);
        assert!(verify_reward_transaction(
            &wrong_amount,
            &wrong_lock,
            &wallet.reward_locking_bytecode,
            &CHIPNET_PHOTON
        )
        .is_err());
        let mut noncanonical = reward_script(&wallet, CHIPNET_PHOTON.category_hex, 77);
        noncanonical.splice(34..35, [0xfd, 77, 0]);
        let noncanonical_raw = synthetic_transaction(&noncanonical, 700);
        let mut noncanonical_candidate = candidates[0].clone();
        noncanonical_candidate.txid = crate::reward::transaction_id(&noncanonical_raw);
        assert!(verify_reward_transaction(
            &noncanonical_candidate,
            &noncanonical_raw,
            &wallet.reward_locking_bytecode,
            &CHIPNET_PHOTON
        )
        .is_err());
        let mut wrong_p2pkh = reward_script(&wallet, CHIPNET_PHOTON.category_hex, 77);
        *wrong_p2pkh.last_mut().unwrap() = 0x00;
        let wrong_p2pkh_raw = synthetic_transaction(&wrong_p2pkh, 700);
        let mut wrong_p2pkh_candidate = candidates[0].clone();
        wrong_p2pkh_candidate.txid = crate::reward::transaction_id(&wrong_p2pkh_raw);
        assert!(verify_reward_transaction(
            &wrong_p2pkh_candidate,
            &wrong_p2pkh_raw,
            &wallet.reward_locking_bytecode,
            &CHIPNET_PHOTON
        )
        .is_err());
        drop(wallet);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reward_discovery_rejects_tip_change_after_final_unspent_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            let mut header_count = 0;
            for _ in 0..7 {
                let request = socket.read().unwrap().into_text().unwrap();
                let request: Value = serde_json::from_str(request.trim()).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "server.version" => json!(["test", "1.4.1"]),
                    "blockchain.scripthash.listunspent" => json!([]),
                    "blockchain.headers.subscribe" => {
                        header_count += 1;
                        json!({"height": if header_count == 4 { 101 } else { 100 }})
                    }
                    other => panic!("unexpected method: {other}"),
                };
                let response = json!({"jsonrpc":"2.0", "id": request["id"], "result":result});
                socket.send(Message::Text(format!("{response}\n"))).unwrap();
            }
        });
        let dir = unique_path();
        let wallet =
            FundingWallet::load_or_create(&dir.join("key"), MiningNetwork::Chipnet).unwrap();
        let mut session =
            ElectrumSession::connect_failover_for_deployment(&[url], &CHIPNET_PHOTON).unwrap();
        let error = wallet
            .confirmed_reward_utxos(&mut session, &CHIPNET_PHOTON)
            .unwrap_err();
        assert!(error.contains("changed chain tip"), "{error}");
        server.join().unwrap();
        drop(wallet);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn optional_funding_selector_distinguishes_absence_from_bad_rpc_data() {
        for (unspent, expect_none) in [(json!([]), true), (json!({"unexpected": []}), false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut socket = accept(stream).unwrap();
                for _ in 0..4 {
                    let request = socket.read().unwrap().into_text().unwrap();
                    let request: Value = serde_json::from_str(request.trim()).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "server.version" => json!(["test", "1.4.1"]),
                        "blockchain.scripthash.listunspent" => unspent.clone(),
                        "blockchain.headers.subscribe" => json!({"height":100}),
                        other => panic!("unexpected method: {other}"),
                    };
                    let response = json!({"jsonrpc":"2.0", "id": request["id"], "result":result});
                    socket.send(Message::Text(format!("{response}\n"))).unwrap();
                }
            });
            let dir = unique_path();
            let wallet =
                FundingWallet::load_or_create(&dir.join("key"), MiningNetwork::Chipnet).unwrap();
            let mut session =
                ElectrumSession::connect_failover_for_deployment(&[url], &CHIPNET_PHOTON).unwrap();
            let result = wallet.select_optional_confirmed_utxo(&mut session, 1915);
            if expect_none {
                assert!(matches!(result, Ok(None)), "{result:?}");
            } else {
                assert!(result.is_err());
            }
            server.join().unwrap();
            drop(wallet);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn exact_reward_outpoint_check_reaches_beyond_discovery_prefix() {
        let dir = unique_path();
        let wallet =
            FundingWallet::load_or_create(&dir.join("key"), MiningNetwork::Chipnet).unwrap();
        let script = reward_script(&wallet, CHIPNET_PHOTON.category_hex, 77);
        let mut entries = Vec::new();
        let mut raws = Vec::new();
        for index in 0..33u8 {
            let mut raw = synthetic_transaction(&script, 700);
            raw[5] = index;
            let txid = crate::reward::transaction_id(&raw);
            entries.push(json!({
                "tx_hash": txid, "tx_pos": 0, "value": 700, "height": 98,
                "token_data": {"category": CHIPNET_PHOTON.category_hex, "amount": "77", "nft": null}
            }));
            raws.push((txid, raw));
        }
        raws.sort_by(|a, b| a.0.cmp(&b.0));
        let (target_txid, target_raw) = raws.last().unwrap().clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let expected_txid = target_txid.clone();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            for _ in 0..8 {
                let request = socket.read().unwrap().into_text().unwrap();
                let request: Value = serde_json::from_str(request.trim()).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "server.version" => json!(["test", "1.4.1"]),
                    "blockchain.scripthash.listunspent" => json!(entries),
                    "blockchain.headers.subscribe" => json!({"height":100}),
                    "blockchain.transaction.get" => {
                        assert_eq!(request["params"][0], expected_txid);
                        json!(hex::encode(&target_raw))
                    }
                    other => panic!("unexpected method: {other}"),
                };
                let response = json!({"jsonrpc":"2.0", "id": request["id"], "result":result});
                socket.send(Message::Text(format!("{response}\n"))).unwrap();
            }
        });
        let mut session =
            ElectrumSession::connect_failover_for_deployment(&[url], &CHIPNET_PHOTON).unwrap();
        wallet
            .verify_confirmed_reward_outpoints(&mut session, &CHIPNET_PHOTON, &[(target_txid, 0)])
            .unwrap();
        server.join().unwrap();
        drop(wallet);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn exact_reward_outpoint_check_reports_spent_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            for _ in 0..4 {
                let request = socket.read().unwrap().into_text().unwrap();
                let request: Value = serde_json::from_str(request.trim()).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "server.version" => json!(["test", "1.4.1"]),
                    "blockchain.scripthash.listunspent" => json!([]),
                    "blockchain.headers.subscribe" => json!({"height":100}),
                    other => panic!("unexpected method: {other}"),
                };
                let response = json!({"jsonrpc":"2.0", "id": request["id"], "result":result});
                socket.send(Message::Text(format!("{response}\n"))).unwrap();
            }
        });
        let dir = unique_path();
        let wallet =
            FundingWallet::load_or_create(&dir.join("key"), MiningNetwork::Chipnet).unwrap();
        let mut session =
            ElectrumSession::connect_failover_for_deployment(&[url], &CHIPNET_PHOTON).unwrap();
        let error = wallet
            .verify_confirmed_reward_outpoints(
                &mut session,
                &CHIPNET_PHOTON,
                &[("00".repeat(32), 0)],
            )
            .unwrap_err();
        assert!(error.contains("no longer unspent"), "{error}");
        server.join().unwrap();
        drop(wallet);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_key_with_group_or_world_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = unique_path();
        let path = dir.join("key");
        let _ = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_key() {
        use std::os::unix::fs::symlink;
        let dir = unique_path();
        let actual = dir.join("actual");
        let link = dir.join("link");
        let _ = FundingWallet::load_or_create(&actual, MiningNetwork::Chipnet).unwrap();
        symlink(&actual, &link).unwrap();
        assert!(FundingWallet::load_or_create(&link, MiningNetwork::Chipnet).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_wallet_process_holds_exclusive_key_lock_until_drop() {
        let dir = unique_path();
        let path = dir.join("key");
        let first = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        let error = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet)
            .err()
            .expect("second wallet must not share funding key");
        assert!(error.contains("already in use"), "{error}");
        drop(first);
        let _second = FundingWallet::load_or_create(&path, MiningNetwork::Chipnet).unwrap();
        drop(_second);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selects_only_confirmed_token_free_candidate() {
        let valid_txid = "ab".repeat(32);
        let unspent = json!([
            {"tx_hash": "cd".repeat(32), "tx_pos": 0, "value": 2000, "height": 0},
            {"tx_hash": "ef".repeat(32), "tx_pos": 1, "value": 2000, "height": 20,
             "token_data": {"category": "aa".repeat(32)}},
            {"tx_hash": valid_txid, "tx_pos": 2, "value": 2000, "height": 20}
        ]);
        let candidates = parse_confirmed_candidates(&unspent, 22, 1500).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].confirmations, 3);
        assert_eq!(candidates[0].vout, 2);
    }

    #[test]
    fn verifies_raw_transaction_output_and_txid() {
        let dir = unique_path();
        let wallet =
            FundingWallet::load_or_create(&dir.join("key"), MiningNetwork::Chipnet).unwrap();
        let raw = synthetic_transaction(wallet.locking_bytecode(), 12_345);
        let candidate = FundingCandidate {
            txid: crate::reward::transaction_id(&raw),
            vout: 0,
            value_sats: 12_345,
            confirmations: 2,
        };
        let verified =
            verify_funding_transaction(&candidate, &raw, wallet.locking_bytecode()).unwrap();
        assert_eq!(verified.raw_transaction, raw);
        assert_eq!(verified.value_sats, 12_345);
        let mut wrong_value = FundingCandidate {
            value_sats: 12_346,
            ..candidate
        };
        assert!(verify_funding_transaction(&wrong_value, &raw, wallet.locking_bytecode()).is_err());
        wrong_value.value_sats = 12_345;
        wrong_value.txid = "00".repeat(32);
        assert!(verify_funding_transaction(&wrong_value, &raw, wallet.locking_bytecode()).is_err());
        let mut token_script = vec![0xef, 0x00];
        token_script.extend_from_slice(wallet.locking_bytecode());
        let token_raw = synthetic_transaction(&token_script, 12_345);
        let token_candidate = FundingCandidate {
            txid: crate::reward::transaction_id(&token_raw),
            vout: 0,
            value_sats: 12_345,
            confirmations: 2,
        };
        assert!(verify_funding_transaction(
            &token_candidate,
            &token_raw,
            wallet.locking_bytecode()
        )
        .is_err());
        drop(wallet);
        fs::remove_dir_all(dir).unwrap();
    }
}
