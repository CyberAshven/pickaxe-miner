//! Portable mining engine on the wgpu library.
//!
//! One engine for every GPU without a native CUDA or HIP engine. wgpu runs it on
//! Vulkan (Windows, Linux), optionally DirectX 12 (Windows,
//! `PICKAXE_WGPU_API=dx12`) and Metal (macOS); compiled to WebAssembly it is the
//! browser GPU miner on the browser's WebGPU (`browser.rs`). Its stages (A, B,
//! C1, T2 preparation, C2, C3) and T2 filters are WGSL generated from the shared
//! Rust engine (`reference/shared-*`); `PICKAXE_WGPU_STAGES=wgsl` selects the
//! original hand-written PHOTON pipeline in `reference/photon-miner.wgsl`.
//! Candidate intermediates stay on the GPU; the host reads only counters plus a
//! bounded number of winner nonce/HASH256 records. Names (wgpu, WebGPU, WGSL,
//! naga): docs/gpu-sources.md.

use crate::gpu_types::{PhotonCudaBatchResult, PhotonCudaWinner};
use crate::m29_table::{self, M29TableSource};
use crate::{protocol::ProofRule, tx::PhotonLayout};
use secp256k1::SecretKey;
use sha2::block_api::compress256;
use std::borrow::Cow;
use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

// #### PR #22: shared Rust stages for portable GPUs
// What: stages A, B and C1 (signing), T2 preparation, and the non-T2 C2 and C3
// come from the shared Rust engine (reference/shared-stages) by default, like
// the T2 filter (reference/shared-t2). PICKAXE_WGPU_STAGES=wgsl selects the
// original hand-written WGSL stages. Both use the same buffers.
// Why: one source for every GPU. Measured per batch, signing takes 0.9 ms
// instead of 3.4 ms on an RTX 5070 Ti Laptop GPU and 2.6 ms instead of
// 90 ms on the integrated Radeon gfx1036, and the stages compile in seconds.
// Check: both sets pass the same GPU tests and CPU reconstruction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PortableStages {
    Wgsl,
    #[default]
    Rust,
}

/// Entry points of one stage set.
struct StageEntries {
    a: &'static str,
    b: &'static [&'static str],
    c1: &'static str,
    t2_prepare: &'static str,
    c2: &'static str,
    c3: &'static str,
}

impl PortableStages {
    /// Parses PICKAXE_WGPU_STAGES; unset or empty selects the shared Rust stages.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") | Some("rust") => Ok(Self::Rust),
            Some("wgsl") => Ok(Self::Wgsl),
            Some(other) => Err(format!(
                "PICKAXE_WGPU_STAGES must be wgsl or rust, not {other:?}"
            )),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_env() -> Result<Self, String> {
        Self::parse(std::env::var("PICKAXE_WGPU_STAGES").ok().as_deref())
    }

    fn entry_points(self) -> StageEntries {
        match self {
            Self::Wgsl => StageEntries {
                a: "photon_m38_stage_a_wg128",
                b: &[
                    "photon_m45_b4a_wg32",
                    "photon_m45_b4b_wg32",
                    "photon_m45_b4c_wg32",
                    "photon_m45_b4d_wg32",
                ],
                c1: "photon_m6729_c1_znorm_wg64",
                t2_prepare: "pickaxe_t2_prepare",
                c2: "photon_m6725_c2_hash_only_wg64",
                c3: "pickaxe_photon_c3_bounded_wg64",
            },
            Self::Rust => StageEntries {
                a: "pickaxe_shared_stage_a",
                b: &["pickaxe_shared_stage_b"],
                c1: "pickaxe_shared_c1_signature",
                t2_prepare: "pickaxe_shared_t2_prepare",
                c2: "pickaxe_shared_c2_hash",
                c3: "pickaxe_shared_c3_winners",
            },
        }
    }
}

// Tests read signing intermediates back; production buffers stay GPU-only.
#[cfg(test)]
const DEBUG_READABLE_STORAGE: wgpu::BufferUsages =
    wgpu::BufferUsages::STORAGE.union(wgpu::BufferUsages::COPY_SRC);
#[cfg(not(test))]
const DEBUG_READABLE_STORAGE: wgpu::BufferUsages = wgpu::BufferUsages::STORAGE;

const SHARED_STAGES_WGSL: &str =
    include_str!("../reference/shared-stages/pickaxe_shared_stages.wgsl");
const SHARED_STAGES_LOOP_COPIES_WGSL: &str =
    include_str!("../reference/shared-stages/pickaxe_shared_stages_dx12_metal.wgsl");

// #### PR #22: shared stages per shader translator
// What: Vulkan (naga's SPIR-V writer) and Chromium browsers (Tint) run the
// generated shared stages unchanged. DirectX 12 and Metal (naga's HLSL and MSL
// writers) and other browsers run the copy whose loop values survive naga's
// continuing-block translation (tools/shared-gpu-proof/sync_filter.py).
// Why: without the copies those translations lose every result of the stage,
// while on Vulkan they cost the RTX 5070 Ti about 0.9% more C1 time, so Vulkan
// and Chromium keep the original byte-identical.
// Check: a stage that returns nothing or hangs on DirectX 12 or Metal means a
// loop needs a copy that the generator did not make.
fn shared_stages_source(backends: wgpu::Backends) -> &'static str {
    if backends == wgpu::Backends::VULKAN
        || (backends == wgpu::Backends::BROWSER_WEBGPU && browser_translates_with_tint())
    {
        SHARED_STAGES_WGSL
    } else {
        SHARED_STAGES_LOOP_COPIES_WGSL
    }
}

/// Chromium's WebGPU translates WGSL with Tint; Firefox uses naga, and Safari
/// and every iOS browser use WebKit's translator.
#[cfg(any(test, target_arch = "wasm32"))]
fn user_agent_uses_tint(user_agent: &str) -> bool {
    user_agent.contains("Chrome/")
}

#[cfg(target_arch = "wasm32")]
fn browser_translates_with_tint() -> bool {
    js_sys::Reflect::get(&js_sys::global(), &"navigator".into())
        .and_then(|navigator| js_sys::Reflect::get(&navigator, &"userAgent".into()))
        .ok()
        .and_then(|agent| agent.as_string())
        .is_some_and(|agent| user_agent_uses_tint(&agent))
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_translates_with_tint() -> bool {
    false
}

#[cfg(test)]
const TX_BYTES: usize = 615;
const TEMPLATE_WORDS: usize = (615 + PhotonLayout::MAX_SHIFT).div_ceil(4);
const INPUT_WORDS: usize = TEMPLATE_WORDS + 4;
const INPUT_BYTES: usize = INPUT_WORDS * 4;
const BASE_NONCE_OFFSET: u64 = (TEMPLATE_WORDS * 4) as u64;
const M38_RECORD_BYTES: usize = 160;
const SIGNATURE_BYTES: usize = 64;
const HASH_BYTES: usize = 32;
const RESULT_BYTES: usize = 16;
const CONTROL_BYTES: usize = 16;
const WINNER_RECORD_WORDS: usize = 9;
const WINNER_RECORD_BYTES: usize = WINNER_RECORD_WORDS * 4;
const WGPU_MIN_LADDER_BATCH: u32 = 1_024;
pub(crate) const WGPU_REFERENCE_MAX_BATCH: u32 = 524_288;
// #### PR #22: amortize the portable queue/readback handoff on fast devices.
// The existing elapsed-time ladder still limits slower GPUs to shorter work.
// CUDA geometry and the conserved 65,536-candidate signature window are unchanged.
pub(crate) const WGPU_T2_MAX_BATCH: u32 = crate::gpu_types::PORTABLE_MAX_BATCH_CANDIDATES;
const WGPU_TARGET_BATCH: Duration = Duration::from_millis(350);
const WGPU_ESCALATE_BATCH: Duration = Duration::from_millis(175);

fn t2_window_bytes(candidates: u32) -> usize {
    (candidates.div_ceil(65536) as usize + 1) * 128 * 4
}

/// Emit constant schedule indices, following the Rust CUDA SHA implementation.
/// This lets WGSL compilers keep the 16-word ring in registers instead of a
/// dynamically indexed 64-word private array. The reference fallback is intact.
fn t2_compression_source() -> String {
    let mut source = String::new();
    for (name, start, end, scheduled) in [
        ("t2_compress", 0, 64, false),
        ("t2_head10", 0, 10, false),
        ("t2_compress_after10", 10, 64, false),
        ("t2_compress_middle", 0, 64, true),
    ] {
        write!(source, "\nfn {name}(initial: array<u32, 8>").unwrap();
        if !scheduled {
            source.push_str(", block: array<u32, 16>");
        }
        if start != 0 {
            source.push_str(", head: array<u32, 8>");
        }
        source.push_str(") -> array<u32, 8> {\n");
        if !scheduled {
            for i in 0..16 {
                writeln!(source, "var w{i} = block[{i}];").unwrap();
            }
        }
        let roles = ["a", "b", "c", "d", "e", "f", "g", "h"];
        for (i, role) in roles.iter().enumerate() {
            let state = if start == 0 { "initial" } else { "head" };
            writeln!(source, "var {role} = {state}[{}];", (i + start) & 7).unwrap();
        }
        for i in start..end {
            let ring = i & 15;
            let word = if scheduled {
                format!("t2Middle[{i}]")
            } else {
                format!("w{ring}")
            };
            if !scheduled && i >= 16 {
                writeln!(
                    source,
                    "w{ring} += smallSigma0(w{}) + w{} + smallSigma1(w{});",
                    (i + 1) & 15,
                    (i + 9) & 15,
                    (i + 14) & 15
                )
                .unwrap();
            }
            let [a, b, c, d, e, f, g, h] = std::array::from_fn(|j| roles[(j + 8 - (i & 7)) & 7]);
            writeln!(source, "let t{i} = {h} + bigSigma1({e}) + choose({e}, {f}, {g}) + K[{i}] + {word};\n{d} += t{i};\n{h} = t{i} + bigSigma0({a}) + majority({a}, {b}, {c});").unwrap();
        }
        source.push_str("return array<u32, 8>(");
        for i in 0..8 {
            let role = roles[(i + 8 - (end & 7)) & 7];
            if end == 64 {
                write!(source, "initial[{i}] + {role},").unwrap();
            } else {
                write!(source, "{role},").unwrap();
            }
        }
        source.push_str(");\n}\n");
    }
    source
}

/// Match the native T2 constant-offset block assembly. Pipeline specialization
/// folds all amount-byte tests before GPU execution; no private-array indexing.
fn t2_blocks_source() -> String {
    let mut source = String::from("\noverride T2_SHIFT: u32 = 0u;\n");
    for block in 0..3 {
        writeln!(source, "fn t2_block{block}(window: u32, batonLo: u32, batonHi: u32, rewardLo: u32, rewardHi: u32) -> array<u32, 16> {{").unwrap();
        for word in 0..16 {
            writeln!(
                source,
                "var w{word} = t2Windows[window * 128u + {}u];",
                8 + block * 16 + word
            )
            .unwrap();
            for byte in 0..4 {
                let offset = 448 + block * 64 + word * 4 + byte;
                let shift = (3 - byte) * 8;
                for (name, start) in [("baton", 491), ("reward", 578)] {
                    if (start..start + PhotonLayout::MAX_SHIFT + 8).contains(&offset) {
                        writeln!(source, "if ({offset}u >= {start}u + T2_SHIFT && {offset}u < {}u + T2_SHIFT) {{ let n = ({offset}u + 32u - {start}u - T2_SHIFT) & 31u; let value = (select({name}Lo, {name}Hi, n >= 4u) >> ((n & 3u) * 8u)) & 255u; w{word} = (w{word} & ~({}u << {shift}u)) | (value << {shift}u); }}", start + 8, 255).unwrap();
                    }
                }
            }
        }
        source.push_str("return array<u32, 16>(");
        for i in 0..16 {
            write!(source, "w{i},").unwrap();
        }
        source.push_str(");\n}\n");
    }
    source
}

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
    // Keep the upstream reference intact; update only the functions reachable
    // from our production entry points. Native Metal/Vulkan and browser WebGPU
    // compile this same source and consume the same PhotonLayout fields.
    source = source.replace(
        "words: array<u32, 154>,",
        &format!("words: array<u32, {TEMPLATE_WORDS}>,"),
    );
    source = source.replacen(
        "byteLength: u32,",
        "byteLength: u32,\n    layoutShift: u32,\n    txLength: u32,\n    positiveProof: u32,",
        1,
    );
    for (name, offsets) in [
        ("m9_message_hash", &[394usize][..]),
        ("m9_completed_tx_byte", &[390, 394, 426, 458, 490][..]),
    ] {
        let start = source
            .find(&format!("fn {name}("))
            .ok_or("missing WGSL layout function")?;
        let end = start
            + source[start..]
                .find("\n}")
                .ok_or("missing WGSL function end")?
            + 2;
        let mut body = source[start..end].to_string();
        for offset in offsets {
            body = body.replace(
                &format!("{offset}u"),
                &format!("({offset}u + input.layoutShift)"),
            );
        }
        source.replace_range(start..end, &body);
    }
    let start = source
        .find("fn m30_transaction_hashes_prefixed(")
        .ok_or("missing prefixed HASH256")?;
    let end = start
        + source[start..]
            .find("\n}")
            .ok_or("missing HASH256 function end")?
        + 2;
    let body = source[start..end].replace("615u", "input.txLength");
    source.replace_range(start..end, &body);
    let start = source
        .find("fn m10_hash_is_below_target(")
        .ok_or("missing target predicate")?;
    let end = start
        + source[start..]
            .find("\n}")
            .ok_or("missing target predicate end")?
        + 2;
    source.replace_range(start..end, include_str!("wgpu_target.wgsl"));
    source.push_str(BOUNDED_C3_WGSL);
    source.push_str(include_str!("wgpu_t2.wgsl"));
    source.push_str(&t2_compression_source());
    source.push_str(&t2_blocks_source());
    Ok(source)
}

