# Pickaxe on HiveOS, mmpOS and RaveOS

#### PR #42

Each release has a custom-miner package for three farm systems:

| System | Release asset | Installs as |
|---|---|---|
| HiveOS | `pickaxe-hiveos-VERSION.tar.gz` | custom miner `pickaxe-hiveos` |
| mmpOS | `pickaxe-mmpos-VERSION.tar.gz` | custom miner `pickaxe` |
| RaveOS | `pickaxe-raveos-VERSION.zip` | custom miner `Pickaxe`, algorithm `photon` |

Each package holds the Linux x86_64 `pickaxe` (built for glibc 2.28, so it
runs on HiveOS's Ubuntu 20.04 and newer systems), its CUDA and HIP kernel
files, and the system's scripts. The scripts call two commands, described in
[farm.md](farm.md#farm-operating-systems-hiveos-mmpos-raveos):
`pickaxe farm-os mine` turns the flight sheet's fields into Pickaxe's flags,
and `pickaxe farm-os stats` reports the running miner in the system's format.

## What to put in the pool fields

The pool field decides where the work comes from:

| Pool | Mines |
|---|---|
| empty, `solo` (mmpOS: server `solo`, port `1`) | PHOTON from the public Fulcrum servers |
| `stratum2+tcp://HOST:3340/KEY` | as a rig of your Pickaxe farm or of a GPU pool; several, space-separated, are backups |
| server `HOST`, port `3340`, password = the coordinator's key (mmpOS) | the same, for systems that split the address |
| `http://USER:PASSWORD@HOST:8332` | PHOTON from your BCH node |
| `wss://HOST:PORT` or `tcp://HOST:50001` | from that Fulcrum server |

The wallet is your PHOTON payout address (a CashAddr); `ADDRESS.WORKER`
names the rig on its coordinator's dashboard. Extra arguments go straight to
the miner: `--chipnet`, `--intensity 90`, `--device 0,1`.

## HiveOS

Flight sheet, custom miner:

- Miner name: `pickaxe-hiveos`.
- Installation URL: the `pickaxe-hiveos-VERSION.tar.gz` release asset.
- Hash algorithm: `photon`.
- Wallet and worker template: `%WAL%.%WORKER_NAME%`, with your CashAddr
  wallet.
- Pool URL: as above.
- Extra config arguments: as above.

`h-config.sh` keeps the flight sheet's fields in an owner-only file (a node
URL can carry its RPC login), `h-run.sh` starts the miner and logs to
`/var/log/miner/pickaxe-hiveos/pickaxe.log`, and `h-stats.sh` gives the
agent `khs` and `stats` (rate per GPU by PCI bus, temperature, fan, uptime,
accepted and rejected winners).

## mmpOS

Custom miner from the `pickaxe-mmpos-VERSION.tar.gz` release asset. mmpOS
starts `mmp-launch.sh` with `--pool HOST:PORT`, `--user WALLET.WORKER`,
`--password` and the extra arguments; Pickaxe ignores `--coin` and
`--api-port`. `mmp-stats.sh` prints the GPUs by bus, their rates and shares.

## RaveOS

Custom miner from the `pickaxe-raveos-VERSION.zip` release asset, with the
coin's algorithm `photon`. `RAVINOS/start.py` builds the command from the
coin's pools (URL, user, password) and the additional arguments, and
`RAVINOS/stats.py` sets each GPU's rate, temperature and shares, matched by
PCI bus. A value with spaces in it is refused rather than split.

## GPUs and drivers

The CUDA kernels target RTX 50 (`sm_120`); other NVIDIA cards, AMD cards
outside the HIP list and Intel GPUs mine through the portable engine on
Vulkan, or through OpenCL, which needs the system's `libvulkan.so.1` or an
OpenCL driver. `pickaxe devices` lists each GPU with its engine.

## Stopping

Farm systems stop a miner with SIGTERM or SIGHUP; Pickaxe takes them like
Ctrl+C: the GPUs stop, the last status is written, and found wins are kept.

## Building the packages

`tools/farm-os/build.sh <linux-x86_64.tar.gz> <out-dir>` makes the three
packages from a Linux x86_64 release archive and `packaging/farm-os/`;
`python tools/test_farm_os_packages.py` tests them with a stub miner.
