# Headless Docker runtime

The release workflow publishes the **runtime miner** separately from the
source-audit image in `.oss-scanner/`. The x86_64 image is published to
[`ghcr.io/cyberashven/pickaxe-miner`](https://github.com/CyberAshven/pickaxe-miner/pkgs/container/pickaxe-miner)
with a version tag and, for stable releases, `latest`.

The image is assembled from the same verified Linux x86_64 release archive,
including its CUDA PTX and AMD HIP code objects. It is headless by default and
starts `pickaxe mine --no-tui`. Persist configuration in `/data` and pass
normal Pickaxe CLI options after the image name:

```sh
docker run --rm -it \
  --gpus all \
  -v pickaxe-data:/data \
  ghcr.io/cyberashven/pickaxe-miner:latest \
  --config /data/pickaxe.json mine --no-tui --address 'bitcoincash:YOUR_ADDRESS'
```

For NVIDIA CUDA, install the NVIDIA driver and NVIDIA Container Toolkit on
the host, then use `--gpus all`; the host driver is exposed by the container
runtime. The image includes the release's sm_120 PTX only, so use a compatible
NVIDIA GPU and driver. For Vulkan/OpenCL devices, pass the host's GPU device
nodes (commonly `--device /dev/dri`). Mesa Vulkan drivers are included in
the image; OpenCL still requires a compatible vendor driver/ICD from the host. GPU access is not emulated: the container does not
provide a CPU mining fallback.

The published runtime image currently targets **Linux x86_64**. The Linux
ARM64 release archive remains available separately; no ARM64 container tag is
published. HIP users may need a ROCm-compatible host/container runtime and
matching userspace libraries; this image does not claim universal ROCm
compatibility. GUI/window support is supplied in desktop release archives,
not this headless container.

The image runs as root by default so container device-node permissions work
with common GPU runtimes. Restrict container capabilities and mounts to the
minimum needed for your host, and do not mount sensitive host paths.
