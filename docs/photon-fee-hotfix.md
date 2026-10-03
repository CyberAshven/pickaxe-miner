# PHOTON fee hotfix validation — 2026-10-03

Version remains 0.0.3. A 629-byte claim now pays 629 sats at the normal
1 sat/byte floor instead of always paying 800 sats. The reward retains 700 sats;
the savings remain in the baton. No payout addresses or donation shares change.

The runtime refreshes Fulcrum relay policy and configured node mempool policy
every 30 seconds. A changed rate replaces the search generation and reaches
both GPU work and independent winner reconstruction. It never rewrites a
winning transaction. The covenant limits the total baton decrease to 1,500
sats, leaving at most 800 sats for fees. An unaffordable rate stops new work.

## Live Chipnet evidence

One Rust T2 miner ran through policy changes of 1.0 → 1.1 → 1.2 → 1.0 sats/byte.
A temporary loopback WebSocket proxy changed only `blockchain.relayfee` replies;
all chain queries and broadcasts went to the real Chipnet server. These were
controlled policy changes, not a claim that Chipnet's actual minimum changed.

Both `chipnet.bch.ninja:50004` and `chipnet.imaginary.cash:50004` returned the
same transaction bytes. Each sample independently passed BCH 2026 standard and
consensus VM verification with its actual previous output.

| Policy (sats/byte) | Bytes | Fee (sats) | Transaction |
| --- | --- | --- | --- |
| 1.0 after decrease | 629 | 629 | `00000000a3bc32a0e011a44d4a2ba91c9f66b6796fb29f4a2c579aa0f28efcda` |
| 1.1 | 629 | 692 | `00000000984cd07c3efb82adcecefc85c4f0953d52b6b349f59deafc9d9cbbad` |
| 1.2 | 629 | 755 | `000000001ca7f7cc257dda03ca0f283d5b1617f429ea10807067db7d2de4031c` |

The 1.1 and 1.2 samples were confirmed by both servers in block
`000000000c203e651b5da94e405f7554082bcc34cbedd56a0c3978be54c0a25f`.
The final 1.0 sample was propagated but still unconfirmed at the last check.
At 3m48s TUI uptime, including the final pause, the run showed 69 found,
one stale winner from the retired generation during the 1.0 → 1.1 change,
zero rejected, zero pending, and zero reconnects. Mainnet mining was restored.

## Local checks

- 299 serial CPU tests passed; GPU-only tests were excluded from this run.
- Five Rust T2 GPU winners covered all active recipients on both deployments,
  at 1.1/1.2 sats/byte, with independent transaction/signature verification and
  standard/consensus VM checks.
- Fifty CPU-generated claim and chained-successor transactions passed the
  same independent VM checks, including age-width boundaries and journal recovery.
- The live-policy regression checks cache expiry, fee-only generation changes,
  upward rounding, and rejection when 1.3 sats/byte exceeds the covenant budget.
- Formatting, all-target/all-feature Clippy, and release-channel tests passed.

Reproduce host and VM checks with:

```sh
cargo test --locked --features rust-t2 -- --skip if_cuda --skip if_wgpu --test-threads=1
npm ci --prefix tools/reward-policy-vm --ignore-scripts
npm test --prefix tools/reward-policy-vm
python tools/test_release_channel.py
```

With the Rust PTX built and no live miner running, run
`cargo test --locked --features rust-t2 direct_work_fee_pays_each_recipient_across_controls_if_cuda_present -- --nocapture --test-threads=1`.
Set `PICKAXE_DIRECT_GPU_PROOF` to a JSON file to export those five GPU winners,
then verify it with `node tools/reward-policy-vm/direct-reward.mjs <file>`.

Local tested Windows executable SHA-256:
`ee2798c0d59cd97871ec8f003129fa6732e8ca7ae7ccea5387878ba611b1ab62`.
Unchanged Rust PTX SHA-256:
`0e3e996528b5cdd66f9b8d1ddacc1d9e246b64461ee9b51f2b583eaa6feec024`.

## Replacing the existing downloads

After merge and successful checks, dispatch Release from the exact merged
commit with `tag=pickaxe-miner-v0.0.3` and `source_commit=<full merged SHA>`.
The workflow requires that commit to be on master and descend from the original
tag. It rebuilds the archives, checksums and attestations, and records the actual
source commit and source archive in the release notes. The original tag and
already-installed binaries remain unchanged.
