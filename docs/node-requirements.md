# What Pickaxe needs from a BCH node

Pickaxe talks to a Bitcoin Cash node over its JSON-RPC, as BCHN and Knuth
both offer it. The node gives the ASIC server its block templates, takes the
blocks it finds, and can be the source of GPU token jobs (PHOTON, ELECTRIC)
instead of a Fulcrum server. These calls follow Bitcoin Core's API, which
BCHN keeps; they are not a formal standard, so each node implements its own
set. This page lists every call Pickaxe makes, what it is for, and what BCHN
and Knuth answer.

Knuth was checked live, read-only, against Knuth 1.7.0 on Chipnet on
2026-11-27 (`live_knuth_answers_everything_pickaxe_asks_of_a_node` in
`src/node.rs`; see [Checking a node](#checking-a-node)).

## Calls

| Call | Pickaxe uses it for | BCHN 29.1 | Knuth 1.7.0 |
|---|---|---|---|
| `getblockchaininfo` | chain, sync state and tip | ✅ | ✅ |
| `getnetworkinfo` | the client's name; `relayfee`, the claim-fee floor | ✅ | ✅ |
| `getblockcount`, `getblockhash`, `getbestblockhash`, `getblockheader` | the tip and its blocks; the setup's chain check | ✅ | ✅ |
| `getblocktemplate` | ASIC block templates | ✅ | ✅ (its proposal mode is not used by mining) |
| `submitblock` | blocks the ASICs find | ✅ | ✅ |
| `getblock` (verbosity 2) | following the PHOTON baton through new blocks; ELECTRIC's spends | ✅ | ✅ inputs `txid`/`vout`, outputs `scriptPubKey.hex` and `tokenData` |
| `gettxout` | the PHOTON baton; the setup's "follows PHOTON" check; the block a confirmed transaction is in | ✅ | ✅ (new in 1.7.0) |
| `getrawtransaction` | a transaction's bytes | ✅ | ✅ with the block hash: Knuth keeps no transaction index by design, so Pickaxe finds the block first (through `gettxout` or recent blocks) |
| `getrawmempool` (verbose: `depends`, `spentby`) | a baton already spent in the mempool | ✅ | ✅ answers; its mempool was empty when checked |
| `getmempoolinfo` | `mempoolminfee`, the mempool's dynamic fee minimum | ✅ | ⚠️ leaves `mempoolminfee` out (k-nuth/kth#1085, open); Pickaxe then uses `relayfee`, the minimum such a node enforces |
| `sendrawtransaction` | claims and refunds | ✅ | ✅ |
| `scantxoutset` | finding the PHOTON baton on a first start with no known baton; an ELECTRIC fee pot's coins on a node-only setup | ✅ (also its own `tok(<category>)` scan object) | ❌ Method not found: requested as k-nuth/kth#1092, see [below](#scantxoutset-on-knuth) |
| ZMQ `zmqpubhashblock` | new blocks reach the ASICs at once | ✅ | ❌ no ZMQ: the ASIC server asks the node for its tip about every 250 ms instead |

`testmempoolaccept` is not used by Pickaxe (Knuth master has it since
k-nuth/kth#867).

## What each node can do with Pickaxe

| | BCHN 29.1 | Knuth 1.7.0 |
|---|---|---|
| ASIC mining (Solo, Public Solo Endpoint, the Pool modes) | ✅ | ✅ |
| GPU tokens with a Fulcrum server for the first start (the default) | ✅ | ✅ the baton is then followed through the node |
| GPU tokens from the node alone, with no Fulcrum server at all | ✅ | ❌ until Knuth has `scantxoutset` |
| ELECTRIC's fee pot read from the node alone | ✅ | ❌ until Knuth has `scantxoutset` |
| Templates from the node's own SV2 Template Provider | — (BCHN has none) | waits for Knuth's Noise transport (k-nuth/kth#807): Pickaxe takes templates over encrypted links only |

## scantxoutset on Knuth

BCHN's `scantxoutset start [descriptors]` scans the whole UTXO set for the
given scripts (`raw(<hex>)`, `addr(<address>)`, and BCHN's own
`tok(<category>)`). It answers `success`, `searched_items`, `unspents` (each
with `txid`, `vout`, `scriptPubKey`, `amount`, `height` and, for CashTokens
outputs, `tokenData`), `total_amount` and, when tokens are found,
`token_total_amounts`. BCHN leaves out the top-level `height` and
`bestblock` that Bitcoin Core has, and Pickaxe accepts either form.

Pickaxe calls `scantxoutset start ["raw(<PHOTON covenant locking bytecode>)"]`
on a first start, and picks the unspent whose `tokenData` has PHOTON's
category and a mutable NFT. After that it follows the baton through the
mempool and new blocks, and scans again only as a last resort (more than 144
blocks behind, or the baton lost). Knuth can already walk its UTXO set, so
the call is a whole-set pass with no new index, as on BCHN. It is
requested as k-nuth/kth#1092, an RPC for the node's own operator (row 47
of k-nuth/kth#773, which tracks Knuth's JSON-RPC gaps).

## Checking a node

- `pickaxe stratum-v2 check-node --chipnet --node-rpc URL` checks the node and
  a full block template without mining.
- The setup's BCH node list checks every saved node and says whether it
  follows PHOTON.
- The full read-only check above runs with
  `PICKAXE_KNUTH_RPC=http://USER:PASSWORD@HOST:PORT cargo test --lib -- --ignored live_knuth_answers_everything_pickaxe_asks_of_a_node --nocapture`.
  It sends nothing that changes the node: submission and broadcast are
  tried with invalid data only.
