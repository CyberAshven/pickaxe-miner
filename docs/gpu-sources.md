# GPU code map

Where every GPU kernel lives, what builds it and where it runs. Nothing here
is deleted when a newer path replaces it: older code stays buildable and
selectable until its replacement is proven on hardware.

## Names

- **wgpu**: the open-source Rust graphics library (gfx-rs) behind the
  portable engine and the `--backend wgpu` option. It is not a GPU API itself.
- **Vulkan** and **Metal**: the native GPU APIs wgpu runs on: Vulkan on
  Windows and Linux, Metal on macOS.
- **WebGPU**: the W3C browser GPU API. The **browser miner** runs the same
  engine through it; native builds never use WebGPU.
- **WGSL**: the WebGPU Shading Language, the standard language every
  portable shader is written or generated in.

## Engines and defaults

| GPU | Engine | Default kernels | Alternative |
|---|---|---|---|
| NVIDIA | CUDA | Shared Rust engine: `cuda/build/photon_rust.ptx` | CUDA C++ kernels: build with `--no-default-features --features tail-grind` |
| AMD Radeon RX 6000, 7000, 9000 | HIP | CUDA C++ kernels compiled for HIP: `hip/build/<arch>/*.hsaco` | Shared Rust engine: set `PICKAXE_HIP_KERNELS=rust` to load `hip/build/<arch>/photon_rust.hsaco` |
| Other AMD GPUs, Intel GPUs, integrated GPUs | wgpu on Vulkan | Shared Rust signer (`reference/shared-signer/`) and T2 filter (`reference/shared-t2/`) | Original PHOTON WGSL signing stages: set `PICKAXE_WGPU_SIGNER=wgsl` |
| Apple Silicon | wgpu on Metal | Same as Vulkan | Same as Vulkan |
| Browser miner | wgpu on WebGPU | Same as Vulkan | Development builds only (`verify_portable_engine`) |

Automatic selection takes the first discrete GPU from CUDA, then HIP (when
code objects for its architecture are installed), then WGPU. An integrated GPU
mines automatically only when no discrete GPU is present; `--device` selects
any GPU, and `--backend` forces an engine. Native HIP refuses integrated GPUs
under Windows, where their launches never complete.

## Sources

| Path | Contents | Built by | Output |
|---|---|---|---|
| `rust-engine/` | Shared Rust GPU engine: SHA-256, the T2 search, RFC6979 and BCH Schnorr signing | `tools/build-rust-kernels.ps1` or `.sh` (NVIDIA); `tools/build_rust_amd.py --arch <gfx>` (AMD); `tools/shared-gpu-proof/sync_filter.py --write` (portable WGSL) | `cuda/build/photon_rust.ptx`; `hip/build/<arch>/photon_rust.hsaco`; `reference/shared-t2/*.wgsl`; `reference/shared-signer/*.wgsl` |
| `cuda/` | CUDA C++ kernels: the legacy NVIDIA build, the default AMD HIP build and the value-grind diagnostic | `tools/build-ptx.sh` (PTX); `tools/build_hip.py --arch <gfx>` through the `hip/*.hip.cpp` wrappers | `cuda/build/*.ptx`; `hip/build/<arch>/*.hsaco` |
| `hip/` | HIP wrappers and kernel contracts | | `kernel_contract.json` (C++ set), `rust_kernel_contract.json` (Rust set) |
| `reference/photon-miner.wgsl` | The original PHOTON web miner, hand-written in WGSL; the portable engine keeps its C2/C3 and T2 preparation stages, and its signing stages as the alternative | Included by `src/wgpu_photon.rs` | |
| `reference/shared-t2/` | WGSL generated from the shared Rust T2 filter; never edited by hand | `tools/shared-gpu-proof/sync_filter.py --write` | |
| `reference/shared-signer/` | WGSL generated from the shared Rust signer (stages A, B and C1); never edited by hand | `tools/shared-gpu-proof/sync_filter.py --write` | |
| `src/wgpu_t2.wgsl`, `src/wgpu_target.wgsl` | Portable-engine glue around the shared filter | Included by `src/wgpu_photon.rs` | |
| `reference/legacy-engine-by-cyberashven/` | Historic engine snapshot | Not built | |

## Checks that keep the builds equal

- **NVIDIA:** `photon_rust.ptx` must stay byte-identical unless a change means
  to alter it. Build it with the pinned compiler and compare its SHA-256; the
  CI "Rust PTX" jobs build it on every pull request.
- **AMD:** every code object is verified against its contract by
  `tools/verify_hip_artifacts.py` (`--contract` selects the Rust set). The C++
  set is built twice and must be byte-identical. CI builds both sets for every
  architecture, and releases ship both.
- **Portable WGSL:** `reference/shared-t2/SHA256SUMS` and
  `reference/shared-signer/SHA256SUMS` cover the shared Rust sources and the
  generated WGSL. Builds verify them, and CI regenerates the files and rejects
  hand edits. Portable-only helpers (`rust-engine/src/sign.rs`) are excluded
  from native GPU builds, and native code expands to its original form, so
  `photon_rust.ptx` stays byte-identical.
- **Portable signing:** `signing_stages_match_cpu_per_window` checks each
  signing stage's output against the CPU; `signer_stage_timing_comparison`
  times both signers per stage on the same GPU. Software Vulkan CI runs the
  T2 end-to-end oracle with both signers.
- **Kernel logic:** the C++ kernels run on NVIDIA through the legacy build's
  GPU tests, and the shared Rust engine through the default build's GPU tests.
  The HIP and portable engines have vector tests that run wherever their
  hardware is present.

## Portable signer measurements

Per T2 batch of 33,554,432 candidates (512 signatures), mean GPU time:

| GPU | Original WGSL signing | Shared Rust signing | Whole batch |
|---|---|---|---|
| RTX 5070 Ti Laptop GPU (Vulkan) | 3.4 ms | 0.9 ms | about 7% shorter |
| Integrated Radeon gfx1036 (Vulkan) | 90 ms | 2.6 ms | about 8% shorter |

With an empty driver cache, the Radeon creates the engine in 9.7 s with the
shared signer and 17.0 s with the original. Chrome on the same Radeon passes
the browser engine check with the shared signer; the original stages failed
there with a lost buffer mapping.

## Not yet run on hardware

Discrete AMD GPUs (both HIP builds), Intel GPUs, Linux GPUs, Apple Silicon
execution and the browser miner on NVIDIA.