/// Rejects non-hardware WGPU adapters for mining.
#[cfg(not(target_arch = "wasm32"))]
fn is_hardware_adapter(info: &wgpu::AdapterInfo) -> bool {
    // #### PR #22
    // CI executes the real kernels on lavapipe. This opt-in exists only in a
    // test binary, requires a software device, and is never a mining fallback.
    #[cfg(test)]
    if std::env::var("PICKAXE_TEST_SOFTWARE_WGPU").as_deref() == Ok("1") {
        return info.device_type == wgpu::DeviceType::Cpu;
    }
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
    compress256(state, std::slice::from_ref(block));
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
fn m30_prefix_words(template: &[u8]) -> [u32; 8] {
    let mut state = SHA256_IV;
    for block in template[..384].as_chunks::<64>().0 {
        compress_block(&mut state, block);
    }
    state
}

/// Checks GPU job material against authoritative PHOTON fields.
fn validate_job_material(
    template: &[u8],
    target: &[u8; 32],
    private_key: &[u8; 32],
) -> Result<(), String> {
    let layout = PhotonLayout::for_tx_len(template.len())?;
    let offset = layout.target_offset();
    if template[offset..offset + 32] != target[..] {
        return Err("PHOTON WGPU target must match transaction template".into());
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

// #### PR #22: import the native Rust hash/layout through the pinned compiler.
// The provenance check and regeneration CI reject separately edited WGSL.
#[cfg(feature = "shared-rust-t2")]
fn create_shared_t2_filter(
    device: &wgpu::Device,
    shift: usize,
    windows: &wgpu::Buffer,
    control: &wgpu::Buffer,
    result: &wgpu::Buffer,
    winners: &wgpu::Buffer,
) -> (wgpu::ComputePipeline, wgpu::BindGroup) {
    let source = match shift {
        0 => include_str!("../reference/shared-t2/pickaxe_shared_t2_0.wgsl"),
        1 => include_str!("../reference/shared-t2/pickaxe_shared_t2_1.wgsl"),
        2 => include_str!("../reference/shared-t2/pickaxe_shared_t2_2.wgsl"),
        3 => include_str!("../reference/shared-t2/pickaxe_shared_t2_3.wgsl"),
        4 => include_str!("../reference/shared-t2/pickaxe_shared_t2_4.wgsl"),
        5 => include_str!("../reference/shared-t2/pickaxe_shared_t2_5.wgsl"),
        6 => include_str!("../reference/shared-t2/pickaxe_shared_t2_6.wgsl"),
        7 => include_str!("../reference/shared-t2/pickaxe_shared_t2_7.wgsl"),
        8 => include_str!("../reference/shared-t2/pickaxe_shared_t2_8.wgsl"),
        9 => include_str!("../reference/shared-t2/pickaxe_shared_t2_9.wgsl"),
        10 => include_str!("../reference/shared-t2/pickaxe_shared_t2_10.wgsl"),
        11 => include_str!("../reference/shared-t2/pickaxe_shared_t2_11.wgsl"),
        12 => include_str!("../reference/shared-t2/pickaxe_shared_t2_12.wgsl"),
        13 => include_str!("../reference/shared-t2/pickaxe_shared_t2_13.wgsl"),
        14 => include_str!("../reference/shared-t2/pickaxe_shared_t2_14.wgsl"),
        15 => include_str!("../reference/shared-t2/pickaxe_shared_t2_15.wgsl"),
        16 => include_str!("../reference/shared-t2/pickaxe_shared_t2_16.wgsl"),
        _ => unreachable!("Validated T2 layout"),
    };
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Shared Rust T2 filter"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Shared Rust T2 filter"),
        layout: None,
        module: &module,
        entry_point: None,
        compilation_options: Default::default(),
        cache: None,
    });
    let binding = create_bind_group(
        device,
        &pipeline,
        "Shared Rust T2 filter",
        &[(0, windows), (1, control), (2, result), (3, winners)],
    );
    (pipeline, binding)
}

pub struct WgpuPhotonEngine {
    _instance: wgpu::Instance,
    /// The original hand-written WGSL, created only when a stage uses it.
    _shader: Option<wgpu::ShaderModule>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    stage_a: wgpu::ComputePipeline,
    stage_b: Vec<wgpu::ComputePipeline>,
    stage_c1: wgpu::ComputePipeline,
    stage_c2: wgpu::ComputePipeline,
    stage_c3: wgpu::ComputePipeline,
    t2_prepare: wgpu::ComputePipeline,
    t2_filter: wgpu::ComputePipeline,
    bind_t2_prepare: wgpu::BindGroup,
    bind_t2_filter: wgpu::BindGroup,
    _t2_windows_gpu: wgpu::Buffer,
    t2_control_gpu: wgpu::Buffer,
    #[cfg(feature = "shared-rust-t2")]
    shared_t2_control: [u32; 17],
    t2_active: bool,
    t2_group_size: u32,
    t2_filter_shift: usize,
    t2_filter_cache: std::collections::HashMap<usize, (wgpu::ComputePipeline, wgpu::BindGroup)>,
    bind_a: wgpu::BindGroup,
    bind_b: Vec<wgpu::BindGroup>,
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
    t2_max_candidates: u32,
    winner_cap: u32,
    readback_bytes: usize,
    #[cfg(test)]
    profile: Option<(wgpu::QuerySet, wgpu::Buffer)>,
    #[cfg(test)]
    last_stage_ms: Vec<f64>,
    table_source: M29TableSource,
    recommended_candidates: u32,
    _adapter_name: String,
    job_ready: bool,
    positive_target_rule: bool,
    stages: PortableStages,
}

