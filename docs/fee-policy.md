# Maintainer fee policy

PHOTON's BCH network fee is separate from the donation policy below. The miner
refreshes the connected Fulcrum's mempool minimum and any configured node's
minimum every 30 seconds, with a floor of 1 sat/byte. It rounds up by transaction
size and starts a new work generation when the rate changes. The reward keeps
700 sats; unused allowance stays in the baton. A rate exceeding the covenant's
1,500-sat total allowance stops new work rather than requesting user funding.
Older Fulcrum servers without `mempool.get_info` fall back to their advertised
relay minimum; they cannot expose a changing mempool floor through that method.

Edit `MiningToken::fee_policy` in `src/config.rs` to select a token's mode,
fee shares and payout addresses. This compiled policy is each token's
minimum: PHOTON's 4% work share, and 1.5% (also the default) for every token
added later (`donation::NEW_TOKEN_DONATION`). A miner can raise it, never lower
it, in 0.5% steps in the mining dashboard's Advanced settings (`a`), with
`--token-donation PERCENT`, or in a saved profile; anything above the minimum is
more of the work for the project's donation address (`fee_policy_at`). A
change applies to every GPU at its next batch and to every rig of a
coordinator at once, and a value below the minimum mines at the minimum. BCH
and its merge-mined tokens keep their own policy (`donation/bch.rs`), which a
miner can set down to 0%.
Addresses are validated before mining; each network may use different recipients.
Do not edit the historical address/percentage constants used by legacy recovery.

`src/donation.rs` defines work, reward-split and hybrid modes. Shares are
configured in basis points. PHOTON currently uses a 4% work
fee without a reward split. In a hybrid, the reward split applies only to
personal-work wins; work-fee wins pay their selected destination directly.
Integer reward allocations always conserve the full amount, including rounding.

A payout adapter must implement the selected mode and prove its transaction
funding and covenant rules. PHOTON's direct two-output claim currently accepts
work fees only. Selecting a reward split or hybrid for it fails before hashing;
it does not silently request a deposit, batch rewards, or ignore the split.
Future tokens may select any supported mode independently. BCH coinbase payouts
and token claims remain different transaction builders. No ASIC proxy or new
token is enabled by selecting a fee mode.

The work scheduler counts completed candidate hashes, not uptime, wins or peak
hashrate. It keeps its position across pauses, intensity changes, job updates and
key rotations, and randomizes the starting position. Each complete allocation
cycle implements the configured shares; short sessions have variance. The
recipient is committed before hashing. Changing recipients creates a fresh search
identity so returning to a recipient does not repeat its previous search.

The runtime independently rebuilds each winner for an authorized recipient and
journals the exact transaction before broadcast. Recovery uses those bytes even
if a later build changes recipient addresses. Journal versions 3 and 4 remain
readable; new direct rewards use version 5. Existing Chipnet local-key rewards
retain their previous recovery rules; new claims do not use that wallet.

Validation:

```powershell
cargo test --locked --all-features -- --skip if_cuda --skip if_wgpu --test-threads=1
cargo clippy --locked --all-targets --all-features -- -D warnings
cd tools/reward-policy-vm
npm ci --ignore-scripts --no-audit --no-fund
npm test
```

`npm test` runs the Rust direct-payout lifecycle and independently checks its
transactions in the BCH 2026 standardness and consensus VMs. With the live miner
stopped, the serial CUDA test also exercises recipient changes, job replacement,
pause/resume and intensity:

```powershell
$env:PICKAXE_DIRECT_GPU_PROOF = "$PWD/artifacts/direct-gpu-proof.json"
cargo test --locked --features rust-t2 search::tests::direct_work_fee_pays_each_recipient_across_controls_if_cuda_present -- --exact --nocapture --test-threads=1
node tools/reward-policy-vm/direct-reward.mjs "$env:PICKAXE_DIRECT_GPU_PROOF"
```

Create the `artifacts` directory first and build the Rust PTX for your GPU using
`tools/build-rust-kernels.ps1`. These offline checks do not claim a live-network
payout or a measured performance improvement.
