# Source-audit Docker image

This repository owns the image and its CI. Building and testing it does not
require acceptance into Anthropic OSS Scanner or any files from its enrollment
repository. It is a source-audit environment, not a GPU mining runtime image.

From the repository root, build the current checkout:

```sh
docker build --pull -f .oss-scanner/Dockerfile -t pickaxe-miner-scanner .
```

The build installs dependencies, compiles the application, and runs the CPU-safe
application and Rust engine tests. Verify the cached tests without network access:

```sh
docker run --rm --network none --entrypoint bash pickaxe-miner-scanner -c \
  'set -euo pipefail; cargo test --locked --offline --all-features --no-fail-fast -- --skip if_cuda --skip if_wgpu; cargo test --locked --offline --manifest-path rust-engine/Cargo.toml'
```

Ordinary CI builds and offline-tests the image on pull requests and configured
branch pushes, including merges with no version bump. Release builds also verify
the image from the same validated commit as the release artifacts, and publication
requires that verification to pass. Neither Docker job publishes an image, tag,
or release.

The release workflow skips publication on a master push when the current Cargo
version already has a release tag. A failed tag lookup stops validation rather
than treating the version as unpublished. Pull-request release runs are dry runs.

External scanner enrollment is optional and separate. Its configuration can point
to `.oss-scanner/Dockerfile` after this file is available on the scanned branch.
