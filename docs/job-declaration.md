# Job Declaration (miners' own templates)

#### PR #42

SV2 Job Declaration lets a miner choose the transactions in the blocks they
mine while still mining at a pool: the miner's own node builds the template,
and the pool checks that the coinbase still pays what the pool requires.
Pickaxe has both sides, in both of the spec's modes:

- **Full-Template**: the miner declares each template with its transactions.
  The pool's node checks it, and the pool sends the miner's blocks to its own
  node too. This is the mode SRI-style clients use.
- **Coinbase-only**: the miner declares only the coinbase. The pool never
  sees the transactions, and only the miner's node submits the blocks.

No other BCH pool offers Job Declaration today. SRI's and ckpool's Job
Declaration servers validate through Bitcoin Core's IPC, which BCH nodes do
not have, so a Pickaxe client's counterpart is a Pickaxe pool.

## Turning it on

Job Declaration needs a public pool, where each miner's blocks pay their own
address:

```text
pickaxe_miner stratum-v2 serve --config pool.json --listen 0.0.0.0:3336 \
  --sv1-listen 0.0.0.0:3333 --public --pool-fee 1 --accept-job-declaration
```

`--accept-job-declaration` alone accepts both modes; `full` or `coinbase`
accepts one. It is off by default. It shares the pool's SV2 port and key: a
client pins one key for both of its sessions. In the setup (Run a pool, ASIC
pool) the **Miner templates** row under Advanced sets it: off, full template
and coinbase only, full template only, or coinbase only.

With Full-Template, the pool's first node checks declared templates with
BCHN's `validateblocktemplate`; a node without that call (such as Knuth)
leaves the structural checks only, and the dashboard says "structural only".
Blocks found on declared templates are saved in `<config>.sv2-jd-blocks.json`
(owner-only) until the pool's node answers.

The overview shows a line such as "Job Declaration clients 1 · 3 tokens · 2
declared (1 missing-transaction rounds, 1 checked: validateblocktemplate) ·
2 custom jobs · 0 refused · 1 blocks · pool's node: 1 accepted / 0 pending /
0 rejected". The status file's `jd_server` has the same counts, never an
identity, a token or an address.

## Tokens and channels (both modes)

| Session | Client sends | Pool answers |
|---|---|---|
| Job Declaration | `SetupConnection{protocol 1, flags}`: flag bit 0 asks for Full-Template, bit 0 clear for Coinbase-only | `Success`, or `Error`: Full-Template at a Coinbase-only pool is `unsupported-feature-flags`, Coinbase-only at a Full-Template pool is `missing-declare-tx-data-flag` (as SRI answers) |
| Job Declaration | `AllocateMiningJobToken{user_identifier, request_id}` | `Success{request_id, token, coinbase_outputs}` |
| Mining | `SetupConnection{protocol 0, flags 0x06}` (work selection, version rolling) | `Success` |
| Mining | `OpenExtendedMiningChannel{user_identity, min_extranonce_size 16}` | `Success{extranonce_prefix 16 bytes, extranonce_size 16}`, and no job of the pool's |

- The `user_identifier` is the miner's payout address, as their mining
  username is at a public pool (an optional `.worker` suffix is allowed). One
  that is not an address ends the Job Declaration session, because the spec
  has no error message for it.
- `coinbase_outputs` names the outputs the coinbase must carry, each worth 0
  in the message: the miner's own script first (the spec's pool payout
  output), then the pool's fee, then the Pickaxe donation. Outputs whose rate
  is 0 are left out.
- A connection may allocate 20 tokens a minute; more are answered late, never
  dropped. A token lives 10 minutes (an hour once declared), works once, and
  only on a channel whose user identity is the same address. A closed Job
  Declaration session's tokens go with it.
- A work-selection connection's extended channels are custom-only: 16
  extranonce prefix bytes and 16 rollable bytes, and only the client's own
  jobs. Its standard channels are refused with
  `standard-channels-not-supported-for-custom-work`.
- The pool's next parent revokes the client's custom jobs on the old one;
  shares on them are `stale-share`.

## The Full-Template flow

| Session | Client sends | Pool answers |
|---|---|---|
| Job Declaration | `DeclareMiningJob{request_id, token, version, coinbase_tx_prefix, coinbase_tx_suffix, wtxid_list, excess_data}` | `ProvideMissingTransactions{request_id, positions}` for transactions its node lacks, or `Success{request_id, new token}`, or `Error{code, details}` |
| Job Declaration | `ProvideMissingTransactions.Success{request_id, transactions}` | the next round, `Success` or `Error` |
| Mining | `SetCustomMiningJob` with the new token | `Success{job_id 0x8000_0001, …}` or `Error{code}` |
| Mining | `SubmitSharesExtended` | `SubmitShares.Success` or `.Error` |
| Job Declaration | `PushSolution{extranonce, prev_hash, nonce, ntime, nbits, version}` on a block | nothing; the pool saves and submits the block |

