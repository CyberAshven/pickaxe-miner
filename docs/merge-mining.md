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
137 32  ext           the split commitment (see Split), or 00 x 32 for none
```

The state hash stops replays (a claim moves it), the payout hash stops
redirection, the target stops a claim at an easier one, and `ext` binds how
each win is shared. A covenant builds the leaf from introspection; a proof
never carries leaf data.

### Split

`ext` commits to a donation slot and a fee slot, in that order:

```text
ext = 00 x 32                                                no split
ext = SHA256(03 ‖ d_bps u16le ‖ d_hash 32 ‖ f_bps u16le ‖ f_hash 32)
```

The preimage is a fixed 69 bytes and canonical:

- each rate (`d_bps`, `f_bps`) is 0 to 10,000 hundredths of a percent;
- a rate of 0 has a zero hash, and a rate above 0 a non-zero hash;
- two empty slots are the zero `ext`; that all-empty preimage is refused;
- a hash is `HASH256` of the recipient's locking bytecode (`script_hash`, as
  `payout_hash` is);
- there is no other use of `ext`.

The claim reveals the preimage; the covenant hashes it, checks it is
canonical, and pays it.

**Shares** of a reward `R` (in u128):

```text
d     = floor(R × d_bps / 10000)
f     = floor((R − d) × f_bps / 10000)
miner = R − d − f
```

A share worth less than `MIN_SHARE_SATS = 1,000` satoshis at the vault's AMM
price counts as 0 and stays with the miner. The price is
`share × (cash + virtual) / tokens`, rounded down, the covenant's formula; an
AMM that holds no tokens prices nothing, so every split share is 0. The fee is
still computed from what the raw donation left: dropping a small donation
never raises the fee.

**Claim outputs**, in order: the vault, the miner's remainder (paying
`payout_hash`), the donation and the fee. Each of the last three is present
only when its amount is above 0. Each of them carries exactly 800 satoshis
and fungible tokens only: the vault's category, no NFT capability, an amount
of at least 1. Each present split output adds about 90 bytes to the claim,
which the vault's fee pays for.

**Work jobs** (donation work and fee work) carry `ext = 0`: their leaves pay
the donation (or the operator) whole. A token whose covenant does not verify
splits (the registry's `split` flag) gets `ext = 0` on every leaf, and its
donation and fee are taken as work instead; under Job Declaration, where no
work rotates, such a token is refused.

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
   output's locking bytecode, the token's `target_bits` and `ext` (zero, or
   the SHA-256 of the split preimage the claim reveals, checked canonical;
   the split's outputs are checked as in Split).
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
  adapter. Each leaf pays the miner's script with the donation in its split,
  or, in a donation-work job, the donation's script whole (see Donation).
- **Pickaxe as a public pool** (serving other people's devices): each miner
  mines a root of their own, on standard and extended channels and SV1
  firmware through the adapter. Each leaf's `payout_hash` binds the miner's
  identity script (the payout address in their login, with or without
  `.worker`), and its split carries:
  - the **donation slot**: the reward share of the miner's donation (two
    thirds of their `,donation=` choice, or of the pool's setting without
    one; see [The login option](#the-login-option)), to the Pickaxe
    donation's script;
  - the **fee slot**: the operator's fee from the coinbase, to the fee
    address's script: the whole rate in `coinbase` mode, two thirds in
    `both`, and none in `work`.

  A donation-work job's leaves pay the donation whole, and a fee-work job's
  (`work` mode, and `both`'s third) pay the operator whole, both with
  `ext = 0`, as their BCH does. The operator's fee reaches tokens as it
  reaches BCH and the miner cannot remove it; the donation is the miner's to
  set, down to 0. A token without split support takes both as work: its
  leaves pay the donation whole for the miner's donation share of the work,
  then the operator whole for the fee's share of what the donation leaves
  (fee × (1 − donation)), and the miner otherwise; BCH in those jobs pays as
  it would. Before a job goes out, the pool checks the own-root rule and the
  audit rule (see [Disclosure](#disclosure)); a job that fails either mines
  BCH alone and the dashboard says why.
- **Job Declaration** (Pickaxe at a pool with its own templates,
  Full-Template only): the declared coinbase carries the commitment as
  output 0 before the pool's outputs. Its leaves bind the miner's own script
  (the pool's first output), and their split is what the coinbase pays: the
  miner's donation (the PX token's donation rate, which a Pickaxe pool takes
  from the login: the client names the miner's own setting as
  `,pickaxe-donation=`) and the pool's fee, both whole, as no work
  rotates under Job Declaration. The client discloses the leaves in
  `DeclareMiningJob.excess_data`, and the pool refuses a declaration whose
  leaves do not match (see [Job Declaration
  leaves](#job-declaration-leaves-excess_data)). Coinbase-only Job
  Declaration has no way to show the pool its leaves, so it mines no
  tokens: the client builds no root and says why, and the pool refuses a
  custom job whose output 0 is a non-empty root
  (`invalid-coinbase-tx-outputs`). Only shares that meet the **pool's**
  share target go to the pool.
  Token-only hits (meeting a token's target but not the pool's) stay local:
  journaled and claimed by Pickaxe, never sent as shares. The local device
  target may be eased toward the token target; that easing is local and never
  leaks upstream.
- **BCH P2Pool:** the same forwarding rule applies. Pickaxe does not yet
  merge-mine at P2Pool: its prepared coinbase carries P2Pool's own auxiliary
  output today, and a P2Pool that builds output 0 in this format is still to
  come. With tokens registered, P2Pool jobs mine BCH alone and the dashboard
  says why.
- **A pool without Job Declaration:** the pool builds the coinbase, so
  Pickaxe adds no commitment of its own. A Pickaxe public pool merge-mines
  for its miners itself (above); another pool does not.
- **Own-root check:** where Pickaxe builds the coinbase its devices hash
  (solo and public pool, standard and extended, and the Job Declaration
  coinbase they share), it checks before a job goes out that output 0 is
  exactly the commitment it expects. If not, the job mines BCH alone, with
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

Merge-mined tokens follow the one BCH ASIC donation setting (default 1.5%,
anything from 0% to 100% in Advanced settings; on a public pool, the miner's
`,donation=` choice or the pool's setting), split as BCH's is:

- **Two thirds from each win:** the reward share (1% at 1.5%,
  `BchDonation::reward_bps`) is the donation slot's rate on every leaf of the
  miner's own jobs, as a block's donation output takes it from the reward.
- **One third as work:** BCH's donation-work jobs (0.5% of the work at 1.5%)
  have leaves that pay the donation whole, with `ext = 0`.
- A setting of 0% means no donation: no slot and no work.

Under Job Declaration no donation work runs (every local job shares the
declared coinbase), so the leaves carry the token's whole donation rate (the
miner's choice at a Pickaxe pool) and the pool's whole fee in the split, as
the declared coinbase pays BCH's. A token whose covenant verifies no split
takes its whole donation (1.5%) as work instead: every BCH donation-work job,
and enough other jobs to make up the rest, whose BCH still pays the miner. On
a public pool such a token takes the operator's whole fee as work too, after
the donation (see Pool and role rules); under Job Declaration it is refused.

## The login option

On a Pickaxe public pool a miner chooses their own donation after their
payout address: `<address>[.worker][,donation=<percent>]`, such as
`bitcoincash:qq….rig1,donation=0.5`. The same login works as an SV1
username, an SV2 `user_identity` and a Job Declaration `user_identifier`.

- `<percent>` is 0 to 100 in 0.5% steps, as the BCH donation setting is.
  Anything else (`7.25`, `-1`, `abc`, a second `donation=`) is refused with
  "donation= takes a percentage from 0 to 100 in 0.5% steps, such as
  ,donation=0.5": SV2 `invalid-donation`, a refused authorize for SV1
  firmware with that reason, and an ended session under Job Declaration (as
  for any identity the pool cannot read).
- Without the option the pool's donation setting applies.
- It sets the miner's whole BCH donation: the coinbase's reward share, the
  donation work and the donation slot in token leaves; under Job
  Declaration, the PX token's donation rate. The pool's fee does not change,
  and is still taken from what the donation leaves.
- Options follow the first comma, separated by commas; keys other than
  `donation` are left alone, so BCH P2Pool's `,p2p=<share address>` passes
  through. The payout is the part before the first comma.
- One connection keeps one choice: its first channel fixes it, and a later
  channel whose login names another donation (or none after one) is refused
  with `donation-mismatch`, as donation work is allocated per connection.
- BCH P2Pool reads the same key for its own donation (0 to 10%, apart from
  its operator fee, refused above 10%). Pickaxe does not check P2Pool logins
  itself.
- `pickaxe-donation=<percent>` means the same on a Pickaxe pool, and only
  Pickaxe pools read it. A login names one or the other, not both.

**One donation at a pool.** Pickaxe's SV1 adapter donates by itself at a
remote pool: its own donation channel, at the miner's setting. A Pickaxe
pool would also take its donation from the device's channel, so the adapter
now names `,pickaxe-donation=0` after the device's login there (unless the
login already names a donation), and asks once more without it if the pool
refuses the option. It never names `donation=`: at a BCH P2Pool node that
key is P2Pool's own donation, which is independent of Pickaxe's and stays
the miner's choice. Under Job Declaration there is no local donation work,
so the donation is only the coinbase's, at the rate the login chooses.

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
  an exact output count, and each output after the vault carries exactly 800
  satoshis. The vault has no sponsor input, so seed every token thread with
  real BCH.
- **Height.** `<cb_height> OP_CHECKLOCKTIMEVERIFY`, with the strict
  `state_height < cb_height`: a claim's coinbase must be newer than the
  thread's state.
- **Difficulty adjustment:** the claim's target is the state's eased by the
  blocks since the last claim, `target × (⌊min(Δh, 288) × 5000 / threads⌋ +
  5000) / 10000`, clamped to 1 and `0x7fffff << 232` and rounded down to
  canonical token bits, with `Δh = cb_height − state_height`. The leaf
  commits these bits, and they become the next state's. A deployment has
  at most 63 threads, as BCH P2Pool's tooling and Pickaxe's deployment
  loader both check (not a covenant rule): well under 288, where even the
  longest wait cannot ease a target and the vault freezes.
- **BCH only.** The covenant checks the header's nBits against ASERT at the
  coinbase's BIP34 height:
  - Per-network parameters, from BCHN's chainparams: `half_life` 172,800 s on
    mainnet and 3,600 s on chipnet and testnets; the anchor (height, nBits,
    parent time) 661,647 / `0x1804dafe` / 1,605,447,844 on mainnet and 16,844 /
    `0x1d00ffff` / 1,605,451,779 on chipnet; `allow_pow_limit`, true on test
    networks.
  - `height_delta = cb_height − 1 − anchor_height`. The header's own time
    stands in for the parent's. The arithmetic is BCHN's `CalculateASERT`,
    in two steps:
    1. `exponent = ((time_diff − 600 × (height_delta + 1)) × 65536) /
       half_life`, a division that truncates toward zero (C++ int64 `/`);
    2. `shifts = exponent >> 16`, an arithmetic shift (floor), and
       `frac = exponent & 0xffff`.

    BCH P2Pool confirmed this is intended and matches BCHN; an earlier draft
    of this rule said floor division. Vector: Chipnet's anchor, 100 blocks
    on and one second behind schedule (`time_diff = 60,599`). The exponent
    is −65536 / 3600, which truncates to −18 (floor division would give
    −19). Then `shifts = −1`, `frac = 65518`, factor 131,047 and target
    `0xfff2800c8 << 188`; floor division would give `0xfff2000d << 192`.
  - Mainnet: `computed/2 ≤ header_target ≤ computed × 1.05` (asymmetric).
    Multi-hour gaps happen, and the header's time is only a stand-in for the
    parent's, hence the room on the hard side.
  - Test networks accept `computed/8 ≤ header_target ≤ computed × 8`, or
    nBits equal to the network's powLimit (`allow_pow_limit`), with their own
    `half_life` and anchor. The easy side is wide because BCHN accepts a
    parent dated up to 2 hours ahead of its child: with chipnet's 3,600 s
    half-life, `× 1.05` refused real headers dated more than about 253 s
    before their parent (BCH P2Pool's review, checked with chipnet block
    329,001). On mainnet 2 hours moves the target only × 1.029, so its band
    is unchanged.
  - After `Δh ≥ 4,032` blocks without a claim, the covenant also accepts the
    fallback band around `ref`, the last claim header's nBits kept in the
    state: `ref >> 4 ≤ target ≤ ref × 1.1`, so a future change to BCH's
    difficulty algorithm cannot freeze a thread. (An earlier form,
    `ref >> min(⌊Δh / 288⌋, 4)`, is the same wherever it applies: from
    4,032 blocks on, `⌊Δh / 288⌋` is at least 14.)

### Risks

- **Forged headers.** Any SHA-256 work, with a header that passes these checks,
  can still mine a token; the checks raise the cost, they do not tie the work
  to BCH's chain.
- **eCash (XEC)** shares BCH's ASERT anchor, so only the height floor stops XEC
  headers. That residual remains.
- **A BCH difficulty-algorithm change** would require a token migration; the
  `Δh ≥ 4,032` fallback band keeps threads moving until then.
- **Fees.** Fees above 1 sat/byte stall claims: the vault pays a fixed rate.
- **Standard channels.** A standard channel's device never sees its coinbase,
  so its leaves on a public pool are a trust assumption. They can be audited
  after the fact: the operator's leaf log (see [Disclosure](#disclosure)),
  which the operator may publish, and every block the pool finds shows its
  output 0.

## Disclosure

A miner on someone else's pool checks that their leaves pay them before
hashing, and claims their own wins. Disclosure is opt-in, so firmware that
does not ask sees nothing new.

### The disclosure (`CTMD` v2)

The leaves behind one output 0, in a small versioned format; the same bytes
go in Job Declaration's `excess_data` and to miners (normative):

```text
off len   field
0   4     "CTMD"
4   1     02
5   1     n, the leaves (0 to 16)
then n times:
    169   the leaf preimage
    1     00: ext is zero; 01: a split follows
    69    the split preimage (with 01 only)
