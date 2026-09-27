//! GPU V-coordinate search: a 32-value BCH output tile per T2 token amount.

use super::*;

const V_BATCH_WINDOWS: u32 = 4;

pub(super) fn fee_options(layout: PhotonLayout) -> u32 {
    211 - layout.shift() as u32
}

fn tile_group(position: u32, fee_count: u32) -> u32 {
    let per_nonce = T2_CANDIDATES * fee_count;
    let window = position / per_nonce;
    let within = position % per_nonce;
    let j = within / fee_count;
    let value = within % fee_count;
    window * (T2_CANDIDATES * 7) + j * 7 + value / 32
}

impl T2Live {
    pub(crate) fn batch_capacity(&self) -> u32 {
        if self.value_mode {
            V_BATCH_WINDOWS * T2_CANDIDATES * fee_options(self.engine.layout)
        } else {
            T2_GROUP_CANDIDATES
        }
    }
}

impl T2Engine {
    pub(super) fn batch_group_value(
        &mut self,
        parent: &CudaPhotonEngine,
        nonce_base: u32,
        candidate_base: u32,
        count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if !self.ready
            || count == 0
            || count > V_BATCH_WINDOWS * T2_CANDIDATES * fee_options(self.layout)
        {
            return Err("V GPU group is unconfigured or exceeds one nonce window".into());
        }
        let per_nonce = T2_CANDIDATES * fee_options(self.layout);
        let window_count = (candidate_base + count).div_ceil(per_nonce);
        if window_count as usize > T2_MAX_WINDOWS {
            return Err("V GPU group exceeds signature window capacity".into());
        }
        unsafe {
            self.stream
                .launch_builder(&self.prepare_group[self.layout.shift()])
                .arg(&self.template_gpu)
                .arg(&parent.midstate_gpu)
                .arg(&parent.signatures_gpu)
                .arg(&parent.negated_nonce_s_gpu)
                .arg(&parent.points_gpu)
                .arg(&nonce_base)
                .arg(&window_count)
                .arg(&mut self.window_txs_gpu)
                .arg(&mut self.window_prefixes_gpu)
                .launch(LaunchConfig {
                    grid_dim: (window_count.div_ceil(128), 1, 1),
                    block_dim: (128, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| format!("prepare V signed windows: {error}"))?;
        }
        self.stream
            .memset_zeros(&mut self.count_gpu)
            .map_err(|error| error.to_string())?;
        let first_group = tile_group(candidate_base, fee_options(self.layout));
        let last_group = tile_group(candidate_base + count - 1, fee_options(self.layout));
        let group_count = last_group - first_group + 1;
        let filter = self
            .value_filter_group
            .as_ref()
            .ok_or("V kernel is not loaded")?;
        unsafe {
            self.stream
                .launch_builder(&filter[self.layout.shift()])
                .arg(&self.window_txs_gpu)
                .arg(&self.window_prefixes_gpu)
                .arg(&self.baton)
                .arg(&self.reward)
                .arg(&self.target_gpu)
                .arg(&nonce_base)
                .arg(&candidate_base)
                .arg(&count)
                .arg(&first_group)
                .arg(&group_count)
                .arg(&self.winner_cap)
                .arg(&mut self.count_gpu)
                .arg(&mut self.winner_nonce_gpu)
                .arg(&mut self.winner_j_gpu)
                .arg(&mut self.winner_sats_gpu)
                .arg(&mut self.winner_hashes_gpu)
                .launch(LaunchConfig {
                    grid_dim: (group_count.div_ceil(128), 1, 1),
                    block_dim: (128, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| format!("filter V GPU group: {error}"))?;
        }
        let total_winners = self
            .stream
            .clone_dtoh(&self.count_gpu)
            .map_err(|error| error.to_string())?[0];
        let returned = total_winners.min(self.winner_cap) as usize;
        let mut winners = Vec::with_capacity(returned);
        if returned != 0 {
            let nonces = self
                .stream
                .clone_dtoh(&self.winner_nonce_gpu)
                .map_err(|error| error.to_string())?;
            let js = self
                .stream
                .clone_dtoh(&self.winner_j_gpu)
                .map_err(|error| error.to_string())?;
            let sats = self
                .stream
                .clone_dtoh(&self.winner_sats_gpu)
                .map_err(|error| error.to_string())?;
            let hashes = self
                .stream
                .clone_dtoh(&self.winner_hashes_gpu)
                .map_err(|error| error.to_string())?;
            for slot in 0..returned {
                winners.push(PhotonCudaWinner {
                    nonce: nonces[slot],
                    digest: hashes[slot * 32..(slot + 1) * 32].try_into().unwrap(),
                    schnorr_k: None,
                    tail_j: Some(js[slot] as u16),
                    tail_value_sats: Some(sats[slot] as u16),
                });
            }
        }
        Ok(PhotonCudaBatchResult {
            candidates: count,
            total_winners,
            winners,
        })
    }
}
