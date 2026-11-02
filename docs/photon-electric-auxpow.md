# PHOTON as the parent of an AuxPoW ELECTRIC

Status: finding, 2026-10-19. PHOTON's part works today, with no change to
PHOTON: its live contract accepts a claim that carries an ELECTRIC commitment
inside its proof (shown offline in the BCH 2026 VM, below). ELECTRIC's part
needs a new ELECTRIC contract from its author. Pickaxe's part (a P2SH32
PHOTON payout and a second target check) is not built.

## Why the two tokens cannot share work today

Both are SHA-256d, but each contract accepts only the hash of its own data:

- PHOTON v3.2: `HASH256` of the claim transaction itself (the covenant
  rebuilds the serialization in script) must be below the target.
- ELECTRIC: `hash256(lastHeight || recipientPkh || nonce)`, a 32-byte message
  (see [electric-token.md](electric-token.md)).

One hash has one preimage, so a hash counts for one token. Running both
miners on one GPU splits it. Measured on the RTX 5070 Ti Laptop GPU on
2026-10-19 (PHOTON's `benchmark`, ELECTRIC's `--dry-run`, nothing sent):

| | Alone | Both at once | Kept |
|---|---|---|---|
| PHOTON | 1,688 MH/s | 1,081 MH/s | 64% |
| ELECTRIC | ~2,900 MH/s | ~1,000 MH/s (500 to 2,000) | ~35% |

## The AuxPoW idea

In AuxPoW (Namecoin on Bitcoin), the parent's miner commits to the aux block
in the coinbase, and the aux chain accepts the parent's hash plus a proof of
that commitment. Here the parent is a PHOTON claim and the commitment slot is
its payout, output 1.

The v3.2 covenant requires exactly two outputs and hashes output 1's locking
bytecode into the proof, but never restricts it
(`reference/photon_v32_template.json`, `lock`). So the miner can pay the
reward to a P2SH32 address of:

```text
<32-byte ELECTRIC commitment> OP_DROP <miner pubkey> OP_CHECKSIG
```

The reward stays the miner's, and PHOTON's proof hash now covers the
commitment. Every PHOTON attempt is also an ELECTRIC attempt: an aux contract
checks the same hash against its own target. A hash that misses PHOTON's
target but meets ELECTRIC's is still sent to ELECTRIC; that PHOTON claim is
simply never broadcast.

## What the test shows

`tools/reward-policy-vm/photon-auxpow.mjs`, part of `npm test`, runs the live
mainnet covenant bytes (checked against `COVENANT_LOCKING_BYTECODE_HEX`) with
test keys and a synthetic baton at an easy target, so no grinding is needed:

- A normal claim with today's 700-sat P2PKH payout passes, in consensus and
  standard mode (control).
- A claim paying the P2SH32 payout above passes in both modes. It is 639
  bytes.
- At 700 sats that payout is dust: a token-carrying P2SH32 output needs 705.
  At 800 it is standard and still fits the covenant's 1,500-sat allowance
  (800 for the payout, 700 fee, at least 1 sat a byte).
- Another commitment gives another proof hash, so the work is bound to it.
- The miner key spends the reward out of the P2SH32 payout; another key
  cannot.

## What each side needs

- **PHOTON:** nothing.
- **ELECTRIC:** a new contract from its author. The current one cannot move
  its supply: its only function, `mine`, returns the remaining supply to the
  same contract, so a merged ELECTRIC is a new deployment with its own
  category. Its claim would take the PHOTON claim bytes and the payout's
  redeem script and check that:
  - `hash256(claim)` meets ELECTRIC's target;
  - the claim's output 1 is the P2SH32 of that redeem script;
  - the redeem script starts with the expected commitment, which covers
    ELECTRIC's current state (its baton outpoint or claim count) and the
    recipient, so a proof cannot be replayed on a later baton (the weakness
    of the current contract).

  The claim and redeem script must fit BCH's unlocking-size and VM limits;
  that check is part of designing the contract.
- **Pickaxe:**
  - PHOTON payouts are P2PKH-only today (`src/tx.rs`, the 25-byte payout
    check), and the GPU search builds that layout. A P2SH32 payout makes
    output 1 ten bytes longer and needs at least 705 sats.
  - Each hash would be checked against both targets.
  - A wallet does not know the payout's redeem script, so Pickaxe would hold
    the key and sweep rewards to the payout address, as the ELECTRIC fee pot
    does with its recovery key. That sweep costs a fee.

## Expected rates

On this GPU, a merged miner would get ELECTRIC tries at PHOTON's rate,
1.69 GH/s, on top of PHOTON at full speed. Mining ELECTRIC alone gives about
2.9 GH/s of tries, because its preimage is one SHA-256 block, but no PHOTON.

## Not shown

- No merged ELECTRIC contract exists, so nothing here mines ELECTRIC.
- Nothing ran on chain. The covenant bytes are mainnet's; the baton and keys
  are synthetic.
- Pickaxe's PHOTON claim builder and kernels are unchanged.