```

A reader accepts exactly one disclosure (nothing trailing; each leaf and
split canonical; each split's SHA-256 equal to its leaf's `ext`), then
rebuilds the tree from these leaves alone, at output 0's `h` and salt, each
leaf in its own slot and every other slot empty, and requires output 0's
root. A leaf left out, added or changed gives another root.

### SV2: the `0x4D4D` extension

A client asks for extension type `0x4D4D` through SV2 extensions negotiation
(type `0x0001`, `RequestExtensions` `0x00`: `u16 request_id | u16 n |
n × u16`, answered by `RequestExtensions.Success` `0x01` with the granted
types), at any time after `SetupConnection`. A Pickaxe server grants it, then
sends that connection, on the channel bit, after each job that commits a
non-empty root:

```text
JobLeaves (0x00):  channel u32 | job u32 | output 0 (53) | u16 n | CTMD (n)
                   | each leaf's aux branch, in CTMD order (32 × h, leaf to root)
WinProof  (0x01):  channel u32 | job u32 | category (32) | u16 n | AuxProof v2 (n)
                   | u8 m (0 or 69) | the split preimage (m)
```

All integers are little-endian. A `WinProof` follows the share's
acknowledgement when the share wins a token. Extended channels see their
coinbase, so with these they can check output 0, rebuild the root and keep
their own proofs. `0x4D4D` ("MM") is Pickaxe's choice: SV2 has no registry
of private extension types yet.

### SV1: `pickaxe-merge`

SV1 firmware asks for `pickaxe-merge` in `mining.configure` (beside
`version-rolling`); the answer is `true` where the server sends the
disclosure, otherwise `false`. Then, per job, a notification

```text
{"id":null,"method":"pickaxe.merge_leaves",
 "params":["<job id>","<output 0 hex>","<CTMD hex>",["<aux branch hex>", ...]]}
