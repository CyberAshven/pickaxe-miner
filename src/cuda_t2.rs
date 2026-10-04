//! T2 token-amount search for the CUDA miner and offline verification.

use super::*;
use crate::tx;

#[path = "cuda_t2_value.rs"]
mod value;

const T2_CANDIDATES: u32 = 65_536;
const T2_PREFIX_BYTES: usize = 448;
pub(super) const T2_GROUP_WINDOWS: u32 = 256;
pub(crate) const T2_GROUP_CANDIDATES: u32 = T2_GROUP_WINDOWS * T2_CANDIDATES;
const T2_MAX_WINDOWS: usize = T2_GROUP_WINDOWS as usize + 1;

#[derive(Debug)]
struct T2Winner {
    j: u16,
    digest: [u8; 32],
}

#[derive(Debug)]
struct T2Batch {
    candidates: u32,
    total_winners: u32,
    winners: Vec<T2Winner>,
}

struct T2Engine {
    _ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    filter: ShiftKernels,
    probes: ShiftKernels,
    prepare_group: ShiftKernels,
    filter_group: ShiftKernels,
    value_filter_group: Option<ShiftKernels>,
    template_gpu: CudaSlice<u8>,
    prefix_gpu: CudaSlice<u32>,
    middle_schedule_gpu: CudaSlice<u32>,
    window_txs_gpu: CudaSlice<u8>,
    window_prefixes_gpu: CudaSlice<u32>,
    target_gpu: CudaSlice<u8>,
    count_gpu: CudaSlice<u32>,
    winner_j_gpu: CudaSlice<u32>,
    winner_sats_gpu: CudaSlice<u32>,
    winner_nonce_gpu: CudaSlice<u32>,
    winner_hashes_gpu: CudaSlice<u8>,
    probe_hash_gpu: CudaSlice<u8>,
    layout: PhotonLayout,
    baton: u64,
    reward: u64,
    winner_cap: u32,
    ready: bool,
}

impl T2Engine {
    fn new(device: usize, _max_candidates: u32, winner_cap: u32) -> Result<Self, String> {
        if winner_cap == 0 {
            return Err("T2 winner cap must be nonzero".into());
        }
        let ctx = CudaContext::new(device).map_err(|error| format!("cuda context: {error}"))?;
        let stream = ctx.default_stream();
        Self::new_shared(ctx, stream, winner_cap, false)
    }

