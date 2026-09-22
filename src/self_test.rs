//! Offline deterministic validation of the production PHOTON native GPU pipeline.
//! No Fulcrum/node connection or broadcast is used.

use crate::backend::BackendKind;
use crate::config::{RuntimeConfig, DONATION_BPS};
use crate::cuda_photon::CudaPhotonEngine;
use crate::hip_photon::HipPhotonEngine;
use crate::wgpu_photon::WgpuPhotonEngine;
use crate::{crypto, reward, search, tx};
use secp256k1::{PublicKey, SecretKey};
use serde::Serialize;

const TX_BYTES: usize = 615;
const VECTOR_BATON_TXID: &str = "000000124712ae4765fe9789372faebca19c99cc1d59f43df2508bf5c42ea042";
const VECTOR_AGE: u32 = 10;
const VECTOR_CONTRACT_VALUE_SATS: u64 = 15_971_500;
const VECTOR_TOKEN_AMOUNT: u128 = 2_099_905_002_035_715;
const VECTOR_REWARD_RAW: u128 = 4_999_773_813;
const CONTROLLED_NONCE: u32 = 0x1234_5678;

#[derive(Debug, Clone, Serialize)]
pub struct SelfTestReport {
    pub status: &'static str,
    pub backend: &'static str,
    pub device: u32,
    pub candidates: u32,
    pub nonce: u32,
    pub digest_hex: String,
    pub gpu_host_digest_equal: bool,
    pub schnorr_valid: bool,
    pub strict_target_valid: bool,
    pub parent_txid: String,
    pub settlement_txid: String,
    pub miner_token_amount: u128,
    pub donation_token_amount: u128,
    pub reward_token_amount: u128,
    pub settlement_fee_sats: u64,
    pub persistent_device_bytes: usize,
    pub network_access: bool,
    pub broadcast: bool,
}

#[derive(Debug)]
struct ValidatedWinner {
    digest: [u8; 32],
    parent_txid: String,
    settlement_txid: String,
    miner_token_amount: u128,
    donation_token_amount: u128,
    settlement_fee_sats: u64,
}
fn deterministic_secret(last_byte: u8) -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[31] = last_byte;
    secret
}

fn public_key(secret: &[u8; 32]) -> Result<[u8; 33], String> {
    let secret =
        SecretKey::from_secret_bytes(*secret).map_err(|error| format!("test key: {error}"))?;
    Ok(PublicKey::from_secret_key(&secret).serialize())
}

fn reference_context(target: &[u8; 32]) -> tx::ReferenceJobContext {
    tx::ReferenceJobContext {
        prev_txid: VECTOR_BATON_TXID.into(),
        prev_vout: 0,
        age: VECTOR_AGE,
        target_le_hex: hex::encode(target),
        contract_value_sats: VECTOR_CONTRACT_VALUE_SATS,
        contract_token_amount: VECTOR_TOKEN_AMOUNT,
        reward_raw: VECTOR_REWARD_RAW,
    }
}

