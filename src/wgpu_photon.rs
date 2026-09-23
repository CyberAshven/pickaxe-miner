//! Persistent reference-correct PHOTON WGPU pipeline.
//!
//! This is the M67.38 WebGPU A -> split-B -> normalized-C1 -> C2 pipeline from
//! `reference/photon-miner.wgsl`, with a bounded C3 result tail. Candidate
//! intermediates stay on the GPU; the host reads only counters plus a fixed
//! number of winner nonce/HASH256 records.

use crate::cuda_photon::{PhotonCudaBatchResult, PhotonCudaWinner};
use crate::m29_table::{self, M29TableSource};
use secp256k1::SecretKey;
use sha2::compress256;
use sha2::digest::generic_array::GenericArray;
use std::borrow::Cow;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const TX_BYTES: usize = 615;
const TARGET_OFFSET: usize = 394;
const INPUT_WORDS: usize = 155;
const INPUT_BYTES: usize = INPUT_WORDS * 4;
const BASE_NONCE_OFFSET: u64 = 154 * 4;
const M38_RECORD_BYTES: usize = 160;
const SIGNATURE_BYTES: usize = 64;
const HASH_BYTES: usize = 32;
const RESULT_BYTES: usize = 16;
const CONTROL_BYTES: usize = 16;
const WINNER_RECORD_WORDS: usize = 9;
const WINNER_RECORD_BYTES: usize = WINNER_RECORD_WORDS * 4;
const WGPU_MIN_LADDER_BATCH: u32 = 1_024;
pub(crate) const WGPU_REFERENCE_MAX_BATCH: u32 = 524_288;
const WGPU_TARGET_BATCH: Duration = Duration::from_millis(350);
const WGPU_ESCALATE_BATCH: Duration = Duration::from_millis(175);

const SHA256_IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const BOUNDED_C3_WGSL: &str = r#"
@group(0) @binding(16)
var<storage, read_write> pickaxeWinnerRecords: array<u32>;

@compute
@workgroup_size(64)
/// Provides the bounded workgroup-size PHOTON C3 compute shader.
fn pickaxe_photon_c3_bounded_wg64(
    @builtin(global_invocation_id) gid: vec3<u32>
) {
    let index = gid.x;
    let candidateCount = atomicLoad(&benchmarkOutput.checksum);
    if (index >= candidateCount) {
        return;
    }

    let nonce = input.byteLength + index;
    let hashBase = index * 8u;
    var finalHash: array<u32, 8>;
    for (var i: u32 = 0u; i < 8u; i = i + 1u) {
        finalHash[i] = m6725Hashes[hashBase + i];
    }

    atomicAdd(&benchmarkOutput.completed, 1u);
    if (m10_hash_is_below_target(finalHash)) {
        let slot = atomicAdd(&benchmarkOutput.winners, 1u);
        let winnerCap = arrayLength(&pickaxeWinnerRecords) / 9u;
        if (slot < winnerCap) {
            let recordBase = slot * 9u;
            pickaxeWinnerRecords[recordBase] = nonce;
            for (var i: u32 = 0u; i < 8u; i = i + 1u) {
                pickaxeWinnerRecords[recordBase + 1u + i] = finalHash[i];
            }
        }
    }
}
"#;

/// Constructs the reference WGSL shader for portable GPU search.
fn reference_shader_source_for_wgpu() -> Result<String, String> {
    let mut source = include_str!("../reference/photon-miner.wgsl").replace("\r\n", "\n");

    // Browser WGSL validators accept the reference's infinite binary-GCD loop
    // as total because every terminating path returns. Naga requires a
    // syntactic return after the loop. Add an unreachable fallback to the
    // in-memory WGPU copy only; the authoritative reference file stays exact.
    let function_start = source
        .find("fn field_inv_binary(")
        .ok_or_else(|| "authoritative WGSL is missing field_inv_binary".to_string())?;
    let function_close = source[function_start..]
        .find("\n}\n\n\n@compute")
        .map(|offset| function_start + offset)
        .ok_or_else(|| "could not locate field_inv_binary closing brace".to_string())?;
    source.insert_str(function_close, "\n    return x1;");
    source.push('\n');
    source.push_str(BOUNDED_C3_WGSL);
    Ok(source)
}

