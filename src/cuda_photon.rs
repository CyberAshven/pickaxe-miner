//! Persistent reference-correct PHOTON CUDA pipeline.
//!
//! One context owns the complete candidate path:
//! Stage A (message SHA-256 + RFC6979) -> M45-style split M29 fixed-base Stage B
//! -> BCH Schnorr C1 (both nonce signs) -> completed transaction HASH256/strict
//! target filter, with the BCH residue test only for a candidate that meets
//! the target.
//! Candidate intermediates remain in device memory. The host reads one winner
//! count and a bounded winner record array only.

use crate::m29_table::{self, M29TableSource};
use crate::tx::PhotonLayout;
use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, LaunchConfig, PushKernelArg,
};
use cudarc::nvrtc::Ptx;
use num_bigint::BigUint;
use secp256k1::{PublicKey, SecretKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Template buffer size: the widest layout the covenant's age bound allows.
const MAX_TX_BYTES: usize = 615 + PhotonLayout::MAX_SHIFT;
const SIGNATURE_BYTES: usize = 64;
/// C2/C3 entry points by layout shift (age push width minus one).
const STAGE_C3_FUNCTIONS: [&str; PhotonLayout::MAX_SHIFT + 1] = [
    "pickaxe_stage_c_dual_filter",
    "pickaxe_stage_c_dual_filter_shift1",
    "pickaxe_stage_c_dual_filter_shift2",
    "pickaxe_stage_c_dual_filter_shift3",
];
const POINT_WORDS: usize = 24;
const FIXED_D_WORDS: usize = 32 * 256 * 8;
/// Candidates per C1 thread that share one field inversion.
const C1_CANDIDATES_PER_THREAD: u32 = 8;
/// Transaction bytes before the nonce's SHA-256 block; fixed for a job.
const MIDSTATE_BYTES: usize = 384;
const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotonCudaWinner {
    pub nonce: u32,
    pub digest: [u8; 32],
    /// Explicit signing scalar for incremental search; never a reward-key nonce.
    pub schnorr_k: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotonCudaBatchResult {
    pub candidates: u32,
    pub total_winners: u32,
    pub winners: Vec<PhotonCudaWinner>,
}

impl PhotonCudaBatchResult {
    /// Checks whether a reported GPU result exceeds the readback limit.
    pub fn truncated(&self) -> bool {
        self.total_winners as usize > self.winners.len()
    }
}

pub struct CudaPhotonEngine {
    _ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    stage_a: CudaFunction,
    stage_b: [CudaFunction; 4],
    stage_c1: CudaFunction,
    stage_c3: [CudaFunction; PhotonLayout::MAX_SHIFT + 1],
    layout: PhotonLayout,
    table_gpu: CudaSlice<u8>,
    target_gpu: CudaSlice<u8>,
    private_key_gpu: CudaSlice<u8>,
    public_key_gpu: CudaSlice<u8>,
    fixed_d_gpu: CudaSlice<u32>,
    message_hashes_gpu: CudaSlice<u8>,
    rfc6979_gpu: CudaSlice<u8>,
    points_gpu: CudaSlice<u32>,
    signatures_gpu: CudaSlice<u8>,
    negated_nonce_s_gpu: CudaSlice<u8>,
    midstate_gpu: CudaSlice<u32>,
    template_gpu: CudaSlice<u8>,
    winner_count_gpu: CudaSlice<u32>,
    winner_nonces_gpu: CudaSlice<u32>,
    winner_hashes_gpu: CudaSlice<u8>,
    max_candidates: u32,
    winner_cap: u32,
    table_source: M29TableSource,
    job_ready: bool,
    incremental: Option<incremental::Incremental>,
    commitment_nonce: u32,
}

/// Lists locations searched for compiled CUDA PTX kernels.
fn cuda_ptx_search_dirs(executable_dir: Option<&Path>, manifest_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(executable_dir) = executable_dir {
        dirs.push(executable_dir.join("cuda").join("build"));
    }
    dirs.push(manifest_dir.join("cuda").join("build"));
    dirs
}

/// Locates a PTX kernel within candidate directories.
fn resolve_ptx_path_in(
    name: &str,
    executable_dir: Option<&Path>,
    manifest_dir: &Path,
) -> Result<PathBuf, String> {
    let dirs = cuda_ptx_search_dirs(executable_dir, manifest_dir);
    if let Some(path) = dirs
        .iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
    {
        return Ok(path);
    }
    let searched = dirs
        .iter()
        .map(|dir| dir.join(name).display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("missing CUDA PTX {name}; searched: {searched}"))
}

/// Finds the deployed PTX kernel for CUDA mining.
fn resolve_ptx_path(name: &str) -> Result<PathBuf, String> {
    let executable = std::env::current_exe().ok();
    let executable_dir = executable.as_deref().and_then(Path::parent);
    resolve_ptx_path_in(name, executable_dir, Path::new(env!("CARGO_MANIFEST_DIR")))
}

/// Loads a CUDA kernel function from its PTX module.
fn load_function(
    ctx: &Arc<CudaContext>,
    ptx_name: &str,
    function_name: &str,
) -> Result<CudaFunction, String> {
    let path = resolve_ptx_path(ptx_name)?;
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let module = ctx
        .load_module(Ptx::from_src(source))
        .map_err(|error| format!("load {ptx_name}: {error}"))?;
    module
        .load_function(function_name)
        .map_err(|error| format!("load {function_name}: {error}"))
}

/// Reports whether a CUDA test error means no usable CUDA device or driver,
/// the only case in which GPU tests may skip. A missing kernel symbol or
/// PTX file, or a launch failure, is a real failure.
#[cfg(test)]
pub(crate) fn cuda_unavailable_for_tests(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("cuda context")
        || lower.contains("no_device")
        || lower.contains("no device")
        || lower.contains("not initialized")
        || lower.contains("not_initialized")
        || lower.contains("dynamically load")
}

/// SHA-256 state after the job-fixed transaction bytes 0..384.
fn transaction_midstate(template: &[u8]) -> [u32; 8] {
    let mut state = SHA256_INITIAL_STATE;
    let blocks = template[..MIDSTATE_BYTES]
        .as_chunks::<64>()
        .0
        .iter()
        .map(|block| *sha2::digest::generic_array::GenericArray::from_slice(block))
        .collect::<Vec<_>>();
    sha2::compress256(&mut state, &blocks);
    state
}

/// Builds the fixed scalar lookup table used by CUDA kernels.
fn fixed_d_table(private_key: &[u8; 32]) -> Vec<u32> {
    let order = BigUint::from_bytes_be(
        &hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
            .expect("secp256k1 order constant"),
    );
    let d = BigUint::from_bytes_be(private_key);
    let mut factor = d;
    let mut table = vec![0u32; FIXED_D_WORDS];
    for byte_position in 0..32usize {
        for digit in 1..256usize {
            let value = (&factor * BigUint::from(digit as u32)) % &order;
            let limbs = value.to_u32_digits();
            let base = (byte_position * 256 + digit) * 8;
            for (limb, word) in limbs.into_iter().take(8).enumerate() {
                table[base + limb] = word;
            }
        }
        factor = (factor * BigUint::from(256u32)) % &order;
    }
    table
}

impl CudaPhotonEngine {
    /// Allocates CUDA buffers and loads the PHOTON pipeline kernels.
    pub fn new(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
    ) -> Result<Self, String> {
        if max_candidates == 0 {
            return Err("PHOTON CUDA max_candidates must be greater than zero".into());
        }
        if winner_cap == 0 {
            return Err("PHOTON CUDA winner_cap must be greater than zero".into());
        }

        let ctx =
            CudaContext::new(device_ordinal).map_err(|error| format!("cuda context: {error}"))?;
        let stream = ctx.default_stream();
        let stage_a = load_function(&ctx, "stage_a_rfc6979.ptx", "pickaxe_stage_a_rfc6979")?;
        let stage_b = [
            load_function(&ctx, "photon_stage_b16.ptx", "pickaxe_photon_b16_part0")?,
            load_function(&ctx, "photon_stage_b16.ptx", "pickaxe_photon_b16_part1")?,
            load_function(&ctx, "photon_stage_b16.ptx", "pickaxe_photon_b16_part2")?,
            load_function(&ctx, "photon_stage_b16.ptx", "pickaxe_photon_b16_part3")?,
        ];
        let stage_c1 = load_function(
            &ctx,
            "photon_c1_schnorr.ptx",
            "pickaxe_photon_c1_schnorr_dual_batched",
        )?;
        let stage_c3 = [
            load_function(&ctx, "photon_c3_dual.ptx", STAGE_C3_FUNCTIONS[0])?,
            load_function(&ctx, "photon_c3_dual.ptx", STAGE_C3_FUNCTIONS[1])?,
            load_function(&ctx, "photon_c3_dual.ptx", STAGE_C3_FUNCTIONS[2])?,
            load_function(&ctx, "photon_c3_dual.ptx", STAGE_C3_FUNCTIONS[3])?,
        ];

        let (table_bytes, table_source) = m29_table::load_or_generate_m29_g16()?;
        let table_gpu = stream
            .clone_htod(&table_bytes)
            .map_err(|error| format!("upload 64 MiB M29 table: {error}"))?;
        drop(table_bytes);

        let target_gpu = stream
            .alloc_zeros::<u8>(32)
            .map_err(|error| format!("alloc target: {error}"))?;
        let private_key_gpu = stream
            .alloc_zeros::<u8>(32)
            .map_err(|error| format!("alloc private key: {error}"))?;
        let public_key_gpu = stream
            .alloc_zeros::<u8>(33)
            .map_err(|error| format!("alloc public key: {error}"))?;
        let fixed_d_gpu = stream
            .alloc_zeros::<u32>(FIXED_D_WORDS)
            .map_err(|error| format!("alloc fixed-d table: {error}"))?;
        let message_hashes_gpu = stream
            .alloc_zeros::<u8>((max_candidates as usize) * 32)
            .map_err(|error| format!("alloc message hashes: {error}"))?;
        let rfc6979_gpu = stream
            .alloc_zeros::<u8>((max_candidates as usize) * 32)
            .map_err(|error| format!("alloc RFC6979 scalars: {error}"))?;
        let points_gpu = stream
            .alloc_zeros::<u32>((max_candidates as usize) * POINT_WORDS)
            .map_err(|error| format!("alloc Stage B points: {error}"))?;
        let signatures_gpu = stream
            .alloc_zeros::<u8>((max_candidates as usize) * SIGNATURE_BYTES)
            .map_err(|error| format!("alloc signatures: {error}"))?;
        let negated_nonce_s_gpu = stream
            .alloc_zeros::<u8>((max_candidates as usize) * 32)
            .map_err(|error| format!("alloc negated-nonce signatures: {error}"))?;
        let midstate_gpu = stream
            .alloc_zeros::<u32>(8)
            .map_err(|error| format!("alloc transaction midstate: {error}"))?;
        let template_gpu = stream
            .alloc_zeros::<u8>(MAX_TX_BYTES)
            .map_err(|error| format!("alloc transaction template: {error}"))?;
        let winner_count_gpu = stream
            .alloc_zeros::<u32>(1)
            .map_err(|error| format!("alloc winner count: {error}"))?;
        let winner_nonces_gpu = stream
            .alloc_zeros::<u32>(winner_cap as usize)
            .map_err(|error| format!("alloc winner nonces: {error}"))?;
        let winner_hashes_gpu = stream
            .alloc_zeros::<u8>((winner_cap as usize) * 32)
            .map_err(|error| format!("alloc winner hashes: {error}"))?;

        Ok(Self {
            _ctx: ctx,
            stream,
            stage_a,
            stage_b,
            stage_c1,
            stage_c3,
            layout: PhotonLayout::BASE,
            table_gpu,
            target_gpu,
            private_key_gpu,
            public_key_gpu,
            fixed_d_gpu,
            message_hashes_gpu,
            rfc6979_gpu,
            points_gpu,
            signatures_gpu,
            negated_nonce_s_gpu,
            midstate_gpu,
            template_gpu,
            winner_count_gpu,
            winner_nonces_gpu,
            winner_hashes_gpu,
            max_candidates,
            winner_cap,
            table_source,
            job_ready: false,
            incremental: None,
            commitment_nonce: 0,
        })
    }

    /// Enable only for the worker's fresh, unfunded search identity, before set_job.
    #[cfg(any(feature = "incremental-k", test))]
    pub(crate) fn enable_incremental_search(&mut self) -> Result<(), String> {
        if self.job_ready {
            return Err("enable incremental search before configuring a job".into());
        }
        let mut incremental = incremental::Incremental::new(self, 32)?;
        incremental.c1_per_thread = 16;
        self.incremental = Some(incremental);
        Ok(())
    }

    /// Returns the source of the CUDA lookup table.
    pub fn table_source(&self) -> M29TableSource {
        self.table_source
    }

    /// Returns the size of persistent CUDA device allocations.
    pub fn persistent_device_bytes(&self) -> usize {
        m29_table::M29_G16_BYTES
            + 32
            + 32
            + 33
            + FIXED_D_WORDS * std::mem::size_of::<u32>()
            + (self.max_candidates as usize) * (32 + 32 + POINT_WORDS * 4 + SIGNATURE_BYTES + 32)
            + MAX_TX_BYTES
            + 8 * std::mem::size_of::<u32>()
            + std::mem::size_of::<u32>()
            + (self.winner_cap as usize) * (4 + 32)
            + if self.incremental.is_some() { 96 } else { 0 }
    }

    /// Uploads validated PHOTON job bytes and target to CUDA.
    pub fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        let layout = PhotonLayout::for_tx_len(template.len())?;
        // The midstate covers job-fixed bytes only, and C3 overwrites R||s
        // inside the transaction.
        if layout.nonce_offset() < MIDSTATE_BYTES
            || layout.signature_offset() + SIGNATURE_BYTES > template.len()
        {
            return Err("PHOTON layout does not fit the CUDA midstate and signature split".into());
        }
        let target_offset = layout.target_offset();
        if template[target_offset..target_offset + 32] != target[..] {
            return Err(format!(
                "PHOTON target must match transaction template bytes {target_offset}..{}",
                target_offset + 31
            ));
        }
        self.job_ready = false;
        let secret = SecretKey::from_secret_bytes(*private_key)
            .map_err(|error| format!("invalid PHOTON signing key: {error}"))?;
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let fixed_d = fixed_d_table(private_key);

        self.stream
            .memcpy_htod(target, &mut self.target_gpu)
            .map_err(|error| format!("upload target: {error}"))?;
        self.stream
            .memcpy_htod(private_key, &mut self.private_key_gpu)
            .map_err(|error| format!("upload private key: {error}"))?;
        self.stream
            .memcpy_htod(&public_key, &mut self.public_key_gpu)
            .map_err(|error| format!("upload public key: {error}"))?;
        self.stream
            .memcpy_htod(&fixed_d, &mut self.fixed_d_gpu)
            .map_err(|error| format!("upload fixed-d table: {error}"))?;
        let mut padded = [0u8; MAX_TX_BYTES];
        padded[..template.len()].copy_from_slice(template);
        self.stream
            .memcpy_htod(&padded, &mut self.template_gpu)
            .map_err(|error| format!("upload transaction template: {error}"))?;
        self.stream
            .memcpy_htod(&transaction_midstate(template), &mut self.midstate_gpu)
            .map_err(|error| format!("upload transaction midstate: {error}"))?;
        self.layout = layout;
        self.commitment_nonce = u32::from_le_bytes(
            template[layout.nonce_offset()..layout.nonce_offset() + 4]
                .try_into()
                .unwrap(),
        );
        if let Some(mut incremental) = self.incremental.take() {
            let result = incremental.set_message(self, target, self.commitment_nonce);
            self.incremental = Some(incremental);
            result?;
        }
        self.job_ready = true;
        Ok(())
    }

    /// Searches a bounded batch with the persistent CUDA pipeline.
    pub fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if let Some(mut incremental) = self.incremental.take() {
            let result = incremental.batch(self, nonce_base, candidate_count);
            self.incremental = Some(incremental);
            return result.map(|mut batch| {
                for winner in &mut batch.winners {
                    winner.schnorr_k = Some(u64::from(winner.nonce) + 1);
                    winner.nonce = self.commitment_nonce;
                }
                batch
            });
        }
        if !self.job_ready {
            return Err("PHOTON CUDA job is not configured".into());
        }
        if candidate_count == 0 {
            return Ok(PhotonCudaBatchResult {
                candidates: 0,
                total_winners: 0,
                winners: Vec::new(),
            });
        }
        if candidate_count > self.max_candidates {
            return Err(format!(
                "PHOTON CUDA batch {candidate_count} exceeds persistent capacity {}",
                self.max_candidates
            ));
        }

        self.stream
            .memset_zeros(&mut self.winner_count_gpu)
            .map_err(|error| format!("reset winner count: {error}"))?;

        let stage_a_cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(128), 1, 1),
            block_dim: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut stage_a = self.stream.launch_builder(&self.stage_a);
        unsafe {
            stage_a
                .arg(&nonce_base)
                .arg(&self.target_gpu)
                .arg(&self.private_key_gpu)
                .arg(&mut self.message_hashes_gpu)
                .arg(&mut self.rfc6979_gpu)
                .arg(&candidate_count);
            stage_a
                .launch(stage_a_cfg)
                .map_err(|error| format!("launch PHOTON Stage A: {error}"))?;
        }

        let stage_b_cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(64), 1, 1),
            block_dim: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        for (part, function) in self.stage_b.iter().enumerate() {
            let mut builder = self.stream.launch_builder(function);
            unsafe {
                builder
                    .arg(&self.rfc6979_gpu)
                    .arg(&self.table_gpu)
                    .arg(&mut self.points_gpu)
                    .arg(&candidate_count);
                builder
                    .launch(stage_b_cfg)
                    .map_err(|error| format!("launch PHOTON Stage B part {part}: {error}"))?;
            }
        }

        self.finish_batch(nonce_base, candidate_count, C1_CANDIDATES_PER_THREAD)
    }

    fn finish_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
        c1_per_thread: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        let c1_cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(c1_per_thread).div_ceil(64), 1, 1),
            block_dim: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut c1 = self.stream.launch_builder(&self.stage_c1);
        unsafe {
            c1.arg(&self.message_hashes_gpu)
                .arg(&self.rfc6979_gpu)
                .arg(&self.points_gpu)
                .arg(&self.public_key_gpu)
                .arg(&self.fixed_d_gpu)
                .arg(&mut self.signatures_gpu)
                .arg(&mut self.negated_nonce_s_gpu)
                .arg(&candidate_count)
                .arg(&c1_per_thread);
            c1.launch(c1_cfg)
                .map_err(|error| format!("launch PHOTON Stage C1: {error}"))?;
        }

        let c3_cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(128), 1, 1),
            block_dim: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut c3 = self
            .stream
            .launch_builder(&self.stage_c3[self.layout.shift()]);
        unsafe {
            c3.arg(&self.template_gpu)
                .arg(&self.midstate_gpu)
                .arg(&self.signatures_gpu)
                .arg(&self.negated_nonce_s_gpu)
                .arg(&self.points_gpu)
                .arg(&nonce_base)
                .arg(&self.target_gpu)
                .arg(&candidate_count)
                .arg(&self.winner_cap)
                .arg(&mut self.winner_count_gpu)
                .arg(&mut self.winner_nonces_gpu)
                .arg(&mut self.winner_hashes_gpu);
            c3.launch(c3_cfg)
                .map_err(|error| format!("launch PHOTON Stage C2/C3: {error}"))?;
        }

        let count: Vec<u32> = self
            .stream
            .clone_dtoh(&self.winner_count_gpu)
            .map_err(|error| format!("read winner count: {error}"))?;
        let total_winners = count.first().copied().unwrap_or(0);
        let returned = total_winners.min(self.winner_cap) as usize;
        let mut winners = Vec::with_capacity(returned);
        if returned != 0 {
            let nonces: Vec<u32> = self
                .stream
                .clone_dtoh(&self.winner_nonces_gpu)
                .map_err(|error| format!("read winner nonces: {error}"))?;
            let hashes: Vec<u8> = self
                .stream
                .clone_dtoh(&self.winner_hashes_gpu)
                .map_err(|error| format!("read winner hashes: {error}"))?;
            for index in 0..returned {
                let mut digest = [0u8; 32];
                digest.copy_from_slice(&hashes[index * 32..(index + 1) * 32]);
                winners.push(PhotonCudaWinner {
                    nonce: nonces[index],
                    digest,
                    schnorr_k: None,
                });
            }
        }

        Ok(PhotonCudaBatchResult {
            candidates: candidate_count,
            total_winners,
            winners,
        })
    }
}

