# Implementation status

What each part of Pickaxe does today, how it has been checked, and what still
waits. Each row links to the guide for that part.

## Checked

| Part | Checked with |
|---|---|
| PHOTON on GPUs ([quick start](../README.md#quick-start-mainnet-tui)) | Claims confirmed on Mainnet and Chipnet; CUDA (RTX 5070 Ti) and the portable engine (an AMD integrated GPU) together in a 20-minute run |
| ELECTRIC and its fee pot ([ELECTRIC](electric-token.md)) | Claims confirmed on Mainnet and Chipnet; the pot's recovery key and signed refunds in host and covenant tests |
| Hash split ([hash split](hash-split.md)) | PHOTON and ELECTRIC sharing the GPUs in one session |
| GPU farms: coordinator and rigs ([farms](farm.md)) | Live two-rig Chipnet runs with hundreds of claimed winners; coordinator crashes during claims; failover between coordinators on one PC and across two |
| ASIC server, Stratum V1 and V2 ([ASIC mining](stratum-v2.md)) | An Avalon Nano 3 finding Chipnet blocks through the SV1 adapter; the reference SV2 device and translator in CI; per-device unique work, share checks, stale and duplicate refusal |
| Templates from BCHN | Full templates, pinned submission and accepted Chipnet blocks |
| Knuth as the node ([node requirements](node-requirements.md)) | Knuth 1.7.0 on Chipnet, read-only: templates, the PHOTON baton read through the node and matching Fulcrum's, transaction lookup without an index, the claim-fee floor |
| Pool modes ([pools](pool.md), [P2Pool](p2pool.md)) | Live on Chipnet with P2Pool v2: each miner's address seen separately; Job Declaration and custom jobs |
| Desktop window ([desktop window](desktop-window.md)) | Offscreen renders of every screen; the same setup and sessions as the terminal |
| Updates ([updates](updates.md)) | Signature checks against minisign's published vectors, a tampered sums file or archive, size and redirect limits; install, apply and roll back in temporary folders |
| Releases | Windows, Linux (x86_64, ARM64, `.deb`), macOS, the browser miner, the mining OS packages and the Docker image, built and checked by the release workflow |

## Waiting

| Part | Waits on |
|---|---|
| Knuth's own SV2 Template Provider | Its Noise transport (k-nuth/kth#807); Pickaxe takes templates over encrypted links only |
| GPU tokens and ELECTRIC's pot from Knuth alone, without Fulcrum | `scantxoutset` in Knuth (k-nuth/kth#1092) |
| ASIC tokens such as SAFA, by merge mining ([merge mining](merge-mining.md)) | The tokens' contracts being deployed; until then the production list is empty and tSAFA tests it on Chipnet |
| Native SV2 ASIC firmware | Hardware to test with; SV1 firmware is covered through the adapter |
| Intel, discrete AMD, Linux and Apple GPUs | Runs on that hardware; builds alone are not counted |
| Automatic updates | Releases signed with Pickaxe's key; until then each update asks first |
| The update's restart on Windows | An end-to-end run on a real install |
| ELECTRIC: a lost race keeping its pot, a top-up after a restart, a live refund | Funded live runs |
