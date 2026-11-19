# Desktop window

`pickaxe gui` opens Pickaxe in a window instead of the terminal: a mining
setup and a mining dashboard, drawn with [egui](https://github.com/emilk/egui).

The window's setup is the terminal setup itself, and it starts what the
terminal setup starts, as `pickaxe mine` runs it: PHOTON, ELECTRIC, a
[hash split](hash-split.md), a rig of a GPU pool or farm, this computer's
own rigs, or the BCH ASIC server. Their chain reads, claims, fee pots,
donation work and saved settings are unchanged. The window takes the
terminal dashboard's place (`tui::SessionView`, `rigs::RigView`) and only
reads and steers a running session, so a fix to the miner reaches both
screens.

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

The window drives the terminal's own setup (`tui::SetupDriver`), so it has
the same steps, rows, words, checks and saved profiles:

- **Profiles**: the saved profiles; one opens its settings with Start
  selected, and **Rename** and **Delete** (after **Delete**/**Keep**) work on
  the profile under the cursor. **+ New profile** starts the steps below.
- **Hardware**: GPU mining, ASIC mining or running a pool.
- **Network**: Mainnet or Chipnet.
- **Token**: the GPU tokens, ELECTRIC and the hash split, with the search
  box; an ASIC's target; or what a pool's miners mine.
- **Settings**: every row of the terminal's settings page for the chosen
  hardware, including the Advanced section. A choice changes with its
  **<** **>** buttons (the terminal's Left/Right), a text row opens its field
  on a click (**Save** or Enter keeps it, **Cancel** or Esc drops it, and a
  wrong value stays open with the reason on the status line), a header such
  as Advanced is a checkbox, and **Start** checks the settings as the
  terminal does and saves the profile. With ELECTRIC, its fee pot shows
  beside the rows with its balance, address and QR code; **Pot QR** shows it
  full-size to scan.
- **GPUs**, **Fulcrum servers** and **BCH nodes**: the same lists, with
  **Mine on it or not**/**All**, and **Remove**, **Other PC**, **Tailscale**,
  **Cookie file** and **Yes** for a public address. A node or server found
  on this or another computer is added with a click.

A click on a row does what Enter does there; the status line shows what
the terminal's would. The terminal's keys work too while no field is being
typed into: the arrows, Enter, Esc, Delete and the letter keys the bottom
line names. **Back** is Esc: it goes back a step, closes a field, or quits
on the first screen.

The setup reads the saved profiles and connections beside the
configuration (`--config`). A new profile starts from the saved settings
and the command-line flags `pickaxe gui` takes as `pickaxe mine` does
(`--network`, `--address`, `--intensity`, `--fulcrum`, `--backend`,
`--device`); a saved profile opens with its own settings, since the window
does not yet lay the terminal's command-line overrides over it. A
`--node-zmq` given on the command line wins over a profile's block notices,
as in the terminal. While the setup looks for GPUs and reads the profiles,
the window shows a spinner. When a session stops, the setup is made again
from what is saved, as the terminal returns to its profiles.

`pickaxe gui --start` opens the profile last mined, or the first, and starts
it as Enter twice on it would, for example when the computer starts; with no
saved profile the setup shows. Start with ELECTRIC reads the fee pot's
balance from the network first, behind a spinner, as the terminal waits for
it.

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

The BCH ASIC server's dashboard: while the server runs, the window says so
and offers **Stop**. Screen readers wait for AccessKit's Linux support to
leave the unmaintained zbus 5 crates (see `deny.toml`); the controls already
carry the names they will read.

## Screens without a desktop

`cargo test --features gui-preview -- --ignored gui_preview` draws the
window's screens offscreen with made-up data (the setup's profiles,
hardware, token, settings, an open field and the Fulcrum list, reached by
clicks; the dashboards) and saves them as PNG files in `target/gui-preview`
(or `PICKAXE_GUI_PREVIEW_DIR`). The same renderer checks that every control
has a name a screen reader can read.