#[path = "cuda_incremental.rs"]
mod incremental;

#[cfg(test)]
#[path = "incremental_k.rs"]
mod incremental_k;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crypto, search, tx};

    /// Length of the age 0..=16 reference vector.
    const TX_BYTES: usize = 615;

    #[test]
    fn cuda_source_tree_contains_no_placeholder_kernel() {
        let cuda_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cuda");
        let placeholders = std::fs::read_dir(&cuda_dir)
            .unwrap_or_else(|error| {
                panic!("read CUDA source directory {}: {error}", cuda_dir.display())
            })
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.to_ascii_lowercase().contains("placeholder"))
            .collect::<Vec<_>>();

        assert!(
            placeholders.is_empty(),
            "production CUDA source tree must not contain placeholder kernels: {placeholders:?}"
        );
    }

    #[test]
    fn transaction_midstate_resumes_to_the_full_transaction_sha256() {
        use sha2::{Digest, Sha256};

        let raw = hex::decode(include_str!("../reference/photon_vector_tx.hex").trim()).unwrap();
        let template: [u8; TX_BYTES] = raw.try_into().unwrap();
        let mut state = transaction_midstate(&template);

        let mut tail = template[MIDSTATE_BYTES..].to_vec();
        tail.push(0x80);
        while (MIDSTATE_BYTES + tail.len()) % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&((TX_BYTES as u64) * 8).to_be_bytes());
        let blocks = tail
            .as_chunks::<64>()
            .0
            .iter()
            .map(|block| *sha2::digest::generic_array::GenericArray::from_slice(block))
            .collect::<Vec<_>>();
        sha2::compress256(&mut state, &blocks);
        let resumed = state
            .iter()
            .flat_map(|word| word.to_be_bytes())
            .collect::<Vec<_>>();

        assert_eq!(resumed, Sha256::digest(template).to_vec());
        // The midstate stops on a block boundary before the nonce at 390.
        const { assert!(MIDSTATE_BYTES <= 390 && MIDSTATE_BYTES.is_multiple_of(64)) };
    }

    #[test]
    fn release_layout_finds_ptx_beside_the_executable_before_the_manifest() {
        let root = std::env::temp_dir().join(format!("pickaxe-ptx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let executable_dir = root.join("exe");
        let manifest_dir = root.join("manifest");
        let executable_ptx = executable_dir.join("cuda").join("build");
        let manifest_ptx = manifest_dir.join("cuda").join("build");
        std::fs::create_dir_all(&executable_ptx).unwrap();
        std::fs::create_dir_all(&manifest_ptx).unwrap();
        std::fs::write(executable_ptx.join("stage_a_rfc6979.ptx"), b"beside-exe").unwrap();
        std::fs::write(manifest_ptx.join("stage_a_rfc6979.ptx"), b"manifest").unwrap();

        let found =
            resolve_ptx_path_in("stage_a_rfc6979.ptx", Some(&executable_dir), &manifest_dir)
                .unwrap();
        assert_eq!(std::fs::read_to_string(&found).unwrap(), "beside-exe");

        let missing =
            resolve_ptx_path_in("photon_stage_b16.ptx", Some(&executable_dir), &manifest_dir)
                .expect_err("missing ptx");
        assert!(missing.contains("photon_stage_b16.ptx"));
        assert!(missing.contains(&executable_ptx.display().to_string()));
        assert!(missing.contains(&manifest_ptx.display().to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    fn should_skip_cuda_error(error: &str) -> bool {
        crate::cuda_photon::cuda_unavailable_for_tests(error)
    }

    fn reference_template_with_target(target: [u8; 32]) -> [u8; TX_BYTES] {
        let raw = hex::decode(include_str!("../reference/photon_vector_tx.hex").trim()).unwrap();
        let mut template: [u8; TX_BYTES] = raw.try_into().unwrap();
        template[390..394].fill(0);
        template[394..426].copy_from_slice(&target);
        template[426..490].fill(0);
        template
    }

    #[test]
    fn fixed_d_table_matches_direct_modular_multiplication() {
        let mut key = [0u8; 32];
        key[31] = 7;
        let table = fixed_d_table(&key);
        let order = BigUint::from_bytes_be(
            &hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
                .unwrap(),
        );
        let d = BigUint::from(7u32);
        let e = BigUint::from_bytes_be(
            &hex::decode("1234567890abcdef00112233445566778899aabbccddeeff1020304050607080")
                .unwrap(),
        );
        let mut sum = BigUint::from(0u32);
        let e_bytes = e.to_bytes_le();
        for byte_position in 0..32usize {
            let digit = e_bytes.get(byte_position).copied().unwrap_or(0) as usize;
            let base = (byte_position * 256 + digit) * 8;
            let mut bytes = Vec::with_capacity(32);
            for limb in 0..8 {
                bytes.extend_from_slice(&table[base + limb].to_le_bytes());
            }
            sum = (sum + BigUint::from_bytes_le(&bytes)) % &order;
        }
        assert_eq!(sum, (e * d) % order);
    }

    #[test]
    fn gpu_full_pipeline_matches_independent_host_reconstruction_if_cuda_present() {
        let target = [0xffu8; 32];
        let template = reference_template_with_target(target);
        let mut private_key = [0u8; 32];
        private_key[31] = 1;
        let nonce = 0x1234_5678;

        let mut engine = match CudaPhotonEngine::new(0, 1, 1) {
            Ok(engine) => engine,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip persistent PHOTON CUDA test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON CUDA init failed: {error}"),
        };
        engine.set_job(&template, &target, &private_key).unwrap();
        let result = engine.search_batch(nonce, 1).unwrap();
        assert_eq!(result.candidates, 1);
        assert_eq!(result.total_winners, 1);
        assert_eq!(result.winners.len(), 1);
        assert!(!result.truncated());

        let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
        let signature = crypto::bch_schnorr_sign(&private_key, &message).unwrap();
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(private_key).unwrap())
                .serialize();
        assert!(crypto::bch_schnorr_verify(&public_key, &message, &signature).unwrap());

        let mut completed = template;
        completed[390..394].copy_from_slice(&nonce.to_le_bytes());
        completed[426..490].copy_from_slice(&signature);
        let expected = search::hash256(&completed);
        assert_eq!(result.winners[0].nonce, nonce);
        assert_eq!(result.winners[0].digest, expected);
        assert!(search::meets_target_le(&expected, &target));
    }

    #[test]
    fn persistent_device_memory_is_bounded_by_configured_capacity_if_cuda_present() {
        let engine = match CudaPhotonEngine::new(0, 256, 8) {
            Ok(engine) => engine,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip PHOTON CUDA bounded-memory test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON CUDA init failed: {error}"),
        };
        let expected = m29_table::M29_G16_BYTES
            + 32
            + 32
            + 33
            + FIXED_D_WORDS * 4
            + 256 * (32 + 32 + POINT_WORDS * 4 + SIGNATURE_BYTES + 32)
            + MAX_TX_BYTES
            + 8 * 4
            + 4
            + 8 * (4 + 32);
        assert_eq!(engine.persistent_device_bytes(), expected);
    }

    #[test]
    fn gpu_batch_winner_readback_is_bounded_and_reconstructable_if_cuda_present() {
        let target = [0xffu8; 32];
        let template = reference_template_with_target(target);
        let mut private_key = [0u8; 32];
        private_key[31] = 1;
        let nonce_base = 0x3456_0000;
        let candidate_count = 256;
        let winner_cap = 4;

        let mut engine = match CudaPhotonEngine::new(0, candidate_count, winner_cap) {
            Ok(engine) => engine,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip PHOTON CUDA bounded-batch test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON CUDA init failed: {error}"),
        };
        engine.set_job(&template, &target, &private_key).unwrap();

        let started = std::time::Instant::now();
        let result = engine.search_batch(nonce_base, candidate_count).unwrap();
        let elapsed = started.elapsed();
        let candidates_per_second = candidate_count as f64 / elapsed.as_secs_f64();
        eprintln!(
            "PHOTON CUDA steady-state batch: {candidate_count} candidates in {:.6}s = {:.2} candidates/s; persistent_device_bytes={}",
            elapsed.as_secs_f64(),
            candidates_per_second,
            engine.persistent_device_bytes()
        );

        assert_eq!(result.candidates, candidate_count);
        assert_eq!(result.total_winners, candidate_count);
        assert_eq!(result.winners.len(), winner_cap as usize);
        assert!(result.truncated());

        for winner in &result.winners {
            assert!(winner.nonce >= nonce_base);
            assert!(winner.nonce < nonce_base + candidate_count);
            let message = tx::photon_message_sha256(winner.nonce, &hex::encode(target)).unwrap();
            let signature = crypto::bch_schnorr_sign(&private_key, &message).unwrap();
            let mut completed = template;
            completed[390..394].copy_from_slice(&winner.nonce.to_le_bytes());
            completed[426..490].copy_from_slice(&signature);
            assert_eq!(winner.digest, search::hash256(&completed));
        }
    }

    /// Host HASH256 of the completed transaction for the real BCH Schnorr
    /// signature and for the same R with the other nonce sign.
    fn real_and_other_sign_hashes(
        template: &[u8; TX_BYTES],
        target: &[u8; 32],
        private_key: &[u8; 32],
        nonce: u32,
    ) -> ([u8; 32], [u8; 32]) {
        use sha2::{Digest, Sha256};

        let order = BigUint::from_bytes_be(
            &hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
                .unwrap(),
        );
        let secret = SecretKey::from_secret_bytes(*private_key).unwrap();
        let public_key = PublicKey::from_secret_key(&secret).serialize();
        let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
        let signature = crypto::bch_schnorr_sign(private_key, &message).unwrap();

        let mut challenge = signature[..32].to_vec();
        challenge.extend_from_slice(&public_key);
        challenge.extend_from_slice(&message);
        let e = BigUint::from_bytes_be(&Sha256::digest(&challenge)) % &order;
        let d = BigUint::from_bytes_be(private_key);
        let s = BigUint::from_bytes_be(&signature[32..]);
        // s_real + s_other = n + 2*e*d (mod n).
        let other = (&e * &d * 2u32 + &order - &s) % &order;
        let other_bytes = other.to_bytes_be();
        let mut other_signature = signature;
        other_signature[32..].fill(0);
        other_signature[64 - other_bytes.len()..].copy_from_slice(&other_bytes);

        let completed = |signature: &[u8; 64]| {
            let mut completed = *template;
            completed[390..394].copy_from_slice(&nonce.to_le_bytes());
            completed[426..490].copy_from_slice(signature);
            search::hash256(&completed)
        };
        (completed(&signature), completed(&other_signature))
    }

    #[test]
    fn gpu_dual_filter_reports_every_real_signature_hash_if_cuda_present() {
        let target = [0xffu8; 32];
        let template = reference_template_with_target(target);
        let mut private_key = [0u8; 32];
        private_key[31] = 1;
        let nonce_base = 0x2718_0000;
        // Not a multiple of the C1 per-thread count or block size, so the last
        // thread and block are partial.
        let candidate_count = 1_000;

        let mut engine = match CudaPhotonEngine::new(0, candidate_count, candidate_count) {
            Ok(engine) => engine,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip PHOTON CUDA dual readback test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON CUDA init failed: {error}"),
        };
        engine.set_job(&template, &target, &private_key).unwrap();
        let result = engine.search_batch(nonce_base, candidate_count).unwrap();
        assert_eq!(result.total_winners, candidate_count);
        assert_eq!(result.winners.len(), candidate_count as usize);

        // Every candidate passes, so about half of them need the n - k
        // signature; each reported hash must be the real one.
        let mut seen = std::collections::BTreeSet::new();
        for winner in &result.winners {
            let (real, other) =
                real_and_other_sign_hashes(&template, &target, &private_key, winner.nonce);
            assert_eq!(winner.digest, real, "nonce {:#x}", winner.nonce);
            assert_ne!(winner.digest, other);
            assert!(seen.insert(winner.nonce));
        }
    }

    #[test]
    fn gpu_dual_filter_emits_only_the_real_signature_variant_if_cuda_present() {
        // Half of the hash space: the covenant ignores digest bit 255, so a
        // hash meets it when its top byte masked to 0x7f is < 0x40.
        let mut target = [0u8; 32];
        target[31] = 0x40;
        let template = reference_template_with_target(target);
        let mut private_key = [0u8; 32];
        private_key[31] = 7;

        let mut engine = match CudaPhotonEngine::new(0, 1, 2) {
            Ok(engine) => engine,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip PHOTON CUDA dual selection test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON CUDA init failed: {error}"),
        };
        engine.set_job(&template, &target, &private_key).unwrap();

        for (real_meets, other_meets) in
            [(false, true), (true, false), (true, true), (false, false)]
        {
            let nonce = (0x5000_0000u32..)
                .find(|&nonce| {
                    let (real, other) =
                        real_and_other_sign_hashes(&template, &target, &private_key, nonce);
                    search::meets_target_le(&real, &target) == real_meets
                        && search::meets_target_le(&other, &target) == other_meets
                })
                .unwrap();
            let (real, _) = real_and_other_sign_hashes(&template, &target, &private_key, nonce);
            let result = engine.search_batch(nonce, 1).unwrap();
            if real_meets {
                assert_eq!(result.total_winners, 1, "real variant missed at {nonce:#x}");
                assert_eq!(result.winners[0].nonce, nonce);
                assert_eq!(result.winners[0].digest, real);
            } else {
                assert_eq!(
                    result.total_winners, 0,
                    "other-sign variant reported at {nonce:#x} (other meets: {other_meets})"
                );
            }
        }
    }

    #[test]
    fn gpu_pipeline_hashes_every_age_layout_if_cuda_present() {
        let payout = hex::decode("76a9146e0810ceea13412b73feb41566a3d2d0ce54e10188ac").unwrap();
        let mut private_key = [0u8; 32];
        private_key[31] = 3;
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(private_key).unwrap())
                .serialize();
        // About one candidate in four meets this target, so both outcomes
        // are exercised for every layout.
        let mut target = [0u8; 32];
        target[31] = 0x20;
        let candidate_count = 512;

        let mut engine = match CudaPhotonEngine::new(0, candidate_count, candidate_count) {
            Ok(engine) => engine,
            Err(error) if should_skip_cuda_error(&error) => {
                eprintln!("skip PHOTON CUDA age layout test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON CUDA init failed: {error}"),
        };
        for age in [16u32, 17, 128, 40_000] {
            let layout = PhotonLayout::for_age(age).unwrap();
            let template = tx::build_photon_template_bytes(&tx::TemplateParams {
                prev_tx_hash_hex:
                    "42a02ec4f58b50f23df4591dcc999ca1bcae2f378997fe6547ae124712000000".into(),
                prev_index: 0,
                age,
                public_key_hex: hex::encode(public_key),
                target_hex: hex::encode(target),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 15_971_500,
                contract_token_amount: 2_099_905_002_035_715,
                reward_amount: 4_999_773_813,
                payout_locking: payout.clone(),
            })
            .unwrap();
            assert_eq!(template.len(), layout.tx_bytes());
            engine.set_job(&template, &target, &private_key).unwrap();
            let nonce_base = 0x7700_0000 + age;
            let result = engine.search_batch(nonce_base, candidate_count).unwrap();

            let mut expected = std::collections::BTreeMap::new();
            for nonce in nonce_base..nonce_base + candidate_count {
                let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
                let signature = crypto::bch_schnorr_sign(&private_key, &message).unwrap();
                let mut completed = template.clone();
                let n = layout.nonce_offset();
                completed[n..n + 4].copy_from_slice(&nonce.to_le_bytes());
                let s = layout.signature_offset();
                completed[s..s + 64].copy_from_slice(&signature);
                let digest = search::hash256(&completed);
                if search::meets_target_le(&digest, &target) {
                    expected.insert(nonce, digest);
                }
            }
            let found = result
                .winners
                .iter()
                .map(|winner| (winner.nonce, winner.digest))
                .collect::<std::collections::BTreeMap<_, _>>();
            assert!(!expected.is_empty(), "age {age}: no host winners");
            assert_eq!(result.total_winners as usize, expected.len(), "age {age}");
            assert_eq!(found, expected, "age {age}");
        }
    }
}
