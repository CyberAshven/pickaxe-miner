# CUDA files

The release build uses the `tail-grind` feature. Build the executable with:

```bash
cargo build --locked --release --features tail-grind
```

Keep the `cuda/build` directory supplied in the release archive beside the executable. The bundled PTX targets NVIDIA `sm_120`; other architectures require compatible files and separate validation.

CUDA source files are in this directory. `tools/build-ptx.sh` builds the NVIDIA PTX artifacts on a system with a compatible CUDA toolkit.