/// Rejects non-hardware WGPU adapters for mining.
fn is_hardware_adapter(info: &wgpu::AdapterInfo) -> bool {
    matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu
            | wgpu::DeviceType::IntegratedGpu
            | wgpu::DeviceType::VirtualGpu
    )
}

/// Computes candidate capacity from a GPU storage-buffer limit.
fn storage_candidates(max_candidates: u32) -> u32 {
    max_candidates.div_ceil(128) * 128
}

/// Finds the largest power of two not exceeding the input.
fn largest_power_of_two_at_most(value: u32) -> u32 {
    1u32 << (31 - value.leading_zeros())
}

/// Caps candidate capacity by available GPU buffer limits.
fn device_limited_wgpu_max_candidates(
    requested: u32,
    max_storage_buffer_binding_size: u64,
    max_buffer_size: u64,
) -> u32 {
    let requested = requested.min(WGPU_REFERENCE_MAX_BATCH);
    let byte_limit = max_storage_buffer_binding_size.min(max_buffer_size);
    let device_capacity = (byte_limit / M38_RECORD_BYTES as u64).min(u64::from(u32::MAX)) as u32;
    let bounded = requested.min(device_capacity);

    if requested < WGPU_MIN_LADDER_BATCH {
        return bounded;
    }
    if bounded < WGPU_MIN_LADDER_BATCH {
        return 0;
    }
    largest_power_of_two_at_most(bounded)
}

/// Selects an initial bounded WGPU batch size.
fn initial_wgpu_batch_size(max_candidates: u32) -> u32 {
    max_candidates.clamp(1, WGPU_MIN_LADDER_BATCH)
}

/// Adapts batch size to the previous GPU execution duration.
fn next_wgpu_batch_size(current: u32, elapsed: Duration, max_candidates: u32) -> u32 {
    let minimum = initial_wgpu_batch_size(max_candidates);
    if elapsed <= WGPU_ESCALATE_BATCH && current < max_candidates {
        current.saturating_mul(2).min(max_candidates)
    } else if elapsed > WGPU_TARGET_BATCH && current > minimum {
        (current / 2).max(minimum)
    } else {
        current.clamp(minimum, max_candidates)
    }
}

/// Packs message bytes into big-endian 32-bit shader words.
fn pack_big_endian_words(bytes: &[u8], word_count: usize) -> Vec<u8> {
    let mut packed = vec![0u8; word_count * 4];
    for word_index in 0..word_count {
        let byte_index = word_index * 4;
        let mut word_bytes = [0u8; 4];
        let available = bytes.len().saturating_sub(byte_index).min(4);
        if available != 0 {
            word_bytes[..available].copy_from_slice(&bytes[byte_index..byte_index + available]);
        }
        let word = u32::from_be_bytes(word_bytes);
        packed[byte_index..byte_index + 4].copy_from_slice(&word.to_le_bytes());
    }
    packed
}

/// Serializes shader words as little-endian bytes.
fn u32_words_to_le_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}

/// Computes SHA-256 compression for one message block.
fn compress_block(state: &mut [u32; 8], block: &[u8; 64]) {
    let block = GenericArray::clone_from_slice(block);
    compress256(state, std::slice::from_ref(&block));
}

/// Precomputes the PHOTON M27 message words.
fn m27_precomputed_words(private_key: &[u8; 32]) -> [u32; 16] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    let mut inner = SHA256_IV;
    let mut outer = SHA256_IV;
    compress_block(&mut inner, &ipad);
    compress_block(&mut outer, &opad);

    let mut first_message_block = [0u8; 64];
    first_message_block[..32].fill(0x01);
    first_message_block[32] = 0x00;
    first_message_block[33..64].copy_from_slice(&private_key[..31]);
    compress_block(&mut inner, &first_message_block);

    let mut out = [0u32; 16];
    out[..8].copy_from_slice(&inner);
    out[8..].copy_from_slice(&outer);

    // Keep the source blocks immutable in spirit and make accidental future
    // key-dependent edits obvious to the optimizer/lints.
    ipad.fill(0);
    opad.fill(0);
    out
}

/// Builds the reusable PHOTON M30 message prefix.
fn m30_prefix_words(template: &[u8; TX_BYTES]) -> [u32; 8] {
    let mut state = SHA256_IV;
    for block in template[..384].as_chunks::<64>().0 {
        compress_block(&mut state, block);
    }
    state
}

