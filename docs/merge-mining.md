# Merge-mining BCH tokens (v1 draft)

Pickaxe can merge-mine covenant tokens on Bitcoin Cash: the same SHA-256 work
that mines BCH also mines tokens, with no hashrate taken from BCH. No such
token exists yet, so both networks' token lists are empty and every coinbase
is exactly what it was without this feature. This page is the specification a
token's authors build against. It is a **draft**: the byte layouts below carry
version numbers and may change before the first token.

## Two kinds of token, in one coinbase

- **Case A, share target.** The token accepts any share whose hash meets the
  token's own target, which is easier than a BCH block. A miner wins tokens
  without finding a BCH block. Any SHA-256 work can win it, so a token's
  difficulty adjustment should follow the token's own pace (its baton's age),
  never the header's time.
- **Case B, block required.** The token accepts only work that became a BCH
  block. The block's coinbase holds a small ticket output for the token; the
  claim spends that ticket once the block is 100 blocks deep, which proves the
  block is in the chain.

One commitment in the coinbase covers every token of both kinds, so one share
is checked against all of them at once.

## No hashrate loss

- Devices hash the same 80-byte headers as before. The coinbase grows by 53
  bytes (and 46 per Case B token), which firmware hashes once per extranonce,
  not per nonce.
- Every accepted share is compared with the easiest Case A target: one
  comparison. A winning share goes to a separate claim worker without the
  share's acknowledgement waiting for it.
- While Case A tokens are merge-mined, a device's share target is made
  easier, up to the easiest token target, so firmware sends every hash that
  wins a token. It is never more than 15 times easier than vardiff's target.
- A token journal that cannot be written turns token claims off and leaves
  BCH mining unaffected.

## Where merge mining works

- **Solo and your own public pool:** yes. Pickaxe builds the coinbase.
- **Join a pool:** no. The pool builds the coinbase, so Pickaxe cannot add the
  commitment (that needs Job Declaration, not yet available for BCH pools).
- **GPU mining:** not applicable. PHOTON's work is over its own transaction,
  not a block header.

## Donation

Merge-mined tokens follow the one BCH ASIC donation setting (default 1.5%,
anything from 0% to 100% in Advanced settings). As for BCH, two thirds is a
split of each claim, bound in the leaf of miner jobs, and one third is mining
work: a share of jobs whose leaves pay the Pickaxe donation address. A public
pool's fee reaches tokens through its fee-work jobs only.

## Byte layouts (normative)

### Commitment: coinbase output 0 (53 bytes)

```text
off len value
0   8   00 00 00 00 00 00 00 00     value 0
8   1   2c                          script length 44
9   1   6a                          OP_RETURN
10  1   2a                          push 42
11  4   43 54 4d 4d                 "CTMM"
15  1   01                          version
16  32  aux_root                    as hashed, not reversed
48  1   h                           tree height, 0..=16
49  4   aux_nonce                   u32 LE
```

A covenant finds it without a search: the coinbase head is
`cb[0 .. 47+L]`, where `L = cb[41]` (at most 100): version (4), `01`, 32 zero
bytes, `ffffffff`, L, the script-sig (L), the sequence (4) and the output count
(one byte, below `0xfd`). Output 0 follows it. A coinbase is at least 104
bytes, so it can never pass for a 64-byte merkle node.

### Leaf (176 bytes, hashed once with SHA-256)

```text
off len field          Case A (share target)                  Case B (block required)
0   4   "CTML"         43 54 4d 4c
4   1   version        01
5   1   mode           41                                      42
6   32  category       the baton's token category (input 0)
38  32  anchor_hash    the baton's outpoint hash (input 0)    00 x 32
70  4   anchor_index   the baton's outpoint index, u32 LE     the ticket's vout, u32 LE
74  32  payout_hash    HASH256(output 1 locking bytecode)
106 4   target_bits    the token's compact target, u32 LE     00000000
110 2   split_bps      u16 LE, 0..=10000
112 32  split_hash     HASH256(output 2 locking bytecode), or 00 x 32 when split_bps is 0
144 32  ext            token-defined, 00 x 32 if unused
```

The anchor stops replays (an outpoint is spent once), the payout and split
stop redirection, and the target stops a claim at an easier one. A covenant
builds the leaf from introspection; a proof never supplies leaf data.

