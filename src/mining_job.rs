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
    /// #### PR #32: in a public GPU pool, the address the rig mined for (its
    /// own, or the operator's in a fee window); `None` is the coordinator's
    /// own payout.
    pub payout: Option<String>,
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
        payout: None,
    })
}

/// #### PR #32
/// A winner from a rig must stand on its own before it reaches the claim
/// path: it answers the job it names, carries a valid Schnorr signature, its
/// digest matches its transaction, the digest meets the target under this
/// deployment's rule, and its search identity is separate from the payout.
/// The claim path then rebuilds the transaction from this miner's own job and
/// payouts, so a rig cannot redirect a reward.
#[cfg_attr(not(feature = "stratum-v2"), allow(dead_code))]
pub(crate) fn verify_rig_winner(winner: &VerifiedWinner, job: &MiningJob) -> Result<(), String> {
    if winner.generation_id != job.generation_id
        || winner.height != job.height
        || winner.baton_txid != job.baton_txid
        || winner.baton_vout != job.baton_vout
        || winner.job_reward_raw != job.reward_raw
    {
        return Err("rig winner answers another job".into());
    }
    let target = validate_job(job)?;
    let message = tx::photon_message_sha256(winner.nonce, &job.target_le_hex)?;
    if !crypto::bch_schnorr_verify(&winner.public_key, &message, &winner.signature)? {
        return Err("rig winner failed BCH Schnorr verification".into());
    }
    if hash256(&winner.transaction) != winner.digest {
        return Err("rig winner digest does not match its transaction".into());
    }
    if !meets_target_le_for_rule(
        &winner.digest,
        &target,
        MiningToken::Photon
            .photon_deployment(job.network)
            .proof_rule,
    ) {
        return Err("rig winner does not meet the PHOTON target".into());
    }
    if tx::cashaddr_to_p2pkh_locking(&job.payout_address)?
        == crate::reward::p2pkh_locking_from_public_key(&winner.public_key)
    {
        return Err("rig search identity must be separate from the payout".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A job with an easy target, so real winners turn up within a few nonces.
    pub(crate) fn easy_job() -> MiningJob {
        MiningJob {
            network: MiningNetwork::Mainnet,
            height: 1_000,
            baton_txid: "11".repeat(32),
            baton_vout: 0,
            baton_height: 999,
            baton_value_sats: 15_971_500,
            relay_fee_sats_per_kb: 1_000,
            age: 1,
            target_le_hex: format!("{}7f", "ff".repeat(31)),
            token_amount: 2_099_905_002_035_715,
            reward_raw: 4_999_773_813,
            payout_address: "bitcoincash:zphqsyxwagf5z2mnl66p2e4r6tgvu48pqys3lr2frh".into(),
            source_identity: "coordinator".into(),
            generation_id: 7,
        }
    }

    /// A real, signed winner for `job` that meets its target.
    pub(crate) fn solved_winner(job: &MiningJob, secret: [u8; 32]) -> VerifiedWinner {
        let public_key = secp256k1::PublicKey::from_secret_key(
            &secp256k1::SecretKey::from_secret_bytes(secret).unwrap(),
        )
        .serialize();
        let target = validate_job(job).unwrap();
        let deployment = MiningToken::Photon.photon_deployment(job.network);
        let context = tx::ReferenceJobContext {
            prev_txid: job.baton_txid.clone(),
            prev_vout: job.baton_vout,
            age: job.age,
            target_le_hex: job.target_le_hex.clone(),
            contract_value_sats: job.baton_value_sats,
            relay_fee_sats_per_kb: job.relay_fee_sats_per_kb,
            contract_token_amount: job.token_amount,
            reward_raw: job.reward_raw,
        };
        for nonce in 0..10_000 {
            let message = tx::photon_message_sha256(nonce, &job.target_le_hex).unwrap();
            let signature = crypto::bch_schnorr_sign(&secret, &message).unwrap();
            // Building the transaction refuses a candidate below the target.
            let Ok(transaction) = tx::apply_reference_signature_for_deployment(
                &context,
                &job.payout_address,
                &hex::encode(public_key),
                nonce,
                &hex::encode(signature),
                deployment,
            ) else {
                continue;
            };
            let digest = hash256(&transaction);
            if meets_target_le_for_rule(&digest, &target, deployment.proof_rule) {
                return VerifiedWinner {
                    generation_id: job.generation_id,
                    height: job.height,
                    baton_txid: job.baton_txid.clone(),
                    baton_vout: job.baton_vout,
                    job_reward_raw: job.reward_raw,
                    nonce,
                    digest,
                    public_key,
                    signature,
                    transaction,
                    payout: None,
                };
            }
        }
        panic!("no winner within 10,000 nonces");
    }

    #[test]
    fn rig_winners_must_carry_their_own_proof() {
        let job = easy_job();
        let winner = solved_winner(&job, [1; 32]);
        assert!(verify_rig_winner(&winner, &job).is_ok());
        let mut bad_signature = winner.clone();
        bad_signature.signature[5] ^= 1;
        assert!(verify_rig_winner(&bad_signature, &job).is_err());
        let mut bad_digest = winner.clone();
        bad_digest.digest[0] ^= 1;
        assert!(verify_rig_winner(&bad_digest, &job).is_err());
        let mut other_job = winner.clone();
        other_job.generation_id += 1;
        assert!(verify_rig_winner(&other_job, &job).is_err());
        // The same winner does not meet a much harder target.
        let hard = MiningJob {
            target_le_hex: format!("01{}", "00".repeat(31)),
            ..job.clone()
        };
        assert!(verify_rig_winner(&winner, &hard).is_err());
        // A rig's search key may not be the payout key.
        let public_key = winner.public_key;
        let own = MiningJob {
            payout_address: crate::reward::p2pkh_cashaddr_from_public_key(&public_key).unwrap(),
            ..job
        };
        let same_key = solved_winner(&own, [1; 32]);
        assert!(verify_rig_winner(&same_key, &own).is_err());
    }
}