/// Checks GPU job material against authoritative PHOTON fields.
fn validate_job_material(
    template: &[u8; TX_BYTES],
    target: &[u8; 32],
    private_key: &[u8; 32],
) -> Result<(), String> {
    if template[TARGET_OFFSET..TARGET_OFFSET + 32] != target[..] {
        return Err("PHOTON WGPU target must match transaction template bytes 394..425".into());
    }
    SecretKey::from_secret_bytes(*private_key)
        .map_err(|error| format!("invalid PHOTON WGPU signing key: {error}"))?;
    Ok(())
}

/// Allocates a WGPU buffer with the required usage flags.
fn create_buffer(
    device: &wgpu::Device,
    label: &'static str,
    size: usize,
    usage: wgpu::BufferUsages,
    mapped_at_creation: bool,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size as u64,
        usage,
        mapped_at_creation,
    })
}

/// Compiles a WGPU compute pipeline for a PHOTON shader.
fn create_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    entry_point: &'static str,
) -> wgpu::ComputePipeline {
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry_point),
        layout: None,
        module: shader,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}

/// Binds WGPU job and result buffers for compute dispatch.
fn create_bind_group(
    device: &wgpu::Device,
    pipeline: &wgpu::ComputePipeline,
    label: &'static str,
    buffers: &[(u32, &wgpu::Buffer)],
) -> wgpu::BindGroup {
    let layout = pipeline.get_bind_group_layout(0);
    let entries = buffers
        .iter()
        .map(|(binding, buffer)| wgpu::BindGroupEntry {
            binding: *binding,
            resource: buffer.as_entire_binding(),
        })
        .collect::<Vec<_>>();
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &layout,
        entries: &entries,
    })
}

pub struct WgpuPhotonEngine {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    stage_a: wgpu::ComputePipeline,
    stage_b: [wgpu::ComputePipeline; 4],
    stage_c1: wgpu::ComputePipeline,
    stage_c2: wgpu::ComputePipeline,
    stage_c3: wgpu::ComputePipeline,
    bind_a: wgpu::BindGroup,
    bind_b: [wgpu::BindGroup; 4],
    bind_c1: wgpu::BindGroup,
    bind_c2: wgpu::BindGroup,
    bind_c3: wgpu::BindGroup,
    _table_gpu: wgpu::Buffer,
    input_gpu: wgpu::Buffer,
    private_key_gpu: wgpu::Buffer,
    m27_gpu: wgpu::Buffer,
    m30_gpu: wgpu::Buffer,
    _intermediate_gpu: wgpu::Buffer,
    dispatch_params_gpu: wgpu::Buffer,
    _signatures_gpu: wgpu::Buffer,
    _hashes_gpu: wgpu::Buffer,
    result_gpu: wgpu::Buffer,
    winner_records_gpu: wgpu::Buffer,
    readback_gpu: wgpu::Buffer,
    max_candidates: u32,
    storage_candidates: u32,
    winner_cap: u32,
    readback_bytes: usize,
    table_source: M29TableSource,
    recommended_candidates: u32,
    _adapter_name: String,
    job_ready: bool,
}

