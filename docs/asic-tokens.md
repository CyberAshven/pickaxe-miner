# ASIC-exclusive tokens (header-shaped jobs)

#### PR #42

An ASIC-exclusive token is mined *instead of* BCH: its proof of work is its
own 80-byte header, not a BCH block. SAFA, a proposal for a CashToken whose
NFT commitment is shaped like a block header, is the first such design.
Nothing of this kind is deployed yet, so Pickaxe ships the layout as data, a
reference of the checks, and the job convention below. Merge-mined tokens
(BCH plus tokens in one coinbase) are a different mode; see
[merge-mining.md](merge-mining.md).

## The commitment and the header slots

A SAFA commitment is exactly 80 bytes, and a device hashes it as a block
header:

| Bytes | SAFA field | What Pickaxe's job puts there |
|---|---|---|
| 0..4 | the version slot ("virtual", a scaler) | the job's `version` |
| 4..36 | HASH256 of the thread's previous commitment | the job's previous-block hash |
| 36..68 | HASH256 of the grinder's payout locking bytecode | the merkle root: HASH256 of the forwarder below |
| 68..72 | a time above 500,000,000 | `ntime`, below the chain's median time so a claim is final at once |
| 72..76 | the target, compact: a 3-byte little-endian mantissa, then the exponent (as nBits) | `nbits` |
| 76..80 | nonce | the device's nonce |

The next target is `prev · (age · 5000 / 71 + 5000) / 10000`, truncating at
each step, where the age is the number of blocks since the thread last moved
(at most 65,534). From `0x1d00ffff`: age 0 gives `0x1c7fff80`, age 1
`0x1d0081ca`, age 71 `0x1d00ffff`, age 142 `0x1d017ffe`, age 65,534
`0x1e01cdff`. The compact form is the 3 most significant bytes of the
target's minimal script number and that number's length, which equals
Bitcoin's compact target for every size from 3 to 32 bytes.

These parameters (the version rule, the byte order of the target, the
adjustment, the fee allowance of 1,500 satoshis, the emission divisor of
420,000) are data in `merge/safa.rs`, so a revised draft needs a new row and
new vectors, not new code.

## The forwarder: one job convention for every device

A SAFA win pays `HASH256(L)` for the grinder's locking bytecode `L`. When a
job's merkle branch is empty, a device computes the merkle root as
`SHA256d(coinbase)`. So the job's coinbase *is* `L`: a keyless P2S script
that carries the extranonce in a pushed salt, drops it, and forwards the
tokens to a fixed destination:

```text
salt push, OP_DROP, then:
  OP_INPUTINDEX OP_OUTPUTBYTECODE <destination> OP_EQUALVERIFY
  OP_INPUTINDEX OP_OUTPUTTOKENCATEGORY OP_INPUTINDEX OP_UTXOTOKENCATEGORY OP_EQUALVERIFY
  OP_INPUTINDEX OP_OUTPUTTOKENAMOUNT OP_INPUTINDEX OP_UTXOTOKENAMOUNT OP_NUMEQUAL
```

The output at the same index must pay the destination the same tokens, so
anyone may sweep the forwarder, and only to that destination. Pickaxe holds
no key for it.

| Channel kind | Script | Length (P2PKH destination) |
|---|---|---|
| SV2 extended and SV1 | prefix `20 50 58 48 31 <job id 4>` · the channel's 16-byte prefix · the device's 8 bytes · suffix `75 <rule>` | 73 bytes |
| SV2 standard | `18 50 58 48 31 <job id 4> <channel prefix 16> 75 <rule>` | 65 bytes |

Both fit P2S's 201 bytes; a P2SH32 destination fits too. The 4-byte tag
`PXH1` after the push keeps byte 4 of every job prefix non-zero: SRI's
translator reads bytes 4 and 5 as a BIP141 marker and flag, and a shorter
prefix fails that check, so an SV1 device would get no job.

Golden vectors, for the destination `76a914 11×20 88ac`:

- extended, salt `22×28` (job id, channel prefix and device bytes):
  `HASH256 = 10243fae278cba699c3ca67039a035f1c92fc75890a2b777ba0c01b17786d8cb`;
- standard, salt `22×20`:
  `HASH256 = ea83f92729d0dd2bd8fe78b99d8af753681f3430ea2497aa0f55e012a8e354a4`.

A win is recorded as `HeaderWin` v1: `"CTMH"`, version 1, the layout (`S`
for SAFA v1), the 80-byte header, the age (u16le), the thread's outpoint, and
the forwarder script with its length.

## Findings for token authors

