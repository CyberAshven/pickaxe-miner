# Merge-mining BCH tokens (format v2)

Pickaxe can merge-mine covenant tokens on Bitcoin Cash: the same SHA-256 work
that mines BCH also mines tokens, with no hashrate taken from BCH. This page is
the **v2 common format**, agreed with BCH P2Pool so that both implementations
build and check the same bytes. It replaces the v1 draft. No such token exists
yet, so both networks' token lists are empty and every coinbase is exactly what
it was without this feature.

A token accepts any share whose header hash meets the token's own target,
which is easier than a BCH block: a miner wins tokens without finding a block.
One commitment in coinbase output 0 covers up to 16 tokens, so one share is
checked against all of them at once.

v1's block-required mode (Case B: coinbase ticket outputs, mode B, the ticket
registry fields) is removed. Version `03` of the leaf and proof is reserved
for such a mode.

## Byte layouts (normative)

All integers are little-endian. `SHA256` is single SHA-256, `HASH256` is
double SHA-256. Hashes are written as hashed, never byte-reversed.

### Commitment: coinbase output 0 (53 bytes)

```text
off len value
0   8   00 00 00 00 00 00 00 00     value 0
8   1   2c                          script length 44
9   1   6a                          OP_RETURN
10  1   2a                          push 42
11  4   43 54 4d 4d                 "CTMM"
15  1   02                          version
16  32  root                        as hashed, not reversed
48  1   h                           tree height, 0..=16
49  4   salt                        u32 LE
```

**The empty commitment** is the zero root with `h = 0` and `salt = 0`. A zero
root is valid only with `h = 0` and `salt = 0`; any other zero-root output is
not a commitment.

**The coinbase head is fixed-shape**, so a covenant finds output 0 without a
search:

```text
version (4) | 01 | 00 x 32 | ffffffff | L (1, at most 100) | scriptSig (L) | sequence (4) | output count (1, 1..=0xfc)
```

`cb_head = cb[0 .. 47+L]` with `L = cb[41]`, and output 0 is
`cb[47+L .. 100+L]`. The version and the sequence are free (BCH P2Pool uses
sequence `fffffffe`). Only a coinbase has the null prevout, and a coinbase is at
least 104 bytes, so it can never pass for a 64-byte merkle node.

### Slot and tree

- **Slot:** `u32le(SHA256(0x02 ‖ salt_le4 ‖ category32)[0..4]) mod 2^h`.
- **Leaf hash:** `SHA256(0x00 ‖ preimage)` (the 169-byte preimage below).
- **Node:** `SHA256(0x01 ‖ left ‖ right)`.
- An empty **leaf** slot is 32 zero bytes. Internal nodes are **always**
  hashed from their children, never replaced by zero bytes.
- At level `i`, bit `i` of the slot set to 1 means the current node is the
  right child: `SHA256(0x01 ‖ sibling ‖ current)`; otherwise
  `SHA256(0x01 ‖ current ‖ sibling)`.
- One leaf per token category per coinbase, at most 16 tokens.

