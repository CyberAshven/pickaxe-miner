# PHOTON reward-policy BCH 2026 VM proof

This developer-only deterministic harness verifies the production reward settlement against Libauth's BCH 2026 VM and the authoritative PHOTON reference fixture.

It is intentionally offline: no Fulcrum or node connection, production key, external funding, or broadcast is used. The test checks consensus and standard-mode validity, CashToken conservation, covenant-preserving state, adversarial mutations, and an exact serialization fingerprint shared with the Rust settlement regression.

Run:

```text
npm ci
npm test
```
