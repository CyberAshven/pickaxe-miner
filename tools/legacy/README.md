# Legacy tools

One-off scripts from earlier experiments. Nothing in CI or the release
workflow uses them; they are kept so the experiments in
[`docs/experiments`](../../docs/experiments/README.md) can be reproduced.

| File | Used by |
|---|---|
| `build-ultrafast-candidate.cmd`, `ultrafast-bchn-probe.def`, `ultrafast-v4.6.0-compat.patch` | [UltrafastSecp256k1 engine evaluation](../../docs/experiments/ultrafast-engine-evaluation.md) |
| `compare-rust-t2.py` | [Rust T2 experiment](../../docs/experiments/rust-t2-experiment.md) |
| `cuda-soak.ps1` | Long CUDA soak runs |
| `package-t2-linux.sh` | Early Linux T2 packaging |

Scripts that resolve the repository root were updated for this folder.
