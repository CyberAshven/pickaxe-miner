# Release packages

Each GitHub Release publishes platform archives and checksums. Linux x86_64
and ARM64 also have Debian packages; desktop builds include a second
GUI-enabled executable while retaining the terminal binary.

| Platform | Release download | Executables |
|---|---|---|
| Linux x86_64 | `pickaxe-miner-vVERSION-linux-x86_64.tar.gz` | `pickaxe`, `pickaxe-gui` |
| Linux ARM64 | `pickaxe-miner-vVERSION-linux-arm64.tar.gz` | `pickaxe`, `pickaxe-gui` |
| Debian amd64 | `pickaxe-miner_VERSION_amd64.deb` | `pickaxe`, `pickaxe-gui` |
| Debian arm64 | `pickaxe-miner_VERSION_arm64.deb` | `pickaxe`, `pickaxe-gui` |
| Windows x86_64 / ARM64 | `pickaxe-miner-vVERSION-windows-ARCH.zip` | `pickaxe.exe`, `pickaxe-gui.exe` |
| macOS Apple Silicon | `pickaxe-miner-vVERSION-macos-arm64.tar.gz` | `pickaxe`, `pickaxe-gui` |

Replace `VERSION` and `ARCH` with the release's actual version and
architecture. Verify downloads with `SHA256SUMS.txt` before installing.

## Debian

Install the package with apt, which resolves its runtime libraries:

```sh
sudo apt install ./pickaxe-miner_VERSION_amd64.deb
pickaxe --version
/opt/pickaxe-miner/pickaxe-gui gui
```

Use the `arm64` package on 64-bit ARM Debian or Ubuntu. GPU driver and device
support remain host responsibilities; the package supplies the Vulkan and
OpenCL loader dependencies, not proprietary GPU drivers. The archive remains
available for users who prefer not to install a system package.

## Desktop GUI

The normal `pickaxe` executable remains the terminal build. Launch the
separate GUI executable with its `gui` command, for example
`./pickaxe-gui gui` on Linux/macOS or `.\\pickaxe-gui.exe gui` in PowerShell.
See [Desktop window](desktop-window.md) for its setup and dashboard.

## Docker

The headless Linux x86_64 runtime image is published at
`ghcr.io/cyberashven/pickaxe-miner`. It is separate from the source-audit
image. See the [Docker runtime guide](container.md) for GPU passthrough and
configuration.
