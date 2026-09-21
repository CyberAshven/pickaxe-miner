# PHOTON reward-policy BCH 2026 VM proof

This deterministic harness proves the protocol-compatible Pickaxe 98%/2% reward path against Libauth's BCH 2026 VM.

The authoritative PHOTON mining transaction stays at exactly two outputs: renewed baton at output 0 and the single PHOTON reward at output 1. The proof spends output 1 in an immediate child transaction together with a BCH-only sponsor-reserve covenant. The child enforces the miner token amount, the exact 2% donation token amount and hard-coded donation locking bytecode, fixed BCH values for both token outputs, exact sponsor-reserve consumption, and the sponsor's next-baton state.

The fixture is based on `reference/photon_vector_tx.hex`. It validates the positive transaction in both BCH 2026 VM modes, checks CashToken conservation, and requires seven adversarial mutations to fail. It also reads `src/config.rs` and fails if the Rust donation basis points or address drift from the proof.

Run:

```text
npm ci
npm test
```

This harness is offline. It does not fund the sponsor covenant, query Fulcrum or a node, use production keys, or broadcast any transaction. Production win handling remains gated until the proven child path is wired to a real sponsor-reserve UTXO and the normal fresh-baton checks.
