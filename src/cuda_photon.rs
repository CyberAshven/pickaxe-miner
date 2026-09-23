//! Persistent reference-correct PHOTON CUDA pipeline.
//!
//! One context owns the complete candidate path:
//! Stage A (message SHA-256 + RFC6979) -> M45-style split M29 fixed-base Stage B
//! -> BCH Schnorr C1 -> completed transaction HASH256/strict target filter.
//! Candidate intermediates remain in device memory. The host reads one winner
//! count and a bounded winner record array only.

use crate::m29_table::{self, M29TableSource};
use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, LaunchConfig, PushKernelArg,
};
use cudarc::nvrtc::Ptx;
use num_bigint::BigUint;
use secp256k1::{PublicKey, SecretKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const TX_BYTES: usize = 615;
const TARGET_OFFSET: usize = 394;
const SIGNATURE_BYTES: usize = 64;
const POINT_WORDS: usize = 24;
const FIXED_D_WORDS: usize = 32 * 256 * 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhotonCudaWinner {
    pub nonce: u32,
    pub digest: [u8; 32],
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
    stage_c3: CudaFunction,
    table_gpu: CudaSlice<u8>,
    target_gpu: CudaSlice<u8>,
    private_key_gpu: CudaSlice<u8>,
    public_key_gpu: CudaSlice<u8>,
    fixed_d_gpu: CudaSlice<u32>,
    message_hashes_gpu: CudaSlice<u8>,
    rfc6979_gpu: CudaSlice<u8>,
    points_gpu: CudaSlice<u32>,
    signatures_gpu: CudaSlice<u8>,
    template_gpu: CudaSlice<u8>,
    winner_count_gpu: CudaSlice<u32>,
    winner_nonces_gpu: CudaSlice<u32>,
    winner_hashes_gpu: CudaSlice<u8>,
    max_candidates: u32,
    winner_cap: u32,
    table_source: M29TableSource,
    job_ready: bool,
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
        let stage_c1 = load_function(&ctx, "photon_c1_schnorr.ptx", "pickaxe_photon_c1_schnorr")?;
        let stage_c3 = load_function(&ctx, "stage_c_hash.ptx", "pickaxe_stage_c_hash_filter")?;

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
        let template_gpu = stream
            .alloc_zeros::<u8>(TX_BYTES)
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
            table_gpu,
            target_gpu,
            private_key_gpu,
            public_key_gpu,
            fixed_d_gpu,
            message_hashes_gpu,
            rfc6979_gpu,
            points_gpu,
            signatures_gpu,
            template_gpu,
            winner_count_gpu,
            winner_nonces_gpu,
            winner_hashes_gpu,
            max_candidates,
            winner_cap,
            table_source,
            job_ready: false,
        })
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
            + (self.max_candidates as usize) * (32 + 32 + POINT_WORDS * 4 + SIGNATURE_BYTES)
            + TX_BYTES
            + std::mem::size_of::<u32>()
            + (self.winner_cap as usize) * (4 + 32)
    }

    /// Uploads validated PHOTON job bytes and target to CUDA.
    pub fn set_job(
        &mut self,
        template: &[u8; TX_BYTES],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        if template[TARGET_OFFSET..TARGET_OFFSET + 32] != target[..] {
            return Err("PHOTON target must match transaction template bytes 394..425".into());
        }
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
        self.stream
            .memcpy_htod(template, &mut self.template_gpu)
            .map_err(|error| format!("upload transaction template: {error}"))?;
        self.job_ready = true;
        Ok(())
    }

    /// Searches a bounded batch with the persistent CUDA pipeline.
    pub fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
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

        let c1_cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(64), 1, 1),
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
                .arg(&candidate_count);
            c1.launch(c1_cfg)
                .map_err(|error| format!("launch PHOTON Stage C1: {error}"))?;
        }

        let c3_cfg = LaunchConfig {
            grid_dim: (candidate_count.div_ceil(128), 1, 1),
            block_dim: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut c3 = self.stream.launch_builder(&self.stage_c3);
        unsafe {
            c3.arg(&self.template_gpu)
                .arg(&self.signatures_gpu)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crypto, search, tx};

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
        let lower = error.to_lowercase();
        lower.contains("cuda")
            || lower.contains("ptx")
            || lower.contains("no device")
            || lower.contains("not initialized")
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
            + 256 * (32 + 32 + POINT_WORDS * 4 + SIGNATURE_BYTES)
            + TX_BYTES
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
}