```

and, on a win,

```text
{"id":null,"method":"pickaxe.merge_proof",
 "params":["<job id>","<category hex>","<AuxProof v2 hex>","<split preimage hex or empty>"]}
```

The adapter asks for the extension only from its own server (solo, a public
pool's SV1 port, Job Declaration's local server); at a remote pool it does
not, and answers `false`.

### Standard channels: the leaf log

Standard channels cannot take extension frames, so a public pool writes one
JSON line per standard-channel job that commits a root to
`<config>.sv2-leaves.jsonl`, beside the other `sv2-*` journals:

```text
{"time":<unix seconds>,"channel":<id>,"job":<id>,"output0":"<hex>","leaves":"<CTMD hex>"}
```

It is readable by the owner alone and never names an address (the leaves
carry script hashes). A writer thread takes the lines through a bounded
queue, so devices never wait on it, and past 8 MiB the log moves to
`.jsonl.1` and starts again. The operator may publish it.

### The audit rule

On a public pool every leaf's `payout_hash` is `HASH256` of the miner's
registered payout script (the address in their login), except in a work job,
whose leaves pay the donation or the operator whole with `ext = 0`. A split
pays only the Pickaxe donation's script and the operator's. Pickaxe checks
this before sending each job (a job that fails mines BCH alone), and anyone
holding a disclosure can check it too. The rates are the miner's to check:
the donation slot is two thirds of their choice, and the fee slot the
operator's published coinbase fee.

## Job Declaration leaves (`excess_data`)

A Full-Template client whose declared coinbase's output 0 is a non-empty root
puts the `CTMD` disclosure of its leaves in `DeclareMiningJob.excess_data`
(empty without tokens). The pool refuses the declaration with
`invalid-coinbase-tx`, saying why, when:

- output 0 is a non-empty root and `excess_data` is empty, or `excess_data`
  is not empty without one;
- the disclosure does not decode, or its leaves do not make output 0's root;
- a leaf names a token that is not in the pool's registry (on Chipnet the
  test token counts);
- a leaf's token does not verify splits (it cannot pay the pool's fee under
  Job Declaration);
- a leaf's `payout_hash` is not `HASH256` of the miner's identity script (the
  first of the token's outputs);
- a leaf's split is not exactly the donation slot (the PX token's donation
  rate, to the Pickaxe donation's script; empty at 0) followed by the fee
  slot (the pool's fee rate, to its fee script; empty at 0).

A Coinbase-only custom job (`SetCustomMiningJob` with an allocated token)
carries no disclosure, so its output 0 must not be a non-empty root.

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

### Split shares

`d_bps` and `f_bps` are the donation's and the fee's rates; shares are
`(donation, fee, miner)`. Unit tests in `src/stratum_v2/merge/split.rs`.

| reward | d_bps | f_bps | shares |
|---|---|---|---|
| 1 | 10000 | 10000 | (1, 0, 0) |
| 1 | 9999 | 9999 | (0, 0, 1) |
| 99 | 10000 | 10000 | (99, 0, 0) |
| 99 | 9999 | 9999 | (98, 0, 1) |
| 100 | 10000 | 10000 | (100, 0, 0) |
| 100 | 9999 | 9999 | (99, 0, 1) |
| 2^63−1 | 10000 | 10000 | (9223372036854775807, 0, 0) |
| 2^63−1 | 9999 | 9999 | (9222449699651090329, 922244969965109, 92233720369) |
| 99 | 100 | 200 | (0, 1, 98): the donation rounds to 0, the fee does not |
| 1,000,000 | 10000 | 500 | (1000000, 0, 0): a whole donation leaves no fee |
| 1,000,000 | 100 | 100 | (10000, 9900, 980100): both slots |
| 1,001 | 5000 | 10000 | (500, 501, 0): no miner remainder |

At one satoshi per token (cash 600,000 and virtual 400,000 satoshis for
1,000,000 tokens), a share of 999 tokens (`MIN_SHARE_SATS − 1`) is worth 999
satoshis and stays with the miner, and one of 1,000 is kept. Refused: a rate of
10,001; a rate above 0 with a zero hash; a rate of 0 with a non-zero hash; the
all-empty preimage.

### A whole split leaf

Token A of the two-token tree, with a 1% donation to P2PKH(`d0` × 20) and a 1%
fee to P2PKH(`fe` × 20).

- The split preimage (69 bytes):

```text
03 6400 8bc613f574964583c7b08b7925373515b513ed15b372bc727599c16434254d1c
   6400 543bb5ec93f33a1c27a5822e4c638e9ab1e90e44c094fd7aaba51efc4e554c50
