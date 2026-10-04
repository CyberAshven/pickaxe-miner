//! Fulcrum snapshot validation shared by native sockets and browser WebSockets.
use crate::config::MiningNetwork;
use crate::mining_job::MiningJob;
use crate::protocol::{derive_photon_state, PhotonDeployment};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Rich live job for CLI / win-tx; converts to search::MiningJob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveJob {
    pub url: String,
    pub server_version: Value,
    pub height: u32,
    pub tip_hash: String,
    pub baton_txid: String,
    pub baton_vout: u32,
    pub baton_height: u32,
    pub baton_value_sats: u64,
    pub relay_fee_sats_per_kb: u64,
    pub commitment_hex: String,
    pub token_amount: u128,
    pub age: u32,
    pub target_le_hex: String,
    pub reward_raw: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveStateSnapshot {
    pub tip_hash: String,
    pub job: LiveJob,
}

impl LiveJob {
    /// Converts a verified Fulcrum snapshot into a mining job.
    pub fn to_mining_job(&self, generation_id: u64, payout_address: &str) -> MiningJob {
        self.to_mining_job_for_network(generation_id, payout_address, MiningNetwork::Mainnet)
    }

    /// Converts a verified snapshot for the selected network into a mining job.
    pub fn to_mining_job_for_network(
        &self,
        generation_id: u64,
        payout_address: &str,
        network: MiningNetwork,
    ) -> MiningJob {
        MiningJob {
            network,
            height: self.height,
            baton_txid: self.baton_txid.clone(),
            baton_vout: self.baton_vout,
            baton_height: self.baton_height,
            baton_value_sats: self.baton_value_sats,
            relay_fee_sats_per_kb: self.relay_fee_sats_per_kb,
            age: self.age,
            target_le_hex: self.target_le_hex.clone(),
            token_amount: self.token_amount,
            reward_raw: self.reward_raw,
            payout_address: payout_address.to_string(),
            source_identity: self.url.clone(),
            generation_id,
        }
    }
}

pub(crate) fn fulcrum_header_height(header: &Value) -> Result<u32, String> {
    header
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "Fulcrum header response omitted a valid height".to_string())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn snapshot_reread(error: &str) -> bool {
    error.contains("changed tip while reading token state")
        || error.contains("snapshot tip identity is inconsistent")
}

pub(crate) fn stable_fulcrum_tip_hash(before: &Value, after: &Value) -> Result<String, String> {
    let before_hash = fulcrum_header_hash(before)?;
    let after_hash = fulcrum_header_hash(after)?;
    let before_height = fulcrum_header_height(before)?;
    let after_height = fulcrum_header_height(after)?;
    if before_height != after_height || !before_hash.eq_ignore_ascii_case(&after_hash) {
        return Err("Fulcrum PHOTON snapshot changed tip while reading token state".into());
    }
    Ok(after_hash)
}

pub(crate) fn fulcrum_header_hash(header: &Value) -> Result<String, String> {
    let header_hex = header
        .get("hex")
        .and_then(Value::as_str)
        .ok_or("Fulcrum header response omitted hex")?;
    let header_bytes =
        hex::decode(header_hex).map_err(|error| format!("invalid Fulcrum header hex: {error}"))?;
    if header_bytes.len() != 80 {
        return Err(format!(
            "Fulcrum header must be exactly 80 bytes; got {}",
            header_bytes.len()
        ));
    }
    let first = Sha256::digest(&header_bytes);
    let second = Sha256::digest(first);
    Ok(second
        .iter()
        .rev()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn live_job_from_fulcrum_values(
    url: &str,
    server_version: Value,
    header: &Value,
    unspent: &Value,
) -> Result<LiveJob, String> {
    live_job_from_fulcrum_values_for_deployment(
        url,
        server_version,
        header,
        unspent,
        &crate::protocol::MAINNET_PHOTON,
    )
}

pub(crate) fn live_job_from_fulcrum_values_for_deployment(
    url: &str,
    server_version: Value,
    header: &Value,
    unspent: &Value,
    deployment: &PhotonDeployment,
) -> Result<LiveJob, String> {
    let height = header
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("Invalid live BCH header response")?;
    let tip_hash = fulcrum_header_hash(header)?;
    let arr = unspent
        .as_array()
        .ok_or("Invalid live PHOTON UTXO response")?;

    let batons = arr
        .iter()
        .filter(|utxo| {
            let category = utxo
                .pointer("/token_data/category")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let capability = utxo
                .pointer("/token_data/nft/capability")
                .and_then(Value::as_str)
                .unwrap_or("");
            category == deployment.category_hex && capability == "mutable"
        })
        .collect::<Vec<_>>();

    if batons.len() != 1 {
        return Err(format!(
            "Expected exactly one live PHOTON baton; found {}",
            batons.len()
        ));
    }
    let baton = batons[0];
    let baton_txid = baton
        .get("tx_hash")
        .and_then(Value::as_str)
        .ok_or("baton missing tx_hash")?
        .to_ascii_lowercase();
    let baton_vout = baton
        .get("tx_pos")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("baton missing tx_pos")?;
    let baton_height = baton
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let baton_value_sats = baton.get("value").and_then(Value::as_u64).unwrap_or(0);
    let commitment_hex = baton
        .pointer("/token_data/nft/commitment")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if deployment.proof_rule == crate::protocol::ProofRule::Positive && commitment_hex.len() >= 72 {
        let previous_target = hex::decode(&commitment_hex[8..72])
            .map_err(|error| format!("invalid PHOTON target: {error}"))?;
        if previous_target.iter().all(|byte| *byte == 0) || previous_target[31] & 0x80 != 0 {
            return Err("PHOTON target must be a positive ScriptNum".into());
        }
    }
    let token_amount_str = baton
        .pointer("/token_data/amount")
        .and_then(Value::as_str)
        .unwrap_or("0");
    let token_amount = token_amount_str
        .parse::<u128>()
        .map_err(|_| format!("bad token amount: {token_amount_str}"))?;
    let derived = derive_photon_state(&commitment_hex, token_amount, height, baton_height)?;

    Ok(LiveJob {
        url: url.to_string(),
        server_version,
        height,
        tip_hash,
        baton_txid,
        baton_vout,
        baton_height,
        baton_value_sats,
        relay_fee_sats_per_kb: 1_000,
        commitment_hex,
        token_amount,
        age: derived.age,
        target_le_hex: derived.target_le_hex,
        reward_raw: derived.reward_raw,
    })
}