impl WgpuPhotonEngine {
    /// Creates a WgpuPhotonEngine for the portable GPU pipeline.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
    ) -> Result<Self, String> {
        let table = m29_table::load_or_generate_m29_g16()?;
        pollster::block_on(Self::new_async(
            device_ordinal,
            max_candidates,
            winner_cap,
            table,
            PortableStages::from_env()?,
        ))
    }

    /// Uses the same asynchronous pipeline on native and browser devices.
    pub async fn new_async(
        device_ordinal: usize,
        max_candidates: u32,
        winner_cap: u32,
        table: (Vec<u8>, M29TableSource),
        stages: PortableStages,
    ) -> Result<Self, String> {
        if !m29_table::valid_table(&table.0) {
            return Err("invalid PHOTON generator table".into());
        }
        if max_candidates == 0 {
            return Err("PHOTON WGPU max_candidates must be greater than zero".into());
        }
        if winner_cap == 0 {
            return Err("PHOTON WGPU winner_cap must be greater than zero".into());
        }

        let backends = production_wgpu_backends()?;
        let instance = wgpu::Instance::new(production_instance_descriptor(backends)?);
        #[cfg(not(target_arch = "wasm32"))]
        let adapters = instance.enumerate_adapters(backends).await;
        #[cfg(not(target_arch = "wasm32"))]
        let adapter = {
            let mut hardware = adapters
                .into_iter()
                .filter(|adapter| is_hardware_adapter(&adapter.get_info()));
            // #### PR #22: GPU tests may name their adapter, because the Vulkan
            // order can differ between processes; a named test never runs elsewhere.
            #[cfg(test)]
            let named = std::env::var("PICKAXE_TEST_WGPU_ADAPTER").ok();
            #[cfg(not(test))]
            let named: Option<String> = None;
            match named {
                Some(name) => hardware.find(|adapter| adapter.get_info().name.contains(&name)),
                None => hardware.nth(device_ordinal),
            }
            .ok_or_else(|| {
                #[cfg(test)]
                if std::env::var("PICKAXE_TEST_SOFTWARE_WGPU").as_deref() == Ok("1") {
                    return "required software WGPU test adapter is unavailable".into();
                }
                format!("no hardware WGPU adapter at backend-local ordinal {device_ordinal}")
            })?
        };
        #[cfg(target_arch = "wasm32")]
        let adapter = {
            if device_ordinal != 0 {
                return Err("the browser selects the GPU adapter".into());
            }
            instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                    ..Default::default()
                })
                .await
                .map_err(|error| format!("WebGPU adapter: {error}"))?
        };
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
        let t2_max_candidates = max_candidates.min(WGPU_T2_MAX_BATCH);
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

        #[cfg(test)]
        let profile_enabled = std::env::var_os("PICKAXE_WGPU_TIMESTAMPS").is_some();
        #[cfg(not(test))]
        let profile_enabled = false;
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("Pickaxe PHOTON WGPU device"),
            required_limits: limits,
            required_features: if profile_enabled {
                wgpu::Features::TIMESTAMP_QUERY
            } else {
                wgpu::Features::empty()
            },
            ..Default::default()
        };
        let (device, queue) = adapter
            .request_device(&descriptor)
            .await
            .map_err(|error| format!("request WGPU device {}: {error}", adapter_info.name))?;

        // #### PR #22: the shared stages never touch the 34k-line original
        // module, so it is parsed only for PICKAXE_WGPU_STAGES=wgsl (or the
        // legacy non-shared filter build); startup is seconds shorter.
        let shader = if stages == PortableStages::Wgsl || cfg!(not(feature = "shared-rust-t2")) {
            Some(device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Pickaxe M67.38 PHOTON reference shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(reference_shader_source_for_wgpu()?)),
            }))
        } else {
            None
        };
        let module = match (stages, &shader) {
            (PortableStages::Wgsl, Some(reference)) => reference,
            _ => &device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Shared Rust stages"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(shared_stages_source(backends))),
            }),
        };
        let entries = stages.entry_points();
        let stage_a = create_pipeline(&device, module, entries.a);
        let stage_b: Vec<_> = entries
            .b
            .iter()
            .map(|entry| create_pipeline(&device, module, entry))
            .collect();
        let stage_c1 = create_pipeline(&device, module, entries.c1);
        let stage_c2 = create_pipeline(&device, module, entries.c2);
        let stage_c3 = create_pipeline(&device, module, entries.c3);
        let t2_prepare = create_pipeline(&device, module, entries.t2_prepare);
        #[cfg(not(feature = "shared-rust-t2"))]
        let t2_filter = create_pipeline(
            &device,
            shader
                .as_ref()
                .expect("the legacy filter build parses the original module"),
            "pickaxe_t2_filter",
        );

        let (table_bytes, table_source) = table;
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
            DEBUG_READABLE_STORAGE,
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
            DEBUG_READABLE_STORAGE,
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
        #[cfg(test)]
        let profile = profile_enabled.then(|| {
            (
                device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("T2 stage timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 16,
                }),
                create_buffer(
                    &device,
                    "T2 timestamp resolve",
                    128,
                    wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    false,
                ),
            )
        });
        let readback_bytes = RESULT_BYTES
            + winner_cap as usize * WINNER_RECORD_BYTES
            + if profile_enabled { 128 } else { 0 };
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
        let bind_b = stage_b
            .iter()
            .map(|pipeline| {
                create_bind_group(
                    &device,
                    pipeline,
                    "PHOTON B bind group",
                    &[
                        (3, &table_gpu),
                        (10, &intermediate_gpu),
                        (12, &dispatch_params_gpu),
                    ],
                )
            })
            .collect();
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

        let t2_windows_gpu = create_buffer(
            &device,
            "PHOTON T2 signed windows",
            t2_window_bytes(t2_max_candidates),
            wgpu::BufferUsages::STORAGE,
            false,
        );
        let t2_control_gpu = create_buffer(
            &device,
            "PHOTON T2 dispatch",
            if cfg!(feature = "shared-rust-t2") {
                80
            } else {
                16
            },
            wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::COPY_DST
                | if cfg!(feature = "shared-rust-t2") {
                    wgpu::BufferUsages::STORAGE
                } else {
                    wgpu::BufferUsages::empty()
                },
            false,
        );
        let bind_t2_prepare = create_bind_group(
            &device,
            &t2_prepare,
            "PHOTON T2 prepare",
            &[
                (0, &input_gpu),
                (6, &m30_gpu),
                (13, &signatures_gpu),
                (17, &t2_windows_gpu),
                (18, &t2_control_gpu),
            ],
        );
        #[cfg(not(feature = "shared-rust-t2"))]
        let bind_t2_filter = create_bind_group(
            &device,
            &t2_filter,
            "PHOTON T2 filter",
            &[
                (0, &input_gpu),
                (2, &result_gpu),
                (16, &winner_records_gpu),
                (17, &t2_windows_gpu),
                (18, &t2_control_gpu),
            ],
        );
        #[cfg(feature = "shared-rust-t2")]
        let (t2_filter, bind_t2_filter) = create_shared_t2_filter(
            &device,
            0,
            &t2_windows_gpu,
            &t2_control_gpu,
            &result_gpu,
            &winner_records_gpu,
        );
        Ok(Self {
            _instance: instance,
            _shader: shader,
            device,
            queue,
            stage_a,
            stage_b,
            stage_c1,
            stage_c2,
            stage_c3,
            t2_prepare,
            t2_filter,
            bind_t2_prepare,
            bind_t2_filter,
            _t2_windows_gpu: t2_windows_gpu,
            t2_control_gpu,
            #[cfg(feature = "shared-rust-t2")]
            shared_t2_control: [0; 17],
            t2_active: false,
            t2_group_size: 64,
            t2_filter_shift: 0,
            t2_filter_cache: std::collections::HashMap::new(),
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
            t2_max_candidates,
            winner_cap,
            readback_bytes,
            #[cfg(test)]
            profile,
            #[cfg(test)]
            last_stage_ms: Vec::new(),
            table_source,
            recommended_candidates: initial_wgpu_batch_size(max_candidates),
            _adapter_name: adapter_info.name,
            job_ready: false,
            positive_target_rule: false,
            stages,
        })
    }

    fn timestamps(&self, index: u32) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        #[cfg(test)]
        if let Some((queries, _)) = &self.profile {
            return Some(wgpu::ComputePassTimestampWrites {
                query_set: queries,
                beginning_of_pass_write_index: Some(index),
                end_of_pass_write_index: Some(index + 1),
            });
        }
        let _ = index;
        None
    }

    /// Returns the source of the portable GPU lookup table.
    pub fn stages(&self) -> PortableStages {
        self.stages
    }

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
            + t2_window_bytes(self.t2_max_candidates)
            + 16
    }

    /// Suggests a batch size within device and workgroup limits.
    fn active_capacity(&self) -> u32 {
        if self.t2_active {
            self.t2_max_candidates
        } else {
            self.max_candidates
        }
    }

    pub fn recommended_batch_candidates(&self) -> u32 {
        self.recommended_candidates
    }

    pub fn set_proof_rule(&mut self, rule: ProofRule) {
        self.positive_target_rule = rule == ProofRule::Positive;
        self.job_ready = false;
    }

    /// Writes validated PHOTON job data to WGPU buffers.
    pub fn set_job(
        &mut self,
        template: &[u8],
        target: &[u8; 32],
        private_key: &[u8; 32],
    ) -> Result<(), String> {
        self.job_ready = false;
        self.t2_active = false;
        validate_job_material(template, target, private_key)?;
        let layout = PhotonLayout::for_tx_len(template.len())?;
        if self.positive_target_rule && (target[31] & 0x80 != 0 || target.iter().all(|b| *b == 0)) {
            return Err("PHOTON target must be a positive ScriptNum".into());
        }
        let mut input = pack_big_endian_words(template, TEMPLATE_WORDS);
        input.extend_from_slice(&u32_words_to_le_bytes(&[
            0,
            layout.shift() as u32,
            template.len() as u32,
            u32::from(self.positive_target_rule),
        ]));
        debug_assert_eq!(input.len(), INPUT_BYTES);
        let private_words = pack_big_endian_words(private_key, 8);
        let m27 = u32_words_to_le_bytes(&m27_precomputed_words(private_key));
        let m30 = u32_words_to_le_bytes(&m30_prefix_words(template));

        self.queue.write_buffer(&self.input_gpu, 0, &input);
        self.queue
            .write_buffer(&self.private_key_gpu, 0, &private_words);
        self.queue.write_buffer(&self.m27_gpu, 0, &m27);
        self.queue.write_buffer(&self.m30_gpu, 0, &m30);
        self.t2_active = crate::tx::supports_t2_window(template)?;
        #[cfg(feature = "shared-rust-t2")]
        if self.t2_active {
            let shift = layout.shift();
            for (i, offset) in [491 + shift, 495 + shift, 578 + shift, 582 + shift]
                .into_iter()
                .enumerate()
            {
                self.shared_t2_control[4 + i] =
                    u32::from_le_bytes(template[offset..offset + 4].try_into().unwrap());
            }
            for (i, word) in target.as_chunks::<4>().0.iter().enumerate() {
                self.shared_t2_control[8 + i] = u32::from_le_bytes(*word);
            }
            self.shared_t2_control[16] = u32::from(self.positive_target_rule);
        }
        if self.t2_active && self.t2_filter_shift != layout.shift() {
            let shift = layout.shift();
            let (pipeline, binding) = self.t2_filter_cache.remove(&shift).unwrap_or_else(|| {
                #[cfg(feature = "shared-rust-t2")]
                {
                    create_shared_t2_filter(
                        &self.device,
                        shift,
                        &self._t2_windows_gpu,
                        &self.t2_control_gpu,
                        &self.result_gpu,
                        &self.winner_records_gpu,
                    )
                }
                #[cfg(not(feature = "shared-rust-t2"))]
                {
                    let pipeline =
                        self.device
                            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                                label: Some("PHOTON T2 specialized filter"),
                                layout: None,
                                module: self
                                    ._shader
                                    .as_ref()
                                    .expect("the legacy filter build parses the original module"),
                                entry_point: Some("pickaxe_t2_filter"),
                                compilation_options: wgpu::PipelineCompilationOptions {
                                    constants: &[("T2_SHIFT", shift as f64)],
                                    ..Default::default()
                                },
                                cache: None,
                            });
                    let binding = create_bind_group(
                        &self.device,
                        &pipeline,
                        "PHOTON T2 filter",
                        &[
                            (0, &self.input_gpu),
                            (2, &self.result_gpu),
                            (16, &self.winner_records_gpu),
                            (17, &self._t2_windows_gpu),
                            (18, &self.t2_control_gpu),
                        ],
                    );
                    (pipeline, binding)
                }
            });
            self.t2_filter_cache.insert(
                self.t2_filter_shift,
                (
                    std::mem::replace(&mut self.t2_filter, pipeline),
                    std::mem::replace(&mut self.bind_t2_filter, binding),
                ),
            );
            self.t2_filter_shift = shift;
        }

        self.job_ready = true;
        Ok(())
    }

    /// Dispatches a bounded portable GPU candidate search.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn search_batch(
        &mut self,
        nonce_base: u32,
        candidate_count: u32,
    ) -> Result<PhotonCudaBatchResult, String> {
        pollster::block_on(self.search_batch_async(nonce_base, candidate_count))
    }

    pub async fn search_batch_async(
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
        if candidate_count > self.active_capacity() {
            return Err(format!(
                "PHOTON WGPU batch {candidate_count} exceeds persistent capacity {}",
                self.active_capacity()
            ));
        }
        nonce_base
            .checked_add(candidate_count - 1)
            .ok_or("PHOTON WGPU candidate range crosses the key-rotation boundary")?;
        let adapt_batch = candidate_count == self.recommended_candidates;
        #[cfg(not(target_arch = "wasm32"))]
        let batch_started = Instant::now();
        #[cfg(target_arch = "wasm32")]
        let batch_started = js_sys::Date::now();

        let (signature_base, signatures) = if self.t2_active {
            let offset = nonce_base & 65535;
            let windows = (offset + candidate_count).div_ceil(65536);
            #[cfg(not(feature = "shared-rust-t2"))]
            self.queue.write_buffer(
                &self.t2_control_gpu,
                0,
                &u32_words_to_le_bytes(&[offset, candidate_count, windows, 0]),
            );
            #[cfg(feature = "shared-rust-t2")]
            {
                // Preparation reads the first four words as a uniform. The
                // shared filter reads the same buffer as storage, so one
                // upload supplies both stages without another queue transfer.
                self.shared_t2_control[..4].copy_from_slice(&[
                    offset,
                    candidate_count,
                    windows,
                    nonce_base / 65536,
                ]);
                self.queue.write_buffer(
                    &self.t2_control_gpu,
                    0,
                    &u32_words_to_le_bytes(&self.shared_t2_control),
                );
            }
            (nonce_base / 65536, windows)
        } else {
            (nonce_base, candidate_count)
        };
        let active_candidates = signatures.div_ceil(128) * 128;
        let groups_a = active_candidates / 128;
        let groups_b = active_candidates / 32;
        let groups_c = active_candidates / 64;
        let dispatch_params = u32_words_to_le_bytes(&[groups_b, active_candidates, 0, 0]);
        let result_init = u32_words_to_le_bytes(&[candidate_count, 0, 0, 0]);
        self.queue.write_buffer(
            &self.input_gpu,
            BASE_NONCE_OFFSET,
            &signature_base.to_le_bytes(),
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
                timestamp_writes: self.timestamps(0),
            });
            pass.set_pipeline(&self.stage_a);
            pass.set_bind_group(0, &self.bind_a, &[]);
            pass.dispatch_workgroups(groups_a, 1, 1);
        }
        for (index, (pipeline, bind_group)) in
            self.stage_b.iter().zip(self.bind_b.iter()).enumerate()
        {
            let label = match index {
                0 => "PHOTON Stage B (part a)",
                1 => "PHOTON Stage B4b",
                2 => "PHOTON Stage B4c",
                _ => "PHOTON Stage B4d",
            };
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: self.timestamps((index as u32 + 1) * 2),
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(groups_b, 1, 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PHOTON Stage C1 normalized Schnorr"),
                timestamp_writes: self.timestamps(10),
            });
            pass.set_pipeline(&self.stage_c1);
            pass.set_bind_group(0, &self.bind_c1, &[]);
            pass.dispatch_workgroups(groups_c, 1, 1);
        }
        if self.t2_active {
            for (stage, (pipeline, binding, groups)) in [
                (
                    &self.t2_prepare,
                    &self.bind_t2_prepare,
                    signatures.div_ceil(64),
                ),
                (
                    &self.t2_filter,
                    &self.bind_t2_filter,
                    candidate_count.div_ceil(self.t2_group_size),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("PHOTON T2 search"),
                    timestamp_writes: self.timestamps(12 + stage as u32 * 2),
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, binding, &[]);
                pass.dispatch_workgroups(groups.min(16_384), groups.div_ceil(16_384), 1);
            }
        } else {
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("PHOTON Stage C2 HASH256"),
                    timestamp_writes: self.timestamps(12),
                });
                pass.set_pipeline(&self.stage_c2);
                pass.set_bind_group(0, &self.bind_c2, &[]);
                pass.dispatch_workgroups(groups_c, 1, 1);
            }
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("PHOTON Stage C3 bounded winners"),
                    timestamp_writes: self.timestamps(14),
                });
                pass.set_pipeline(&self.stage_c3);
                pass.set_bind_group(0, &self.bind_c3, &[]);
                pass.dispatch_workgroups(groups_c, 1, 1);
            }
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
        #[cfg(test)]
        if let Some((queries, buffer)) = &self.profile {
            encoder.resolve_query_set(queries, 0..16, buffer, 0);
            encoder.copy_buffer_to_buffer(
                buffer,
                0,
                &self.readback_gpu,
                (self.readback_bytes - 128) as u64,
                128,
            );
        }
        self.queue.submit([encoder.finish()]);

        let slice = self.readback_gpu.slice(..self.readback_bytes as u64);
        let ready = Arc::new(Mutex::new((None, None::<std::task::Waker>)));
        let callback = Arc::clone(&ready);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let mut state = callback.lock().unwrap();
            state.0 = Some(result);
            if let Some(waker) = state.1.take() {
                waker.wake();
            }
        });
        #[cfg(not(target_arch = "wasm32"))]
        self._instance.poll_all(true);
        std::future::poll_fn(|cx| {
            let mut state = ready.lock().unwrap();
            if let Some(result) = state.0.take() {
                Poll::Ready(result)
            } else {
                state.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
        .map_err(|error| format!("map WGPU bounded result: {error}"))?;

        let view = slice
            .get_mapped_range()
            .map_err(|error| format!("read mapped WGPU bounded result: {error}"))?;
        let bytes = view.as_ref();
        #[cfg(test)]
        if self.profile.is_some() {
            let stamps: Vec<u64> = bytes[self.readback_bytes - 128..]
                .as_chunks::<8>()
                .0
                .iter()
                .map(|v| u64::from_le_bytes(*v))
                .collect();
            let times: Vec<f64> = stamps
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| {
                    v[1].saturating_sub(v[0]) as f64 * f64::from(self.queue.get_timestamp_period())
                        / 1e6
                })
                .collect();
            if std::env::var_os("PICKAXE_WGPU_PROFILE_QUIET").is_none() {
                eprintln!(
                    "T2_PROFILE candidates={candidate_count} group={} ms={times:?}",
                    self.t2_group_size
                );
            }
            self.last_stage_ms = times;
        }
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
            winners.push(PhotonCudaWinner {
                nonce: if self.t2_active { nonce / 65536 } else { nonce },
                digest,
                schnorr_k: None,
                tail_j: self.t2_active.then_some((nonce & 65535) as u16),
                tail_value_sats: None,
            });
        }
        drop(view);
        self.readback_gpu.unmap();
        #[cfg(not(target_arch = "wasm32"))]
        let elapsed = batch_started.elapsed();
        #[cfg(target_arch = "wasm32")]
        let elapsed =
            Duration::from_secs_f64(((js_sys::Date::now() - batch_started) / 1000.0).max(0.0));
        if adapt_batch {
            self.recommended_candidates =
                next_wgpu_batch_size(candidate_count, elapsed, self.active_capacity());
        }

        Ok(PhotonCudaBatchResult {
            candidates: candidate_count,
            total_winners,
            winners,
        })
    }
}

