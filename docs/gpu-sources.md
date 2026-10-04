# GPU code map

Where every GPU kernel lives, what builds it and where it runs. Nothing here
is deleted when a newer path replaces it: older code stays buildable and
selectable until its replacement is proven on hardware.

## Engines and defaults

| GPU | Engine | Default kernels | Alternative |
|---|---|---|---|
| NVIDIA | CUDA | Shared Rust engine: `cuda/build/photon_rust.ptx` | CUDA C++ kernels: build with `--no-default-features --features tail-grind` |
| AMD Radeon RX 6000, 7000, 9000 | HIP | CUDA C++ kernels compiled for HIP: `hip/build/<arch>/*.hsaco` | Shared Rust engine: set `PICKAXE_HIP_KERNELS=rust` to load `hip/build/<arch>/photon_rust.hsaco` |
| Other AMD GPUs, Intel GPUs, integrated GPUs | WGPU on Vulkan | Shared Rust T2 filter (`reference/shared-t2/`) and the original PHOTON WebGPU signing stages (`reference/photon-miner.wgsl`) | None yet |
| Apple Silicon | WGPU on Metal | Same as Vulkan | None yet |
| Browser | WGPU on WebGPU | Same as Vulkan | None yet |

Automatic selection takes the first discrete GPU from CUDA, then HIP (when
code objects for its architecture are installed), then WGPU. An integrated GPU
mines automatically only when no discrete GPU is present; `--device` selects
any GPU, and `--backend` forces an engine. Native HIP refuses integrated GPUs
under Windows, where their launches never complete.

## Sources

| Path | Contents | Built by | Output |
|---|---|---|---|
| `rust-engine/` | Shared Rust GPU engine: SHA-256, the T2 search, RFC6979 and BCH Schnorr signing | `tools/build-rust-kernels.ps1` or `.sh` (NVIDIA); `tools/build_rust_amd.py --arch <gfx>` (AMD); `tools/shared-gpu-proof/sync_filter.py --write` (portable T2 WGSL) | `cuda/build/photon_rust.ptx`; `hip/build/<arch>/photon_rust.hsaco`; `reference/shared-t2/*.wgsl` |
| `cuda/` | CUDA C++ kernels: the legacy NVIDIA build, the default AMD HIP build and the value-grind diagnostic | `tools/build-ptx.sh` (PTX); `tools/build_hip.py --arch <gfx>` through the `hip/*.hip.cpp` wrappers | `cuda/build/*.ptx`; `hip/build/<arch>/*.hsaco` |
| `hip/` | HIP wrappers and kernel contracts | | `kernel_contract.json` (C++ set), `rust_kernel_contract.json` (Rust set) |
| `reference/photon-miner.wgsl` | The original PHOTON WebGPU miner; the portable engine uses its signing stages | Included by `src/wgpu_photon.rs` | |
| `reference/shared-t2/` | WGSL generated from the shared Rust T2 filter; never edited by hand | `tools/shared-gpu-proof/sync_filter.py --write` | |
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
- **Portable T2:** `reference/shared-t2/SHA256SUMS` covers the shared Rust
  sources and the generated WGSL. Builds verify it, and CI regenerates the
  files and rejects hand edits.
- **Kernel logic:** the C++ kernels run on NVIDIA through the legacy build's
  GPU tests, and the shared Rust engine through the default build's GPU tests.
  The HIP and portable engines have vector tests that run wherever their
  hardware is present.

## Not yet run on hardware

Discrete AMD GPUs (both HIP builds), Intel GPUs, Linux GPUs and Apple Silicon
execution.