    fn new_shared(
        ctx: Arc<CudaContext>,
        stream: Arc<CudaStream>,
        winner_cap: u32,
        value_mode: bool,
    ) -> Result<Self, String> {
        let tail = |kernel: &str| {
            load_shift_functions(&ctx, "photon_t2_tail.ptx", |shift| {
                format!("pickaxe_t2_{kernel}_shift{shift}")
            })
        };
        let filter = tail("filter")?;
        let probes = tail("probe")?;
        let prepare_group = tail("prepare")?;
        let filter_group = tail("filter_group")?;
        let value_filter_group = if value_mode {
            Some(load_shift_functions(
                &ctx,
                "photon_t2_value.ptx",
                |shift| format!("pickaxe_t2_value_filter_shift{shift}"),
            )?)
        } else {
            None
        };
        let template_gpu = stream
            .alloc_zeros::<u8>(MAX_TX_BYTES)
            .map_err(|error| error.to_string())?;
        let prefix_gpu = stream
            .alloc_zeros::<u32>(8)
            .map_err(|error| error.to_string())?;
        let middle_schedule_gpu = stream
            .alloc_zeros::<u32>(64)
            .map_err(|error| error.to_string())?;
        let window_txs_gpu = stream
            .alloc_zeros::<u8>(T2_MAX_WINDOWS * MAX_TX_BYTES)
            .map_err(|error| error.to_string())?;
        let window_prefixes_gpu = stream
            // Rust stores the block-7 round-10 states after the existing prefix
            // array, once per signature window instead of once per thread block.
            .alloc_zeros::<u32>(T2_MAX_WINDOWS * if cfg!(feature = "rust-t2") { 16 } else { 8 })
            .map_err(|error| error.to_string())?;
        let target_gpu = stream
            .alloc_zeros::<u8>(33)
            .map_err(|error| error.to_string())?;
        let count_gpu = stream
            .alloc_zeros::<u32>(1)
            .map_err(|error| error.to_string())?;
        let winner_j_gpu = stream
            .alloc_zeros::<u32>(winner_cap as usize)
            .map_err(|error| error.to_string())?;
        let winner_sats_gpu = stream
            .alloc_zeros::<u32>(winner_cap as usize)
            .map_err(|error| error.to_string())?;
        let winner_nonce_gpu = stream
            .alloc_zeros::<u32>(winner_cap as usize)
            .map_err(|error| error.to_string())?;
        let winner_hashes_gpu = stream
            .alloc_zeros::<u8>(winner_cap as usize * 32)
            .map_err(|error| error.to_string())?;
        let probe_hash_gpu = stream
            .alloc_zeros::<u8>(32)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            _ctx: ctx,
            stream,
            filter,
            probes,
            prepare_group,
            filter_group,
            value_filter_group,
            template_gpu,
            prefix_gpu,
            middle_schedule_gpu,
            window_txs_gpu,
            window_prefixes_gpu,
            target_gpu,
            count_gpu,
            winner_j_gpu,
            winner_sats_gpu,
            winner_nonce_gpu,
            winner_hashes_gpu,
            probe_hash_gpu,
            layout: PhotonLayout::BASE,
            baton: 0,
            reward: 0,
            winner_cap,
            ready: false,
        })
    }

    fn set_template(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        total: u128,
        reward: u128,
        positive_target: bool,
    ) -> Result<(), String> {
        let layout = PhotonLayout::for_tx_len(template.len())?;
        tx::t2_reward_amount(total, reward, u16::MAX)?;
        let baton = u64::try_from(total - reward).map_err(|_| "T2 baton amount exceeds u64")?;
        let reward_u64 = u64::try_from(reward).map_err(|_| "T2 reward amount exceeds u64")?;
        let shift = layout.shift();
        if template[394 + shift..426 + shift] != target[..]
            || template[490 + shift] != 0xff
            || template[577 + shift] != 0xff
            || template[491 + shift..499 + shift] != baton.to_le_bytes()
            || template[578 + shift..586 + shift] != reward_u64.to_le_bytes()
        {
            return Err(
                "T2 template target or token amount bytes disagree with declared job".into(),
            );
        }
        let mut state = SHA256_INITIAL_STATE;
        sha2::block_api::compress256(&mut state, template[..T2_PREFIX_BYTES].as_chunks::<64>().0);
        // Bytes 512..575 are unchanged by the nonce and signature, and
        // supports_job keeps the T2 amounts out of them.
        let mut middle_schedule = [0u32; 64];
        for (word, bytes) in template[512..576].as_chunks::<4>().0.iter().enumerate() {
            middle_schedule[word] = u32::from_be_bytes(*bytes);
        }
        for word in 16..64 {
            let a = middle_schedule[word - 15];
            let b = middle_schedule[word - 2];
            let sigma0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let sigma1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            middle_schedule[word] = middle_schedule[word - 16]
                .wrapping_add(sigma0)
                .wrapping_add(middle_schedule[word - 7])
                .wrapping_add(sigma1);
        }
        let mut padded = [0u8; MAX_TX_BYTES];
        padded[..template.len()].copy_from_slice(template);
        self.stream
            .memcpy_htod(&padded, &mut self.template_gpu)
            .map_err(|error| error.to_string())?;
        self.stream
            .memcpy_htod(&state, &mut self.prefix_gpu)
            .map_err(|error| error.to_string())?;
        self.stream
            .memcpy_htod(&middle_schedule, &mut self.middle_schedule_gpu)
            .map_err(|error| error.to_string())?;
        let mut gpu_target = [0u8; 33];
        gpu_target[..32].copy_from_slice(target);
        gpu_target[32] = u8::from(positive_target);
        self.stream
            .memcpy_htod(&gpu_target, &mut self.target_gpu)
            .map_err(|error| error.to_string())?;
        self.layout = layout;
        self.baton = baton;
        self.reward = reward_u64;
        self.ready = true;
        Ok(())
    }

    fn probe(&mut self, j: u16) -> Result<[u8; 32], String> {
        if !self.ready {
            return Err("T2 template is not set".into());
        }
        unsafe {
            self.stream
                .launch_builder(&self.probes[self.layout.kernel_index()])
                .arg(&self.template_gpu)
                .arg(&self.prefix_gpu)
                .arg(&self.baton)
                .arg(&self.reward)
                .arg(&j)
                .arg(&mut self.probe_hash_gpu)
                .launch(LaunchConfig {
                    grid_dim: (1, 1, 1),
                    block_dim: (1, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| error.to_string())?;
        }
        let bytes = self
            .stream
            .clone_dtoh(&self.probe_hash_gpu)
            .map_err(|error| error.to_string())?;
        Ok(bytes.try_into().unwrap())
    }

    fn batch(&mut self, base: u32, count: u32) -> Result<T2Batch, String> {
        if !self.ready
            || base
                .checked_add(count)
                .is_none_or(|end| end > T2_CANDIDATES)
        {
            return Err("T2 batch is unconfigured or exceeds 65,536 candidate offsets".into());
        }
        if count == 0 {
            return Ok(T2Batch {
                candidates: 0,
                total_winners: 0,
                winners: vec![],
            });
        }
        self.stream
            .memset_zeros(&mut self.count_gpu)
            .map_err(|error| error.to_string())?;
        unsafe {
            self.stream
                .launch_builder(&self.filter[self.layout.kernel_index()])
                .arg(&self.template_gpu)
                .arg(&self.prefix_gpu)
                .arg(&self.baton)
                .arg(&self.reward)
                .arg(&self.target_gpu)
                .arg(&base)
                .arg(&count)
                .arg(&self.winner_cap)
                .arg(&mut self.count_gpu)
                .arg(&mut self.winner_j_gpu)
                .arg(&mut self.winner_hashes_gpu)
                .launch(LaunchConfig {
                    grid_dim: (count.div_ceil(128), 1, 1),
                    block_dim: (128, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| error.to_string())?;
        }
        let total_winners = self
            .stream
            .clone_dtoh(&self.count_gpu)
            .map_err(|error| error.to_string())?[0];
        let returned = total_winners.min(self.winner_cap) as usize;
        let mut winners = Vec::with_capacity(returned);
        if returned != 0 {
            let js: Vec<u32> = self
                .stream
                .clone_dtoh(&self.winner_j_gpu)
                .map_err(|error| error.to_string())?;
            let hashes: Vec<u8> = self
                .stream
                .clone_dtoh(&self.winner_hashes_gpu)
                .map_err(|error| error.to_string())?;
            for i in 0..returned {
                winners.push(T2Winner {
                    j: js[i] as u16,
                    digest: hashes[i * 32..(i + 1) * 32].try_into().unwrap(),
                });
            }
        }
        Ok(T2Batch {
            candidates: count,
            total_winners,
            winners,
        })
    }

    fn batch_group(
        &mut self,
        parent: &CudaPhotonEngine,
        nonce_base: u32,
        j_base: u32,
        count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if !self.ready || count == 0 || count > T2_GROUP_CANDIDATES {
            return Err("T2 GPU group is unconfigured or exceeds capacity".into());
        }
        let window_count = (j_base + count).div_ceil(T2_CANDIDATES);
        if window_count as usize > T2_MAX_WINDOWS {
            return Err("T2 GPU group exceeds signature window capacity".into());
        }
        unsafe {
            self.stream
                .launch_builder(&self.prepare_group[self.layout.kernel_index()])
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
                .map_err(|error| format!("prepare T2 signed windows: {error}"))?;
        }
        self.stream
            .memset_zeros(&mut self.count_gpu)
            .map_err(|error| error.to_string())?;
        // Use eight-warp blocks for the Rust group kernel.
        // Keep the released native kernel's measured launch geometry.
        let threads = if cfg!(feature = "rust-t2") { 256 } else { 128 };
        unsafe {
            self.stream
                .launch_builder(&self.filter_group[self.layout.kernel_index()])
                .arg(&self.window_txs_gpu)
                .arg(&self.window_prefixes_gpu)
                .arg(&self.middle_schedule_gpu)
                .arg(&self.baton)
                .arg(&self.reward)
                .arg(&self.target_gpu)
                .arg(&nonce_base)
                .arg(&j_base)
                .arg(&count)
                .arg(&self.winner_cap)
                .arg(&mut self.count_gpu)
                .arg(&mut self.winner_nonce_gpu)
                .arg(&mut self.winner_j_gpu)
                .arg(&mut self.winner_hashes_gpu)
                .launch(LaunchConfig {
                    grid_dim: (count.div_ceil(threads), 1, 1),
                    block_dim: (threads, 1, 1),
                    shared_mem_bytes: 0,
                })
                .map_err(|error| format!("filter T2 GPU group: {error}"))?;
        }
        let mut count_host = [0u32; 1];
        self.stream
            .memcpy_dtoh(&self.count_gpu, &mut count_host)
            .map_err(|error| error.to_string())?;
        let total_winners = count_host[0];
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
                    tail_value_sats: None,
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

fn launch_gpu_signatures(
    parent: &mut CudaPhotonEngine,
    nonce_base: u32,
    window_count: u32,
) -> Result<(), String> {
    let stage_a_cfg = LaunchConfig {
        grid_dim: (window_count.div_ceil(128), 1, 1),
        block_dim: (128, 1, 1),
        shared_mem_bytes: 0,
    };
    unsafe {
        parent
            .stream
            .launch_builder(&parent.stage_a)
            .arg(&nonce_base)
            .arg(&parent.target_gpu)
            .arg(&parent.private_key_gpu)
            .arg(&mut parent.message_hashes_gpu)
            .arg(&mut parent.rfc6979_gpu)
            .arg(&window_count)
            .launch(stage_a_cfg)
            .map_err(|error| format!("launch T2 Stage A: {error}"))?;
    }
    let stage_b_cfg = LaunchConfig {
        grid_dim: (window_count.div_ceil(64), 1, 1),
        block_dim: (64, 1, 1),
        shared_mem_bytes: 0,
    };
    for function in &parent.stage_b {
        unsafe {
            parent
                .stream
                .launch_builder(function)
                .arg(&parent.rfc6979_gpu)
                .arg(&parent.table_gpu)
                .arg(&mut parent.points_gpu)
                .arg(&window_count)
                .launch(stage_b_cfg)
                .map_err(|error| format!("launch T2 Stage B: {error}"))?;
        }
    }
    let per_thread = 1u32;
    let c1_cfg = LaunchConfig {
        grid_dim: (window_count.div_ceil(64), 1, 1),
        block_dim: (64, 1, 1),
        shared_mem_bytes: 0,
    };
    unsafe {
        parent
            .stream
            .launch_builder(&parent.stage_c1)
            .arg(&parent.message_hashes_gpu)
            .arg(&parent.rfc6979_gpu)
            .arg(&parent.points_gpu)
            .arg(&parent.public_key_gpu)
            .arg(&parent.fixed_d_gpu)
            .arg(&mut parent.signatures_gpu)
            .arg(&mut parent.negated_nonce_s_gpu)
            .arg(&window_count)
            .arg(&per_thread)
            .launch(c1_cfg)
            .map_err(|error| format!("launch T2 Stage C1: {error}"))?;
    }
    Ok(())
}

/// Production T2 job state: one signature for each 2^16 consecutive amounts.
pub(super) struct T2Live {
    engine: T2Engine,
    template: Vec<u8>,
    target: [u8; 32],
    key: [u8; 32],
    total: u128,
    reward: u128,
    signed_nonce: Option<u32>,
    value_mode: bool,
}

impl T2Live {
    pub(super) fn supports_job(template: &[u8]) -> Result<bool, String> {
        tx::supports_t2_window(template)
    }

    pub(super) fn new(
        ctx: Arc<CudaContext>,
        stream: Arc<CudaStream>,
        winner_cap: u32,
        value_mode: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            engine: T2Engine::new_shared(ctx, stream, winner_cap, value_mode)?,
            template: Vec::new(),
            target: [0; 32],
            key: [0; 32],
            total: 0,
            reward: 0,
            signed_nonce: None,
            value_mode,
        })
    }

    pub(super) fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        key: &[u8; 32],
        positive_target: bool,
    ) -> Result<(), String> {
        if !Self::supports_job(template)? {
            return Err("T2 job cannot cover the complete 65,536-amount window".into());
        }
        let shift = PhotonLayout::for_tx_len(template.len())?.shift();
        let baton = u64::from_le_bytes(template[491 + shift..499 + shift].try_into().unwrap());
        let reward = u64::from_le_bytes(template[578 + shift..586 + shift].try_into().unwrap());
        let total = u128::from(baton) + u128::from(reward);
        self.engine
            .set_template(template, target, total, u128::from(reward), positive_target)?;
        self.key.fill(0);
        self.key = *key;
        self.template = template.to_vec();
        self.target = *target;
        self.total = total;
        self.reward = u128::from(reward);
        self.signed_nonce = Some(u32::from_le_bytes(
            template[390 + shift..394 + shift].try_into().unwrap(),
        ));
        Ok(())
    }

    pub(super) fn batch(
        &mut self,
        parent: &mut CudaPhotonEngine,
        base: u32,
        count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if self.template.is_empty() || count == 0 || count > self.batch_capacity() {
            return Err("T2 batch exceeds the GPU signature group".into());
        }
        let per_nonce = if self.value_mode {
            T2_CANDIDATES * value::fee_options(self.engine.layout)
        } else {
            T2_CANDIDATES
        };
        let nonce_base = base / per_nonce;
        let candidate_base = base % per_nonce;
        let window_count = (candidate_base + count).div_ceil(per_nonce);
        if window_count as usize > T2_MAX_WINDOWS {
            return Err("T2 batch needs too many signed windows".into());
        }
        launch_gpu_signatures(parent, nonce_base, window_count)?;
        if self.value_mode {
            self.engine
                .batch_group_value(parent, nonce_base, candidate_base, count)
        } else {
            self.engine
                .batch_group(parent, nonce_base, candidate_base, count)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crypto, search, tx};
    use std::time::{Duration, Instant};

    const TOTAL: u128 = 2_099_905_002_035_715;
    const REWARD: u128 = 4_999_773_813;
    const NONCE: u32 = 0x1234_5678;

    /// Every layout shift the kernels are built for.
    const SHIFTS: [usize; PhotonLayout::GPU_SHIFTS.len()] = PhotonLayout::GPU_SHIFTS;

    fn signed_template(shift: usize, target: [u8; 32], reward: u128) -> Vec<u8> {
        signed_template_with_nonce(shift, target, reward, NONCE)
    }

    fn signed_template_with_nonce(
        shift: usize,
        target: [u8; 32],
        reward: u128,
        nonce: u32,
    ) -> Vec<u8> {
        let key = [0x11; 32];
        let public = crypto::compressed_pubkey(&key).unwrap();
        let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
        let signature = crypto::bch_schnorr_sign(&key, &message).unwrap();
        tx::template_for_shift(shift, |age| tx::TemplateParams {
            prev_tx_hash_hex: "aa".repeat(32),
            prev_index: 0,
            age,
            public_key_hex: hex::encode(public),
            target_hex: hex::encode(target),
            signature_hex: hex::encode(signature),
            nonce,
            contract_value_sats: 15_971_500,
            relay_fee_sats_per_kb: 1_000,
            contract_token_amount: TOTAL,
            reward_amount: reward,
            payout_locking: tx::cashaddr_to_p2pkh_locking(
                "zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3",
            )
            .unwrap(),
        })
    }

    #[test]
    fn t2_keeps_wide_layout_baton_bytes_out_of_the_shared_middle_block() {
        let template = |shift, baton: u64| {
            tx::template_for_shift(shift, |age| tx::TemplateParams {
                prev_tx_hash_hex: "aa".repeat(32),
                prev_index: 0,
                age,
                public_key_hex: hex::encode(crypto::compressed_pubkey(&[0x11; 32]).unwrap()),
                target_hex: "ff".repeat(32),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 15_971_500,
                relay_fee_sats_per_kb: 1_000,
                contract_token_amount: u128::from(baton) + REWARD,
                reward_amount: REWARD,
                payout_locking: tx::cashaddr_to_p2pkh_locking(
                    "zqqpfwsvht3uaf4y5sm53me90edmtx8cmyd0xx3fv3",
                )
                .unwrap(),
            })
        };
        // At shift 16 baton bytes 5..=7 sit in bytes 512..575.
        let steady = u64::try_from(TOTAL - REWARD).unwrap();
        let carries = 0x0007_00ff_ffff_8000;
        assert!(T2Live::supports_job(&template(16, steady)).unwrap());
        assert!(!T2Live::supports_job(&template(16, carries)).unwrap());
        // The narrow v0 layout keeps every baton byte in block 7.
        assert!(T2Live::supports_job(&template(0, carries)).unwrap());
    }

    #[test]
    fn t2_gpu_hash_matches_independent_host_serialization_at_each_age_width_if_cuda_present() {
        let mut engine = T2Engine::new(0, 65_536, 8).unwrap();
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        for shift in SHIFTS {
            let template = signed_template(shift, target, REWARD);
            engine
                .set_template(&template, &target, TOTAL, REWARD, false)
                .unwrap();
            for j in [0, 1, 255, 256, 65_535] {
                let actual = engine.probe(j).unwrap();
                let expected =
                    search::hash256(&signed_template(shift, target, REWARD - u128::from(j)));
                assert_eq!(actual, expected, "shift={shift} j={j}");
            }
        }
    }

    #[test]
    fn t2_gpu_hash_matches_confirmed_chipnet_parent_if_cuda_present() {
        let template = hex::decode(
            include_str!("../reference/photon_v32_chipnet_confirmed_parent.hex").trim(),
        )
        .unwrap();
        let layout =
            PhotonLayout::for_age_with_deployment(8, &crate::protocol::CHIPNET_PHOTON).unwrap();
        assert_eq!(layout.shift(), 14);
        let target: [u8; 32] = template[layout.target_offset()..layout.target_offset() + 32]
            .try_into()
            .unwrap();
        let mut engine = match T2Engine::new(0, 65_536, 8) {
            Ok(engine) => engine,
            Err(error) if cuda_unavailable_for_tests(&error) => {
                eprintln!("skip chipnet T2 GPU vector: {error}");
                return;
            }
            Err(error) => panic!("chipnet T2 GPU setup failed: {error}"),
        };
        engine
            .set_template(
                &template,
                &target,
                2_095_454_920_205_042,
                4_989_178_380,
                true,
            )
            .unwrap();
        assert_eq!(engine.probe(0).unwrap(), search::hash256(&template));
    }

    #[test]
    fn t2_gpu_all_pass_readback_is_bounded_and_reconstructable_if_cuda_present() {
        let mut engine = T2Engine::new(0, 65_536, 8).unwrap();
        let target = [0xff; 32];
        engine
            .set_template(
                &signed_template(0, target, REWARD),
                &target,
                TOTAL,
                REWARD,
                false,
            )
            .unwrap();
        let result = engine.batch(0, 128).unwrap();
        assert_eq!(result.candidates, 128);
        assert_eq!(result.total_winners, 128);
        assert_eq!(result.winners.len(), 8);
        for winner in result.winners {
            assert_eq!(
                winner.digest,
                search::hash256(&signed_template(0, target, REWARD - u128::from(winner.j)))
            );
        }
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn chipnet_t2_filter_does_not_let_negative_hashes_fill_winner_buffer_if_cuda_present() {
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        let key = [0x11; 32];
        let template = signed_template_with_nonce(0, target, REWARD, 0);
        let baton = u64::try_from(TOTAL - REWARD).unwrap();
        let reward = u64::try_from(REWARD).unwrap();
        let digest_at = |j: u32| {
            let mut tx = template.clone();
            tx[491..499].copy_from_slice(&(baton + u64::from(j)).to_le_bytes());
            tx[578..586].copy_from_slice(&(reward - u64::from(j)).to_le_bytes());
            search::hash256(&tx)
        };
        let base = (0..65_528)
            .find(|base| {
                (0..8).all(|offset| digest_at(base + offset)[31] & 0x80 != 0)
                    && digest_at(base + 8)[31] & 0x80 == 0
            })
            .expect("fixture has eight negative hashes followed by a positive hash");
        let mut engine = match CudaPhotonEngine::new(0, 65_536, 1) {
            Ok(engine) => engine,
            Err(error) if cuda_unavailable_for_tests(&error) => {
                eprintln!("skip chipnet signed T2 GPU filter: {error}");
                return;
            }
            Err(error) => panic!("chipnet T2 GPU setup failed: {error}"),
        };
        engine.enable_t2_search().unwrap();
        engine.set_positive_target_rule(true);
        engine.set_job(&template, &target, &key).unwrap();
        let result = engine.search_batch(base, 9).unwrap();
        assert_eq!(result.total_winners, 1);
        assert_eq!(result.winners.len(), 1);
        assert_eq!(result.winners[0].tail_j, Some((base + 8) as u16));
        assert_eq!(result.winners[0].digest, digest_at(base + 8));
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn t2_live_mode_refreshes_signature_across_nonce_windows_if_cuda_present() {
        let target = [0xff; 32];
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_search().unwrap();
        engine
            .set_job(
                &signed_template_with_nonce(0, target, REWARD, 0),
                &target,
                &key,
            )
            .unwrap();
        for (base, count) in [(65_520, 16), (65_536, 16), (131_056, 16)] {
            let result = engine.search_batch(base, count).unwrap();
            assert_eq!(result.candidates, count);
            assert_eq!(result.total_winners, count);
            for winner in result.winners {
                let j = winner.tail_j.unwrap();
                assert_eq!(winner.nonce, base >> 16);
                let raw =
                    signed_template_with_nonce(0, target, REWARD - u128::from(j), winner.nonce);
                assert_eq!(winner.digest, search::hash256(&raw));
            }
        }
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn t2_live_falls_back_when_reward_cannot_cover_the_full_window_if_cuda_present() {
        let target = [0xff; 32];
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_search().unwrap();
        let boundary_reward = u128::from(u32::MAX) + 100;
        engine
            .set_job(
                &signed_template_with_nonce(0, target, boundary_reward, 0),
                &target,
                &key,
            )
            .unwrap();
        assert!(engine.t2.is_none());
        let result = engine.search_batch(0, 4).unwrap();
        assert_eq!(result.candidates, 4);
        assert!(result.winners.iter().all(|winner| winner.tail_j.is_none()));

        engine
            .set_job(
                &signed_template_with_nonce(0, target, REWARD, 0),
                &target,
                &key,
            )
            .unwrap();
        assert!(engine.t2.is_some());
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn t2_live_group_spans_two_signed_windows_with_bounded_winners_if_cuda_present() {
        let target = [0xff; 32];
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_search().unwrap();
        engine
            .set_job(
                &signed_template_with_nonce(0, target, REWARD, 0),
                &target,
                &key,
            )
            .unwrap();
        let result = engine.search_batch(0, 131_072).unwrap();
        assert_eq!(result.candidates, 131_072);
        assert_eq!(result.total_winners, 131_072);
        assert_eq!(result.winners.len(), 8);
        for winner in result.winners {
            let j = winner.tail_j.unwrap();
            let expected =
                signed_template_with_nonce(0, target, REWARD - u128::from(j), winner.nonce);
            assert_eq!(winner.digest, search::hash256(&expected));
        }
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn t2_gpu_signature_and_hash_match_host_at_each_age_and_window_edge_if_cuda_present() {
        let target = [0xff; 32];
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_search().unwrap();
        for shift in SHIFTS {
            engine
                .set_job(
                    &signed_template_with_nonce(shift, target, REWARD, 0),
                    &target,
                    &key,
                )
                .unwrap();
            for (base, count) in [(0, 3), (65_534, 4), (131_071, 2), (u32::MAX, 1)] {
                let result = engine.search_batch(base, count).unwrap();
                assert_eq!(result.total_winners, count);
                for winner in result.winners {
                    let j = winner.tail_j.unwrap();
                    let tx = signed_template_with_nonce(
                        shift,
                        target,
                        REWARD - u128::from(j),
                        winner.nonce,
                    );
                    assert_eq!(
                        winner.digest,
                        search::hash256(&tx),
                        "shift={shift} base={base}"
                    );
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn t2_gpu_filter_has_no_missing_winners_across_targets_and_partial_blocks_if_cuda_present() {
        let mut engine = CudaPhotonEngine::new(0, 65_536, 1024).unwrap();
        engine.enable_t2_search().unwrap();
        for shift in SHIFTS {
            for high_byte in [0, 0x3f, 0x7f, 0xff] {
                let mut target = [0xff; 32];
                target[31] = high_byte;
                engine
                    .set_job(
                        &signed_template_with_nonce(shift, target, REWARD, 0),
                        &target,
                        &[0x11; 32],
                    )
                    .unwrap();
                for base in [0, 65_280] {
                    let result = engine.search_batch(base, 513).unwrap();
                    let mut expected = Vec::new();
                    for position in base..base + 513 {
                        let nonce = position >> 16;
                        let j = (position & 0xffff) as u16;
                        let tx = signed_template_with_nonce(
                            shift,
                            target,
                            REWARD - u128::from(j),
                            nonce,
                        );
                        let digest = search::hash256(&tx);
                        if search::meets_target_le(&digest, &target) {
                            expected.push((nonce, j, digest));
                        }
                    }
                    let mut actual: Vec<_> = result
                        .winners
                        .into_iter()
                        .map(|winner| (winner.nonce, winner.tail_j.unwrap(), winner.digest))
                        .collect();
                    actual.sort_unstable();
                    assert_eq!(result.total_winners as usize, expected.len());
                    assert_eq!(
                        actual, expected,
                        "shift={shift} high={high_byte} base={base}"
                    );
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    fn t2_gpu_full_group_returns_exact_bounded_winners_if_cuda_present() {
        let mut target = [0xff; 32];
        target[31] = 0;
        target[30] = 0;
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_search().unwrap();
        engine
            .set_job(
                &signed_template_with_nonce(1, target, REWARD, 0),
                &target,
                &key,
            )
            .unwrap();
        let result = engine.search_batch(0, T2_GROUP_CANDIDATES).unwrap();
        assert_eq!(result.candidates, T2_GROUP_CANDIDATES);
        assert!(result.total_winners > 8);
        assert_eq!(result.winners.len(), 8);
        for winner in result.winners {
            let j = winner.tail_j.unwrap();
            let tx = signed_template_with_nonce(1, target, REWARD - u128::from(j), winner.nonce);
            assert_eq!(winner.digest, search::hash256(&tx));
        }
    }

    #[test]
    #[cfg(feature = "tail-value-grind")]
    fn t2_value_group_winners_match_host_across_age_and_nonce_boundaries_if_cuda_present() {
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_value_search().unwrap();
        for shift in SHIFTS {
            engine
                .set_job(
                    &signed_template_with_nonce(shift, target, REWARD, 0),
                    &target,
                    &key,
                )
                .unwrap();
            let per_nonce = 65_536 * (211 - shift as u32);
            assert_eq!(engine.t2_group_batch_candidates(), Some(4 * per_nonce));
            for (base, count) in [(0, 64), (17, 31), (per_nonce - 2, 4)] {
                let result = engine.search_batch(base, count).unwrap();
                assert_eq!(result.total_winners, count);
                for winner in result.winners {
                    let j = winner.tail_j.unwrap();
                    let sats = winner.tail_value_sats.unwrap();
                    let mut raw = signed_template_with_nonce(
                        shift,
                        target,
                        REWARD - u128::from(j),
                        winner.nonce,
                    );
                    tx::set_payout_value_sats(&mut raw, sats).unwrap();
                    assert_eq!(winner.digest, search::hash256(&raw));
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    #[ignore = "serial offline SHA throughput benchmark; requires CUDA and matching local PTX"]
    fn t2_sha_filter_group_benchmark() {
        let target = [0u8; 32];
        let key = [0x11; 32];
        let mut engine = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        engine.enable_t2_search().unwrap();
        engine
            .set_job(
                &signed_template_with_nonce(1, target, REWARD, 0),
                &target,
                &key,
            )
            .unwrap();
        for _ in 0..32 {
            assert_eq!(
                engine
                    .search_batch(0, T2_GROUP_CANDIDATES)
                    .unwrap()
                    .candidates,
                T2_GROUP_CANDIDATES
            );
        }
        let start = Instant::now();
        let mut candidates = 0u64;
        for _ in 0..96 {
            candidates += u64::from(
                engine
                    .search_batch(0, T2_GROUP_CANDIDATES)
                    .unwrap()
                    .candidates,
            );
        }
        let seconds = start.elapsed().as_secs_f64();
        eprintln!(
            "t2_sha_filter_group_benchmark: {candidates} candidates in {seconds:.6} s = {:.3} MH/s",
            candidates as f64 / seconds / 1e6
        );
    }

    #[test]
    #[ignore = "serial offline throughput comparison; requires CUDA and matching local PTX"]
    fn t2_ab_benchmark() {
        let target: [u8; 32] =
            hex::decode("ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000")
                .unwrap()
                .try_into()
                .unwrap();
        let key = [0x11; 32];
        let mut old = CudaPhotonEngine::new(0, 565_248, 8).unwrap();
        let mut incremental = super::super::incremental::Incremental::new(&old, 32).unwrap();
        incremental.c1_per_thread = 16;
        old.set_job(&signed_template(0, target, REWARD), &target, &key)
            .unwrap();
        incremental.set_message(&old, &target, NONCE).unwrap();
        let mut t2 = T2Engine::new(0, 65_536, 8).unwrap();
        t2.set_template(
            &signed_template(0, target, REWARD),
            &target,
            TOTAL,
            REWARD,
            false,
        )
        .unwrap();
        let mut old_base = 0u32;
        let mut t2_nonce = NONCE;
        let mut records = Vec::new();
        for (round, kind) in ["old", "t2_gpu", "t2_full", "t2_full", "t2_gpu", "old"]
            .into_iter()
            .enumerate()
        {
            let mut run = || match kind {
                "old" => {
                    let result = incremental.batch(&mut old, old_base, 565_248).unwrap();
                    assert_eq!(result.candidates, 565_248);
                    old_base += 565_248;
                    565_248u64
                }
                "t2_gpu" => {
                    assert_eq!(t2.batch(0, 65_536).unwrap().candidates, 65_536);
                    65_536
                }
                _ => {
                    let template = signed_template_with_nonce(0, target, REWARD, t2_nonce);
                    t2.set_template(&template, &target, TOTAL, REWARD, false)
                        .unwrap();
                    assert_eq!(t2.batch(0, 65_536).unwrap().candidates, 65_536);
                    t2_nonce = t2_nonce.wrapping_add(1);
                    65_536
                }
            };
            let warm = Instant::now();
            while warm.elapsed() < Duration::from_millis(500) {
                run();
            }
            let start = Instant::now();
            let mut candidates = 0u64;
            while start.elapsed() < Duration::from_secs(3) {
                candidates += run();
            }
            let seconds = start.elapsed().as_secs_f64();
            let rate = candidates as f64 / seconds / 1e6;
            eprintln!("round={round} kind={kind} candidates={candidates} seconds={seconds:.6} M_candidates_per_s={rate:.3}");
            records.push(serde_json::json!({"round":round,"kind":kind,"candidates":candidates,"seconds":seconds,"million_candidates_per_second":rate}));
        }
        std::fs::create_dir_all("artifacts/t2-tail").unwrap();
        std::fs::write(
            "artifacts/t2-tail/benchmark.json",
            serde_json::to_vec_pretty(&records).unwrap(),
        )
        .unwrap();
    }

    #[test]
    #[cfg(feature = "tail-grind")]
    #[ignore = "serial offline comparison of the integrated T2 worker and incremental CUDA"]
    fn t2_live_ab_benchmark() {
        let target: [u8; 32] =
            hex::decode("ae9b80bd66e57a8a081b68832ee48cf7f1be0d06ab3e33a34c1e61f014000000")
                .unwrap()
                .try_into()
                .unwrap();
        let key = [0x11; 32];
        let mut old = CudaPhotonEngine::new(0, 565_248, 8).unwrap();
        let mut incremental = super::super::incremental::Incremental::new(&old, 32).unwrap();
        incremental.c1_per_thread = 16;
        old.set_job(&signed_template(0, target, REWARD), &target, &key)
            .unwrap();
        incremental.set_message(&old, &target, NONCE).unwrap();
        let mut live = CudaPhotonEngine::new(0, 65_536, 8).unwrap();
        live.enable_t2_search().unwrap();
        live.set_job(
            &signed_template_with_nonce(0, target, REWARD, 0),
            &target,
            &key,
        )
        .unwrap();
        let mut old_base = 0u32;
        let mut t2_base = 0u32;
        let mut records = Vec::new();
        for (round, kind) in ["old", "t2_live", "t2_live", "old"].into_iter().enumerate() {
            let mut run = || {
                if kind == "old" {
                    let result = incremental.batch(&mut old, old_base, 565_248).unwrap();
                    assert_eq!(result.candidates, 565_248);
                    old_base += 565_248;
                    565_248u64
                } else {
                    let result = live.search_batch(t2_base, 65_536).unwrap();
                    assert_eq!(result.candidates, 65_536);
                    t2_base += 65_536;
                    65_536u64
                }
            };
            let warm = Instant::now();
            while warm.elapsed() < Duration::from_millis(500) {
                run();
            }
            let start = Instant::now();
            let mut candidates = 0u64;
            while start.elapsed() < Duration::from_secs(3) {
                candidates += run();
            }
            let seconds = start.elapsed().as_secs_f64();
            let rate = candidates as f64 / seconds / 1e6;
            eprintln!("round={round} kind={kind} candidates={candidates} seconds={seconds:.6} M_candidates_per_s={rate:.3}");
            records.push(serde_json::json!({"round":round,"kind":kind,"candidates":candidates,"seconds":seconds,"million_candidates_per_second":rate}));
        }
        std::fs::create_dir_all("artifacts/t2-tail").unwrap();
        std::fs::write(
            "artifacts/t2-tail/live-benchmark.json",
            serde_json::to_vec_pretty(&records).unwrap(),
        )
        .unwrap();
    }
}
