//! Shared job construction and independent winner verification.
use crate::gpu_types::PhotonCudaWinner;
use crate::proof::{hash256, meets_target_le_for_rule, parse_hex32};
use crate::{
    config::{MiningNetwork, MiningToken},
    crypto,
    protocol::ProofRule,
    tx,
};

#[derive(Debug, Clone, Default)]
pub struct MiningJob {
    pub network: MiningNetwork,
    pub height: u32,
    pub baton_txid: String,
    pub baton_vout: u32,
    pub baton_height: u32,
    pub baton_value_sats: u64,
    pub relay_fee_sats_per_kb: u64,
    pub age: u32,
    pub target_le_hex: String,
    pub token_amount: u128,
    pub reward_raw: u128,
    pub payout_address: String,
    pub source_identity: String,
    pub generation_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedWinner {
    pub generation_id: u64,
    pub height: u32,
    pub baton_txid: String,
    pub baton_vout: u32,
    /// Base reward of the job that produced this winner; anchors T2 validation.
    pub job_reward_raw: u128,
    pub nonce: u32,
    pub digest: [u8; 32],
    pub public_key: [u8; 33],
    pub signature: [u8; 64],
    pub transaction: Vec<u8>,
}

pub(crate) struct PreparedJob {
    pub(crate) job: MiningJob,
    pub(crate) template: Vec<u8>,
    pub(crate) target: [u8; 32],
}

pub(crate) fn validate_job(job: &MiningJob) -> Result<[u8; 32], String> {
    if job.generation_id == 0 {
        return Err("mining job generation_id must be nonzero".into());
    }
    tx::PhotonLayout::for_age_with_deployment(
        job.age,
        MiningToken::Photon.photon_deployment(job.network),
    )?;
    tx::require_covenant_hash_preimage(job.token_amount, job.reward_raw)?;
    if job.payout_address.trim().is_empty() {
        return Err("mining payout address is required".into());
    }
    let target = parse_hex32(&job.target_le_hex)?;
    if MiningToken::Photon
        .photon_deployment(job.network)
        .proof_rule
        == ProofRule::Positive
        && (target[31] & 0x80 != 0 || target.iter().all(|byte| *byte == 0))
    {
        return Err("PHOTON target must be a positive ScriptNum".into());
    }
    Ok(target)
}

pub(crate) fn prepare_job(
    job: MiningJob,
    sk: &[u8; 32],
    public_key: &[u8; 33],
) -> Result<PreparedJob, String> {
    let target = validate_job(&job)?;
    let payout_locking = tx::cashaddr_to_p2pkh_locking(&job.payout_address)?;
    if payout_locking == crate::reward::p2pkh_locking_from_public_key(public_key) {
        return Err(
            "PHOTON search identity must be separate from the funded reward identity".into(),
        );
    }
    let message = tx::photon_message_sha256(0, &job.target_le_hex)?;
    let signature = crypto::bch_schnorr_sign(sk, &message)?;
    if !crypto::bch_schnorr_verify(public_key, &message, &signature)? {
        return Err("generated PHOTON setup signature failed verification".into());
    }
    let params = tx::TemplateParams {
        prev_tx_hash_hex: job.baton_txid.clone(),
        prev_index: job.baton_vout,
        age: job.age,
        public_key_hex: hex::encode(public_key),
        target_hex: job.target_le_hex.clone(),
        signature_hex: hex::encode(signature),
        nonce: 0,
        contract_value_sats: job.baton_value_sats,
        relay_fee_sats_per_kb: job.relay_fee_sats_per_kb,
        contract_token_amount: job.token_amount,
        reward_amount: job.reward_raw,
        payout_locking,
    };
    let deployment = MiningToken::Photon.photon_deployment(job.network);
    let template = tx::build_photon_template_bytes_for_deployment(&params, deployment)?;
    let layout = tx::PhotonLayout::for_age_with_deployment(job.age, deployment)?;
    if template.len() != layout.tx_bytes() {
        return Err(format!(
            "PHOTON live template is {} bytes; age {} needs {}",
            template.len(),
            job.age,
            layout.tx_bytes()
        ));
    }
    let target_offset = layout.target_offset();
    if template[target_offset..target_offset + 32] != target[..] {
        return Err("PHOTON template target bytes do not match live target".into());
    }
    Ok(PreparedJob {
        job,
        template,
        target,
    })
}

pub(crate) fn verify_gpu_winner(
    prepared: &PreparedJob,
    sk: &[u8; 32],
    public_key: &[u8; 33],
    winner: &PhotonCudaWinner,
) -> Result<VerifiedWinner, String> {
    if winner.tail_j.is_some() && winner.schnorr_k.is_some() {
        return Err("GPU winner cannot combine T2 amount and incremental scalar modes".into());
    }
    if let Some(sats) = winner.tail_value_sats {
        return Err(format!(
            "GPU payout BCH value must remain 700 sats (got {sats})"
        ));
    }
    let actual_reward = match winner.tail_j {
        Some(j) => tx::t2_reward_amount(prepared.job.token_amount, prepared.job.reward_raw, j)?,
        None => prepared.job.reward_raw,
    };
    let message = tx::photon_message_sha256(winner.nonce, &prepared.job.target_le_hex)?;
    let signature = match winner.schnorr_k {
        Some(k) => crypto::bch_schnorr_sign_search_candidate(sk, &message, k)?,
        None => crypto::bch_schnorr_sign(sk, &message)?,
    };
    if !crypto::bch_schnorr_verify(public_key, &message, &signature)? {
        return Err("returned GPU winner failed BCH Schnorr verification".into());
    }
    let context = tx::ReferenceJobContext {
        prev_txid: prepared.job.baton_txid.clone(),
        prev_vout: prepared.job.baton_vout,
        age: prepared.job.age,
        target_le_hex: prepared.job.target_le_hex.clone(),
        contract_value_sats: prepared.job.baton_value_sats,
        relay_fee_sats_per_kb: prepared.job.relay_fee_sats_per_kb,
        contract_token_amount: prepared.job.token_amount,
        reward_raw: actual_reward,
    };
    let transaction = tx::apply_reference_signature_for_deployment(
        &context,
        &prepared.job.payout_address,
        &hex::encode(public_key),
        winner.nonce,
        &hex::encode(signature),
        MiningToken::Photon.photon_deployment(prepared.job.network),
    )?;
    let digest = hash256(&transaction);
    if digest != winner.digest {
        return Err(format!(
            "GPU winner HASH256 mismatch: gpu={} host={}",
            hex::encode(winner.digest),
            hex::encode(digest)
        ));
    }
    if !meets_target_le_for_rule(
        &digest,
        &prepared.target,
        MiningToken::Photon
            .photon_deployment(prepared.job.network)
            .proof_rule,
    ) {
        return Err("returned GPU winner failed strict host hash < target verification".into());
    }
    Ok(VerifiedWinner {
        generation_id: prepared.job.generation_id,
        height: prepared.job.height,
        baton_txid: prepared.job.baton_txid.clone(),
        baton_vout: prepared.job.baton_vout,
        job_reward_raw: prepared.job.reward_raw,
        nonce: winner.nonce,
        digest,
        public_key: *public_key,
        signature,
        transaction,
    })
}
