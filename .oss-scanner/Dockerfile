# Pickaxe Miner source-audit image for Anthropic OSS Scanner.
# This image is for reproducible security analysis, not a prepackaged GPU miner.
FROM rust:bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_TERM_COLOR=never \
    RUSTFLAGS="-C debuginfo=2 -C force-frame-pointers=yes"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates git pkg-config clang cmake make g++ python3 \
    libssl-dev libudev-dev libvulkan-dev libx11-dev libxkbcommon-dev \
    && rm -rf /var/lib/apt/lists/*

# Always build the exact checkout supplied as the Docker build context.
COPY . /src
WORKDIR /src

# Populate Cargo caches so tests and security review work without network access.
RUN cargo fetch --locked && \
    cargo fetch --locked --manifest-path rust-engine/Cargo.toml

# Compile the default application and all main Rust targets.
RUN cargo build --locked --lib --bins

# Match the CPU-safe test suite used by the Linux project CI.
RUN cargo test --locked --all-features --no-fail-fast -- --skip if_cuda --skip if_wgpu
RUN cargo test --locked --manifest-path rust-engine/Cargo.toml

CMD ["/bin/bash"]