/// Platform APIs used by both discovery and execution.
pub fn production_wgpu_backends() -> Result<wgpu::Backends, String> {
    if cfg!(target_arch = "wasm32") {
        return Ok(wgpu::Backends::BROWSER_WEBGPU);
    }
    native_wgpu_backends(
        std::env::var("PICKAXE_WGPU_API").ok().as_deref(),
        cfg!(windows),
        cfg!(target_os = "macos"),
    )
}

// #### PR #22: optional DirectX 12 on Windows
// What: PICKAXE_WGPU_API=dx12 runs the portable engine on DirectX 12 instead
// of Vulkan, compiling shaders with Microsoft's DXC (dxcompiler.dll and
// dxil.dll beside the executable or on PATH). Vulkan stays the default, and
// macOS always uses Metal.
// Why: some Windows GPUs have a broken Vulkan driver but a working DirectX 12
// one (issue #30). Windows' built-in FXC compiler cannot build these shaders,
// so a missing DXC is reported instead of wgpu's silent FXC fallback.
// Check: `PICKAXE_WGPU_API=dx12 pickaxe_miner devices --backend wgpu` lists
// the DirectX 12 adapters, and the GPU checks pass there as on Vulkan.
fn native_wgpu_backends(
    api: Option<&str>,
    windows: bool,
    macos: bool,
) -> Result<wgpu::Backends, String> {
    let api = api.map(str::trim).unwrap_or_default();
    if macos {
        return if api.is_empty() {
            Ok(wgpu::Backends::METAL)
        } else {
            Err("PICKAXE_WGPU_API is for Windows and Linux; macOS always uses Metal".into())
        };
    }
    match api {
        "" | "vulkan" => Ok(wgpu::Backends::VULKAN),
        "dx12" if windows => Ok(wgpu::Backends::DX12),
        "dx12" => Err("DirectX 12 (PICKAXE_WGPU_API=dx12) is only available on Windows".into()),
        other => Err(format!(
            "PICKAXE_WGPU_API must be vulkan or dx12, not {other:?}"
        )),
    }
}

