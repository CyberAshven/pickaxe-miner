# Rust T2 experiment

`perf/rust-t2` ports the v0.0.2 CUDA T2 mining algorithm to Rust. Enable it
with `--features rust-t2`; the default build remains the released CUDA C++ path.
This experiment builds on `pickaxe-miner-v0.0.2` (`1d34584`).

The Rust path includes signature preparation, point multiplication, transaction
hashing and winner filtering. It reuses the existing pinned `ufsecp-core` Rust
arithmetic port and Pickaxe's Rust kernels. Production CPU signing still uses
the existing `secp256k1` dependency. HIP and the optional value-grinding diagnostic
retain their native kernels. GPU pointer access and CUDA calls remain unsafe
boundaries; this is not a claim that the entire miner is pure safe Rust.

T2 comes from shrec's v0.0.2 implementation. The Rust port preserves its
transaction layouts, signature windows, reward arithmetic and bounded winner
buffers. It calculates the first ten fixed hash rounds once per signature
window, then uses the pre-expanded middle-block schedule for every candidate.

## Reproduce

Use stable Rust for host code and `nightly-2026-04-02` with `rust-src`,
`llvm-tools-preview` and `llvm-bitcode-linker` for PTX. Select your GPU architecture;
the recorded hardware run uses `sm_120`.

```powershell
./tools/build-rust-kernels.ps1 -Architecture sm_120
cargo build --locked --release --features rust-t2
cargo test --locked --manifest-path rust-engine/Cargo.toml
cargo test --locked --release --features rust-t2 -- --test-threads=1 --skip if_wgpu_present
```

For a standalone candidate, place the built executable beside
`cuda/build/photon_rust.ptx`. Keep the baseline release in a separate directory.
Stop live mining before GPU tests or benchmarks and restart it afterward.
Only one miner or benchmark should use the GPU at a time.

```powershell
python tools/compare-rust-t2.py PATH_TO_RELEASE_EXE PATH_TO_RUST_EXE
```

The script warms both implementations for 15 seconds each, then runs six
20-second trials in C++ / Rust / Rust / C++ / C++ / Rust order. It saves full
reports, telemetry and executable/PTX hashes under `artifacts/rust-t2-comparison`.
This measures the offline GPU mining pipeline, including preparation and
readback, without network access or broadcasts. It does not measure live payout
frequency. GPU clocks and cooling can affect the result.

## Validation

The candidate passed 24 serial CUDA tests, 247 non-GPU host tests and
four Rust arithmetic tests. GPU checks reconstruct transactions independently
on the host, cover all four age layouts, signature-window crossings, nonce
wraparound, bounded readback and a complete 16,777,216-candidate group. Formatting
and host Clippy with all features passed. Physical testing is limited to one
NVIDIA GPU; no cross-hardware performance or live Rust settlement claim is made.

## Measured result

Windows, RTX 5070 Ti Laptop GPU, NVIDIA driver 616.92, intensity 100. Each row
contains three trials per backend, with the order and warmup described above.

| Rust candidate | Released C++ mean | Rust mean | Difference |
| --- | ---: | ---: | ---: |
| Initial port | 1,548.14 MH/s | 1,483.66 MH/s | -4.16% |
| Inlined group kernel (retained) | 1,538.05 MH/s | 1,475.62 MH/s | -4.06% |

The Rust port is currently slower. Inlining did not materially close the gap.
The initial Rust group kernels used 44-47 registers with no spills, compared
with 64 registers and no spills for C++; register pressure alone does not
explain the difference. Clocks were not locked, so these measurements do not
isolate compiler effects from hardware behavior.

Both versions passed the 24 GPU checks. Full timed reports, temperatures,
clocks and artifact hashes are in [rust-t2-results.json](rust-t2-results.json).
The released miner was restored after testing. This experiment changes neither
the default backend nor the release packaging.