```

- ext: `79f31f395015b16c76e12673ec8f58329d81f1884d76a0d210d1f3158a8faa85`
- The leaf (169 bytes):

```text
43544d4c02
1111111111111111111111111111111111111111111111111111111111111111
953ccfa596a6c6d39e5980194539124fdcff116a571455a212baed811f585ee0
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
22650758a42ebfd7306e771ad24c442cad795ed9e549392233729f647a2b1be5
ffff7f20
79f31f395015b16c76e12673ec8f58329d81f1884d76a0d210d1f3158a8faa85
```

- Leaf hash: `c40271368b4287df8fe3430e1bc3f9410b554874de698144fa04fe5df7033e41`
- Claim outputs at one satoshi per token: a reward of 1,000,000 pays the
  miner 980,100, the donation 10,000 and the fee 9,900 (each split output
  with 800 satoshis). A reward of 50,000 pays the miner all 50,000: the
  donation's 500 and the fee's 495 are worth under 1,000 satoshis.

## tSAFA on Chipnet

tSAFA is the Chipnet test deployment of BCH P2Pool's SAFA AuxPoW covenant
(the merge-mined variant of the SAFA draft, in P2Pool's
`tools/safa-auxpow`). Pickaxe merge-mines it and claims its wins, written
from the covenant's rules (`src/stratum_v2/merge/safa_auxpow.rs`); nothing
of P2Pool's is copied or run.

### The gate

- Only `stratum-v2 serve --safa-deployment <FILE>` (a hidden flag) mines
  it, and only on Chipnet, with this server's own templates (solo or
  `--public`).
- It is refused on mainnet, at a pool (`--upstream`; a Pickaxe pool's Job
  Declaration check knows no tSAFA row), and beside `--merge-test-token`.
- The setup never offers it, and neither network's registry has a SAFA row.

### The deployment file

One JSON object, at most 1 MiB:

```json
{
  "version": 1,
  "name": "tSAFA",
  "network": "chipnet",
  "category": "<the genesis txid, as a txid is shown>",
  "threads": 4,
  "virtual_sats": "84000000000",
  "redeem_script": "<the covenant's bytes, hex>"
}
```

- The redeem script fixes the covenant. Pickaxe derives the vaults' P2SH32
  lock (`aa20 HASH256(redeem) 87`), the leaves' `lock_hash` and the fee
  constant `K = 577 + the redeem script's push length + its length`.