fn build_reference_shaped_template(
    target: &[u8; 32],
    reward_public_key: &[u8; 33],
) -> Result<[u8; TX_BYTES], String> {
    let context = reference_context(target);
    let reward_address = reward::p2pkh_cashaddr_from_public_key(reward_public_key)?;
    let payout_locking = tx::cashaddr_to_p2pkh_locking(&reward_address)?;
    let params = tx::TemplateParams {
        prev_tx_hash_hex: context.prev_txid,
        prev_index: context.prev_vout,
        age: context.age,
        public_key_hex: hex::encode(reward_public_key),
        target_hex: context.target_le_hex,
        signature_hex: "00".repeat(64),
        nonce: 0,
        contract_value_sats: context.contract_value_sats,
        contract_token_amount: context.contract_token_amount,
        reward_amount: context.reward_raw,
        payout_locking,
    };
    let bytes = tx::build_photon_template_bytes(&params)?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "self-test PHOTON template is {} bytes; expected {TX_BYTES}",
            bytes.len()
        )
    })
}
fn validate_gpu_winner_and_reward(
    template: &[u8; TX_BYTES],
    target: &[u8; 32],
    reward_secret: &[u8; 32],
    reward_public_key: &[u8; 33],
    nonce: u32,
    gpu_digest: &[u8; 32],
) -> Result<ValidatedWinner, String> {
    let context = reference_context(target);
    let reward_address = reward::p2pkh_cashaddr_from_public_key(reward_public_key)?;
    let message = tx::photon_message_sha256(nonce, &context.target_le_hex)?;
    let signature = crypto::bch_schnorr_sign(reward_secret, &message)?;
    if !crypto::bch_schnorr_verify(reward_public_key, &message, &signature)? {
        return Err("host BCH Schnorr verification rejected self-test signature".into());
    }

    let completed = tx::apply_reference_signature(
        &context,
        &reward_address,
        &hex::encode(reward_public_key),
        nonce,
        &hex::encode(signature),
    )?;
    if completed.len() != TX_BYTES {
        return Err(format!(
            "host reconstructed transaction is {} bytes; expected {TX_BYTES}",
            completed.len()
        ));
    }

    let mut expected = *template;
    expected[390..394].copy_from_slice(&nonce.to_le_bytes());
    expected[426..490].copy_from_slice(&signature);
    if completed.as_slice() != expected.as_slice() {
        return Err("host reconstructed transaction differs from GPU template completion".into());
    }

    let host_digest = search::hash256(&completed);
    if &host_digest != gpu_digest {
        return Err(format!(
            "GPU/host HASH256 mismatch: gpu={} host={}",
            hex::encode(gpu_digest),
            hex::encode(host_digest)
        ));
    }
    if !search::meets_target_le(&host_digest, target) {
        return Err("controlled winner failed strict host hash < target verification".into());
    }
    let miner_secret = deterministic_secret(2);
    let miner_public_key = public_key(&miner_secret)?;
    let miner_payout = reward::p2pkh_cashaddr_from_public_key(&miner_public_key)?;
    let settlement = reward::build_self_funded_settlement(
        &completed,
        reward_secret,
        reward_public_key,
        &miner_payout,
        VECTOR_REWARD_RAW,
    )?;

    let (expected_miner, expected_donation) = RuntimeConfig::split_reward(VECTOR_REWARD_RAW);
    if settlement.miner_token_amount != expected_miner
        || settlement.donation_token_amount != expected_donation
        || settlement.miner_token_amount + settlement.donation_token_amount != VECTOR_REWARD_RAW
        || settlement.donation_token_amount
            != VECTOR_REWARD_RAW.saturating_mul(u128::from(DONATION_BPS)) / 10_000
    {
        return Err("self-funded settlement failed exact 98/2 token conservation".into());
    }
    let parent_txid = reward::transaction_id(&completed);
    if settlement.parent_txid != parent_txid {
        return Err("settlement does not spend the reconstructed PHOTON parent".into());
    }
    if settlement.fee_sats != settlement.required_relay_fee_sats {
        return Err("self-funded settlement fee does not equal its required relay fee".into());
    }

    Ok(ValidatedWinner {
        digest: host_digest,
        parent_txid,
        settlement_txid: settlement.settlement_txid,
        miner_token_amount: settlement.miner_token_amount,
        donation_token_amount: settlement.donation_token_amount,
        settlement_fee_sats: settlement.fee_sats,
    })
}
pub fn run_self_test(backend: BackendKind, device: u32) -> Result<SelfTestReport, String> {
    let target = [0xffu8; 32];
    let reward_secret = deterministic_secret(1);
    let reward_public_key = public_key(&reward_secret)?;
    let template = build_reference_shaped_template(&target, &reward_public_key)?;

    let (backend_name, persistent_device_bytes, batch) = match backend {
        BackendKind::Cuda => {
            let mut engine = CudaPhotonEngine::new(device as usize, 1, 1)?;
            let bytes = engine.persistent_device_bytes();
            engine.set_job(&template, &target, &reward_secret)?;
            let batch = engine.search_batch(CONTROLLED_NONCE, 1)?;
            ("cuda", bytes, batch)
        }
        BackendKind::Hip => {
            let mut engine = HipPhotonEngine::new(device as usize, 1, 1)?;
            let bytes = engine.persistent_device_bytes();
            engine.set_job(&template, &target, &reward_secret)?;
            let batch = engine.search_batch(CONTROLLED_NONCE, 1)?;
            ("hip", bytes, batch)
        }
        BackendKind::Wgpu => {
            let mut engine = WgpuPhotonEngine::new(device as usize, 1, 1)?;
            let bytes = engine.persistent_device_bytes();
            engine.set_job(&template, &target, &reward_secret)?;
            let batch = engine.search_batch(CONTROLLED_NONCE, 1)?;
            ("wgpu", bytes, batch)
        }
        BackendKind::Auto => return Err("self-test requires a resolved native backend".into()),
    };
    if batch.candidates != 1
        || batch.total_winners != 1
        || batch.winners.len() != 1
        || batch.truncated()
    {
        return Err(format!(
            "controlled {backend_name} batch returned candidates={} total_winners={} returned={} truncated={}",
            batch.candidates,
            batch.total_winners,
            batch.winners.len(),
            batch.truncated()
        ));
    }
    let winner = &batch.winners[0];
    if winner.nonce != CONTROLLED_NONCE {
        return Err(format!(
            "controlled {backend_name} winner nonce {} != expected {}",
            winner.nonce, CONTROLLED_NONCE
        ));
    }

    let validated = validate_gpu_winner_and_reward(
        &template,
        &target,
        &reward_secret,
        &reward_public_key,
        winner.nonce,
        &winner.digest,
    )?;

    Ok(SelfTestReport {
        status: "PASS",
        backend: backend_name,
        device,
        candidates: batch.candidates,
        nonce: winner.nonce,
        digest_hex: hex::encode(validated.digest),
        gpu_host_digest_equal: true,
        schnorr_valid: true,
        strict_target_valid: true,
        parent_txid: validated.parent_txid,
        settlement_txid: validated.settlement_txid,
        miner_token_amount: validated.miner_token_amount,
        donation_token_amount: validated.donation_token_amount,
        reward_token_amount: VECTOR_REWARD_RAW,
        settlement_fee_sats: validated.settlement_fee_sats,
        persistent_device_bytes,
        network_access: false,
        broadcast: false,
    })
}
pub fn print_report(report: &SelfTestReport, json: bool) {
    if json {
        match serde_json::to_string_pretty(report) {
            Ok(value) => println!("{value}"),
            Err(error) => eprintln!("error: serialize self-test report: {error}"),
        }
        return;
    }

    println!("Pickaxe PHOTON self-test: {}", report.status);
    println!(
        "{} device: {}",
        report.backend.to_ascii_uppercase(),
        report.device
    );
    println!(
        "GPU A->B->C: {} controlled candidate, winner nonce=0x{:08x}",
        report.candidates, report.nonce
    );
    println!("GPU == host HASH256: {}", report.digest_hex);
    println!("BCH Schnorr: valid");
    println!("Strict hash < target: valid");
    println!(
        "Self-funded settlement: miner={} donation={} total={} fee={} sats",
        report.miner_token_amount,
        report.donation_token_amount,
        report.reward_token_amount,
        report.settlement_fee_sats
    );
    println!("Parent txid: {}", report.parent_txid);
    println!("Settlement txid: {}", report.settlement_txid);
    println!(
        "Persistent {} allocation: {} bytes",
        report.backend.to_ascii_uppercase(),
        report.persistent_device_bytes
    );
    println!("Network access: none; broadcast: none");
}
#[cfg(test)]
mod tests {
    use super::*;

