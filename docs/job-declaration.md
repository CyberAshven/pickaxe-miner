# Job Declaration (miners' own templates)

#### PR #42

SV2 Job Declaration lets a miner choose the transactions in the blocks they
mine while still mining at a pool: the miner's own node builds the template,
and the pool checks that the coinbase still pays what the pool requires. This
page describes Pickaxe's pool side, which ships first in **Coinbase-only**
mode. The client side (Join a pool with your own templates) and Full-Template
mode follow.

No other BCH pool offers Job Declaration today. SRI's and ckpool's Job
Declaration servers validate through Bitcoin Core's IPC, which BCH nodes do
not have, so a Pickaxe client's counterpart is a Pickaxe pool.

## Turning it on

Job Declaration needs a public pool, where each miner's blocks pay their own
address:

```text
pickaxe_miner stratum-v2 serve --config pool.json --listen 0.0.0.0:3336 \
  --sv1-listen 0.0.0.0:3333 --public --pool-fee 1 --accept-job-declaration coinbase
```

It is off by default. It shares the pool's SV2 port and key: a client pins one
key for both of its sessions. The overview shows a line such as "Job
Declaration clients 1 · 3 tokens · 2 custom jobs · 0 refused · 0 blocks", and
the status file's `jd_server` has the same counts, never an identity, a token
or an address.

## The Coinbase-only flow

| Session | Client sends | Pool answers |
|---|---|---|
| Job Declaration | `SetupConnection{protocol 1, flags 0}` | `Success`; Full-Template (flag bit 0) is refused with `unsupported-feature-flags` until it ships |
| Job Declaration | `AllocateMiningJobToken{user_identifier, request_id}` | `Success{request_id, token, coinbase_outputs}` |
| Mining | `SetupConnection{protocol 0, flags 0x06}` (work selection, version rolling) | `Success` |
| Mining | `OpenExtendedMiningChannel{user_identity, min_extranonce_size 16}` | `Success{extranonce_prefix 16 bytes, extranonce_size 16}`, and no job of the pool's |
| Mining | `SetCustomMiningJob{token, version, prev_hash, min_ntime, nbits, coinbase fields, merkle_path}` | `Success{job_id 0x8000_0001, …}` or `Error{code}` |
| Mining | `SubmitSharesExtended` | `SubmitShares.Success` or `.Error` |

- The `user_identifier` is the miner's payout address, as their mining
  username is at a public pool (an optional `.worker` suffix is allowed). One
  that is not an address ends the Job Declaration session, because the spec
  has no error message for it.
- `coinbase_outputs` names the outputs the coinbase must carry, each worth 0
  in the message: the miner's own script first (the spec's pool payout
  output), then the pool's fee, then the Pickaxe donation. Outputs whose rate
  is 0 are left out.
- A connection may allocate 20 tokens a minute; more are answered late, never
  dropped. A token lives 10 minutes, works once, and only on a channel whose
  user identity is the same address. A closed Job Declaration session's tokens
  go with it.
- A work-selection connection's extended channels are custom-only: 16
  extranonce prefix bytes and 16 rollable bytes, and only the client's own
  jobs. Its standard channels are refused with
  `standard-channels-not-supported-for-custom-work`.
- The pool's next parent revokes the client's custom jobs on the old one;
  shares on them are `stale-share`.

The client's coinbase is: version (1 or 2) ‖ one input with the null prevout ‖
a script of `coinbase_prefix`, the channel's 16-byte prefix and 16 rollable
bytes (at most 100 bytes) ‖ `coinbase_tx_input_n_sequence` ‖
`coinbase_tx_outputs` ‖ `coinbase_tx_locktime`. Its merkle root folds the
coinbase's hash up `merkle_path`.

### What the pool checks on `SetCustomMiningJob`

| Check | Error code |
|---|---|
| work selection not negotiated, or the pool does not accept Job Declaration | `jd-not-supported` |
| not a custom-only channel | `invalid-channel-id` |
| `prev_hash` is not the pool's current parent | `stale-chain-tip` |
| `nbits` differs from the pool's | `invalid-nbits` |
| `version` differs outside the version-rolling bits | `invalid-version` |
| `min_ntime` before the template's minimum, or over 10 minutes ahead | `invalid-min-ntime` |
| the prefix does not start with the minimal BIP34 height push, or the script would exceed 100 bytes | `invalid-coinbase-prefix` |
| the coinbase version is not 1 or 2 | `invalid-coinbase-tx-version` |
| the coinbase would be under 65 bytes | `invalid-coinbase-tx` |
| the token is unknown, expired, spent, forged or another identity's | `invalid-mining-job-token` |
| the outputs break the payout rule below | `invalid-coinbase-tx-outputs` |

### The payout rule

A custom job cannot rotate work the way the pool's own jobs do, so it pays
the whole donation and the whole pool fee in its coinbase, whatever the
pool's fee mode. With V the sum of the coinbase's outputs:

- D = V × donation, F = (V − D) × fee, both rounded down.
- The donation's script gets at least D, the fee's script at least F, and the
  miner's script something whenever V − D − F > 0. Amounts are summed per
  script, so a fee paid to the miner's own address needs the sum.
- Other outputs are the miner's own business and are allowed.
- Merge-mining: at most one commitment, as output 0, worth 0 and exact;
  tickets anywhere; with a commitment, fewer than 253 outputs.
- No output may carry CashTokens (a coinbase cannot create them).

At 1.5% donation and a 1% fee, a 312,500,000-satoshi coinbase pays the
donation 4,687,500, the fee 3,078,125 and the miner 304,734,375. The rates a
job must pay are fixed when its token is allocated.

### Blocks

The pool never sees a Coinbase-only job's transactions, so it cannot build the
block: the client's node submits it. The pool lists it on the dashboard as
"submitted by the miner's node", never saves it in its block journal, and does
not count its parent as solved, so the pool's own block on that parent is still
saved. Merge-mined token wins on custom jobs belong to the miner.

## The PX v1 token

The spec leaves `mining_job_token` opaque. A Pickaxe pool's tokens carry the
rates a custom job must pay, so a Pickaxe client can build a coinbase the pool
accepts; other clients treat them as opaque, as the spec says. 26 bytes:

```text
0  2 "PX" | 2 1 version 01 | 3 2 donation_bps u16le | 5 2 fee_bps u16le
7  1 donation_output (index into coinbase_outputs, ff = none) | 8 1 fee_output (ff = none)
9  1 flags: bit 0 declared, bit 1 full-template connection
10 8 serial u64le | 18 8 secret (never logged)
```

Example, donation 1.50%, fee 1.00%, on a Full-Template connection:
`50 58 01 96 00 64 00 02 01 02 <serial 8> <secret 8>`. A client that is not
Pickaxe can follow the rule above only when it knows the rates, so its custom
jobs pass when the pool's fee and donation are both 0, or when it reads them
from the token.

## Not yet

- Full-Template mode (`DeclareMiningJob`, missing transactions, BCHN's
  `validateblocktemplate`, `PushSolution`), so the pool can propagate blocks
  and SRI-style clients can connect.
- The client side: Join a pool with your node's templates, falling back to the
  pool's jobs when the pool refuses them.
- Setup rows and the dashboard's Job Declaration page.
