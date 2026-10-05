# GPU code map

Where every GPU kernel lives, what builds it and where it runs. Nothing here
is deleted when a newer path replaces it: older code stays buildable and
selectable until its replacement is proven on hardware.

## Names

- **wgpu**: the open-source Rust graphics library (gfx-rs) behind the
  portable engine and the `--backend wgpu` option. It is not a GPU API itself.
- **Vulkan**, **DirectX 12** and **Metal**: the native GPU APIs wgpu runs on:
  Vulkan on Windows and Linux, optionally DirectX 12 on Windows, Metal on macOS.
- **WebGPU**: the W3C browser GPU API. The **browser miner** runs the same
  engine through it; native builds never use WebGPU.
- **WGSL**: the WebGPU Shading Language, the standard language every
  portable shader is written or generated in.
- **naga**: wgpu's shader translator, from WGSL to SPIR-V for Vulkan, HLSL for
  DirectX 12 and MSL for Metal. Firefox's WebGPU uses it too; Chromium uses its
  own translator, Tint.

## Engines and defaults

| GPU | Engine | Default kernels | Alternative |
|---|---|---|---|
| NVIDIA | CUDA | Shared Rust engine: `cuda/build/photon_rust.ptx` | CUDA C++ kernels: build with `--no-default-features --features tail-grind` |
| AMD Radeon RX 6000, 7000, 9000 | HIP | CUDA C++ kernels compiled for HIP: `hip/build/<arch>/*.hsaco` | Shared Rust engine: set `PICKAXE_HIP_KERNELS=rust` to load `hip/build/<arch>/photon_rust.hsaco` |
| Other AMD GPUs, Intel GPUs, integrated GPUs | wgpu on Vulkan | Shared Rust stages (`reference/shared-stages/`) and T2 filter (`reference/shared-t2/`) | Original hand-written WGSL stages: set `PICKAXE_WGPU_STAGES=wgsl` |
| Windows GPUs whose Vulkan driver fails | wgpu on DirectX 12: set `PICKAXE_WGPU_API=dx12` and add Microsoft's DXC ([portable builds](portable.md)) | Same as Vulkan, with the shared stages' DirectX 12/Metal copy | Same as Vulkan |
| Apple Silicon | wgpu on Metal | Same as Vulkan, with the shared stages' DirectX 12/Metal copy | Same as Vulkan |
| Browser miner | wgpu on browser WebGPU | Same as Vulkan; browsers other than Chromium use the DirectX 12/Metal copy | Development builds only (`verify_portable_engine`) |

Automatic selection takes the first discrete GPU from CUDA, then HIP (when
code objects for its architecture are installed), then WGPU. An integrated GPU
mines automatically only when no discrete GPU is present; `--device` selects
any GPU, and `--backend` forces an engine. Native HIP refuses integrated GPUs
under Windows, where their launches never complete.

## Sources

| Path | Contents | Built by | Output |
|---|---|---|---|
| `rust-engine/` | Shared Rust GPU engine: SHA-256, the T2 search, RFC6979 and BCH Schnorr signing | `tools/build-rust-kernels.ps1` or `.sh` (NVIDIA); `tools/build_rust_amd.py --arch <gfx>` (AMD); `tools/shared-gpu-proof/sync_filter.py --write` (portable WGSL) | `cuda/build/photon_rust.ptx`; `hip/build/<arch>/photon_rust.hsaco`; `reference/shared-t2/*.wgsl`; `reference/shared-stages/*.wgsl` |
| `cuda/` | CUDA C++ kernels: the legacy NVIDIA build, the default AMD HIP build and the value-grind diagnostic | `tools/build-ptx.sh` (PTX); `tools/build_hip.py --arch <gfx>` through the `hip/*.hip.cpp` wrappers | `cuda/build/*.ptx`; `hip/build/<arch>/*.hsaco` |
| `hip/` | HIP wrappers and kernel contracts | | `kernel_contract.json` (C++ set), `rust_kernel_contract.json` (Rust set) |
| `reference/photon-miner.wgsl` | The original PHOTON web miner, hand-written in WGSL; its stages are the alternative (`PICKAXE_WGPU_STAGES=wgsl`) | Included by `src/wgpu_photon.rs` | |
| `reference/shared-t2/` | WGSL generated from the shared Rust T2 filter; never edited by hand | `tools/shared-gpu-proof/sync_filter.py --write` | |
| `reference/shared-stages/` | WGSL generated from the shared Rust stages: signing (A, B, C1), T2 preparation and the non-T2 hash and winner stages (C2, C3); never edited by hand. `pickaxe_shared_stages.wgsl` runs on Vulkan and Chromium; `pickaxe_shared_stages_dx12_metal.wgsl`, the same code with loop values kept for naga's HLSL and MSL writers, runs on DirectX 12, Metal and other browsers | `tools/shared-gpu-proof/sync_filter.py --write` | |
| `src/wgpu_t2.wgsl`, `src/wgpu_target.wgsl` | Glue for the original stages: T2 preparation and the target check | Included by `src/wgpu_photon.rs` | |
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
  `reference/shared-stages/SHA256SUMS` cover the shared Rust sources and the
  generated WGSL. Builds verify them, and CI regenerates the files and rejects
  hand edits. Portable-only helpers (`rust-engine/src/sign.rs`, `window.rs`)
  are excluded from native GPU builds, and native code expands to its original
  form, so `photon_rust.ptx` stays byte-identical.