### Tree and slots

- A token's slot: `u32le(SHA256(category ‖ mode ‖ u32le(aux_nonce))[0..4]) mod 2^h`.
- A node is `SHA256(left ‖ right)`. At level i, bit i of the slot set to 1
  means `SHA256(sibling ‖ current)`. An empty slot is 32 zero bytes. The depth
  is the commitment's `h`.
- One leaf per (token, mode) per coinbase. At most 16 tokens and 16 tickets.

### Ticket output (46 bytes, Case B only)

```text
value (8, the registry's value; 0 by default) | 25 | 00 ce 21 <category 32> <capability 1> 87
```

The script is `OP_0 OP_UTXOTOKENCATEGORY <category ‖ capability> OP_EQUAL`:
spendable only beside the token's baton at input 0. Tickets follow the payout
outputs; their vouts are bound in the Case B leaves. Coinbase outputs cannot
carry tokens, so tickets hold plain BCH.

### Proof (`AuxProof` v1)

```text
0    4    "CTMP"
4    1    01
5    1    mode
6    170  leaf bytes 6..176
176  1    h
177  4    aux_nonce, u32 LE
181  32h  aux branch, leaf to root
..   2+n  u16 LE n, then the coinbase head (47+L bytes)
..   2+n  u16 LE n, then the coinbase tail (outputs 1.. and the locktime)
..   1+32d  d (at most 32), then the coinbase's merkle branch (Case B: d = 0)
..   80   the header (Case A only)
```

## What a covenant checks

**Case A:**
1. The proof decodes canonically.
2. Rebuild the leaf from introspection: the category, input 0's outpoint,
   `HASH256(output 1)`, the split, the token's own target and `ext`.
3. Compute the slot and fold the aux branch (its length is `32·h`, `h` at
   most the token's limit) to the aux root.
4. Build output 0 from the root, `h` and the nonce.
5. Check the coinbase head: its length is `47 + head[41]`, `head[41]` is at
   most 100, `head[4..41]` is `01`, 32 zero bytes and `ffffffff`, and the
   output count is from 1 to `0xfc`.
6. `txid = HASH256(head ‖ output 0 ‖ tail)`, the tail being at least 4 bytes.
7. Fold the merkle branch at index 0 to the merkle root.
8. Rebuild the header from its first 36 bytes, the root and its last 12.
9. `HASH256(header)`, read as an unsigned little-endian number, is at most the
   token's target (`mant << 8·(exp−3)`, with `3 ≤ exp ≤ 32` and
   `0 < mant < 0x800000`).

**Case B:** steps 1 to 6 with mode `B`, the anchor (32 zero bytes, the ticket's
vout) and target 0; then input 1 spends output `vout` of `txid`. Optionally,
the block's height (from the script-sig) is at least the token's start
height. The coinbase check in step 5 is what makes Case B sound: only a real
coinbase can be the ticket's parent, and a coinbase output can be spent only
once it is 100 blocks deep in the active chain.

Pickaxe's reference verifier (`src/stratum_v2/merge/verify.rs`) performs these
steps and checks every proof before keeping it.

## What Pickaxe does today

- Builds the commitment, the tree and the tickets into every job when a token
  is merge-mined, for SV2 standard and extended channels and SV1 firmware
  through the adapter.
- Checks every share against each token, hands wins to a claim worker, proves
  them and saves them to `<config>.sv2-token-proofs.json`, readable by the
  owner alone.
- Shows token wins on the dashboard and in the status file, never with an
  address.
- A hidden `stratum-v2 serve --merge-test-token <DIFFICULTY>` merge-mines a
  Chipnet test token to try all of this with real devices; mainnet refuses it.

## Still open

- **A first real token.** Its authors write the covenant (with these checks)
  and Pickaxe adds its row to the registry and a claim builder that broadcasts
  its claims.
- **Case B ticket maturity.** Pickaxe proves a found block's tickets at once;
  waiting 100 blocks and broadcasting is the claim builder's job.
- **Live checks:** a BCHN node accepting a template with the commitment and a
  ticket (`validateblocktemplate`), and a covenant judged by a BCHN regtest
  node.