- `coinbase_tx_prefix` runs up to the whole extranonce (the channel's 16-byte
  prefix and the 16 rolled bytes), `coinbase_tx_suffix` from the input's
  sequence. `wtxid_list` holds the transaction ids (BCH has no witnesses) in
  canonical order; `excess_data` is ignored.
- A Full-Template session's frames may take 16 MiB each way: a declaration
  lists up to 65,535 transaction ids, and missing transactions come back in
  one frame. A Coinbase-only session keeps a device's 1 MiB.

### What the pool checks on `DeclareMiningJob`

| Check | Error code |
|---|---|
| more than 30 declarations a minute on the connection | `invalid-job` |
| the token is not one this connection allocated, is spent, expired or already declared | `invalid-mining-job-token` |
| the pool has no template now | `stale-chain-tip` |
| a segwit coinbase, a version other than 1 or 2, a script over 100 bytes, an extranonce outside 1 to 32 bytes, malformed outputs, or under 65 bytes | `invalid-coinbase-tx` |
| the input is not one null input | `invalid-coinbase-tx-input` |
| the script does not start with the pool's height: the block before or after the pool's (a race at a new block) | `stale-chain-tip` |
| any other height | `invalid-coinbase-tx` |
| the outputs break the payout rule below | `invalid-coinbase-tx` |
| the transaction ids are not in canonical (CTOR) order, or more than 65,535 | `invalid-job` |
| `version` differs from the pool's outside the version-rolling bits | `invalid-job` |
| a provided transaction is malformed, a coinbase, or not the one asked for | `invalid-job` |
| transactions still missing after 16 rounds, or 30 seconds | `missing-txs` |
| the block would exceed the pool's block size | `invalid-job` |

Missing transactions are looked up in the pool's current template, its two
previous ones and the latest 128 MiB of transactions clients provided; at
most 2,048 are asked for per round. Details never name an address.

### The node check

With every transaction in hand, the pool builds the block (a zero extranonce,
nonce 0) and its node checks it with `validateblocktemplate`, which checks
everything but the proof of work: the parent, the merkle root, CTOR, the
coinbase's value and every transaction's inputs.

- It runs on the connection's first declaration on each parent, then at most
  once a minute, and always when the client had to provide a transaction (one
  the pool's node has never seen). Other declarations pass on the structural
  checks above.
- At most 4 checks run at once across the pool; a declaration waits up to 20
  seconds for one, then gets `internal-error` ("validation busy"). A node that
  cannot be asked gives `internal-error` ("validation unavailable").
- A refusal that only means a race at a new block ("does not build on chain
  tip", an unknown parent, `bad-cb-height`, `prev-blk-not-found`,
  `inconclusive-not-best-prevblk`, `time-too-old`, `time-too-new`) is answered
  `stale-chain-tip`; any other is `invalid-job`. Either way `error_details`
  carries the node's reason verbatim. SRI clients fall back on any other
  error, so a race must not look like one (ckpool maps them the same way).

### `SetCustomMiningJob` on a declared token

The job must be exactly the one declared:

| Check | Error code |
|---|---|
| the token is not a declared one for this channel's address, or its job is gone (the connection keeps its last 8) | `invalid-mining-job-token` |
| `prev_hash` is not the declared parent, or the pool has moved on | `stale-chain-tip` |
| `nbits` differs | `invalid-nbits` |
| `version` differs outside the version-rolling bits | `invalid-version` |
| `min_ntime` before the pool's minimum, or over 10 minutes ahead | `invalid-min-ntime` |
| the coinbase version, prefix, sequence, outputs or locktime differ from the declaration | `invalid-coinbase-tx-version`, `invalid-coinbase-prefix`, `invalid-coinbase-tx-input-n-sequence`, `invalid-coinbase-tx-outputs`, `invalid-coinbase-tx-locktime` |
| the merkle path is not the declared transactions' | `invalid-merkle-path` |
| the declared extranonce is not the channel's 16 bytes plus 16 | `invalid-coinbase-tx` |

An allocated token from a Full-Template connection that has not been declared
gets `job-not-yet-validated`.

### Blocks

A block on a declared job is whole: the pool saves it in the JD journal, then
its node gets it, retried until the node answers. It reaches the pool either
as a share on the custom job or as the client's `PushSolution`, which the pool
matches by proof of work against the connection's last 8 declared jobs; one
block per parent is saved, and a solution no job matches is only counted. The
dashboard lists the block with the node's answer.

## The Coinbase-only flow

The client sets its job with the allocated token itself: `SetCustomMiningJob`
on the mining session, with no declaration. Its coinbase is: version (1 or 2)
‖ one input with the null prevout ‖ a script of `coinbase_prefix`, the
channel's 16-byte prefix and 16 rollable bytes (at most 100 bytes) ‖
`coinbase_tx_input_n_sequence` ‖ `coinbase_tx_outputs` ‖
`coinbase_tx_locktime`. Its merkle root folds the coinbase's hash up
`merkle_path`.

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

### Blocks

The pool never sees a Coinbase-only job's transactions, so it cannot build the
block: the client's node submits it. The pool lists it on the dashboard as
"submitted by the miner's node", never saves it in a journal, and does not
count its parent as solved, so the pool's own block on that parent is still
saved. Merge-mined token wins on custom jobs belong to the miner.

## The payout rule (both modes)

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

## Mining at a pool with your node's templates

The client side runs your node and a local server, as solo mining does, and
declares each template to the first pool:

```text
pickaxe_miner stratum-v2 serve --config miner.json --sv1-listen 0.0.0.0:3333 \
  --upstream stratum2+tcp://POOL:3336/KEY --job-declaration
```

`--job-declaration` alone is Full-Template; `coinbase` declares the coinbase
only. In the setup (ASIC, Join a pool) the **Your templates** row under
Advanced sets it: off, full template or coinbase only. While it is on, the BCH
node row follows it, and Start asks for your node, a valid payout address and
no other username at the pool. A saved profile keeps the mode, and the
server's Advanced page names it; `stratum-v2 watch` shows the same state and
counts as the overview line below.

- The pool must be a Pickaxe pool that accepts Job Declaration: other pools'
  tokens are opaque, so their fee and donation cannot be known.
- The identity at the pool is your payout address (the pool pays your blocks
  there); `--upstream-user` must be that address too.
- An uplink keeps a Job Declaration session and one work-selection channel at
  the pool, both pinned to the pool's key, and two tokens in hand. While it is
  active, the local server builds its jobs for the pool: the coinbase pays the
  pool's outputs (you, the pool's fee and the donation, as its tokens say) and
  its script carries the pool channel's prefix, then the job id, the lane of
  the device's channel and the device's 8 bytes, which are the 16 bytes the
  pool lets you roll.
