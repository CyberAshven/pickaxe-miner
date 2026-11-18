# Desktop window

`pickaxe gui` opens Pickaxe in a window instead of the terminal: a mining
setup and a mining dashboard, drawn with [egui](https://github.com/emilk/egui).

The window starts the same sessions as the terminal setup: PHOTON alone,
ELECTRIC alone or a [hash split](hash-split.md). Their chain reads, claims,
fee pots, donation work and saved settings are unchanged. The window takes
the terminal dashboard's place (`tui::SessionView`) and only reads and steers
a running session, so a fix to the miner reaches both screens.

## Build and run

The window is a cargo feature, not yet in the release downloads:

```sh
cargo build --release --features gui
./target/release/pickaxe_miner gui
```

Windows needs the same build tools as any Pickaxe build (Visual Studio Build
Tools with "Desktop development with C++"). Linux needs no extra packages:
the window loads Wayland or X11 when it opens, so the same binary still runs
on rigs without a desktop. On GNOME's Wayland the window draws plain borders
of its own.

## Setup

- **Network**: Mainnet or Chipnet.
- **Payout address**: checked as the terminal setup checks it
  (`bitcoincash:` on Mainnet, `bchtest:` on Chipnet).
- **Mine**: each GPU token on the network, or **Hash split** with a share for
  each token in 5% steps, at most 100% in all.
- **Intensity**: 10% to 100%.
- **GPUs**: every GPU the miner can use, with the saved selection ticked.

The setup starts from the saved settings and the same command-line flags as
`pickaxe mine` (`--network`, `--address`, `--intensity`, `--fulcrum`,
`--backend`, `--device`, `--config`). `pickaxe gui --start` starts mining
with them as soon as the GPUs are found, as pressing **Start mining** would,
for example when the computer starts.

## Dashboard

- Each token's hash rate, share, rewards and state.
- Ten minutes of hash rate, one lane per token, each on its own scale:
  tokens count different work, so their rates are never added.
- Each GPU's rate, temperature, power, load and fan.
- The job source, server and baton.
- For ELECTRIC, its fee pot's address as text and as a QR code to fund it.
- The session's events.

**Pause**, the intensity slider and the split's **−**/**+** steer the running
session as the terminal dashboard's keys do. **Stop** waits for a pending
reward to settle, then saves the session's intensity, address and shares to
its profile as leaving for profiles does. **Stop now** does not wait; a
pending reward is kept and resolved by the next session. Closing the window
while mining stops the same way as **Stop**.

With `PICKAXE_TUI_LOG` set, the window writes the terminal dashboard's log:
events, and status lines every 10 seconds.

## Not in the window yet

The terminal setup's other choices: the node and Fulcrum connection lists,
ASIC mining (the BCH ASIC server, pools, Job Declaration), P2Pool, rigs and
farms, and profiles. Screen readers wait for AccessKit's Linux support to
leave the unmaintained zbus 5 crates (see `deny.toml`); the controls already
carry the names they will read.

## Screens without a desktop

`cargo test --features gui-preview -- --ignored gui_preview` draws both
screens offscreen with made-up data and saves them as PNG files in
`target/gui-preview` (or `PICKAXE_GUI_PREVIEW_DIR`). The same renderer checks
that every control has a name a screen reader can read.