These come from reading the canonical SAFA template (the forum post's
Bitauth template, 2026-09-24/29) op by op; they are offered to its author,
and Pickaxe's own checks stay safe either way.

1. **The hash sign.** The draft compares `BIN2NUM(HASH256(commitment))` with
   the target, so every hash whose last byte is 0x80 or more is negative and
   passes any target: about half of all hashes. The template's own "release"
   scenario passes only this way (its hash ends in `0xbb`; read unsigned it
   is above its target). Prepending a zero byte before `BIN2NUM` would fix
   it. Pickaxe counts a win only when the hash is positive and, read
   unsigned, at or below the target.
2. **Version rolling.** The draft fixes all four bytes of the version slot.
   Current ASICs roll the BIP320 bits (13 to 28) in hardware, so a Bitaxe
   cannot mine it at full speed. Comparing `virtual & 0xe0001fff`, with the
   scaler in bits 0 to 12, would let them.
3. **A thread can freeze.** When the next target reaches 2^255, its minimal
   script number needs 33 bytes, so the stored exponent is 33, and every later
   claim fails the `exp < 33` check. At the draft's `0x207fffff` target any
   age of 72 or more does this. Pickaxe refuses to build such a claim.
4. **One frozen version with test vectors** (one adjustment formula, one
   target byte order, one reserve multiplier) would let miners check their
   work against the token; three variants differ today.
5. **No change needed for SV1:** with the forwarder above, SV1 and SV2
   devices mine SAFA as specified.

## Token work in the share path

A server's template can be BCH block work (the default) or a token's job,
mined instead of BCH. A token job has no transactions and no BCH value; its
previous hash, bits and target (at full precision) are the token's, and its
start time is below the chain's median time.

- **Every channel kind mines it.** On a SAFA job the coinbase is the
  forwarder: a standard channel's job carries its HASH256 as the merkle root,
  an extended channel's job carries the forwarder's prefix and suffix with an
  empty merkle path, and the SV1 adapter turns that into `mining.notify` as
  usual (`coinb1` starts `2050584831`).
- **Never a block.** No share on token work is a block: it never reaches the
  block journal or a node.
- **A win** is a share whose hash is positive and at or below the token's
  target; the share is accepted as any other and handed on as a win.
- **The version rule is the token's.** A BIP320 token lets devices roll bits
  13 to 28; a token that fixes its version slot refuses rolled shares. On such
  a token the server refuses a client that requires version rolling (as every
  Bitaxe does) and tells others the version is fixed; the SV1 adapter then
  asks for no version rolling and answers `mining.configure` without it.
  Firmware that rolls anyway makes invalid shares, which only hardware tests
  can show.

## The token worker and the Chipnet test token

A server in token mode needs no BCH node: its jobs come from the token's
thread. No ASIC-exclusive token is registered on either network, so
`--asic-token NAME` is refused for now, and a hidden Chipnet-only flag runs
a test token instead:

```text
pickaxe_miner stratum-v2 serve --config chipnet.json --listen 0.0.0.0:3336 \
  --sv1-listen 0.0.0.0:3333 --asic-test-token 1000
```

- **The test token** has SAFA's canonical layout with BIP320 version
  rolling, a 1.5% donation minimum and no covenant. Its simulated thread
  starts from a commitment naming no predecessor, keeps age 71 (so its
  target stays the one the difficulty sets), and starts its jobs two hours
  behind now, below the chain's median time.
- **Wins.** A share that wins is handed to the token worker through a
  bounded queue (64) the device threads never wait on; a full queue drops
  and counts the win. The worker checks each win as the covenant would (it
  must follow the thread's commitment, pay one of this server's forwarders,
  and pass the claim checks), saves it to the owner-only
  `<config>.sv2-token-proofs.json` (mode `H`, the `HeaderWin` record), and
  moves the simulated thread to the winning header, so devices get a new job
  that follows it. A later win on the old thread is counted stale. A win
  that fails Pickaxe's own check is a bug: proofs turn off and devices keep
  mining.
- **The donation** is the token's share of the work, never below its minimum
  (the profile's token donation, 1.5% by default): donation jobs pay the
  donation's forwarder.
- The overview reads "no node: an ASIC-exclusive token instead of BCH" and
  lists the token's wins (proven, stale, dropped); the status file's
  `header_token` has the same counts, never a script or an address.

## What is not done yet

Reading real threads from Fulcrum or a node, setup rows, Case A pure-token
jobs in this mode, and real claims and sweeps (judged by a local BCHN on
regtest) follow in later slices.
