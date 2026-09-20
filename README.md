# Pickaxe Miner

Native Rust PHOTON miner — interactive CLI first; high-perf GPU kernel later; GUI later.

## Stage 2 CLI

```text
cargo run
```

Commands: `help`, `status`, `intensity <0-100>`, `payout <cashaddr>`, `donation`, `start`, `stop`, `split <raw>`, `quit`.

### Donation (distribution builds)

- **2% donation** (never "dev fee") → `bitcoincash:qqn3aqnrarpvecss9vned5v9693j9p37w5pmzz4mn3`
- Miner keeps **98%**
- **Coinbase-style:** two outputs on the verified win tx only — visible before arm; never skim unrelated funds or keys

`reference/` mirrors https://photon.postcorps.com/ for protocol study only (not the product).
