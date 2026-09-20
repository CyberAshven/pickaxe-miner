# Codex sec15 — PHOTON job source

PHOTON jobs = Fulcrum CashToken baton discovery (listunspent). Node = validate/broadcast. Never map previousblockhash to baton_txid. GBT template/submitblock stay as optional tooling only.

# Pickaxe Miner

Native Rust **GPU** PHOTON miner (CLI now; Ratatui TUI next). Windows + Linux. NVIDIA (CUDA) and AMD (HIP/wgpu) backends — Lead Dev owns kernels.

**Definition of done:** Bandar mining live with a commit on [CyberAshven/pickaxe-miner](https://github.com/CyberAshven/pickaxe-miner).


## Templates / submit (architecture lock)

**Node RPC is first-class** for mining templates and block submit:

- `getblocktemplatelight` with fallback to `getblocktemplate`
- `submitblocklight` with fallback to `submitblock`

```text
pickaxe_miner --source node --node-rpc http://user:pass@127.0.0.1:8332 [--fulcrum wss://…]
```

Interactive: `source node`, `node http://…`, `template`, `submitblock <hex> [job_id]`.

**Fulcrum/Electrum is auxiliary** (wallet/UTXO/PHOTON baton index until a node-indexed path exists) — not the template source. Ban-safe sequential bootstrap stays.

## Quick start

```text
cargo run
```

## Win path (Electrum / win-tx) — production flow

Coinbase-style **98% miner / 2% donation** on the win tx only (`bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3`). Never skim unrelated funds. Never keys/mnemonics in this lane.

1. `payout bitcoincash:…` — your address
2. `fulcrum wss://…` (optional Start9) or use bootstrap
3. `job` / `arm` — live baton + unsigned 98/2 template + `message_sha256` for Schnorr
4. Lead Dev / GPU supplies nonce + Schnorr → `applysig <nonce> <pk33hex> <sig64hex>`
5. `broadcast` — submits last armed hex (Fulcrum `blockchain.transaction.broadcast`, then node `sendrawtransaction` if set). **Never auto.**

Also: `dryrun`, `connect`, `servers`, `node http://user:pass@host:8332`, `nodeprobe`.

### Ban-safe connectivity

Sequential endpoint tries + exponential backoff. No parallel fan-out.

- Fulcrum/Electrum WSS bootstrap: `FULCRUM_WSS_BOOTSTRAP` in `src/protocol.rs`
- Native node bootstrap: `NODE_RPC_BOOTSTRAP` (tiny; custom `node` is the usual path)

## Ownership

| Lane | Owner |
|------|--------|
| Electrum, win-tx template, arm/applysig/broadcast | Dev Assist |
| CUDA/HIP/wgpu kernels, intensity, Stage B→C, Ratatui | Lead Dev |

`reference/` mirrors https://photon.postcorps.com/ for protocol study only (not the product).