- **DirectX 12 and Metal translation:** naga, wgpu's shader translator, writes
  a WGSL loop's `continuing` block at the top of the next iteration for HLSL
  (DirectX 12) and MSL (Metal), and recomputes loop-body values that block uses
  after the block's own assignments. Where such a value loads a variable
  written in between, the translated loop reads the new contents: the shared
  field inverse tested the dummy value it had just stored, never left its loop,
  and the stage produced nothing. `sync_filter.py` also writes
  `pickaxe_shared_stages_dx12_metal.wgsl`, a copy in which each such value is
  stored in a variable where it is computed and the continuing block reads the
  variable (15 loop conditions; the T2 filters have none). DirectX 12, Metal
  and non-Chromium browsers run that copy. SPIR-V (Vulkan) and Tint (Chromium)
  were already correct and keep the original shader byte-identical; on Vulkan
  the copy cost the RTX 5070 Ti about 0.9% more C1 time.
- **Portable stages:** `signing_stages_match_cpu_per_window` checks each
  signing stage's output against the CPU; the T2 and non-T2 end-to-end tests
  reconstruct every candidate; `stage_timing_comparison` times both stage sets
  per stage on the same GPU. Software Vulkan CI runs the T2 end-to-end oracle
  with both sets.
- **Kernel logic:** the C++ kernels run on NVIDIA through the legacy build's
  GPU tests, and the shared Rust engine through the default build's GPU tests.
  The HIP and portable engines have vector tests that run wherever their
  hardware is present.

## Portable stage measurements

Per T2 batch of 33,554,432 candidates (512 signatures), mean GPU time over
interleaved batches on the same GPU:

| GPU | Signing, original WGSL | Signing, shared Rust | T2 preparation | Whole batch |
|---|---|---|---|---|
| RTX 5070 Ti Laptop GPU (Vulkan) | 3.39 ms | 0.91 ms | 0.048 → 0.066 ms | about 7% shorter |
| Integrated Radeon gfx1036 (Vulkan) | 90.7 ms | 2.6 ms | 0.136 → 0.100 ms | about 8% shorter |

The non-T2 path signs every candidate: for 16,384 candidates the Radeon
spends 2,758 ms signing and 5.7 ms hashing with the original stages, 58.7 ms
and 1.3 ms with the shared stages. The original C1 dispatch alone exceeds
Windows' two-second GPU timeout well before the 524,288-candidate maximum.

With an empty driver cache the Radeon creates the engine in 7.1 s with the
shared stages and 13.4 s with the original; the shared stages compile in
about 5 s and the original 34k-line module is not parsed. The browser engine
check (6,256 candidates) passes in Chrome with the shared stages on both GPUs:
in 24 s on the Radeon and 37 s on the RTX 5070 Ti, mostly shader compilation.
The original stages pass on the RTX 5070 Ti but failed on the Radeon with a
lost buffer mapping.

## Not yet run on hardware

Discrete AMD GPUs (both HIP builds), Intel GPUs, Linux GPUs and Apple Silicon
execution.