impl WgpuPhotonEngine {
    /// Creates a WgpuPhotonEngine for the portable GPU pipeline.
    pub fn new(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
    ) -> Result<Self, String> {
        if max_candidates == 0 {
            return Err("PHOTON WGPU max_candidates must be greater than zero".into());
        }
        if winner_cap == 0 {
            return Err("PHOTON WGPU winner_cap must be greater than zero".into());
        }

        let backends = crate::backend::production_wgpu_backends();
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = backends;
        let instance = wgpu::Instance::new(instance_descriptor);
        let adapters = pollster::block_on(instance.enumerate_adapters(backends));
        let adapter = adapters
            .into_iter()
            .filter(|adapter| is_hardware_adapter(&adapter.get_info()))
            .nth(device_ordinal)
            .ok_or_else(|| {
                format!("no hardware WGPU adapter at backend-local ordinal {device_ordinal}")
            })?;
        let adapter_info = adapter.get_info();
        let limits = adapter.limits();
        if (limits.max_storage_buffer_binding_size as usize) < m29_table::M29_G16_BYTES {
            return Err(format!(
                "WGPU adapter {} max storage binding {} bytes is below the 64 MiB PHOTON table requirement {}",
                adapter_info.name,
                limits.max_storage_buffer_binding_size,
                m29_table::M29_G16_BYTES
            ));
        }
        if (limits.max_buffer_size as usize) < m29_table::M29_G16_BYTES {
            return Err(format!(
                "WGPU adapter {} max buffer {} bytes is below the 64 MiB PHOTON table requirement {}",
                adapter_info.name,
                limits.max_buffer_size,
                m29_table::M29_G16_BYTES
            ));
        }
        let max_candidates = device_limited_wgpu_max_candidates(
            max_candidates,
            limits.max_storage_buffer_binding_size,
            limits.max_buffer_size,
        );
        if max_candidates == 0 {
            return Err(format!(
                "WGPU adapter {} cannot allocate the minimum {}-candidate PHOTON batch within its storage-buffer limits",
                adapter_info.name, WGPU_MIN_LADDER_BATCH
            ));
        }

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("Pickaxe PHOTON WGPU device"),
            required_limits: limits,
            ..Default::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(|error| format!("request WGPU device {}: {error}", adapter_info.name))?;

        let shader_source = reference_shader_source_for_wgpu()?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Pickaxe M67.38 PHOTON reference shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Owned(shader_source)),
        });
        let stage_a = create_pipeline(&device, &shader, "photon_m38_stage_a_wg128");
        let stage_b = [
            create_pipeline(&device, &shader, "photon_m45_b4a_wg32"),
            create_pipeline(&device, &shader, "photon_m45_b4b_wg32"),
            create_pipeline(&device, &shader, "photon_m45_b4c_wg32"),
            create_pipeline(&device, &shader, "photon_m45_b4d_wg32"),
        ];
        let stage_c1 = create_pipeline(&device, &shader, "photon_m6729_c1_znorm_wg64");
        let stage_c2 = create_pipeline(&device, &shader, "photon_m6725_c2_hash_only_wg64");
        let stage_c3 = create_pipeline(&device, &shader, "pickaxe_photon_c3_bounded_wg64");

        let (table_bytes, table_source) = m29_table::load_or_generate_m29_g16()?;
        let table_gpu = create_buffer(
            &device,
            "PHOTON M29 16-bit generator table",
            m29_table::M29_G16_BYTES,
            wgpu::BufferUsages::STORAGE,
            true,
        );
        {
            let mut mapped = table_gpu
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|error| format!("map WGPU M29 table at creation: {error}"))?;
            mapped.copy_from_slice(&table_bytes);
        }
        table_gpu.unmap();
        drop(table_bytes);

        let storage_candidates = storage_candidates(max_candidates);
        let input_gpu = create_buffer(
            &device,
            "PHOTON shared input",
            INPUT_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let private_key_gpu = create_buffer(
            &device,
            "PHOTON private key",
            32,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let m27_gpu = create_buffer(
            &device,
            "PHOTON M27 RFC6979 precompute",
            64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let m30_gpu = create_buffer(
            &device,
            "PHOTON M30 transaction prefix",
            32,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let intermediate_gpu = create_buffer(
            &device,
            "PHOTON M38 intermediate records",
            storage_candidates as usize * M38_RECORD_BYTES,
            wgpu::BufferUsages::STORAGE,
            false,
        );
        let dispatch_params_gpu = create_buffer(
            &device,
            "PHOTON M45 dispatch params",
            CONTROL_BYTES,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let signatures_gpu = create_buffer(
            &device,
            "PHOTON BCH Schnorr signatures",
            storage_candidates as usize * SIGNATURE_BYTES,
            wgpu::BufferUsages::STORAGE,
            false,
        );
        let hashes_gpu = create_buffer(
            &device,
            "PHOTON HASH256 candidates",
            storage_candidates as usize * HASH_BYTES,
            wgpu::BufferUsages::STORAGE,
            false,
        );
        let result_gpu = create_buffer(
            &device,
            "PHOTON result counters",
            RESULT_BYTES,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            false,
        );
        let winner_records_gpu = create_buffer(
            &device,
            "PHOTON bounded winner records",
            winner_cap as usize * WINNER_RECORD_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            false,
        );
        let readback_bytes = RESULT_BYTES + winner_cap as usize * WINNER_RECORD_BYTES;
        let readback_gpu = create_buffer(
            &device,
            "PHOTON bounded result readback",
            readback_bytes,
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            false,
        );

        let bind_a = create_bind_group(
            &device,
            &stage_a,
            "PHOTON A bind group",
            &[
                (0, &input_gpu),
                (4, &private_key_gpu),
                (5, &m27_gpu),
                (10, &intermediate_gpu),
            ],
        );
        let bind_b = [
            create_bind_group(
                &device,
                &stage_b[0],
                "PHOTON B4a bind group",
                &[
                    (3, &table_gpu),
                    (10, &intermediate_gpu),
                    (12, &dispatch_params_gpu),
                ],
            ),
            create_bind_group(
                &device,
                &stage_b[1],
                "PHOTON B4b bind group",
                &[
                    (3, &table_gpu),
                    (10, &intermediate_gpu),
                    (12, &dispatch_params_gpu),
                ],
            ),
            create_bind_group(
                &device,
                &stage_b[2],
                "PHOTON B4c bind group",
                &[
                    (3, &table_gpu),
                    (10, &intermediate_gpu),
                    (12, &dispatch_params_gpu),
                ],
            ),
            create_bind_group(
                &device,
                &stage_b[3],
                "PHOTON B4d bind group",
                &[
                    (3, &table_gpu),
                    (10, &intermediate_gpu),
                    (12, &dispatch_params_gpu),
                ],
            ),
        ];
        let bind_c1 = create_bind_group(
            &device,
            &stage_c1,
            "PHOTON C1 bind group",
            &[
                (0, &input_gpu),
                (4, &private_key_gpu),
                (10, &intermediate_gpu),
                (13, &signatures_gpu),
            ],
        );
        let bind_c2 = create_bind_group(
            &device,
            &stage_c2,
            "PHOTON C2 bind group",
            &[
                (0, &input_gpu),
                (6, &m30_gpu),
                (13, &signatures_gpu),
                (14, &hashes_gpu),
            ],
        );
        let bind_c3 = create_bind_group(
            &device,
            &stage_c3,
            "PHOTON bounded C3 bind group",
            &[
                (0, &input_gpu),
                (2, &result_gpu),
                (14, &hashes_gpu),
                (16, &winner_records_gpu),
            ],
        );

        Ok(Self {
            instance,
            device,
            queue,
            stage_a,
            stage_b,
            stage_c1,
            stage_c2,
            stage_c3,
            bind_a,
            bind_b,
            bind_c1,
            bind_c2,
            bind_c3,
            _table_gpu: table_gpu,
            input_gpu,
            private_key_gpu,
            m27_gpu,
            m30_gpu,
            _intermediate_gpu: intermediate_gpu,
            dispatch_params_gpu,
            _signatures_gpu: signatures_gpu,
            _hashes_gpu: hashes_gpu,
            result_gpu,
            winner_records_gpu,
            readback_gpu,
            max_candidates,
            storage_candidates,
            winner_cap,
            readback_bytes,
            table_source,
            recommended_candidates: initial_wgpu_batch_size(max_candidates),
            _adapter_name: adapter_info.name,
            job_ready: false,
        })
    }

    /// Returns the source of the portable GPU lookup table.
    pub fn table_source(&self) -> M29TableSource {
        self.table_source
    }

    /// Returns the bytes allocated for persistent WGPU buffers.
    pub fn persistent_device_bytes(&self) -> usize {
        m29_table::M29_G16_BYTES
            + INPUT_BYTES
            + 32
            + 64
            + 32
            + self.storage_candidates as usize * M38_RECORD_BYTES
            + CONTROL_BYTES
            + self.storage_candidates as usize * SIGNATURE_BYTES
            + self.storage_candidates as usize * HASH_BYTES
            + RESULT_BYTES
            + self.winner_cap as usize * WINNER_RECORD_BYTES
            + self.readback_bytes
    }

    /// Suggests a batch size within device and workgroup limits.
    pub fn recommended_batch_candidates(&self) -> u32 {
        self.recommended_candidates
    }

    /// Writes validated PHOTON job data to WGPU buffers.
    pub fn set_job(
        &mut self,
        template: &[u8; TX_BYTES],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        validate_job_material(template, target, private_key)?;

        let mut input = pack_big_endian_words(template, 154);
        input.extend_from_slice(&0u32.to_le_bytes());
        debug_assert_eq!(input.len(), INPUT_BYTES);
        let private_words = pack_big_endian_words(private_key, 8);
        let m27 = u32_words_to_le_bytes(&m27_precomputed_words(private_key));
        let m30 = u32_words_to_le_bytes(&m30_prefix_words(template));

        self.queue.write_buffer(&self.input_gpu, 0, &input);
        self.queue
            .write_buffer(&self.private_key_gpu, 0, &private_words);
        self.queue.write_buffer(&self.m27_gpu, 0, &m27);
        self.queue.write_buffer(&self.m30_gpu, 0, &m30);
        self.job_ready = true;
        Ok(())
    }

    /// Dispatches a bounded portable GPU candidate search.
    pub fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        if !self.job_ready {
            return Err("PHOTON WGPU job is not configured".into());
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
                "PHOTON WGPU batch {candidate_count} exceeds persistent capacity {}",
                self.max_candidates
            ));
        }
        let adapt_batch = candidate_count == self.recommended_candidates;
        let batch_started = Instant::now();

        let active_candidates = candidate_count.div_ceil(128) * 128;
        let groups_a = active_candidates / 128;
        let groups_b = active_candidates / 32;
        let groups_c = active_candidates / 64;
        let dispatch_params = u32_words_to_le_bytes(&[groups_b, active_candidates, 0, 0]);
        let result_init = u32_words_to_le_bytes(&[candidate_count, 0, 0, 0]);
        self.queue.write_buffer(
            &self.input_gpu,
            BASE_NONCE_OFFSET,
            &nonce_base.to_le_bytes(),
        );
        self.queue
            .write_buffer(&self.dispatch_params_gpu, 0, &dispatch_params);
        self.queue.write_buffer(&self.result_gpu, 0, &result_init);

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PHOTON WGPU batch"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PHOTON Stage A"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.stage_a);
            pass.set_bind_group(0, &self.bind_a, &[]);
            pass.dispatch_workgroups(groups_a, 1, 1);
        }
        for (index, (pipeline, bind_group)) in
            self.stage_b.iter().zip(self.bind_b.iter()).enumerate()
        {
            let label = match index {
                0 => "PHOTON Stage B4a",
                1 => "PHOTON Stage B4b",
                2 => "PHOTON Stage B4c",
                _ => "PHOTON Stage B4d",
            };
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(groups_b, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PHOTON Stage C1 normalized Schnorr"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.stage_c1);
            pass.set_bind_group(0, &self.bind_c1, &[]);
            pass.dispatch_workgroups(groups_c, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PHOTON Stage C2 HASH256"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.stage_c2);
            pass.set_bind_group(0, &self.bind_c2, &[]);
            pass.dispatch_workgroups(groups_c, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PHOTON Stage C3 bounded winners"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.stage_c3);
            pass.set_bind_group(0, &self.bind_c3, &[]);
            pass.dispatch_workgroups(groups_c, 1, 1);
        }

        encoder.copy_buffer_to_buffer(
            &self.result_gpu,
            0,
            &self.readback_gpu,
            0,
            RESULT_BYTES as u64,
        );
        encoder.copy_buffer_to_buffer(
            &self.winner_records_gpu,
            0,
            &self.readback_gpu,
            RESULT_BYTES as u64,
            (self.winner_cap as usize * WINNER_RECORD_BYTES) as u64,
        );
        self.queue.submit([encoder.finish()]);

        let slice = self.readback_gpu.slice(..self.readback_bytes as u64);
        let (map_tx, map_rx) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = map_tx.send(result);
        });
        self.instance.poll_all(true);
        map_rx
            .recv()
            .map_err(|_| "WGPU result map callback disconnected".to_string())?
            .map_err(|error| format!("map WGPU bounded result: {error}"))?;

        let view = slice
            .get_mapped_range()
            .map_err(|error| format!("read mapped WGPU bounded result: {error}"))?;
        let bytes = view.as_ref();
        let completed = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let total_winners = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if completed != candidate_count {
            drop(view);
            self.readback_gpu.unmap();
            return Err(format!(
                "PHOTON WGPU C3 completed {completed}; expected {candidate_count}"
            ));
        }

        let returned = total_winners.min(self.winner_cap) as usize;
        let mut winners = Vec::with_capacity(returned);
        for index in 0..returned {
            let record = RESULT_BYTES + index * WINNER_RECORD_BYTES;
            let nonce = u32::from_le_bytes(bytes[record..record + 4].try_into().unwrap());
            let mut digest = [0u8; 32];
            for word_index in 0..8 {
                let start = record + 4 + word_index * 4;
                let word = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap());
                digest[word_index * 4..word_index * 4 + 4].copy_from_slice(&word.to_be_bytes());
            }
            winners.push(PhotonCudaWinner { nonce, digest });
        }
        drop(view);
        self.readback_gpu.unmap();
        if adapt_batch {
            self.recommended_candidates = next_wgpu_batch_size(
                candidate_count,
                batch_started.elapsed(),
                self.max_candidates,
            );
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
    use secp256k1::{PublicKey, SecretKey};

    fn reference_template_with_target(target: [u8; 32]) -> [u8; TX_BYTES] {
        let raw = hex::decode(include_str!("../reference/photon_vector_tx.hex").trim()).unwrap();
        let mut template: [u8; TX_BYTES] = raw.try_into().unwrap();
        template[390..394].fill(0);
        template[394..426].copy_from_slice(&target);
        template[426..490].fill(0);
        template
    }

    fn assert_reconstructs(
        template: &[u8; TX_BYTES],
        target: &[u8; 32],
        private_key: &[u8; 32],
        winner: &PhotonCudaWinner,
    ) {
        let message = tx::photon_message_sha256(winner.nonce, &hex::encode(target)).unwrap();
        let signature = crypto::bch_schnorr_sign(private_key, &message).unwrap();
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(*private_key).unwrap())
                .serialize();
        assert!(crypto::bch_schnorr_verify(&public_key, &message, &signature).unwrap());

        let mut completed = *template;
        completed[390..394].copy_from_slice(&winner.nonce.to_le_bytes());
        completed[426..490].copy_from_slice(&signature);
        let expected = search::hash256(&completed);
        assert_eq!(winner.digest, expected);
        assert!(search::meets_target_le(&expected, target));
    }

    #[test]
    fn job_validation_rejects_target_mismatch_and_invalid_key() {
        let target = [0x11u8; 32];
        let template = reference_template_with_target(target);
        let mut key = [0u8; 32];
        key[31] = 1;
        assert!(validate_job_material(&template, &target, &key).is_ok());

        let wrong_target = [0x22u8; 32];
        assert!(validate_job_material(&template, &wrong_target, &key)
            .unwrap_err()
            .contains("target must match"));
        assert!(validate_job_material(&template, &target, &[0u8; 32])
            .unwrap_err()
            .contains("invalid PHOTON WGPU signing key"));
    }

    #[test]
    fn internal_storage_capacity_covers_full_workgroups_without_changing_logical_limit() {
        assert_eq!(storage_candidates(1), 128);
        assert_eq!(storage_candidates(128), 128);
        assert_eq!(storage_candidates(129), 256);
        assert_eq!(storage_candidates(65_536), 65_536);
    }

    #[test]
    fn adaptive_batching_matches_authoritative_m67_cadence() {
        assert_eq!(initial_wgpu_batch_size(WGPU_REFERENCE_MAX_BATCH), 1_024);
        assert_eq!(initial_wgpu_batch_size(128), 128);
        assert_eq!(
            next_wgpu_batch_size(1_024, Duration::from_millis(175), WGPU_REFERENCE_MAX_BATCH,),
            2_048
        );
        assert_eq!(
            next_wgpu_batch_size(
                262_144,
                Duration::from_millis(175),
                WGPU_REFERENCE_MAX_BATCH,
            ),
            WGPU_REFERENCE_MAX_BATCH
        );
        assert_eq!(
            next_wgpu_batch_size(2_048, Duration::from_millis(350), WGPU_REFERENCE_MAX_BATCH,),
            2_048
        );
        assert_eq!(
            next_wgpu_batch_size(2_048, Duration::from_millis(351), WGPU_REFERENCE_MAX_BATCH,),
            1_024
        );
        assert_eq!(
            next_wgpu_batch_size(1_024, Duration::from_secs(2), WGPU_REFERENCE_MAX_BATCH,),
            1_024
        );
    }

    #[test]
    fn wgpu_reference_batch_envelope_is_capped_by_real_buffer_limits() {
        let mib = 1024u32 * 1024;
        assert_eq!(
            device_limited_wgpu_max_candidates(
                WGPU_REFERENCE_MAX_BATCH,
                u64::from(128 * mib),
                u64::from(128 * mib),
            ),
            WGPU_REFERENCE_MAX_BATCH
        );
        assert_eq!(
            device_limited_wgpu_max_candidates(
                WGPU_REFERENCE_MAX_BATCH,
                u64::from(64 * mib),
                u64::from(64 * mib),
            ),
            262_144
        );
        assert_eq!(
            device_limited_wgpu_max_candidates(128, u64::from(64 * mib), u64::from(64 * mib)),
            128
        );
    }

    #[test]
    fn bounded_c3_reuses_authoritative_strict_target_comparator() {
        assert!(BOUNDED_C3_WGSL.contains("m10_hash_is_below_target(finalHash)"));
        assert!(BOUNDED_C3_WGSL.contains("atomicAdd(&benchmarkOutput.winners, 1u)"));
        assert!(BOUNDED_C3_WGSL.contains("atomicLoad(&benchmarkOutput.checksum)"));
        assert!(BOUNDED_C3_WGSL.contains("arrayLength(&pickaxeWinnerRecords) / 9u"));
    }

    #[test]
    fn full_pipeline_matches_host_and_winner_readback_is_bounded_if_wgpu_present() {
        let target = [0xffu8; 32];
        let template = reference_template_with_target(target);
        let mut private_key = [0u8; 32];
        private_key[31] = 1;
        let candidate_count = 128;
        let winner_cap = 4;

        let mut engine = match WgpuPhotonEngine::new(0, candidate_count, winner_cap) {
            Ok(engine) => engine,
            Err(error) if error.contains("no hardware WGPU adapter") => {
                eprintln!("skip PHOTON WGPU hardware test: {error}");
                return;
            }
            Err(error) => panic!("PHOTON WGPU init failed: {error}"),
        };
        engine.set_job(&template, &target, &private_key).unwrap();

        let single_nonce = 0x1234_5678;
        let single = engine.search_batch(single_nonce, 1).unwrap();
        assert_eq!(single.candidates, 1);
        assert_eq!(single.total_winners, 1);
        assert_eq!(single.winners.len(), 1);
        assert!(!single.truncated());
        assert_eq!(single.winners[0].nonce, single_nonce);
        assert_reconstructs(&template, &target, &private_key, &single.winners[0]);

        let nonce_base = 0x3456_0000;
        let started = std::time::Instant::now();
        let batch = engine.search_batch(nonce_base, candidate_count).unwrap();
        let elapsed = started.elapsed();
        eprintln!(
            "PHOTON WGPU {} steady-state batch: {candidate_count} candidates in {:.6}s = {:.2} candidates/s; persistent_device_bytes={}; table_source={:?}",
            engine._adapter_name,
            elapsed.as_secs_f64(),
            candidate_count as f64 / elapsed.as_secs_f64(),
            engine.persistent_device_bytes(),
            engine.table_source,
        );
        assert_eq!(batch.candidates, candidate_count);
        assert_eq!(batch.total_winners, candidate_count);
        assert_eq!(batch.winners.len(), winner_cap as usize);
        assert!(batch.truncated());
        for winner in &batch.winners {
            assert!(winner.nonce >= nonce_base);
            assert!(winner.nonce < nonce_base + candidate_count);
            assert_reconstructs(&template, &target, &private_key, winner);
        }

        let expected_bytes = m29_table::M29_G16_BYTES
            + INPUT_BYTES
            + 32
            + 64
            + 32
            + candidate_count as usize * M38_RECORD_BYTES
            + CONTROL_BYTES
            + candidate_count as usize * SIGNATURE_BYTES
            + candidate_count as usize * HASH_BYTES
            + RESULT_BYTES
            + winner_cap as usize * WINNER_RECORD_BYTES
            + (RESULT_BYTES + winner_cap as usize * WINNER_RECORD_BYTES);
        assert_eq!(engine.persistent_device_bytes(), expected_bytes);
    }
}