- P2Pool's `cli.mjs params` prints the redeem script with `fee_k`,
  `redeem_script_bytes`, `locking_bytecode` and `lock_hash`. When the file
  carries any of them, they must agree.
- A redeem script that does not push the file's `K`, its virtual reserve,
  the reward divisor 420,000 and the `CTMP 02` proof magic is refused as
  another deployment's covenant.
- `name` is optional (1 to 32 printable characters).

### Threads and jobs

- A vault (one per thread) is a covenant output holding fungible tokens,
  satoshis and one mutable NFT whose 16-byte commitment is the thread's
  state: thread, state height, target bits, ref bits (each u32 LE).
- The hub reads the vaults from Fulcrum (`blockchain.scripthash.listunspent`
  with tokens, on the lock's script hash) every 20 seconds.
- A coinbase holds one leaf per category, so a job carries one thread. A
  thread is minable at a height when:
  - its state is older than the height;
  - its state is not already won here (a state is claimed once; the won
    states are rebuilt from the journal's proven and broadcast rows when the
    server starts);
  - its vault holds enough for a claim (`K + 4,000 + 3 × 800 + 800`
    satoshis).
- **The thread rule** (agreed with BCH P2Pool): a leaf paying `payout_hash`
  in a coinbase at `cb_height` mines

  ```text
  thread = u32le(HASH256(payout_hash ‖ u32le(cb_height))[0..4]) mod threads
  ```

  or, when that thread is not minable, the next index mod `threads` that
  is. `payout_hash` is the leaf's own (the miner's, or a work job's
  recipient's), so on a public pool each miner's leaf may mine another
  thread. With a power-of-two thread count (tSAFA has 4) this equals
  reading the whole hash as a little-endian number mod `threads`.
- Vector: `payout_hash = HASH256(76a914 12×20 88ac) =
  22650758a42ebfd7306e771ad24c442cad795ed9e549392233729f647a2b1be5`. At
  `cb_height` 329,001 the hash is `90d29130…67c0d7bf`; its u32le is
  814,862,992, so the leaf takes thread 0 of 4 (1 of 3). At 329,002 the hash
  is `af5ced2f…`, and the leaf takes thread 3 of 4. If thread 0 is not
  minable at 329,001, the leaf takes thread 1, then 2, then 3.
- The claim does not check the rule: the covenant accepts any thread.
- The leaf binds the vault's `lock_hash`, `HASH256` of its commitment, the
  claim target for that height, and the payout and split as any leaf does.

### The claim transaction

The covenant fixes the claim completely. It needs no signature, so two
claimants who build it with the same fee rate and lock time build the same
transaction.

| Part | Value |
|---|---|
| version | 2 |
| inputs | exactly one, the vault, sequence `0xfffffffe` |
| unlocking | minimal pushes: the split preimage (69 bytes, or empty), `payout_hash` (32), the `AuxProof` v2, the redeem script |
| output 0 | the vault: its own script; its category with a mutable NFT whose commitment is the next state (thread, coinbase height, claim target bits, the header's nBits); its tokens less the reward; its satoshis less the fee and 800 per share output |
| outputs 1.. | the miner's share (to `payout_hash`), the donation's, the fee's, in that order, each only when above 0, each exactly 800 satoshis and fungible tokens only |
| lock time | the coinbase height (any height at or above it is valid; never a time) |

- The reward is `⌊vault tokens / 420,000⌋`; the shares follow Split above.
- The vault pays `S` to `3S` satoshis, `S = K + |proof|`. Pickaxe pays `S`
  (1 sat/byte of the bound; the claim is always smaller than `S`).
- The covenant requires that every share output (miner, donation, fee)
  pays a script of at most 35 bytes (P2PKH, P2SH20 or P2SH32, as `K`
  assumes). It also requires the vault to keep at least 800 satoshis, the
  fee coming from its cash alone.
- Pickaxe refuses a claim that would leave the vault fewer than 800
  satoshis or BCHN's dust, `3 × (output size + 148)` (756 for the largest
  vault output, so 800 rules). It also refuses a claim whose unlocking
  bytecode exceeds 10,000 bytes.
- An operator whose fee script were longer than 35 bytes could not take
  tSAFA's fee slot. Pickaxe never has one: payout, donation and pool fee
  addresses are CashAddrs (25, 23 or 35-byte scripts). `--safa-deployment`
  still refuses to start if any of them is longer.
- The header's nBits must pass the ASERT band at the coinbase height, or the
  fallback band (see Token-level rules).

### The claimer

Every 20 seconds (60 after a failure) a thread reads the vaults and the
tip, then moves each tSAFA row of the proof journal on:

- **proven**: waiting. Once its `broadcast_after` has passed and the chain
  has reached its coinbase height (its lock time is final from then on),
  the claimer builds the claim against the thread's current vault.
  - If the vault is already in another state, the row becomes **stale**
    (another claim was first).
  - If the covenant would refuse the claim, the row becomes **failed**, with
    the reason.
  - Otherwise the claimer checks the claim's bytes and broadcasts it
    through Fulcrum, and the row becomes **broadcast**, with its `txid`.
  - A refused broadcast or a missing vault is retried after 30 seconds,
    doubling up to an hour. After 10 tries the row becomes **failed**.
- **broadcast**: the row becomes **confirmed** once the vault's output is
  the claim's and in a block, or once the vault has moved on with the claim
  known to Fulcrum.
  - It becomes **stale** if another claim moved the thread first.
  - While the vault is still in the claim's state but not its output, the
    claimer asks Fulcrum for the claim. In the mempool or a block, it is
    left alone. If it is gone and a market trade moved the vault's outpoint,
    it is built again against the new one without counting a try. If it is
    gone and the vault is where the claim found it, it is sent again as a
    retry.
- A market trade replaces the vault's outpoint but keeps its state, so a
  win is still claimed against the traded vault.

Journal rows for tSAFA carry, besides a proof's fields:

- `commitment`, the vault's state when mined;
- `payout_script`, plus `donation_script` and `fee_script` (or null);
- once claimed: `txid`, `vault_outpoint` (the outpoint it spent),
  `claimed_at`, `fee_sats`;
- on retries: `attempts`, `next_attempt` and `error`.

The file stays owner-only.

The dashboard's records line shows `tSAFA claims: N confirmed, N sent, N
waiting, N lost, N failed (N vaults)`. The status file's `tokens.claims`
has the same counts, never an address or a script.

### Before broadcast

Pickaxe has no BCH script VM, so no Pickaxe test runs a built claim through
the covenant itself. Instead, `check_claim` reads the claim's bytes back the
way the covenant's introspection does. It checks every rule above, written
apart from the builder:

- the one input and its four pushes, ending in this redeem script;
- output 0's script, category, capability and next state;
- CLTV, the leaf and proof, the ASERT and fallback bands;
- the reward, the shares and their outputs;
- the `S` to `3S` fee, the size bound, the vault's 800 satoshis and dust,
  and share scripts of at most 35 bytes.

Every claim the builder makes passes it before it is broadcast.

### Cross-checks with BCH P2Pool

- `src/stratum_v2/merge/test_data/tsafa-chipnet-fixture.json` is BCH
  P2Pool's fixture, copied verbatim from its
  `tools/safa-auxpow/test_data` (its covenant as of P2Pool commit
  88b696e8, with the test-network band and thread bound). It is what
  P2Pool's `cli.mjs params` prints for a fake Chipnet deployment: funding
  outpoint `11…11:0`, 4 threads, 84,000,000,000 virtual satoshis, never
  broadcast.
- Pickaxe's loader reproduces P2Pool's values from that file's redeem
  script (1,364 bytes):
  - `K` 1,944;
  - the lock `aa20af6a8469…de0787`;
  - `lock_hash` `f1a16f57…d1135caf`;
  - the vault address `bchtest:rwhk4prf…`.
- `safa_auxpow::tests::the_fixture_claim_matches_p2pools` builds a claim for
  it from fixed inputs. With `PICKAXE_SAFA_EXPORT=DIR`, it writes P2Pool's
  `cli.mjs claim` inputs there (`deployment.json`, `claim.json`) beside
  Pickaxe's hex. Once P2Pool's hex for them is in
  `test_data/tsafa-chipnet-claim.json`, the test compares the two byte for
  byte.

## What Pickaxe does today

- Builds the commitment and the tree into every job when a token is
  merge-mined: solo, with the donation's split in each miner job's leaves,
  and on a public pool one root per miner, with their donation and the
  operator's fee.
- Lets a public pool's miners choose their donation in their login, and
  discloses their leaves and proofs to extended channels and SV1 firmware
  that ask, and in the leaf log for standard channels.
- Under Job Declaration, discloses the declared leaves to the pool, and as a
  pool, checks them.
- Checks every share against each token, hands wins to a claim worker, proves
  them, verifies each proof from its bytes as a covenant would, and saves it
  to `<config>.sv2-token-proofs.json`, readable by the owner alone, one claim
  per row with its `broadcast_after` time and the split preimage the claim
  reveals.
- Shows token wins on the dashboard and in the status file, never with an
  address, and says when the own-root check refused a job.
- Merge-mines tSAFA on Chipnet from an explicit deployment file and claims
  its wins through Fulcrum (see [tSAFA on Chipnet](#tsafa-on-chipnet)).
- A hidden `stratum-v2 serve --merge-test-token <DIFFICULTY>` merge-mines a
  Chipnet test token (a synthetic lock hash and state hash; the state moves to
  the winning header's hash after each proven win) to try all of this with real
  devices; mainnet refuses it. It rides Job Declaration too (`serve --upstream
  ... --job-declaration --merge-test-token 1000`); without `--job-declaration`
  the server refuses it at a pool.

## Still open

- **A first real token.** Its authors write the covenant (with these checks
  and the token-level rules) and Pickaxe adds its row to the registry; its
  claims follow tSAFA's builder if it is the SAFA covenant.
- **tSAFA in a VM.** Pickaxe's claims are checked against the covenant's
  rules in Rust, not run through the covenant: that needs tSAFA's compiled
  redeem script and a BCH 2026 VM (Libauth, as P2Pool's tests use), and a
  run against BCHN once tSAFA is broadcast.
- **BCH P2Pool** building output 0 in this format, so Pickaxe can merge-mine
  there.
- **Disclosure at remote pools:** the adapter asks only its own server for
  the disclosure; asking a remote Pickaxe pool needs a way to tell it from a
  pool that does not know the negotiation.
- **Live checks:** a BCHN node accepting a template with the commitment
  (`validateblocktemplate`), and a covenant judged by a BCHN regtest node.