    fn host_controlled_digest(
        template: &[u8; TX_BYTES],
        target: &[u8; 32],
        secret: &[u8; 32],
        public: &[u8; 33],
    ) -> [u8; 32] {
        let message = tx::photon_message_sha256(CONTROLLED_NONCE, &hex::encode(target)).unwrap();
        let signature = crypto::bch_schnorr_sign(secret, &message).unwrap();
        assert!(crypto::bch_schnorr_verify(public, &message, &signature).unwrap());
        let mut completed = *template;
        completed[390..394].copy_from_slice(&CONTROLLED_NONCE.to_le_bytes());
        completed[426..490].copy_from_slice(&signature);
        search::hash256(&completed)
    }

    #[test]
    fn offline_host_validation_builds_exact_self_funded_98_2_settlement() {
        let target = [0xffu8; 32];
        let secret = deterministic_secret(1);
        let public = public_key(&secret).unwrap();
        let template = build_reference_shaped_template(&target, &public).unwrap();
        let digest = host_controlled_digest(&template, &target, &secret, &public);

        let validated = validate_gpu_winner_and_reward(
            &template,
            &target,
            &secret,
            &public,
            CONTROLLED_NONCE,
            &digest,
        )
        .unwrap();

        assert_eq!(validated.digest, digest);
        assert_eq!(
            validated.miner_token_amount + validated.donation_token_amount,
            VECTOR_REWARD_RAW
        );
        assert_eq!(
            validated.donation_token_amount,
            VECTOR_REWARD_RAW * u128::from(DONATION_BPS) / 10_000
        );
        assert_eq!(validated.settlement_fee_sats, 794);
    }
    #[test]
    fn offline_host_validation_rejects_gpu_digest_mismatch() {
        let target = [0xffu8; 32];
        let secret = deterministic_secret(1);
        let public = public_key(&secret).unwrap();
        let template = build_reference_shaped_template(&target, &public).unwrap();
        let mut digest = host_controlled_digest(&template, &target, &secret, &public);
        digest[0] ^= 1;

        let error = validate_gpu_winner_and_reward(
            &template,
            &target,
            &secret,
            &public,
            CONTROLLED_NONCE,
            &digest,
        )
        .unwrap_err();
        assert!(error.contains("GPU/host HASH256 mismatch"));
    }

    #[test]
    fn self_test_requires_resolved_native_backend() {
        let error = run_self_test(BackendKind::Auto, 0).unwrap_err();
        assert!(error.contains("resolved native backend"));
    }
}
