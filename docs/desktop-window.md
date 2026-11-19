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

- Each token's hash rate, share, rewards, donation and state, with
  **Reconnect** for PHOTON.
- Ten minutes of hash rate, one lane per token, each on its own scale:
  tokens count different work, so their rates are never added.
- Each GPU's rate, temperature, power, load and fan.
- The job source, server (credentials removed, as in the terminal) and baton.
- PHOTON alone: its payout and Fulcrum server can change while it mines, as
  the terminal's `address` and `endpoint` commands do; with ELECTRIC or a
  split they are set up again.
- A coordinator's rigs, and the command each rig runs to join, with
  **Copy**.
- For ELECTRIC, its fee pot's address as text and as a QR code to fund it,
  and for an owner-controlled pot **Max/send**: a destination, a preview
  of what it receives and the fees, then **Send** for exactly that quote.
- The session's events.

**Pause**, the intensity slider, each token's donation **−**/**+** and the
split's **−**/**+** steer the running session as the terminal dashboard's
keys do; with no text field in use, P or Space pauses, + and - step the
intensity, B stops and Q closes the window. **Stop** works as the
terminal's B: it refuses while every split share is 0, pauses the GPUs,
waits for pending rewards to settle, then saves the session's intensity,
address, donations and shares to its profile; **Keep mining** (or Esc)
takes it back. **Stop now** does not wait or save, as the terminal's q: a
pending reward is kept and resolved by the next session. While a session is
still starting, **Stop** stops it as Ctrl+C would. Closing the window while
mining stops as **Stop** does, minimized or not, and the window stays until
the session has stopped. Ctrl+C in the terminal the window was started from
closes the window from the setup, and stops a running session as in the
terminal.

With `PICKAXE_TUI_LOG` set, the window writes the terminal dashboard's log:
events, and status lines every 10 seconds.

## ASIC server dashboard

The BCH ASIC server runs as from the terminal (`stratum-v2 serve`): the
same node, pools, Job Declaration, threads, status file
(`<config>.sv2-status.json`) and owner-only devices file. The window takes
its terminal dashboard's place through `stratum_v2::view::ServerView`: five
times a second the server hands the window what its pages show and applies
the window's requests as its keys do. With the window it prints nothing to
the terminal.

- The bar: network, BCH or token mining, the server's state, uptime and
  **Stop** (as `q`).
- The lights: your own P2Pool node and BCH node, the pool's when joining
  one, or the node's and Job Declaration's.
- **Workers**: the terminal's header and its workers table, column for
  column, with a **Panel** button on each row (as Enter).
- **Overview**: workers online, shares, blocks, height and the best share,
  then the terminal overview's status lines (node, ZMQ notices, templates,
  Job Declaration, token wins, validated work) and its devices table.
- **Connection info**: every SV1, SV2 and template address at each place
  this computer is reached (this computer, your network, Tailscale), each
  with **Copy**, what to type as the username and password, and the
  template provider settings for an SRI pool. Opening it reads this
  computer's addresses again, as `i` does.
- **Advanced settings**: the donation with ◀ and ▶ (or ←/→, + and −) in
  the terminal's 0.5% steps, saved before new jobs use it; a failed save
  keeps the old setting and says so. Below it, what the server started with.
- **Device panel**: the terminal's panel for one worker, online or offline,
  under the same rules (its address is never shown, an offline row's device
  must be confirmed, every action asks first). The keys it names are
  buttons, and keys typed in the window reach it as in the terminal: the
  numbers, letters, Enter, Esc, Tab and Backspace, so pools, fan, power and
  the device's login are entered the same way. **Back to workers** closes it.

## Not in the window yet

Screen readers wait for AccessKit's Linux support to
leave the unmaintained zbus 5 crates (see `deny.toml`); the controls already
carry the names they will read.

## Screens without a desktop

`cargo test --features gui-preview -- --ignored gui_preview` draws the
window's screens offscreen with made-up data (the setup's profiles,
hardware, token, settings, an open field and the Fulcrum list, reached by
clicks; the token, coordinator and rig dashboards; and, with
`gui_preview_asic`, the ASIC server's pages and Device panel) and saves them
as PNG files in `target/gui-preview` (or `PICKAXE_GUI_PREVIEW_DIR`). The
same renderer checks that every control has a name a screen reader can read.
