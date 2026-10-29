# PHOTON reward-policy BCH 2026 VM proof

This developer-only deterministic harness verifies the production reward settlement against Libauth's BCH 2026 VM and the authoritative PHOTON reference fixture.

`photon-layout.mjs` proves the mining transaction itself against the PHOTON covenant: the 615/616/617/618-byte layouts for baton ages 0..=16, 17..=127, 128..=32767 and 32768..=65534, rejection of age 65535, and the proof-of-work rule `ABS(BIN2NUM(HASH256(tx))) < target`, under which a digest with bit 255 set still wins.

`verify.mjs` checks the four-output settlement child: baton, 98% miner reward, and two outputs sharing the existing 2% donation equally. The original recipient receives any odd token remainder. Each additional token output carries 700 BCH satoshis, so the baton decrease includes both donation output values and the exact relay fee.

`photon-auxpow.mjs` checks that the live PHOTON v3.2 covenant accepts a claim whose reward goes to a P2SH32 payout carrying an ELECTRIC commitment, so PHOTON's proof covers it (AuxPoW style; see `docs/photon-electric-auxpow.md`). A 700-sat payout is dust for that output, 800 sats is standard, and only the miner key spends the reward.

`merge-forwarder.mjs` executes the existing ASIC header-token payout forwarders
exported by Rust, for personal and donation-work recipients on standard and
extended channels. Both BCH 2026 VMs accept 12 intended spends and reject 32
recipient, category, amount and composed-input mutations. The existing Rust
merge suite separately checks Case A share-target and Case B block-required
proofs, including payout/donation commitments and tampering. This is a small
offline check of existing adapters; it registers no token, adds no claim
contract, and does not establish compatibility with an unspecified future token.

`electric-v2-reference.mjs` runs the author's pinned future-token v2 artifact in
both BCH 2026 VMs: 14 intended spends and 32 rejected mutations. It checks the
full-state proof, incompatible v1 proofs, an actual same-height successor,
recipient binding, nonce length, transaction shape and counter/height limits.
The [reference notes](../../reference/electric-v2/README.md) explain why this is
not an upgrade to existing ELECTRIC. The live v1 miner and its GPU hash format
remain unchanged.

It is intentionally offline: no Fulcrum or node connection, production key, external funding, or broadcast is used. The test checks consensus and standard-mode validity, CashToken conservation, covenant-preserving state, adversarial mutations, and an exact serialization fingerprint shared with the Rust settlement regression.

Run:

```text
npm ci
npm test
```
