# Browser T2 validation

#### PR #22

The portable engine reuses each BCH Schnorr signature across 65,536 token-amount
pairs, as the CUDA T2 engine does. Shared Rust checks transaction eligibility,
conserved amounts, recipient work allocation, fees and every returned winner.
The TypeScript interface only handles browser I/O and display. Browser intensity
starts at 50%, changes live, and displays a time-weighted rate once per second.

## Local evidence

RTX 5070 Ti Laptop GPU, NVIDIA driver 616.92, Brave WebGPU, intensity 100:

| Complete pipeline trial | Seconds | Million valid candidates/second |
| --- | ---: | ---: |
| 1 | 30.0139 | 778.128 |
| 2 | 30.0168 | 783.616 |
| 3 | 30.0004 | 771.182 |

These offline trials use the actual browser WASM and normal key and recipient
rotation. Work is counted once. They do not include live network waits and are
not a native CUDA speed claim. WASM SHA-256:
`4e53229e74aeb16d8beb65c2efb673378c09321f5e9387844e4073d7fa2117df`.

- 6,256 independent GPU signature, serialization and digest checks covered
  deployments, layouts, carries, partial windows and key/job rotation.
- A 4,194,432-candidate dispatch matched serial chunks and 1,076 independent
  winner reconstructions.
- Five synthetic T2 transactions passed BCH 2026 standard and consensus VM
  checks across recipients and layouts; tampered rewards were rejected.
- One full work cycle recorded 805,306,368 miner candidates and 16,777,216 for
  each donation recipient, preserving the compiled 96/2/2 allocation.
- Host-only suite: 264 passed, none failed, one ignored and 65 GPU tests filtered.
  Strict TypeScript and ten browser transport/submission checks passed.

Physical AMD and Apple performance comparisons remain separate requirements.
Mac compilation is not proof of Apple GPU mining.

## Mainnet acceptance and current dev validation

Source `ddb6159fed37fb16f86d445cfffc579257f5a469` mined one accepted
mainnet reward on 2026-10-04. Two independent Electrum servers returned the
exact submitted transaction. Independent Libauth BCH 2026 standard and
consensus VM checks passed, with the entered miner recipient and conserved
token reward verified; an edited reward failed. The transaction used one
input, two outputs and 629 sats for 629 bytes at the observed relay floor.
There was no extra funding input or split payment. The browser ended with
one accepted reward, no pending reward and no page errors.

The tested WASM SHA-256 was
`434a9966e8c096e42c7f6992c136bbb75a1bf45e8f69c27144cd9d3b24d0ca78`.
Three 30-second offline complete-pipeline trials on that build measured
910.305, 894.552 and 880.051 million candidates/second. These trials were
not interleaved with the older build, so they do not establish a speedup.
The actual full recipient work cycle and five independent covenant VM samples
also passed on this build. Shared GPU source migration remains unfinished;
see `tools/shared-gpu-proof/README.md` for the separate compiler proof.

Temporary local telemetry verified continued work while the page was hidden.
The telemetry hook is not included in distributed files. Browser discard and
operating-system sleep remain distinct from turning off the screen.
