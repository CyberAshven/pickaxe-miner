# PHOTON reward-policy BCH 2026 VM proof

This developer-only deterministic harness verifies the production reward settlement against Libauth's BCH 2026 VM and the authoritative PHOTON reference fixture.

`photon-layout.mjs` proves the mining transaction itself against the PHOTON covenant: the 615/616/617/618-byte layouts for baton ages 0..=16, 17..=127, 128..=32767 and 32768..=65534, rejection of age 65535, and the proof-of-work rule `ABS(BIN2NUM(HASH256(tx))) < target`, under which a digest with bit 255 set still wins.

It is intentionally offline: no Fulcrum or node connection, production key, external funding, or broadcast is used. The test checks consensus and standard-mode validity, CashToken conservation, covenant-preserving state, adversarial mutations, and an exact serialization fingerprint shared with the Rust settlement regression.

Run:

```text
npm ci
npm test
```
