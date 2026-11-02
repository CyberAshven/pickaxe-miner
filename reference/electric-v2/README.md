# PoWMineStatefulV2 reference

This is a reference for future token deployments, **not an upgrade to the
existing ELECTRIC token**. Pickaxe's live ELECTRIC deployments, GPU proof format,
donation policy, fee pots and recovery paths are unchanged by these files.

The author explicitly states that ELECTRIC will not be redeployed onto v2 and
that its original v1 covenant and category stay live. Read the
[author's explanation](https://gitlab.com/electricdude/electrictoken/-/blob/2a8a9cfba0fa2d6a3e571c236e912fa6e2ef5766/v2/README.md).
Source and compiler artifact are pinned to that revision. The artifact records
cashc 0.13.3 and its fingerprint; `bytecodeHex` is the compiler's executable
bytecode, alongside its assembly representation. It is not loaded by the miner.

| Rule | Existing v1 | Reference v2 |
| --- | --- | --- |
| PoW input | Previous height (4 bytes), recipient (20), nonce (8 in Pickaxe) | Full previous commitment (16), recipient (20), nonce (exactly 8) |
| Same-height proof | A matching nonce can remain valid as state advances | State changes alter the hash input; every new state needs a valid hash for that state |
| Difficulty bounds | Published ELECTRIC constructor values | Minimum at least 1, maximum at most 6, ordered bounds |
| Claim inputs | No explicit count restriction | Exactly 2 or 3 |
| Counter | Existing v1 behavior | Nonnegative counter plus an explicit overflow guard; four-byte signed Script-number encoding also limits the next value |
| Continuation | Existing mutable baton rule | Explicit mutable category and 16-byte updated commitment |

These are consensus spending conditions. They cannot be retrofitted by changing
the miner: a 44-byte v2 proof is not a valid replacement for ELECTRIC's 32-byte
v1 proof. Off-chain checks, independently verified proofs and durable reward
recovery can protect the application without changing the deployed covenant.

The author's `.env.example` selects Chipnet category
`1562a19be3a973b9c2f72e4680a6d6eefd0836cd7868d8e7fc5ce590d9883b26`,
with reward 100,000,000 base units and difficulty 1..3 in the example miner.
That is a separate test token. A network switch does not deploy its contract or
turn it into mainnet ELECTRIC. No mainnet v2 deployment is assumed here.

`tools/reward-policy-vm/electric-v2-reference.mjs` executes synthetic transactions
against this artifact in both BCH 2026 VMs. It covers intended claims, full-state
binding, incompatible v1 proofs, recipient binding, input/output shape and
counter/height boundaries. No wallets, GPU searches, network requests, deployment
or broadcasts are used. The existing `electric.mjs` suite separately protects
the deployed v1 bytecode, signed claims, donations and pot recovery.