- Full-Template: each template is declared with its transactions, the
  transactions the pool asks for are sent from the template, and the custom
  job is set with the declared token. A template over 65,535 transactions is
  not declared, and a request for transactions that cannot fit one 16 MiB
  frame drops that declaration: shares on that template do not reach the
  pool, though its blocks still go to your node. Coinbase-only: each template
  is set with `SetCustomMiningJob` and an allocated token.
- Devices connect to the SV1 listener as usual; the SV1 adapter tries the
  local server first, then the pools' own jobs. Accepted shares that meet the
  pool's target go to the pool on the custom job; until the pool confirms a
  job, up to 256 wait. The local server never waits on the pool.
- Blocks: your node builds and submits them, after they are saved in
  `<config>.sv2-jd-blocks.json` (owner-only, apart from the solo block
  journal). With Full-Template the block also goes to the pool as
  `PushSolution`, so the pool's node gets it too.
- A refusal for a race at a new block (`stale-chain-tip`) only drops that
  template. The fourth other refusal in a row on one parent, a refused custom
  job, 5 rejected shares among the last 20 sent (not counting races at a new
  block), a failed link or a change in the pool's fee or donation makes the
  local server stop offering work, and devices move to the pool's own jobs.
- The uplink then tries the next pool in `--upstream` order (wrapping),
  after 30 seconds, then 60, 120 and 300 while failures follow each other; a
  session whose pool accepted custom jobs starts the count again. Devices come
  back by themselves once Job Declaration has been active for 30 seconds:
  their sessions at the pool end and the adapter takes them to the local
  server first.
- A new template on the same parent reaches devices only once the pool has
  accepted its custom job, so devices never mine work the pool may refuse; a
  new parent goes at once. Templates on one parent go to the pool at most once
  every 5 seconds, so refreshes do not spend the pool's limit of 30
  declarations a minute.
- The overview shows a line such as "Job Declaration at pool.example:3336
  (full-template): active · 12 custom jobs · 0 refused · 12 declared · 0
  dropped · 1 blocks pushed · 340 shares sent (338 accepted, 2 rejected) · 0
  fallbacks", and the status file's `jd_client` the same counts.
- The donation is paid in the coinbase, as the pool's rule requires, so there
  is no donation work under Job Declaration.

## Not yet

- Merge-mined tokens under Job Declaration: the commitment and tickets in the
  declared coinbase.
- Job Declaration servers apart from the pools (`--jd-server`), and the
  client's own jobs when every pool is down.
- A live run against BCHN on Chipnet, and SRI's Job Declaration client against
  a Pickaxe pool.