**The builder** starts at the smallest `h` that holds the leaves
(`2^h ≥ count`), tries salts `0..N` at each `h` (Pickaxe: `N = 65,536`), then
increments `h`. `h` never exceeds 16; if no salt gives every category its own
slot at `h = 16` (or below a token's own height limit), building fails. The
search depends only on the categories, so it runs once per token set. A
verifier accepts any salt.

### Leaf preimage (169 bytes)

```text
off len field         value
0   4   "CTML"        43 54 4d 4c
4   1   version       02
5   32  category      the token category
37  32  lock_hash     HASH256(the token baton's locking bytecode)
69  32  state_hash    token-defined; changes only on a successful claim
101 32  payout_hash   HASH256(the payout output's locking bytecode)
133 4   target_bits   the token's compact target, u32 LE
137 32  ext           token-defined (a split, a donation), or 00 x 32
```

The state hash stops replays (a claim moves it), the payout hash stops
redirection, and the target stops a claim at an easier one. A covenant builds
the leaf from introspection; a proof never carries leaf data.

### Proof (`AuxProof` v2)

```text
off  len    field
0    4      "CTMP"
4    1      02
5    80     header
85   1      h (at most 16)
86   4      salt, u32 LE
90   32h    aux_branch, leaf to root
..   2+n    u16 LE n, then cb_head (47+L bytes)
..   2+n    u16 LE n, then cb_tail (outputs 1.. and the locktime)
..   1      d (at most 25)
..   32d    cb_branch, the coinbase at index 0
```

Output 0 and the leaf are always rebuilt by the verifier, never carried. The
encoding is canonical: exact lengths, nothing trailing. Its length is
`95 + 32h + len(cb_head) + len(cb_tail) + 32d`. The `d ≤ 25` limit (2^25
transactions) belongs to version `02`.

### Target bits

`target_bits` is canonical compact: `3 ≤ exp ≤ 32` and `0 < mant < 0x800000`
(no sign bit, no zero mantissa), so `target = mant << 8·(exp−3)` is one shift.
The target is always the token's, never the header's nBits.

## What a covenant checks

Pickaxe's reference verifier (`src/stratum_v2/merge/verify.rs`) performs
these steps, and runs them on every proof before keeping it (fail closed):

1. **Decode** exactly one canonical `AuxProof` v2.
2. **Rebuild the leaf** from introspection: the category, `HASH256` of the
   baton's locking bytecode, the token's state hash, `HASH256` of the payout
   output's locking bytecode, the token's `target_bits` and `ext`.
3. **Slot** from the category, the salt and `h` (`h` at most the token's
   limit).
4. **Fold the aux branch** from the leaf hash to the root.
5. **Rebuild output 0** from the root, `h` and the salt.
6. **Check the head's shape**: its length is `47 + head[41]`, `head[41]` is at
   most 100, `head[4..41]` is `01 ‖ 00×32 ‖ ffffffff`, and its last byte is
   1..=0xfc; the tail is at least 4 bytes.
7. **`txid = HASH256(head ‖ output 0 ‖ tail)`.**
8. **Fold `cb_branch` at index 0** (`HASH256(current ‖ sibling)`) to the
   header's merkle root (`header[36..68]`).
9. **Work:** `HASH256(header)`, read as an unsigned little-endian number, is
   at most `expand(target_bits)`.

## Pool and role rules

- **Solo** (the operator is the miner): Pickaxe commits its own root for its
  own devices, on standard and extended channels and SV1 firmware through the
  adapter. Each leaf pays the miner's script or, in a donation-work job, the
  donation's.
- **Pickaxe as a public pool** (serving other people's devices): their jobs
  carry the **empty commitment**. An operator-chosen token root never sits on
  other people's devices, the pool fee never reaches tokens, and no share of
  theirs wins a token. Their device target is never eased toward a token's.
- **Job Declaration** (Pickaxe at a pool with its own templates): the
  declared coinbase carries the commitment as output 0 before the pool's
  outputs, and its leaves bind the miner's own script (the pool's first
  output). Only shares that meet the **pool's** share target go to the pool.
  Token-only hits (meeting a token's target but not the pool's) stay local:
  journaled and claimed by Pickaxe, never sent as shares. The local device
  target may be eased toward the token target; that easing is local and never
  leaks upstream.
- **BCH P2Pool:** the same forwarding rule applies. Pickaxe does not yet
  merge-mine at P2Pool: its prepared coinbase carries P2Pool's own auxiliary
  output today, and a P2Pool that builds output 0 in this format is still to
  come.
- **A pool without Job Declaration:** no merge mining. The pool builds the
  coinbase, so Pickaxe cannot add the commitment.
- **Own-root check:** where Pickaxe is the miner side and sees the coinbase
  (the coinbases it builds for its devices, standard and extended, and the
  Job Declaration coinbase they share), it checks before hashing that output 0
  is exactly the commitment it expects. If not, the job mines BCH alone, with
  no token merge mining, and the dashboard says why.
- **GPU mining:** not applicable. PHOTON's work is over its own transaction,
  not a block header.

While tokens are merge-mined, a device's share target is made easier, up to
the easiest token target but never more than 15 times easier than vardiff's,
so firmware sends every hash that wins a token. Vardiff counts and estimates at
its own target. A token journal that cannot be written turns token claims off
and leaves BCH mining unaffected.

## Privacy

- **One payout address.** Everything pays the BCH payout address the user
  configured. There is no address rotation.
- **A fresh extranonce per job** where Pickaxe fills it: on standard channels a
  merge-mined job gets fresh random salt bytes (under Job Declaration, fresh
  device bytes), so claims are not linked by a stable extranonce. Extended
  channels and SV1 devices are told their prefix once per channel; it is
  random per connection.
- **No custom coinbase tag** in merge-mined jobs by default: the script is the
  BIP34 height, the node's flags and the extranonce. A `--pool-tag` the user
  sets explicitly is their choice.
- **One claim per transaction.** Each journal row is one claim; proofs from
  different shares are never batched.
- **A broadcast delay** for claims: `--claim-delay SECONDS` or `MIN-MAX`,
  drawn at random per claim (default 5 to 60 seconds; `0` for none). Each
  journal row records `broadcast_after`.
- **Tor** is out of scope; the operator can route this computer's traffic
  through it.

## Donation

Merge-mined tokens follow the one BCH ASIC donation setting through work: in
a donation-work job each leaf pays the Pickaxe donation's script, as that job's
BCH does. A split of each claim is token-defined (the leaf's `ext`); no
registered token defines one yet, so Pickaxe sets `ext` to 32 zero bytes. A
public pool's fee never reaches tokens. Under Job Declaration no donation work
runs (the pool's rule takes the donation from the coinbase), so leaves pay the
miner.

## Coinbase size

The format does not fix a coinbase size, and does not depend on one. A claim
carries the whole coinbase in its proof, so the coinbase must fit within the
claim's unlocking limit of 10,000 bytes together with the proof's other parts:
the claimable bound is about 7,518 bytes of coinbase, with fewer than 253
outputs (a one-byte output count). Pools with many payees keep their coinbase
small. BCH P2Pool will use a hybrid coinbase: direct payouts within a fixed
`T = 3,072` bytes (proposed, pending its owner), and the rest through one
payout-tree output.

## Token-level rules (not part of the format)

These are the agreed rules of a SAFA-style token covenant, as guidance for
token authors. The format above does not depend on them.

- **Fees and seeding.** The vault pays its own claim fee, computed at 1
  sat/byte from the sizes it parses out of the proof plus a constant `K`;
  `K` must include the redeem script and the commitment output. The claim has
  an exact output count, and its payout output is capped at token dust. The
  vault has no sponsor input, so seed every token thread with real BCH.
- **Height.** `<cb_height> OP_CHECKLOCKTIMEVERIFY`, with the strict
  `state_height < cb_height`: a claim's coinbase must be newer than the
  thread's state.
- **Difficulty adjustment:** `min(Δh, 288) / threads`, with
  `Δh = cb_height − state_height`.
- **BCH only.** The covenant checks the header's nBits against ASERT at the
  coinbase's BIP34 height:
  - Per-network parameters, from BCHN's chainparams: `half_life` 172,800 s on
    mainnet and 3,600 s on chipnet and testnets; the anchor (height, nBits,
    parent time) 661,647 / `0x1804dafe` / 1,605,447,844 on mainnet and 16,844 /
    `0x1d00ffff` / 1,605,451,779 on chipnet; `allow_pow_limit`, true on test
    networks.
  - `height_delta = cb_height − 1 − anchor_height`. The header's own time
    stands in for the parent's. The exponent uses floor division (not
    truncation) when it is negative.
  - Mainnet band: `computed / 2 ≤ header_target ≤ computed × 1.05`
    (asymmetric). Multi-hour gaps happen, and the header's time is only a
    stand-in for the parent's, hence the room on the hard side.
  - Test networks also accept `nBits == powLimit` (`allow_pow_limit`, for
    min-difficulty blocks), and get a ×/÷8 band on the hard side.
  - After `Δh ≥ 4,032` blocks without a claim, the covenant also accepts the
    fallback band, capped at 4 doublings on the hard side and ×1.1 on the easy
    side (`CAP = 4`), so a future change to BCH's difficulty algorithm cannot
    freeze a thread.

### Risks

- **Forged headers.** Any SHA-256 work, with a header that passes these checks,
  can still mine a token; the checks raise the cost, they do not tie the work
  to BCH's chain.
- **eCash (XEC)** shares BCH's ASERT anchor, so only the height floor stops XEC
  headers. That residual remains.
- **A BCH difficulty-algorithm change** would require a token migration; the
  `Δh ≥ 4,032` fallback band keeps threads moving until then.
- **Fees.** Fees above 1 sat/byte stall claims: the vault pays a fixed rate.
- **Standard channels.** A public pool's empty commitment on standard channels
  (whose devices never see the coinbase) is a trust assumption. It can be
  audited after the fact: every block the pool finds shows its output 0.

## Golden vectors

Built from synthetic data only, and checked byte for byte by the unit tests in
`src/stratum_v2/merge/vectors.rs` and `commitment.rs`. The payout in all of
them is P2PKH to twenty `0x12` bytes:
`76a914121212121212121212121212121212121212121288ac`.

### A two-token tree

| | token A | token B |
|---|---|---|
| category | `11` × 32 | `22` × 32 |
| baton locking bytecode | `51` | `52` |
| state_hash | `aa` × 32 | `bb` × 32 |
| target_bits | `0x207fffff` | `0x1f00ffff` |
| ext | `00` × 32 | `00` × 32 |

Leaf A's preimage:

```text
43544d4c02
1111111111111111111111111111111111111111111111111111111111111111
953ccfa596a6c6d39e5980194539124fdcff116a571455a212baed811f585ee0
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
22650758a42ebfd7306e771ad24c442cad795ed9e549392233729f647a2b1be5
ffff7f20
0000000000000000000000000000000000000000000000000000000000000000
```

- Leaf A hash: `0ab882d24f8c85b880ca7cfb6f60a7773f377222e84ac28afd14f3ce46bf307a`
- Leaf B hash: `9520e74e37e5dffaef19eb2f61da54e502d296c8f1baa3f5dc7db772711def59`
- The builder: `h = 1`, salt `0`; slots A = 0, B = 1.
- Root: `5b61ecfe96c714397bc35ed813890f49f2b0287a78111e19d156f4eecd1c38d0`
- Output 0:

```text
0000000000000000 2c 6a 2a 43544d4d 02
5b61ecfe96c714397bc35ed813890f49f2b0287a78111e19d156f4eecd1c38d0
01 00000000
```

### A whole proof for one leaf

Token A alone: `h = 0`, salt `0`, so the root is leaf A's hash. The coinbase
(script-sig `0315f904` + `07` × 8, then output 0 and one payout of
312,500,000 satoshis):

```text
02000000 01 0000000000000000000000000000000000000000000000000000000000000000 ffffffff
0c 0315f9040707070707070707 ffffffff 02
0000000000000000 2c 6a2a 43544d4d 02
0ab882d24f8c85b880ca7cfb6f60a7773f377222e84ac28afd14f3ce46bf307a 00 00000000
205fa01200000000 19 76a914121212121212121212121212121212121212121288ac
00000000
```

- txid (as hashed): `245dbc4c7d02cc87b5a41eb8af9f8aebf36ffa528864177931fb3b3d527f35e9`
- The coinbase branch: one sibling, `44` × 32.
- The header: version `0x20000000`, previous hash `55` × 32, the merkle root
  folded from the txid and the sibling, time 1,700,000,000, nBits
  `0x207fffff`, nonce 0 (the first that meets token A's target).
- The proof (224 bytes):

```text
43544d5002
0000002055555555555555555555555555555555555555555555555555555555
555555556c933baf900d7d82a08e5689a968f70b77802a116c671c63c12b9ed5
34e67a1d00f15365ffff7f2000000000
00 00000000
3b00 02000000010000000000000000000000000000000000000000000000000000000000000000ffffffff0c0315f9040707070707070707ffffffff02
2600 205fa012000000001976a914121212121212121212121212121212121212121288ac00000000
01 4444444444444444444444444444444444444444444444444444444444444444
```

It verifies against token A's claim (category, lock hash, state hash, payout,
target and ext above); with any other state hash the same bytes fail.

### The empty commitment

```text
0000000000000000 2c 6a 2a 43544d4d 02
0000000000000000000000000000000000000000000000000000000000000000
00 00000000
```

## What Pickaxe does today

- Builds the commitment and the tree into every job when a token is
  merge-mined (solo), and the empty commitment into a public pool's jobs.
- Checks every share against each token, hands wins to a claim worker, proves
  them, verifies each proof from its bytes as a covenant would, and saves it
  to `<config>.sv2-token-proofs.json`, readable by the owner alone, one claim
  per row with its `broadcast_after` time.
- Shows token wins on the dashboard and in the status file, never with an
  address, and says when the own-root check refused a job.
- A hidden `stratum-v2 serve --merge-test-token <DIFFICULTY>` merge-mines a
  Chipnet test token (a synthetic lock hash and state hash; the state moves to
  the winning header's hash after each proven win) to try all of this with real
  devices; mainnet refuses it. It rides Job Declaration too (`serve --upstream
  ... --job-declaration --merge-test-token 1000`); without `--job-declaration`
  the server refuses it at a pool.

## Still open

- **A first real token.** Its authors write the covenant (with these checks
  and the token-level rules) and Pickaxe adds its row to the registry and a
  claim builder that broadcasts its claims.
- **BCH P2Pool** building output 0 in this format, so Pickaxe can merge-mine
  there.
- **Live checks:** a BCHN node accepting a template with the commitment
  (`validateblocktemplate`), and a covenant judged by a BCHN regtest node.
