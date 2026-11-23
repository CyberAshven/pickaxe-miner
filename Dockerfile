# Headless Pickaxe GPU runtime, assembled from the verified Linux release archive.
# This is intentionally separate from .oss-scanner/Dockerfile, which is a source-audit image.
FROM debian:bookworm-slim

ARG PICKAXE_ARCHIVE
ARG PICKAXE_VERSION

LABEL org.opencontainers.image.source="https://github.com/CyberAshven/pickaxe-miner" \
      org.opencontainers.image.description="Headless Pickaxe Miner GPU runtime" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.version="${PICKAXE_VERSION}"

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates libgcc-s1 libudev1 libvulkan1 mesa-vulkan-drivers ocl-icd-libopencl1 \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /opt/pickaxe-miner /data

COPY dist/ /tmp/pickaxe-release/
RUN test -n "$PICKAXE_ARCHIVE" \
    && tar -xzf "/tmp/pickaxe-release/$PICKAXE_ARCHIVE" --strip-components=1 -C /opt/pickaxe-miner \
    && test -x /opt/pickaxe-miner/pickaxe \
    && /opt/pickaxe-miner/pickaxe --version | grep -F "pickaxe $PICKAXE_VERSION" \
    && ln -s /opt/pickaxe-miner/pickaxe /usr/local/bin/pickaxe \
    && rm -rf /tmp/pickaxe-release

COPY docker/pickaxe-entrypoint.sh /usr/local/bin/pickaxe-entrypoint
RUN chmod 0755 /usr/local/bin/pickaxe-entrypoint

WORKDIR /data
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/pickaxe-entrypoint"]
CMD ["--config", "/data/pickaxe.json", "mine", "--no-tui"]
