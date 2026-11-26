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

Release archives include a separate GUI-enabled executable beside the usual
terminal binary: `pickaxe-gui` on Linux and macOS, or `pickaxe-gui.exe` on
Windows. A double-click on it opens the window, as does starting it with no
arguments:

```sh
./pickaxe-gui
```

On Windows the console that a double-click opens closes once the window
starts; if the window cannot open, a console comes back with the reason.
With arguments, `pickaxe-gui` runs them like `pickaxe` does (`pickaxe-gui
gui --start` opens the window and starts mining the saved profile).

The original `pickaxe` / `pickaxe.exe` remains the terminal build, so a
machine without a desktop can continue using the same TUI or headless command.
To build the GUI executable yourself:

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

- **Profiles**: the saved profiles, each with its own **Open**, **Rename**
  and **Delete** (Delete asks first: **Delete** or **Keep**); a click on a
  profile opens it too, with Start selected. **+ New profile** starts the
  steps below.
- **Hardware**: GPU mining, ASIC mining (**Solo** or **Join a pool**) or
  running a pool.
- **Network**: Mainnet or Chipnet.
- **Token**: the GPU tokens, ELECTRIC and the hash split, with the search
  box; an ASIC's target; or what a pool's miners mine.
- **Settings**: every row of the terminal's settings page for the chosen
  hardware, including the Advanced section. A value changes with its
  **<** **>** buttons (the terminal's Left/Right; a click on its name only
  moves the cursor there), a text row opens its field on a click anywhere
  on it (**Save** or Enter keeps it, **Cancel** or Esc drops it, and a wrong
  value stays open with the reason on the status line), a header such as
  Advanced is a checkbox, and **Start mining** (or the server's or pool's
  Start) checks the settings as the terminal does and saves the profile.
  With ELECTRIC, its fee pot shows beside the rows, or under them in a
  narrow window, with its balance, address and QR code; **Pot QR** shows it
  full-size to scan. Running a pool for ASICs, the **Mode** row's **<**
  **>** choose a **Public Solo Endpoint**, a **Non-custodial Pool** or
  **P2Pool v2**, with a line under it saying what differs, and Start starts
  that server ([pool.md](pool.md)).
- **GPUs**, **Fulcrum servers** and **BCH nodes**: the same lists. Each
  GPU's checkbox ticks it, and **All** ticks them all; each saved server or
  node has its own **Remove** (and a node its **Cookie file**), with **Other
  PC**, **Tailscale** and **Yes** for a public address below. A node or
  server found on this or another computer is added with a click. **Done**
  goes back to the settings. As in the terminal, a synced node on this
  computer is used first while no node is saved, and a Fulcrum server there
  before the built-in ones (see
  [Chain connections](../README.md#chain-connections)); the BCH node and
  Fulcrum rows say so, and Start waits a moment, behind a spinner, for a
  look at this computer still under way.

A click on a row does what Enter does there, and the status line shows
what the terminal's would. Where the terminal's hint says more than its
keys (such as "0.5% steps"), the row says it too. The terminal's keys work
while no widget has the keyboard: the arrows, Enter, Esc, Delete and the
letter keys the bottom line names; Tab moves between the buttons instead,
and Space or Enter presses one. In the token search, Up and Down move
through the list and Esc goes back. **Back** is Esc: it goes back a step,
closes a field, or, on the first screen, asks **Quit Pickaxe?** before the
window closes.

The setup reads the saved profiles and connections beside the
configuration (`--config`). The first setup takes the command line as
`pickaxe mine` does: a new profile starts from the saved settings and the
flags, and a saved profile opens with `--network`/`--chipnet`, `--token`,
`--intensity`, `--fulcrum`, `--node-rpc`, `--source` and the GPUs of
`--backend`/`--device`/`--include-integrated` laid over it; `--node-zmq`
wins over a profile's block notices in the first session. While it looks
for GPUs and reads the profiles, the window shows a spinner. When a session
stops, the setup is made again from what is saved alone, without the
command line, as the terminal's return to its profiles is.

`pickaxe gui --start` opens the profile Start last started (kept in
`last-profile.txt` beside the profiles, by the terminal's setup too), or
the first, and starts it as Enter twice on it would, for example when the
computer starts; with no saved profile the setup shows. Start with ELECTRIC
reads the fee pot's balance from the network first, behind a spinner, as
the terminal waits for it.

## Dashboard

The dashboard leads with the hash rate, the session's rewards, the GPUs'
temperature and power, the intensity, the tokens and their chart. GPUs,
the network and PHOTON's settings sit under **Details**, one click away.
Each control's answer shows next to it, in red when it failed. **Stop now**
and Max/send's **Send** ask before they act, since neither can be undone.
The screens use one accent color, numbers in the monospace font, and show
the terminal's keys when the pointer rests on **Keyboard**.

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
  the device's login are entered the same way. **Back** is Esc: one page
  back, and from the panel's first page back to the workers. Pasted text
  reaches only the login and pool pages, never the menu or a confirmation.

The terminal's page keys work while no button or field holds the keyboard
(Esc lets it go): Tab between Workers and Overview, `i` for Connection info
(a number copies its line), `a` for Advanced settings, Esc back, and `q` to
stop. While a Device panel page reads text or a number it keeps the
keyboard; on its other pages Tab reaches **Stop**. A **Stop** pressed while
the server starts (your own P2Pool node, the node, the ports) ends it
between those steps, so closing the window then does not wait for it to
serve.

## Not in the window yet

Screen readers wait for AccessKit's Linux support to
leave the unmaintained zbus 5 crates (see `deny.toml`); the controls already
carry the names they will read.

## Screens without a desktop

`cargo test --features gui-preview -- --ignored gui_preview` draws the
window's screens offscreen with made-up data (the setup's profiles,
hardware, token, settings, an open field, the Fulcrum list and Run a pool in
P2Pool v2, reached by clicks; the token, coordinator and rig dashboards;
and, with `gui_preview_asic`, the ASIC server's pages and Device panel) and
saves them as PNG files in `target/gui-preview` (or
`PICKAXE_GUI_PREVIEW_DIR`). The same renderer checks that every control has
a name a screen reader can read.
