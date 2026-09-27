param([string]$Architecture = 'sm_120', [switch]$Upstream = $true)
$ErrorActionPreference = 'Stop'
if ($Architecture -notmatch '^sm_[0-9]+$') { throw 'Expected an SM architecture such as sm_120' }
$repo = Split-Path $PSScriptRoot -Parent
$oldFlags = $env:RUSTFLAGS
$oldTarget = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = Join-Path $repo 'artifacts/rust-engine/target'
    $env:RUSTFLAGS = "-C target-cpu=$Architecture -C panic=abort"
    $featureArgs = if ($Upstream) { @('--features', 'upstream-rust') } else { @() }
    cargo +nightly-2026-04-02 build --locked --release @featureArgs --manifest-path (Join-Path $repo 'rust-engine/Cargo.toml') --target nvptx64-nvidia-cuda -Z build-std=core
    if ($LASTEXITCODE -ne 0) { throw 'Rust GPU compilation failed' }
    New-Item -ItemType Directory -Force -Path (Join-Path $repo 'cuda/build') | Out-Null
    Copy-Item -LiteralPath (Join-Path $env:CARGO_TARGET_DIR 'nvptx64-nvidia-cuda/release/pickaxe_rust_engine.ptx') -Destination (Join-Path $repo 'cuda/build/photon_rust.ptx')
} finally {
    $env:RUSTFLAGS = $oldFlags
    $env:CARGO_TARGET_DIR = $oldTarget
}
