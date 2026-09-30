# CUDA files

The default CUDA T2 backend is Rust. Build `photon_rust.ptx` with the pinned
compiler described in the root README, then build the executable:

```bash
cargo build --locked --release
```

For the legacy CUDA C++ backend:

```bash
cargo build --locked --release --no-default-features --features tail-grind
```

Keep the `cuda/build` directory supplied in the release archive beside the executable. The bundled PTX targets NVIDIA `sm_120`; other architectures require compatible files and separate validation.

Legacy CUDA C++ source files are in this directory; Rust kernels are in `rust-engine/`. `tools/build-ptx.sh` builds the NVIDIA PTX artifacts on a system with a compatible CUDA toolkit.
