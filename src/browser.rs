//! Browser bindings. Protocol, payouts, fees and verification stay in the native library.
use crate::config::{MiningNetwork, MiningToken, RuntimeConfig};
use crate::donation::{Recipient, Schedule};
use crate::live_job::{
    live_job_from_fulcrum_values_for_deployment, stable_fulcrum_tip_hash, LiveJob,
};
use crate::mining_job::{prepare_job, verify_gpu_winner, PreparedJob};
use crate::{fee, m29_table, reward, wgpu_photon::WgpuPhotonEngine};
use rand::Rng;
use secp256k1::{PublicKey, SecretKey};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

fn error(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}

#[wasm_bindgen]
pub fn browser_config(network: &str) -> Result<String, JsValue> {
    let network = MiningNetwork::parse(network).map_err(error)?;
    let deployment = MiningToken::Photon.photon_deployment(network);
    deployment.verify().map_err(error)?;
    let endpoints = match network {
        MiningNetwork::Mainnet => crate::protocol::FULCRUM_WSS_BOOTSTRAP,
        MiningNetwork::Chipnet => crate::protocol::CHIPNET_FULCRUM_WSS_BOOTSTRAP,
    };
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"), "tokens": [MiningToken::Photon.as_str()],
        "scriptHash": deployment.script_hash_hex, "endpoints": endpoints,
    })
    .to_string())
}

#[wasm_bindgen]
pub struct BrowserMiner {
    engine: WgpuPhotonEngine,
    network: MiningNetwork,
    payouts: [String; 3],
    schedule: Schedule,
    recipient: Recipient,
    live: Option<LiveJob>,
    prepared: Option<PreparedJob>,
    key: [u8; 32],
    public_key: [u8; 33],
    nonce: u64,
    generation: u64,
}

#[wasm_bindgen]
impl BrowserMiner {
    pub async fn create(network: &str, address: &str, table: Vec<u8>) -> Result<Self, JsValue> {
        let network = MiningNetwork::parse(network).map_err(error)?;
        let cfg = RuntimeConfig {
            network,
            payout_address: address.to_lowercase(),
            ..Default::default()
        };
        cfg.validate_payout_network().map_err(error)?;
        crate::tx::cashaddr_to_p2pkh_locking(address).map_err(error)?;
        let policy = MiningToken::Photon.fee_policy(network);
        crate::donation::require_direct_reward_policy(policy.scheme).map_err(error)?;
        let payouts = policy.payouts(network, address).map_err(error)?;
        let engine =
            WgpuPhotonEngine::new_async(0, 524_288, 8, (table, m29_table::M29TableSource::Cache))
                .await
                .map_err(error)?;
        let quantum = u64::from(engine.recommended_batch_candidates()) * 64;
        let schedule =
            Schedule::new(policy.scheme, quantum, rand::rng().random()).map_err(error)?;
        let recipient = schedule.recipient();
        Ok(Self {
            engine,
            network,
            payouts,
            schedule,
            recipient,
            live: None,
            prepared: None,
            key: [0; 32],
            public_key: [0; 33],
            nonce: 0,
            generation: 0,
        })
    }

    /// The JS adapter supplies raw JSON, preserving integral server amounts.
    pub fn set_snapshot(
        &mut self,
        before: &str,
        unspent: &str,
        after: &str,
        relay_fee: &str,
    ) -> Result<String, JsValue> {
        let parse = |s| serde_json::from_str::<Value>(s).map_err(error);
        let before = parse(before)?;
        let after = parse(after)?;
        stable_fulcrum_tip_hash(&before, &after).map_err(error)?;
        let mut live = live_job_from_fulcrum_values_for_deployment(
            "browser",
            Value::Null,
            &after,
            &parse(unspent)?,
            MiningToken::Photon.photon_deployment(self.network),
        )
        .map_err(error)?;
        live.relay_fee_sats_per_kb = fee::bch_value_to_sats(&parse(relay_fee)?)
            .map_err(error)?
            .max(reward::MIN_RELAY_FEE_SATS_PER_KB);
        if self.live.as_ref() != Some(&live) || self.prepared.is_none() {
            self.prepared = None;
            self.live = Some(live);
            self.rotate()?;
        }
        self.context()
    }

    pub fn context(&self) -> Result<String, JsValue> {
        let live = self
            .live
            .as_ref()
            .ok_or_else(|| error("waiting for network state"))?;
        Ok(json!([
            live.tip_hash,
            live.height,
            live.baton_txid,
            live.baton_vout,
            live.target_le_hex,
            live.relay_fee_sats_per_kb,
            live.token_amount.to_string(),
            live.reward_raw.to_string(),
        ])
        .to_string())
    }

    /// Every returned transaction passes the same independent host check as CUDA.
    pub async fn search(&mut self) -> Result<String, JsValue> {
        if self.prepared.is_none() {
            return Err(error("job not prepared"));
        }
        if self.nonce == 1u64 << 32 || self.recipient != self.schedule.recipient() {
            self.rotate()?;
        }
        let requested = self
            .schedule
            .limit_batch(self.engine.recommended_batch_candidates());
        let count = u64::from(requested).min((1u64 << 32) - self.nonce) as u32;
        let result = self
            .engine
            .search_batch_async(self.nonce as u32, count)
            .await
            .map_err(error)?;
        self.nonce += u64::from(result.candidates);
        self.schedule.record(result.candidates).map_err(error)?;
        let mut response = json!({"candidates": result.candidates, "context": self.context()?});
        if let Some(winner) = result.winners.first() {
            let verified = verify_gpu_winner(
                self.prepared
                    .as_ref()
                    .ok_or_else(|| error("job not prepared"))?,
                &self.key,
                &self.public_key,
                winner,
            )
            .map_err(error)?;
            response["transaction"] = json!(hex::encode(&verified.transaction));
            response["txid"] = json!(reward::transaction_id(&verified.transaction));
        }
        Ok(response.to_string())
    }
}

impl BrowserMiner {
    fn rotate(&mut self) -> Result<(), JsValue> {
        self.prepared = None;
        self.recipient = self.schedule.recipient();
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| error("generation overflow"))?;
        let live = self
            .live
            .as_ref()
            .ok_or_else(|| error("waiting for network state"))?;
        let job = live.to_mining_job_for_network(
            self.generation,
            &self.payouts[self.recipient as usize],
            self.network,
        );
        let key = SecretKey::new(&mut rand::rng());
        self.key = key.to_secret_bytes();
        self.public_key = PublicKey::from_secret_key(&key).serialize();
        let prepared = prepare_job(job, &self.key, &self.public_key).map_err(error)?;
        self.engine.set_proof_rule(
            MiningToken::Photon
                .photon_deployment(self.network)
                .proof_rule,
        );
        self.engine
            .set_job(&prepared.template, &prepared.target, &self.key)
            .map_err(error)?;
        self.prepared = Some(prepared);
        self.nonce = 0;
        Ok(())
    }
}