/// Instance settings for `backends`; DirectX 12 compiles with Microsoft's DXC.
pub(crate) fn production_instance_descriptor(
    backends: wgpu::Backends,
) -> Result<wgpu::InstanceDescriptor, String> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = backends;
    #[cfg(windows)]
    if backends.contains(wgpu::Backends::DX12) {
        descriptor.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::DynamicDxc {
            dxc_path: dxc_compiler_path()?,
        };
    }
    Ok(descriptor)
}

/// Finds `dxcompiler.dll` beside the executable or on the DLL search path.
#[cfg(windows)]
fn dxc_compiler_path() -> Result<String, String> {
    let path = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("dxcompiler.dll")))
        .filter(|path| path.is_file())
        .map_or_else(
            || "dxcompiler.dll".to_owned(),
            |path| path.to_string_lossy().into_owned(),
        );
    // SAFETY: loads Microsoft's DXC library only to confirm it is present;
    // wgpu loads it again for compilation.
    unsafe { libloading::Library::new(&path) }.map_err(|error| {
        format!(
            "DirectX 12 needs Microsoft's DXC shader compiler: put dxcompiler.dll and dxil.dll \
             from a DirectXShaderCompiler release (v1.8.2502 or newer) next to the miner \
             executable ({error})"
        )
    })?;
    Ok(path)
}

