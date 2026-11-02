# Documentation

## Guides

- [Development and branch flow](development.md)
- [GPU code map](gpu-sources.md): every GPU kernel, what builds it and where it runs, and which GPUs mine
- [Portable builds](portable.md): Apple Silicon, the browser miner and optional DirectX 12 on Windows
- [Browser T2](browser-t2.md)
- [PHOTON protocol profile](protocol-profile.md)
- [ELECTRIC](electric-token.md): native GPU mining, signed fee-pot claims, funding QR and owner recovery on Mainnet and Chipnet
- [Token hash split](hash-split.md): PHOTON and ELECTRIC in one GPU session, with per-token shares, donations and dashboard details
- [Fee policy](fee-policy.md)
- [Rust dependency security](rust-security.md)
- [Incremental-k search](incremental-k.md) (optional CUDA build feature)
- [Running a pool](pool.md): an ASIC or GPU pool for other miners, as a service
- [BCH P2Pool, managed](p2pool.md): P2Pool installed from its releases, kept updated and run by Pickaxe, with lights for P2Pool and the node
- [BCH ASIC mining (SV1/SV2)](stratum-v2.md): solo and pool connections, native-node templates and Template Distribution
- [GPU farms](farm.md): rigs on many machines mining as one
- [HiveOS, mmpOS and RaveOS](farm-os.md): the custom-miner packages for farm systems
- [Merge-mining BCH tokens](merge-mining.md): the v2 common format, upcoming production tokens and explicit Chipnet tSAFA test support
- [ASIC-exclusive tokens](asic-tokens.md): header-shaped token jobs (SAFA layout v1, draft), the forwarder convention and findings for token authors
- [PHOTON as an AuxPoW parent](photon-electric-auxpow.md): PHOTON's live contract can carry an ELECTRIC commitment in its payout; what ELECTRIC and Pickaxe would need
- [Job Declaration](job-declaration.md): miners' own templates at a Pickaxe pool (Full-Template and Coinbase-only)

## Planning

- [Definition of done](DEFINITION_OF_DONE.md)
- [GPU ship path](GPU_SHIP_PATH.md) (historical plan, 2026-09-20)
- [Next steps](next-steps.md): BCH Stratum V2 first, then GPU scaling
- [Architecture and research](architecture.md): the end-to-end design and what exists in BCH mining

## Records

- [PHOTON fee hotfix](photon-fee-hotfix.md)
- [Implementation status](implementation-status.md): what each change does and its evidence
- [Experiments](experiments/README.md): engine evaluations and their raw results