/// #### PR #22: an independent CPU check of the portable T2 search
/// What: serializer -> GPU signatures and search -> CPU signature, complete
/// transaction and digest for every deployment, layout, carry and window edge.
/// Why: native GPU tests and the browser check run this one implementation.
/// Check: the engine needs capacity for 4096 candidates and winners.
#[cfg(any(test, all(feature = "browser-check", target_arch = "wasm32")))]
pub(crate) async fn verify_t2_against_cpu(engine: &mut WgpuPhotonEngine) -> Result<usize, String> {
    use crate::{crypto, proof, tx};
    use secp256k1::PublicKey;
    let mut checked = 0usize;
    for (key_index, deployment) in [
        crate::protocol::MAINNET_V0_PHOTON,
        crate::protocol::MAINNET_PHOTON,
        crate::protocol::CHIPNET_PHOTON,
    ]
    .into_iter()
    .enumerate()
    {
        let key = [0x11 + key_index as u8; 32];
        let public_key = PublicKey::from_secret_key(
            &SecretKey::from_secret_bytes(key).map_err(|error| error.to_string())?,
        )
        .serialize();
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        for age in [0, 1, 8, 17, 128, 32768] {
            let Ok(layout) = PhotonLayout::for_age_with_deployment(age, &deployment) else {
                continue;
            };
            // Both limbs exercise a carry/borrow within the amount window.
            let reward = (1u128 << 33) + 7;
            let baton = (1u128 << 49) + u128::from(u32::MAX) - 7;
            let template = tx::build_photon_template_bytes_for_deployment(
                &tx::TemplateParams {
                    prev_tx_hash_hex: "11".repeat(32),
                    prev_index: key_index as u32,
                    age,
                    public_key_hex: hex::encode(public_key),
                    target_hex: hex::encode(target),
                    signature_hex: "00".repeat(64),
                    nonce: 0,
                    contract_value_sats: 48_635_000,
                    relay_fee_sats_per_kb: 1100,
                    contract_token_amount: baton + reward,
                    reward_amount: reward,
                    payout_locking: tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)?,
                },
                &deployment,
            )?;
            engine.set_proof_rule(deployment.proof_rule);
            engine.set_job(&template, &target, &key)?;
            if !engine.t2_active {
                return Err(format!("age {age}: T2 was not enabled"));
            }
            for (base, count) in [
                (0, 67),
                (65500, 128),
                (0x1234ffff, 129),
                (u32::MAX - 66, 67),
            ] {
                let actual = engine.search_batch_async(base, count).await?;
                let mut expected = std::collections::BTreeMap::new();
                for candidate in u64::from(base)..u64::from(base) + u64::from(count) {
                    let nonce = (candidate / 65536) as u32;
                    let j = (candidate & 65535) as u16;
                    let message = tx::photon_message_sha256(nonce, &hex::encode(target))?;
                    let signature = crypto::bch_schnorr_sign(&key, &message)?;
                    if !crypto::bch_schnorr_verify(&public_key, &message, &signature)? {
                        return Err(format!("CPU signature failed for nonce {nonce}"));
                    }
                    let mut completed = template.clone();
                    let n = layout.nonce_offset();
                    completed[n..n + 4].copy_from_slice(&nonce.to_le_bytes());
                    let s = layout.signature_offset();
                    completed[s..s + 64].copy_from_slice(&signature);
                    let shift = layout.shift();
                    completed[491 + shift..499 + shift]
                        .copy_from_slice(&((baton + u128::from(j)) as u64).to_le_bytes());
                    completed[578 + shift..586 + shift]
                        .copy_from_slice(&((reward - u128::from(j)) as u64).to_le_bytes());
                    let digest = proof::hash256(&completed);
                    if proof::meets_target_le_for_rule(&digest, &target, deployment.proof_rule) {
                        expected.insert((nonce, j), digest);
                    }
                    checked += 1;
                }
                let returned: std::collections::BTreeMap<_, _> = actual
                    .winners
                    .iter()
                    .map(|w| ((w.nonce, w.tail_j.unwrap_or_default()), w.digest))
                    .collect();
                if actual.candidates != count
                    || actual.total_winners as usize != expected.len()
                    || actual.truncated()
                    || returned != expected
                {
                    return Err(format!(
                        "age {age} base {base}: GPU result differs from the CPU"
                    ));
                }
            }
            if engine.search_batch_async(u32::MAX, 2).await.is_ok() {
                return Err("a batch crossing the key-rotation boundary was accepted".into());
            }
        }
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{crypto, search, tx};
    use secp256k1::{PublicKey, SecretKey};

    // An explicit serial GPU gate: actual serializer -> GPU signatures/search
    // -> independent CPU signature, complete transaction and digest checks.
    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn t2_gpu_end_to_end_boundaries_and_rotations() {
        let ordinal = std::env::var("PICKAXE_TEST_WGPU_ORDINAL")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        let mut engine = WgpuPhotonEngine::new(ordinal, 4096, 4096).unwrap();
        let checked = pollster::block_on(verify_t2_against_cpu(&mut engine)).unwrap();
        eprintln!(
            "T2 independent candidate checks={checked}; adapter={}",
            engine._adapter_name
        );
    }

    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn t2_gpu_large_dispatch_matches_serial_and_cpu() {
        let ordinal = std::env::var("PICKAXE_TEST_WGPU_ORDINAL")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        let mut engine = WgpuPhotonEngine::new(ordinal, WGPU_T2_MAX_BATCH, 4096).unwrap();
        let key = [0x31; 32];
        let public =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(key).unwrap()).serialize();
        let mut target = [0xff; 32];
        target[31] = 0;
        target[30] = 0;
        let total = 2_100_000_000_000_000_000u128;
        let reward = 4_999_999_999_999u128;
        let template = tx::build_photon_template_bytes_for_deployment(
            &tx::TemplateParams {
                prev_tx_hash_hex: "11".repeat(32),
                prev_index: 0,
                age: 128,
                public_key_hex: hex::encode(public),
                target_hex: hex::encode(target),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 48_635_000,
                relay_fee_sats_per_kb: 1100,
                contract_token_amount: total,
                reward_amount: reward,
                payout_locking: tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)
                    .unwrap(),
            },
            &crate::protocol::MAINNET_PHOTON,
        )
        .unwrap();
        engine.set_proof_rule(crate::protocol::ProofRule::Positive);
        engine.set_job(&template, &target, &key).unwrap();
        let base = 65500;
        // Exercise the full enlarged 2D grid, an unaligned first window and
        // a partial final workgroup against many independent serial searches.
        let count = WGPU_T2_MAX_BATCH - 113;
        let large = engine.search_batch(base, count).unwrap();
        assert!(!large.truncated());
        let mut serial = std::collections::BTreeMap::new();
        let mut offset = 0;
        while offset < count {
            let size = (count - offset).min(524288);
            let batch = engine.search_batch(base + offset, size).unwrap();
            assert!(!batch.truncated());
            for winner in batch.winners {
                serial.insert((winner.nonce, winner.tail_j.unwrap()), winner.digest);
            }
            offset += size;
        }
        let actual: std::collections::BTreeMap<_, _> = large
            .winners
            .iter()
            .map(|w| ((w.nonce, w.tail_j.unwrap()), w.digest))
            .collect();
        assert_eq!(large.total_winners as usize, actual.len());
        assert_eq!(actual, serial);
        assert!(actual.keys().any(|(nonce, _)| *nonce >= 32));
        let layout = PhotonLayout::for_tx_len(template.len()).unwrap();
        for ((nonce, j), digest) in &actual {
            let message = tx::photon_message_sha256(*nonce, &hex::encode(target)).unwrap();
            let sig = crypto::bch_schnorr_sign(&key, &message).unwrap();
            assert!(crypto::bch_schnorr_verify(&public, &message, &sig).unwrap());
            let mut tx = template.clone();
            let n = layout.nonce_offset();
            let p = layout.signature_offset();
            let shift = layout.shift();
            tx[n..n + 4].copy_from_slice(&nonce.to_le_bytes());
            tx[p..p + 64].copy_from_slice(&sig);
            tx[491 + shift..499 + shift]
                .copy_from_slice(&((total - reward + u128::from(*j)) as u64).to_le_bytes());
            tx[578 + shift..586 + shift]
                .copy_from_slice(&((reward - u128::from(*j)) as u64).to_le_bytes());
            assert_eq!(*digest, search::hash256(&tx));
            assert!(search::meets_target_le_for_rule(
                digest,
                &target,
                crate::protocol::ProofRule::Positive
            ));
        }
        eprintln!("T2 large dispatch: {count} candidates, {} independently checked winners, serial/full sets identical", actual.len());
    }

    #[test]
    #[ignore = "serial complete-pipeline performance comparison on an idle GPU"]
    fn t2_gpu_interleaved_pipeline_benchmark() {
        let ordinal = std::env::var("PICKAXE_TEST_WGPU_ORDINAL")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        let mut engine = WgpuPhotonEngine::new(ordinal, WGPU_T2_MAX_BATCH, 8).unwrap();
        let mut key = [0x11; 32];
        let mut target = [0; 32];
        target[0] = 1;
        engine.set_proof_rule(crate::protocol::ProofRule::Positive);
        let template_for_key = |key: [u8; 32]| {
            let public_key =
                PublicKey::from_secret_key(&SecretKey::from_secret_bytes(key).unwrap()).serialize();
            tx::build_photon_template_bytes_for_deployment(
                &tx::TemplateParams {
                    prev_tx_hash_hex: "11".repeat(32),
                    prev_index: 0,
                    age: 128,
                    public_key_hex: hex::encode(public_key),
                    target_hex: hex::encode(target),
                    signature_hex: "00".repeat(64),
                    nonce: 0,
                    contract_value_sats: 48_635_000,
                    relay_fee_sats_per_kb: 1100,
                    contract_token_amount: 2_100_000_000_000_000_000,
                    reward_amount: 4_999_999_999_999,
                    payout_locking: tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)
                        .unwrap(),
                },
                &crate::protocol::MAINNET_PHOTON,
            )
            .unwrap()
        };
        engine
            .set_job(&template_for_key(key), &target, &key)
            .unwrap();
        assert!(engine.t2_active);
        // #### PR #22: compare the actual shared Rust filter against the current
        // 64-lane WGSL filter; keep the historical geometry probe without it.
        let alternate_group_size = if cfg!(feature = "shared-rust-t2") {
            64
        } else {
            256
        };
        let reference = engine
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Pickaxe M67.38 PHOTON reference shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(
                    reference_shader_source_for_wgpu().unwrap(),
                )),
            });
        let mut alternate =
            engine
                .device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("T2 baseline comparison"),
                    layout: None,
                    module: &reference,
                    entry_point: Some("pickaxe_t2_filter"),
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[
                            ("T2_SHIFT", 16.0),
                            ("T2_GROUP_SIZE", alternate_group_size as f64),
                        ],
                        ..Default::default()
                    },
                    cache: None,
                });
        let mut alternate_bind = create_bind_group(
            &engine.device,
            &alternate,
            "T2 comparison",
            &[
                (0, &engine.input_gpu),
                (2, &engine.result_gpu),
                (16, &engine.winner_records_gpu),
                (17, &engine._t2_windows_gpu),
                (18, &engine.t2_control_gpu),
            ],
        );
        let seconds = std::env::var("PICKAXE_TRIAL_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(20)
            .clamp(1, 120);
        let rounds = std::env::var("PICKAXE_TRIAL_ROUNDS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(6)
            .clamp(2, 8);
        let warmup = std::env::var("PICKAXE_WARMUP_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10)
            .min(60);
        for round in 0..rounds {
            std::mem::swap(&mut engine.t2_filter, &mut alternate);
            std::mem::swap(&mut engine.bind_t2_filter, &mut alternate_bind);
            let group_size = if round % 2 == 1 {
                64
            } else {
                alternate_group_size
            };
            engine.t2_group_size = group_size;
            let count = WGPU_T2_MAX_BATCH;
            engine.search_batch(0, count).unwrap();
            let warming = Instant::now();
            while warming.elapsed() < Duration::from_secs(warmup) {
                engine.search_batch(0, count).unwrap();
            }
            let start = Instant::now();
            let mut candidates = 0u64;
            let mut position = u64::from(count);
            let mut rotations = 0;
            while start.elapsed() < Duration::from_secs(seconds) {
                if position == 1u64 << 32 {
                    key[0] += 1;
                    engine
                        .set_job(&template_for_key(key), &target, &key)
                        .unwrap();
                    position = 0;
                    rotations += 1;
                }
                let result = engine.search_batch(position as u32, count).unwrap();
                assert_eq!(result.total_winners, 0);
                candidates += u64::from(result.candidates);
                position += u64::from(result.candidates);
            }
            let variant = if round % 2 == 0 {
                "baseline"
            } else {
                "candidate"
            };
            eprintln!("T2_TRIAL round={round} variant={variant} group_size={group_size} candidates={candidates} elapsed_s={:.6} mh_s={:.4} rotations={rotations} adapter={}", start.elapsed().as_secs_f64(), candidates as f64 / start.elapsed().as_secs_f64() / 1e6, engine._adapter_name);
        }
    }

    // #### PR #22: pipeline compilation time; clear the driver cache to see
    // the cost of a first launch.
    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn engine_creation_time() {
        let started = Instant::now();
        let engine = WgpuPhotonEngine::new(0, 4096, 8).unwrap();
        eprintln!(
            "ENGINE_CREATED stages={:?} adapter={} seconds={:.1}",
            engine.stages,
            engine._adapter_name,
            started.elapsed().as_secs_f64()
        );
    }

    // Compile time of each shared stage pipeline (clear the driver cache first).
    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn shared_stages_pipeline_compile_times() {
        let engine = WgpuPhotonEngine::new(0, 4096, 8).unwrap();
        let module = engine
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Shared Rust stages"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(SHARED_STAGES_WGSL)),
            });
        let entries = PortableStages::Rust.entry_points();
        for entry in [
            entries.a,
            entries.b[0],
            entries.c1,
            entries.t2_prepare,
            entries.c2,
            entries.c3,
        ] {
            let started = Instant::now();
            let _pipeline = create_pipeline(&engine.device, &module, entry);
            eprintln!(
                "PIPELINE_COMPILED entry={entry} seconds={:.1}",
                started.elapsed().as_secs_f64()
            );
        }
    }

    // #### PR #22: per-stage GPU time of both stage sets on the same GPU and job.
    // Run with PICKAXE_WGPU_TIMESTAMPS=1; prints mean milliseconds per stage.
    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn stage_timing_comparison() {
        let table = m29_table::load_or_generate_m29_g16().unwrap();
        // PICKAXE_BENCH_NON_T2=1 times the non-T2 C2/C3 path instead.
        let non_t2 = std::env::var_os("PICKAXE_BENCH_NON_T2").is_some();
        let candidates = std::env::var("PICKAXE_BENCH_CANDIDATES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(if non_t2 {
                WGPU_REFERENCE_MAX_BATCH
            } else {
                WGPU_T2_MAX_BATCH
            });
        let rounds: usize = std::env::var("PICKAXE_TRIAL_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        // Creation order decides which engine gets device memory first;
        // PICKAXE_BENCH_RUST_FIRST=1 reverses it to separate that from signing.
        let order = if std::env::var_os("PICKAXE_BENCH_RUST_FIRST").is_some() {
            [PortableStages::Rust, PortableStages::Wgsl]
        } else {
            [PortableStages::Wgsl, PortableStages::Rust]
        };
        let mut engines = order.map(|stages| {
            pollster::block_on(WgpuPhotonEngine::new_async(
                0,
                WGPU_T2_MAX_BATCH,
                8,
                table.clone(),
                stages,
            ))
            .unwrap()
        });
        assert!(
            engines[0].profile.is_some(),
            "set PICKAXE_WGPU_TIMESTAMPS=1"
        );
        let key = [0x11; 32];
        let mut target = [0; 32];
        target[0] = 1;
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(key).unwrap()).serialize();
        let template = tx::build_photon_template_bytes_for_deployment(
            &tx::TemplateParams {
                prev_tx_hash_hex: "11".repeat(32),
                prev_index: 0,
                age: 128,
                public_key_hex: hex::encode(public_key),
                target_hex: hex::encode(target),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 48_635_000,
                relay_fee_sats_per_kb: 1100,
                contract_token_amount: 2_100_000_000_000_000_000,
                reward_amount: 4_999_999_999_999,
                payout_locking: tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)
                    .unwrap(),
            },
            &crate::protocol::MAINNET_PHOTON,
        )
        .unwrap();
        for engine in &mut engines {
            engine.set_proof_rule(crate::protocol::ProofRule::Positive);
            engine.set_job(&template, &target, &key).unwrap();
            assert!(engine.t2_active);
            engine.t2_active = !non_t2;
            for _ in 0..2 {
                engine.search_batch(0, candidates).unwrap();
            }
        }
        // Stage slots: A, B parts (four for WGSL, one for Rust), C1, prepare, filter.
        let mut sums = [[0f64; 5]; 2];
        for round in 0..rounds {
            // Alternate which engine runs first, so neither always follows the other.
            for step in 0..2 {
                let index = (step + round) % 2;
                let engine = &mut engines[index];
                let base = (round as u32 + 1) * candidates;
                engine.search_batch(base, candidates).unwrap();
                let ms = &engine.last_stage_ms;
                let stages = [ms[0], ms[1] + ms[2] + ms[3] + ms[4], ms[5], ms[6], ms[7]];
                for (sum, value) in sums[index].iter_mut().zip(stages) {
                    *sum += value;
                }
            }
        }
        for (index, sum) in sums.iter().enumerate() {
            let mean = sum.map(|value| value / rounds as f64);
            eprintln!(
                "STAGE_TIMING stages={:?} adapter={} candidates={candidates} a={:.4} b={:.4} c1={:.4} signing={:.4} {}={:.4} {}={:.4} ms",
                engines[index].stages,
                engines[index]._adapter_name,
                mean[0],
                mean[1],
                mean[2],
                mean[0] + mean[1] + mean[2],
                if non_t2 { "c2" } else { "prepare" },
                mean[3],
                if non_t2 { "c3" } else { "filter" },
                mean[4],
            );
        }
    }

    /// Copies a GPU-only buffer prefix to the host (test builds add COPY_SRC).
    fn read_words(engine: &WgpuPhotonEngine, buffer: &wgpu::Buffer, words: usize) -> Vec<u32> {
        let staging = create_buffer(
            &engine.device,
            "test readback",
            words * 4,
            wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            false,
        );
        let mut encoder = engine
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, (words * 4) as u64);
        engine.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |result| result.unwrap());
        engine._instance.poll_all(true);
        let words = slice
            .get_mapped_range()
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        staging.unmap();
        words
    }

    /// Runs only the signing stages A, B and C1 for a batch.
    fn run_signing_stages(engine: &WgpuPhotonEngine, nonce_base: u32, count: u32) {
        let active = count.div_ceil(128) * 128;
        engine.queue.write_buffer(
            &engine.input_gpu,
            BASE_NONCE_OFFSET,
            &nonce_base.to_le_bytes(),
        );
        engine.queue.write_buffer(
            &engine.dispatch_params_gpu,
            0,
            &u32_words_to_le_bytes(&[active / 32, active, 0, 0]),
        );
        let mut encoder = engine
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut stage = |pipeline: &wgpu::ComputePipeline, bind: &wgpu::BindGroup, groups: u32| {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        };
        stage(&engine.stage_a, &engine.bind_a, active / 128);
        for (pipeline, bind) in engine.stage_b.iter().zip(&engine.bind_b) {
            stage(pipeline, bind, active / 32);
        }
        stage(&engine.stage_c1, &engine.bind_c1, active / 64);
        engine.queue.submit([encoder.finish()]);
    }

    /// The T2 job shared by the signing tests.
    fn signing_test_job() -> ([u8; 32], [u8; 32], Vec<u8>) {
        let key = [0x11; 32];
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(key).unwrap()).serialize();
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        let template = tx::build_photon_template_bytes_for_deployment(
            &tx::TemplateParams {
                prev_tx_hash_hex: "11".repeat(32),
                prev_index: 0,
                age: 128,
                public_key_hex: hex::encode(public_key),
                target_hex: hex::encode(target),
                signature_hex: "00".repeat(64),
                nonce: 0,
                contract_value_sats: 48_635_000,
                relay_fee_sats_per_kb: 1100,
                contract_token_amount: (1 << 50) + 7,
                reward_amount: (1 << 33) + 7,
                payout_locking: tx::cashaddr_to_p2pkh_locking(crate::config::DONATION_ADDRESS)
                    .unwrap(),
            },
            &crate::protocol::MAINNET_PHOTON,
        )
        .unwrap();
        (key, target, template)
    }

    // #### PR #22: checks each signing stage separately, for either signer.
    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn signing_stages_match_cpu_per_window() {
        let mut engine = WgpuPhotonEngine::new(0, 4096, 8).unwrap();
        let (key, target, template) = signing_test_job();
        engine.set_proof_rule(crate::protocol::MAINNET_PHOTON.proof_rule);
        engine.set_job(&template, &target, &key).unwrap();
        engine.t2_active = false;
        let windows = 128;
        run_signing_stages(&engine, 0x0102_0304, windows);
        let records = read_words(&engine, &engine._intermediate_gpu, windows as usize * 40);
        let signatures = read_words(&engine, &engine._signatures_gpu, windows as usize * 16);
        let words_be = |bytes: &[u8]| -> Vec<u32> {
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|w| u32::from_be_bytes(*w))
                .collect()
        };
        let limbs_be = |limbs: &[u32]| -> Vec<u8> {
            limbs.iter().rev().flat_map(|w| w.to_be_bytes()).collect()
        };
        let mut failures = Vec::new();
        for index in 0..windows as usize {
            let nonce = 0x0102_0304 + index as u32;
            let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
            let record = &records[index * 40..index * 40 + 40];
            if record[..8] != words_be(&message)[..] {
                failures.push(format!("{index}: message"));
                continue;
            }
            let signature = crypto::bch_schnorr_sign(&key, &message).unwrap();
            let k = limbs_be(&record[8..16]);
            if k != crypto::bch_rfc6979_nonce(&key, &message).unwrap() {
                failures.push(format!("{index}: k"));
                continue;
            }
            let r_point = PublicKey::from_secret_key(
                &SecretKey::from_secret_bytes(k.try_into().unwrap()).unwrap(),
            )
            .serialize_uncompressed();
            // Affine x of the Jacobian point the B stages produced.
            let field = |limbs: &[u32]| num_bigint::BigUint::from_bytes_be(&limbs_be(limbs));
            let p = num_bigint::BigUint::parse_bytes(
                b"fffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2f",
                16,
            )
            .unwrap();
            let z = field(&record[32..40]);
            let z2 = z.modpow(&(&p - 2u32), &p).pow(2) % &p;
            let x = field(&record[16..24]) * z2 % &p;
            if x != num_bigint::BigUint::from_bytes_be(&r_point[1..33]) {
                failures.push(format!("{index}: point"));
                continue;
            }
            if signatures[index * 16..index * 16 + 8] != words_be(&signature[..32])[..] {
                failures.push(format!("{index}: r"));
                continue;
            }
            if signatures[index * 16 + 8..index * 16 + 16] != words_be(&signature[32..])[..] {
                failures.push(format!("{index}: s"));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {windows} windows failed ({:?}): {:?}",
            failures.len(),
            engine.stages,
            &failures[..failures.len().min(8)]
        );
    }

    // #### PR #22: development check of the shared stages' arithmetic on GPU.
    // Needs `PICKAXE_BUILD_SHARED_STAGES=debug` output; skipped without it.
    #[test]
    #[ignore = "requires exclusive access to a physical GPU"]
    fn shared_stages_arithmetic_matches_integers() {
        use num_bigint::BigUint;
        let path = "artifacts/shared-gpu-proof/stages-debug/pickaxe_shared_stages.wgsl";
        let Ok(source) = std::fs::read_to_string(path) else {
            eprintln!("skip: build {path} first");
            return;
        };
        let engine = WgpuPhotonEngine::new(0, 128, 1).unwrap();
        let module = engine
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("debug stages"),
                source: wgpu::ShaderSource::Wgsl(Cow::Owned(source)),
            });
        let pipeline = create_pipeline(&engine.device, &module, "pickaxe_debug_arithmetic");
        let p = BigUint::parse_bytes(
            b"fffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2f",
            16,
        )
        .unwrap();
        let limbs = |v: &BigUint| -> Vec<u32> {
            let mut d = v.to_u32_digits();
            d.resize(8, 0);
            d
        };
        let number = |w: &[u32]| BigUint::new(w.to_vec());
        let digest = |tag: &str, i: u32| {
            BigUint::from_bytes_be(&<sha2::Sha256 as sha2::Digest>::digest(format!("{tag}{i}")))
                % &p
        };
        let point = |k: &BigUint| {
            let mut bytes = [0u8; 32];
            let be = k.to_bytes_be();
            bytes[32 - be.len()..].copy_from_slice(&be);
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(bytes).unwrap())
        };
        let cases = 64u32;
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for i in 0..cases {
            let (a, b) = (digest("a", i), digest("b", i));
            let (k, m) = (
                digest("k", i) % (&p >> 2) + 1u32,
                digest("m", i) % (&p >> 2) + 1u32,
            );
            let (pk, qm) = (point(&k), point(&m));
            let affine = |key: &PublicKey| {
                let raw = key.serialize_uncompressed();
                (
                    BigUint::from_bytes_be(&raw[1..33]),
                    BigUint::from_bytes_be(&raw[33..65]),
                )
            };
            let (px, py) = affine(&pk);
            let (qx, qy) = affine(&qm);
            let lambda = digest("z", i) + 1u32;
            let l2 = &lambda * &lambda % &p;
            input.extend(limbs(&a));
            input.extend(limbs(&b));
            input.extend(limbs(&(&px * &l2 % &p)));
            input.extend(limbs(&(&py * &l2 % &p * &lambda % &p)));
            input.extend(limbs(&lambda));
            input.extend(limbs(&qx));
            input.extend(limbs(&qy));
            let sum = affine(&pk.combine(&qm).unwrap());
            expected.push((
                &a * &b % &p,
                &a * &a % &p,
                (&a + &b) % &p,
                (&a + &p - &b) % &p,
                a.modpow(&(&p - 2u32), &p),
                sum,
            ));
        }
        let to_bytes = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let cases_gpu = create_buffer(
            &engine.device,
            "debug cases",
            input.len() * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            false,
        );
        engine.queue.write_buffer(&cases_gpu, 0, &to_bytes(&input));
        let results_gpu = create_buffer(
            &engine.device,
            "debug results",
            cases as usize * 64 * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            false,
        );
        let bind = create_bind_group(
            &engine.device,
            &pipeline,
            "debug",
            &[(0, &cases_gpu), (1, &results_gpu)],
        );
        let mut encoder = engine
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        engine.queue.submit([encoder.finish()]);
        let results = read_words(&engine, &results_gpu, cases as usize * 64);
        let mut failures = std::collections::BTreeMap::<&str, u32>::new();
        for (i, (mul, square, add, sub, inverse, (sx, sy))) in expected.iter().enumerate() {
            let r = &results[i * 64..i * 64 + 64];
            for (name, got, want) in [
                ("mul", number(&r[0..8]), mul),
                ("square", number(&r[8..16]), square),
                ("add", number(&r[16..24]), add),
                ("sub", number(&r[24..32]), sub),
                ("inverse", number(&r[32..40]), inverse),
            ] {
                if &got != want {
                    *failures.entry(name).or_default() += 1;
                }
            }
            let z = number(&r[56..64]);
            let zi = z.modpow(&(&p - 2u32), &p);
            let x = number(&r[40..48]) * &zi % &p * &zi % &p;
            let y = number(&r[48..56]) * &zi % &p * &zi % &p * &zi % &p;
            if (&x, &y) != (sx, sy) {
                *failures.entry("point add").or_default() += 1;
            }
        }
        assert!(failures.is_empty(), "failures out of {cases}: {failures:?}");
    }

    fn reference_template_with_target(target: [u8; 32]) -> [u8; TX_BYTES] {
        let raw = hex::decode(include_str!("../reference/photon_vector_tx.hex").trim()).unwrap();
        let mut template: [u8; TX_BYTES] = raw.try_into().unwrap();
        template[390..394].fill(0);
        template[394..426].copy_from_slice(&target);
        template[426..490].fill(0);
        template
    }

    fn assert_reconstructs(
        template: &[u8],
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

        let mut completed = template.to_vec();
        completed[390..394].copy_from_slice(&winner.nonce.to_le_bytes());
        completed[426..490].copy_from_slice(&signature);
        let expected = search::hash256(&completed);
        assert_eq!(winner.digest, expected);
        assert!(search::meets_target_le(&expected, target));
    }

    #[test]
    fn portable_shader_validates_current_layout_and_proof_fields_without_a_gpu() {
        let source = reference_shader_source_for_wgpu().unwrap();
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap();
        assert!(source.contains("words: array<u32, 158>"));
        for shift in PhotonLayout::GPU_SHIFTS {
            let layout = PhotonLayout::for_tx_len(615 + shift).unwrap();
            let target = [0x42; 32];
            let mut template = vec![0; layout.tx_bytes()];
            template[layout.target_offset()..layout.target_offset() + 32].copy_from_slice(&target);
            let mut key = [0; 32];
            key[31] = 1;
            validate_job_material(&template, &target, &key).unwrap();
            let packed = pack_big_endian_words(&template, TEMPLATE_WORDS);
            let byte_at = |i: usize| {
                let start = i / 4 * 4;
                let word = u32::from_le_bytes(packed[start..start + 4].try_into().unwrap());
                ((word >> ((3 - i % 4) * 8)) & 255) as u8
            };
            for (i, expected) in target.iter().enumerate() {
                assert_eq!(byte_at(394 + shift + i), *expected);
            }
            assert!(layout.tx_bytes() <= 631);
        }
    }

    // #### PR #22: optional DirectX 12 on Windows
    #[test]
    fn graphics_api_defaults_to_vulkan_and_offers_dx12_on_windows() {
        use wgpu::Backends;
        // (PICKAXE_WGPU_API, windows, macos)
        assert_eq!(
            native_wgpu_backends(None, false, false),
            Ok(Backends::VULKAN)
        );
        assert_eq!(
            native_wgpu_backends(None, true, false),
            Ok(Backends::VULKAN)
        );
        assert_eq!(
            native_wgpu_backends(Some(" "), true, false),
            Ok(Backends::VULKAN)
        );
        assert_eq!(
            native_wgpu_backends(Some("vulkan"), true, false),
            Ok(Backends::VULKAN)
        );
        assert_eq!(
            native_wgpu_backends(Some("dx12"), true, false),
            Ok(Backends::DX12)
        );
        assert!(native_wgpu_backends(Some("dx12"), false, false).is_err());
        assert!(native_wgpu_backends(Some("dx11"), true, false).is_err());
        assert_eq!(native_wgpu_backends(None, false, true), Ok(Backends::METAL));
        assert!(native_wgpu_backends(Some("dx12"), false, true).is_err());
        assert!(native_wgpu_backends(Some("vulkan"), false, true).is_err());
    }

    // #### PR #22: shared stages per shader translator
    #[test]
    fn shared_stages_follow_the_shader_translator() {
        use wgpu::Backends;
        // Compared by content: consts need not share an address.
        assert!(shared_stages_source(Backends::VULKAN) == SHARED_STAGES_WGSL);
        for backends in [Backends::DX12, Backends::METAL, Backends::BROWSER_WEBGPU] {
            assert!(shared_stages_source(backends) == SHARED_STAGES_LOOP_COPIES_WGSL);
        }
        let chrome = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36";
        assert!(user_agent_uses_tint(chrome));
        assert!(user_agent_uses_tint(&format!("{chrome} Edg/154.0.0.0")));
        assert!(!user_agent_uses_tint(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) Gecko/20100101 Firefox/150.0"
        ));
        assert!(!user_agent_uses_tint("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15"));
        assert!(!user_agent_uses_tint("Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/154.0.0.0 Mobile/15E148 Safari/604.1"));
        // The copy is the original plus loop-value copies, nothing else.
        let mut copies = 0;
        let restored: Vec<String> = SHARED_STAGES_LOOP_COPIES_WGSL
            .lines()
            .filter(|line| {
                let copy = line.contains("_late = ")
                    || (line.trim_start().starts_with("var _e") && line.contains("_late:"));
                copies += usize::from(copy);
                !copy
            })
            .map(|line| line.replace("_late", ""))
            .collect();
        assert!(copies > 0);
        assert_eq!(restored, SHARED_STAGES_WGSL.lines().collect::<Vec<_>>());
        // Both copies parse and validate as WGSL.
        for source in [SHARED_STAGES_WGSL, SHARED_STAGES_LOOP_COPIES_WGSL] {
            let module = naga::front::wgsl::parse_str(source).unwrap();
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap();
        }
    }

    #[test]
    fn portable_stages_default_to_shared_rust_and_keep_wgsl_selectable() {
        assert_eq!(PortableStages::parse(None), Ok(PortableStages::Rust));
        assert_eq!(PortableStages::parse(Some(" ")), Ok(PortableStages::Rust));
        assert_eq!(
            PortableStages::parse(Some("rust")),
            Ok(PortableStages::Rust)
        );
        assert_eq!(
            PortableStages::parse(Some("wgsl")),
            Ok(PortableStages::Wgsl)
        );
        assert!(PortableStages::parse(Some("cuda")).is_err());
        assert_eq!(PortableStages::default(), PortableStages::Rust);
        // Every entry point either set names exists in its shader.
        let reference = reference_shader_source_for_wgpu().unwrap();
        for (stages, source) in [
            (PortableStages::Wgsl, reference.as_str()),
            (PortableStages::Rust, SHARED_STAGES_WGSL),
            (PortableStages::Rust, SHARED_STAGES_LOOP_COPIES_WGSL),
        ] {
            let entries = stages.entry_points();
            for entry in entries.b.iter().copied().chain([
                entries.a,
                entries.c1,
                entries.t2_prepare,
                entries.c2,
                entries.c3,
            ]) {
                assert!(
                    source.contains(&format!("fn {entry}(")),
                    "{stages:?} lacks {entry}"
                );
            }
        }
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
        engine.t2_active = false; // Retain coverage of the reference fallback.

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
        assert_eq!(
            engine.persistent_device_bytes(),
            expected_bytes + t2_window_bytes(candidate_count) + 16
        );

        // Exercise actual serializer layouts, including both current deployments.
        let mut target = [0xff; 32];
        target[31] = 0x7f;
        let public_key =
            PublicKey::from_secret_key(&SecretKey::from_secret_bytes(private_key).unwrap())
                .serialize();
        for deployment in [
            crate::protocol::MAINNET_V0_PHOTON,
            crate::protocol::MAINNET_PHOTON,
            crate::protocol::CHIPNET_PHOTON,
        ] {
            for age in [8, 17, 128, 32_768] {
                let Ok(layout) = PhotonLayout::for_age_with_deployment(age, &deployment) else {
                    continue;
                };
                let template = tx::build_photon_template_bytes_for_deployment(
                    &tx::TemplateParams {
                        prev_tx_hash_hex: "11".repeat(32),
                        prev_index: 0,
                        age,
                        public_key_hex: hex::encode(public_key),
                        target_hex: hex::encode(target),
                        signature_hex: "00".repeat(64),
                        nonce: 0,
                        contract_value_sats: 48_635_000,
                        relay_fee_sats_per_kb: 1_100,
                        contract_token_amount: 2_095_454_920_205_042,
                        reward_amount: 4_989_178_380,
                        payout_locking: tx::cashaddr_to_p2pkh_locking(
                            crate::config::DONATION_ADDRESS,
                        )
                        .unwrap(),
                    },
                    &deployment,
                )
                .unwrap();
                engine.set_proof_rule(deployment.proof_rule);
                engine.set_job(&template, &target, &private_key).unwrap();
                engine.t2_active = false;
                let actual = engine.search_batch(nonce_base, candidate_count).unwrap();
                let mut expected = std::collections::BTreeMap::new();
                for nonce in nonce_base..nonce_base + candidate_count {
                    let message = tx::photon_message_sha256(nonce, &hex::encode(target)).unwrap();
                    let signature = crypto::bch_schnorr_sign(&private_key, &message).unwrap();
                    assert!(crypto::bch_schnorr_verify(&public_key, &message, &signature).unwrap());
                    let mut completed = template.clone();
                    let offset = layout.nonce_offset();
                    completed[offset..offset + 4].copy_from_slice(&nonce.to_le_bytes());
                    let offset = layout.signature_offset();
                    completed[offset..offset + 64].copy_from_slice(&signature);
                    let digest = search::hash256(&completed);
                    if search::meets_target_le_for_rule(&digest, &target, deployment.proof_rule) {
                        expected.insert(nonce, digest);
                    }
                }
                assert_eq!(actual.total_winners as usize, expected.len());
                for winner in actual.winners {
                    assert_eq!(expected.get(&winner.nonce), Some(&winner.digest));
                }
            }
        }
    }
}
