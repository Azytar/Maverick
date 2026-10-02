# Maverick

Maverick is an X11 tiling window manager for Linux, written in Rust. It is
Unix-oriented and deliberately narrow in scope: it arranges windows on an X11
display, publishes the EWMH properties a desktop expects, and stops there.

Its model is built around **logical Views**. Each View holds a set of tiled
columns and its own floating windows; a **Carousel** selects which View is
current; a **layout** decides where the current View's tiled clients go; and a
`DesiredState`/`Reconciler` pair turns that decision into X11 requests.

[Overview](#overview) · [What Maverick is not](#what-maverick-is-not) ·
[Functionality](#functionality) · [Architecture](#architecture) ·
[Installation](#installation) · [Running](#running) ·
[Configuration](#configuration) · [Control](#control) ·
[Development](#development) · [Status](#status) · [License](#license)

Read this in Spanish: [README.es.md](README.es.md).

## Overview

- **An X11 tiling window manager written in Rust.** Linux, X11, ICCCM/EWMH for
  the parts a window manager needs. There is no Wayland backend.
- **Built around logical Views.** A View is a container of windows, not an X11
  window: creating, selecting and dropping one is a pure logical transition.
  The Rust type behind a View is `Workspace`.
- **Navigated with a Carousel.** Each monitor owns a `Carousel` that records
  which View is `current` and which is `origin`, and moves around them one step
  at a time. Navigation is logical and instantaneous.
- **Arranged by layouts.** A layout turns a View's tiled clients into
  rectangles. Scroll is the only layout Maverick provides today.
- **Floating clients sit outside the layout.** A floating window keeps its own
  screen-space geometry; the tiled layout neither places nor moves it.
- **Materialised through a DesiredState and a Reconciler.** The engine
  computes a pure intent, the reconciler diffs it against what X11 already has,
  and only the difference becomes `ConfigureWindow` calls.

Five things are deliberately kept apart, and the whole design turns on keeping
them apart:

| concept | what it is | what it is not |
|---|---|---|
| **View identity** (`ViewId`) | A stable, monotonically minted, never-reused name for a View on a monitor | a position, an X11 window id, or a layout tag |
| **View order** | The position a View occupies in a monitor's list | the View's identity: removing a View shifts every later one |
| **Carousel selection** | Which View is `current`, and which is `origin` | layout state; the Carousel knows nothing about layouts |
| **Layout geometry** | The rectangles a View's tiled clients are given | View membership; no layout adds, removes or re-homes a client |
| **X11 materialisation** | The `ConfigureWindow` calls that make the server agree | a second source of truth; applied state is a backend cache |

## What Maverick is not

Maverick is a window manager, not a desktop environment. It does not contain,
ship or start:

- a desktop environment of any kind;
- a compositor, renderer or GPU path — there is no frame loop, no GL and no
  Vulkan;
- a wallpaper subsystem — the root window's background is not a window to
  manage;
- a notification daemon;
- a system tray;
- an animation or transition system — geometry is written once, at its final
  position;
- a Monocle mode;
- any layout other than Scroll.

Compositing, a panel, a launcher, notifications and a wallpaper are somebody
else's programs. Maverick starts them if you list them in `[autostart]`, reads
the struts they publish, and never talks to them again.

There is no second layout. `LayoutKind` is a one-variant enum, `set_layout`
accepts only `column`, and no Mosaic layout exists in this repository.

## Functionality

Everything below is implemented in this tree. The logical state, the layout
geometry, the command layer and the control surfaces are covered by the test
suite; protocol, stacking and focus behaviour have their own real-X11
harnesses.

### Views and View identity

- Every monitor owns a list of Views. A monitor starts with `n_tags` of them
  (default 9, maximum 9).
- Each View carries a `ViewId`: a `u32` minted by that monitor's Carousel,
  strictly increasing and never reused. A View that is deleted frees its id for
  good, so a stale `ViewId` is *detectable* rather than silently re-pointing at
  whatever inherited the old position.
- A View is either tiled (its columns) or floating (its own float list). Every
  managed window is referenced from exactly one of the two, on exactly one
  monitor.
- `view_create` adds a View, up to 9 per monitor. `view_remove` is refused while
  the View it names still holds clients: where they would go is a policy
  decision, so the removal is not made for you.

### Carousel navigation

- `view_next` / `view_prev` step one place around a **circular** carousel: next
  from the last View is the first, previous from the first is the last.
- `view_return` selects the `origin` — the View the carousel was pinned to when
  that monitor's first View was created. It is a total operation: `origin` is
  repaired on every removal, so it can never name a deleted View.
- `view <n>` selects a View by its position in the monitor's list, resolved
  through the Carousel so the switch is a change of View identity rather than a
  positional index.
- Creating a View while others exist does **not** move you: only the empty →
  non-empty transition makes a new View current and pins the origin.
- Removing a View repairs `current` and `origin` independently, each adopting
  the successor at the freed position, or the new tail when the last View goes.
- Both Carousel pointers are `Some` exactly when the monitor has at least one
  View, and are both `None` only when it has none.

### Scroll layout

Scroll is the only layout. It is a horizontally scrolling ribbon of columns,
each column a vertical stack of windows, with a scroll camera that keeps the
focused column in view.

- Columns have a width expressed as a fraction of the monitor's workarea.
  Adding a column extends the ribbon instead of shrinking its neighbours.
- `ideal_scroll` derives the camera offset that makes the focused column fully
  visible, and it is recomputed after every change to the column tree, so the
  camera can never be stranded past the end of a shorter ribbon.
- Viewport zoom (`viewport_zoom`) enlarges the ribbon for close inspection, and
  `page_snap` scrolls the camera one screen-width at a time.
- Overview (`toggle_overview`, `overview_nav`, `overview_enter`) is a zoomed-out
  projection of the current View for picking a column. It changes the projection,
  not the layout and not View membership.
- `grow_col` resizes the focused column by a pixel amount; `maverickctl resize`
  addresses the same operation as a percentage. `new_column` and
  `collapse_column` add and remove columns.
- Fullscreen and maximize are **presentation**, applied after the layout
  (`present::present_into`), not separate layouts. A window that takes a real
  exclusive fullscreen also gets `_NET_WM_BYPASS_COMPOSITOR` published, so an
  external compositor steps aside for it.

### Floating clients

- A window floats because it is a transient or dialog, matches a
  floating-heuristic or a rule, or is toggled with `Super+Shift+Space`.
- Floating windows are projected from their own `Client::geom` and are never
  placed by the layout. Scrolling the ribbon, resizing a column and entering
  Overview leave them where they are, in global X11 coordinates.
- Sticky floats stay visible on every View of their monitor. Ordinary floats
  follow the visibility of the View they belong to.
- `[[rules]]` can force a float's size and position (relative to the workarea
  origin), its opacity, its border width, and whether a client's own
  fullscreen requests are honoured, normalised or refused.
- `Super`-drag moves and `Super`-right-drag resizes a window that is already
  floating. Tiled clients keep their layout rectangle: a client cannot set its
  own tile through `ConfigureRequest`.
- Map-time `_NET_WM_STATE_MAXIMIZED_*` / `_NET_WM_STATE_FULLSCREEN` is
  normalised for every client by default, so applications that remember being
  maximized open as a normal tile. `honor_initial_state` opts in, globally or
  per rule.

### X11

- One Xlib `Display*` whose event queue is owned by XCB, handed to the window
  manager as the connection it issues requests on. There is no second reader of
  the socket.
- EWMH: `_NET_SUPPORTED`, `_NET_CLIENT_LIST`, `_NET_CLIENT_LIST_STACKING`,
  `_NET_NUMBER_OF_DESKTOPS`, `_NET_DESKTOP_NAMES`, `_NET_CURRENT_DESKTOP`,
  `_NET_DESKTOP_GEOMETRY`, `_NET_WORKAREA`, `_NET_ACTIVE_WINDOW`,
  `_NET_SUPPORTING_WM_CHECK`, `_NET_WM_DESKTOP`, `_NET_WM_STATE` (including
  `MODAL`, `MAXIMIZED_VERT`, `MAXIMIZED_HORZ`, `FULLSCREEN`,
  `DEMANDS_ATTENTION`), `_NET_CLOSE_WINDOW`, `_NET_FRAME_EXTENTS`,
  `_NET_WM_PID`, `_NET_WM_BYPASS_COMPOSITOR` and `_NET_WM_WINDOW_OPACITY`.
- Dock reservations through `_NET_WM_STRUT` and `_NET_WM_STRUT_PARTIAL` shrink
  the workarea, so tiled windows never cover a panel the window manager does not
  own.
- RandR monitor discovery and topology updates. Each monitor has its own Views
  and its own Carousel.
- `_NET_DESKTOP_GEOMETRY` and `_NET_WORKAREA` stay physical. Maverick does not
  publish `_NET_DESKTOP_VIEWPORT`: scrolling is an internal layout transform
  over the physical desktop.
- An idle session costs no CPU. The event loop blocks on X11 plus the control
  self-pipe with no frame deadline, no heartbeat and no timer.
- `--replace` requests a handover from a running window manager and adopts its
  windows. Restart re-executes in place with the same arguments.

### maverickctl

`maverickctl` is a separate binary — a thin control client that never links the
window manager and never touches X11. It speaks to a running instance over that
instance's Unix control socket.

- `maverickctl list`, `state`, `query <topic>`, `subscribe`, `msg <action>`,
  `reload`, `restart`, `quit`, `quit-all`, `prune`.
- `maverickctl view <session> goto <n> | next | prev | return | create |
  remove <n>` — the Carousel, encoded as the same actions a keybinding runs.
- `maverickctl window <session> list | inspect | focus | close | move | float |
  fullscreen`, plus `camera`, `resize` and `layout` for the layout itself.
- `maverickctl session …` manages whole graphical sessions: a nested X server, a
  Maverick, the programs launched into it, a cookie, logs and a lifecycle. See
  [`docs/sessions.md`](docs/sessions.md).
- Each instance has a private runtime directory under
  `$XDG_RUNTIME_DIR/maverick/<session-id>/` (`0700`), a `0600` socket
  peer-checked with `SO_PEERCRED`, and an identity record. Discovery prefers
  `--session`, then `--name`, then `$MAVERICK_INSTANCE`, then the
  display/TTY context; it refuses to guess when several candidates match.
- Any word `maverickctl` does not recognise as a command is forwarded verbatim
  to the window manager, which is the only thing that can tell an action from a
  query topic from a typo.

### Configuration

See [Configuration](#configuration) below.

## Architecture

The high-level flow, from the active View to the pixels X11 holds:

```text
        Carousel
           ↓
    active View
           ↓
    tiled clients
           ↓
        Layout            (Scroll: the only implementation)
           ↓
    DesiredState         (pure intent: window + rect + border)
           ↓
     Reconciler          (diffs DesiredState against AppliedState)
           ↓
          X11
```

The boundaries that matter:

- **The Carousel does not know about layouts.** It holds two `ViewId`s and
  moves between them. There is no `match layout` in it, and navigation must
  answer the same question whichever layout is installed.
- **Scroll does not own View navigation.** `layout::arrange` is handed one
  View — the active one, resolved through the Carousel — and returns geometry.
  It never creates, removes, selects or re-orders a View, and it never mutates
  View membership: it reads `columns` and `floats` and computes rectangles.
- **View order and View identity are separate.** Membership is keyed by
  `ViewId`, so removing a View shifts positions without invalidating any
  client reference.
- **Floating clients are outside the layout.** They live in the View's
  `floats` list, which is exactly why they are excluded from the layout's
  input; the presentation stage projects them from their own geometry.
- **One projection, and it is the geometry.** There is no interpolated view
  alongside the settled one. A scroll rewrites the camera and the next arrange
  *is* the final geometry.
- **One geometry sink.** Every `ConfigureWindow` for a client is issued from
  the reconciler's diff; no other code positions a window. Applied state is a
  backend cache, not a claim that asynchronous X11 requests cannot fail.
- **The engine is pure.** `Engine::dispatch(Action)` executes a `Command`,
  which mutates `State` and emits `Effect`s. Only the backend performs effects,
  and only the backend touches X11.

| Location | Responsibility |
| --- | --- |
| `src/main.rs` | CLI, configuration selection, signals, instance identity, backend startup |
| `maverick-core/` | Dependency-free domain types: `State`, `Monitor`, `Workspace` (a View), `ViewId`, `Carousel`, `Column`, `Client`, `Rect` |
| `src/core/` | Engine, actions/commands/effects/events, layout, presentation, `DesiredState` |
| `src/backend/x11/` | Event handling, client management, input, EWMH, struts, reconciliation |
| `src/config.rs`, `src/userconfig.rs` | Compiled defaults, configuration merging, validation |
| `maverick-x11/` | Shared Xlib/XCB connection bootstrap |
| `maverick-sys/` | OS/FFI boundary, instance identity, control socket and hub |
| `maverick-toml/` | TOML-subset parser |
| `maverickctl/` | Control client binary: CLI, control-socket IPC client, discovery, session lifecycle |
| `tests/` | Real-X11 probes, integration scripts, installer smoke tests |
| `installer/` | The installer and its test suite |

`maverick-core` has no X11, no clock, no filesystem and no environment
dependency, which is why most of the test suite needs no display. The
implementation does depend on native X11 libraries, and `x11rb` is used with an
XCB FFI connection rather than as a wholly pure-Rust protocol stack. There is
no async runtime and no GUI toolkit.

`docs/architecture.md` describes the same boundaries with `file:line`
anchors.

## Installation

### Requirements

Linux, an X11 server, a C linker and Rust 1.82 or newer. Maverick links
`libX11` and `libX11-xcb`, and the in-tree parser is a TOML subset.

```bash
# Arch Linux
sudo pacman -S --needed base-devel rust libx11 libxcb
# Debian / Ubuntu
sudo apt install --no-install-recommends build-essential cargo libx11-dev libxcb1-dev
# Fedora
sudo dnf install -y cargo gcc libX11-devel libxcb-devel
```

For an X11 session started with `startx`, also install `xorg-server` and
`xorg-xinit` (Arch: `xorg-server xorg-xinit`).

The compiled default keybindings launch `alacritty` and `rofi`, and the
compiled autostart launches `xdg-desktop-portal` and
`xdg-desktop-portal-gtk`. Those are conveniences, not requirements: override the
bindings or supply your own `[autostart] commands` list. The layout engine
needs none of them.

### Install

```bash
git clone https://github.com/Azytar/Maverick.git
cd Maverick
./installer/install.sh
```

The installer builds the release binaries and installs them into `$HOME/.local`
by default, which needs no privileges. It refuses to run as root, never
invokes `sudo`, and never enables a service.

```bash
./installer/install.sh --help          # every option
./installer/install.sh --system       # install into /usr/local instead
./installer/install.sh --prefix DIR   # install into DIR instead
./installer/install.sh --no-config    # do not create a configuration file
./installer/install.sh --no-build     # install existing $CARGO_TARGET_DIR/release binaries
./installer/install.sh --no-path      # never edit a shell startup file
```

What it installs, and where:

| path | what |
| --- | --- |
| `<prefix>/bin/maverick` | the window manager |
| `<prefix>/bin/maverickctl` | the control client |
| `<prefix>/share/xsessions/maverick.desktop` | the X11 session entry |

Outside the prefix it writes only your own files, and only when you agree:

- `${XDG_CONFIG_HOME:-$HOME/.config}/maverick/config.toml`, seeded from
  [`config/config.toml`](config/config.toml) unless one is already there
  (`--no-config` skips this step; answering "no" to the overwrite question
  keeps yours);
- one marked, self-guarding block in a shell startup file under `$HOME`, offered
  only when the bin directory is missing from `PATH` and never with `--no-path`.

The only write that deliberately leaves the prefix is the session file, and only
when you name the directory: `--xsessions-dir /usr/share/xsessions`. Display
managers generally read only system locations, so a user-local session entry
will not appear in a session chooser on its own.

The prefix is a hard boundary: nothing outside it is created or modified except
the two files above. A prefix you cannot write is reported as a permission
error rather than escalated around, so a system-wide install needs write access
to `/usr/local` arranged by you.

Each step fails loudly. The installer runs the binaries it just installed —
`maverick --version`, `maverickctl --help`, `maverickctl session --help` — and a
partial, stale or broken set is reported as a failure rather than as a
successful install. It is safe to run repeatedly: a second run converges, fixes
umask-hostile permissions and does not duplicate the `PATH` block.

`CARGO_TARGET_DIR` is honoured as given. When it is unset, the build happens in
a cache directory under `$XDG_CACHE_HOME` and the checkout is never used as a
build directory. The first build attempt passes `-C target-cpu=native` and falls
back to a plain build if that fails, so the installed binary is tuned for the
machine that built it; use a normal `cargo build` when you need artifacts for a
different CPU.

To uninstall, delete the two binaries and the session file from the prefix, and
remove the block between the `# >>> maverick (install.sh) >>>` markers from any
startup file it touched.

See [`installer/README.md`](installer/README.md) for the installer's own
documentation and its test suite.

## Running

For a `startx` session, put this at the end of `~/.xinitrc`:

```sh
exec maverick
```

Or select the installed Maverick session in your display manager.

```bash
maverick --check-config "$HOME/.config/maverick/config.toml"   # validate, start nothing
maverick --config "$HOME/.config/maverick/config.toml" --name desktop
maverick --help
```

- `--check-config [path]` validates a configuration and exits: `0` for clean,
  `1` if there are warnings or errors. It never opens an X display.
- `--config <path>` replaces the default configuration location and is reused
  by reload and restart.
- `--name <id>` labels the instance; `--session-id <id>` publishes it under a
  fixed session id, which is what `maverickctl session` uses.
- `--replace` hands over from a running window manager and adopts its windows.
- `--debug` / `--log-level <off|error|warn|info|debug|trace>` set the log level.
  `--log-level` wins over `--debug`.

### Compiled default bindings

`Super` is Mod4, normally the Windows key. `Super+1…9` and `Super+Shift+1…9`
are generated for `n_tags`; set `auto_workspace_binds = false` to manage those
yourself.

| binding | action |
| --- | --- |
| `Super+Return` | terminal (`alacritty`) |
| `Super+P` / `Super+Shift+P` | launcher (`rofi`) |
| `Super+H/J/K/L` | focus left / down / up / right |
| `Super+Shift+H/J/K/L` | move window left / down / up / right |
| `Super+Shift+Return` | put the window in a new column |
| `Super+Ctrl+H` / `Super+Ctrl+L` / `Super+Ctrl+J` | shrink / grow the column / collapse it |
| `Super+T` | set the layout (`column`) |
| `Super+Shift+Space` | toggle floating |
| `Super+Shift+F` / `Super+Shift+M` | toggle fullscreen / maximize |
| `Super+1…9` | select View |
| `Super+Shift+1…9` | send the window to View |
| `Super+Tab` / `Super+Shift+Tab` | focus the next monitor / move the window to it |
| `Super+O` / `Super+E` / `Super+N` / `Super+Shift+O` | Overview: toggle / enter / next / previous |
| `Super+=` / `Super+-` | viewport zoom in / out |
| `Super+]` / `Super+[` | page-snap right / left |
| `Super+Shift+C` | close the focused window |
| `Super+Shift+R` / `Super+F5` | restart in place |
| `Super+Shift+Q` | quit |

Carousel stepping (`view_next`, `view_prev`, `view_return`) and View lifecycle
(`view_create`, `view_remove`) are not bound by default; reach them with
`maverickctl view` or add your own `[[keybindings]]`.

### Diagnostics

Build with the opt-in `input-trace` and/or `window-trace` features to add
structured input/focus and desired/applied/X11-state traces. Both are off by
default and both only add logging.

```bash
cargo build -p maverick --features input-trace,window-trace
./target/debug/maverick --config /path/to/test.toml 2> /tmp/maverick-debug.log
```

Run that only on a `DISPLAY` you are willing to hand to a window manager.

## Configuration

Configuration is optional. Maverick reads
`$XDG_CONFIG_HOME/maverick/config.toml`, falling back to
`~/.config/maverick/config.toml`, and uses compiled defaults for anything the
file does not set.

```toml
[general]
column_width = 0.5
gaps_inner = 10
gaps_outer = 14
focus_mouse = false
```

Two rules decide how your file is combined with the defaults:

- Ordinary settings are merged field by field.
- `[[keybindings]]` and `[[rules]]` **replace** the compiled list outright when
  you declare them. Supplying one `[[rules]]` drops the compiled
  per-application float policy, so repeat the entries you want.

Malformed TOML falls back to the compiled defaults; an invalid individual entry
is diagnosed and ignored, and the rest of the file still loads. A table Maverick
does not know (one written for a different Maverick) is skipped silently, while
an unknown key inside a table it *does* know is reported. Validate before you
rely on it:

```bash
maverick --check-config ~/.config/maverick/config.toml
```

[`config/config.toml`](config/config.toml) is a commented sample covering the
wider vocabulary. It is a preset, not a copy of the compiled defaults: copying
it changes your bindings, rules and autostart.

### Application rules

`class`, `instance` and `title` match case-insensitive substrings of the
window's own strings; `window_type` matches one complete normalised
`_NET_WM_WINDOW_TYPE` name. Every criterion present in a rule must match.

```toml
[[rules]]
class = "calculator"
float = true
size = [480, 360]
position = [120, 100]
```

Rules also accept `sticky`, `workspace` (1-based), `opacity`, `border_width`,
`ignore_initial_state`, `honor_initial_state`, `deny_fullscreen` and
`true_fullscreen`. `deny_fullscreen` refuses the client's own fullscreen
requests, not your `Super+Shift+F`. `true_fullscreen` asks for a real exclusive
overlay and takes precedence over `deny_fullscreen`.

### Autostart

`[autostart] commands` is a list of argument lists:

```toml
[autostart]
commands = [["polybar", "main"], ["picom", "--vsync"]]
```

A non-empty list replaces the compiled one. This is where a compositor, a panel
or a wallpaper program belongs: Maverick starts the command and never speaks to
it again. Docks that publish struts reserve workarea automatically. Session
startup and restart are not a general-purpose service supervisor.

## Control

```bash
maverickctl list
maverickctl state --name desktop
maverickctl query tree --name desktop
maverickctl msg view 3 --name desktop
maverickctl subscribe --name desktop
maverickctl reload --name desktop
maverickctl restart --name desktop
maverickctl quit --name desktop --confirm
```

Views and windows are addressed semantically, by View or by window id or name,
and every operation is the same action a keybinding runs:

```bash
maverickctl view debug next
maverickctl window list debug --json
maverickctl window focus debug firefox
maverickctl window float debug 0x42003
maverickctl resize debug +10%
maverickctl process list debug --json
maverickctl inspect debug
maverickctl session stop debug
```

Every listing has a `--json` form. Quitting a session asks its clients to close
through `WM_DELETE_WINDOW` and force-closes the survivors after a bounded wait;
save your work first. See [`docs/sessions.md`](docs/sessions.md) for the session
model, its limitations and the security boundary.

## Development

```bash
cargo fmt --all
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`cargo fmt --all -- --check` is the read-only form. Keep layout, Carousel and
command work covered by pure state tests, which need no display; use an isolated
X server for protocol, stacking and focus behaviour. When changing EWMH interop
(struts, bypass hints, opacity), validate against a real session.

The installer has its own checks:

```bash
bash installer/lint.sh                  # bash -n, and shellcheck when present
python3 installer/tests/partition.py    # the installer behaviour suite
```

Real-X11 harnesses live in `tests/`: `tests/xvfb-stacking.py` is the automated
regression, and the `tests/xephyr-*.sh` scripts are manual integration
scenarios. They are **not** all isolated to the same standard — some older
helpers in `tests/common.sh` kill processes by name or use fixed displays — so
read a script before running it, and run the legacy suite only in a disposable
graphical session.

CI (`.github/workflows/ci.yml`) runs three jobs: workspace tests with strict
Clippy, the installer checks, and an Xvfb stacking smoke test.

## Status

Maverick is in preview. It is not declared production-ready, and the integration
scripts are regression checks rather than a certification of application
compatibility.

- **Scope:** Linux and X11 only. No Wayland backend, compositor, animation
  subsystem, desktop shell, blur or shadows.
- **Layouts:** Scroll is the only layout. `LayoutKind` has one variant; a second
  layout is not implemented and must not be documented as if it were.
- **Views:** at most 9 per monitor. `n_tags` sets how many exist at startup, and
  the digit row has no tenth key.
- **Compatibility:** ICCCM and EWMH are implemented for what the window manager
  needs, which is not a claim of complete protocol or application coverage.
- **Monitors:** focus and move cycle monitor enumeration order, not physical
  direction. Topology recovery uses rectangles and indices, not stable connector
  identities, so arbitrary hotplug or reordering does not preserve assignments.
- **Geometry:** X11 has one global root coordinate space with protocol size and
  coordinate bounds; Scroll's projection and multi-monitor workareas respect
  them.
- **Interfaces:** configuration, internal APIs and presentation policy can
  change. The in-tree TOML parser supports a subset of TOML, not the whole
  specification.
- **Naming:** the action vocabulary says `view`; the configuration still says
  `n_tags` and `workspace` for the same objects. Both spellings are live.

Before using Maverick as the only window manager for important work, validate a
disposable X11 session on the target machine: login and clean exit, application
launch and close, focus and input, fullscreen, floating and transient dialogs,
View switching, display sleep/wake, and monitor changes. Keep a way back to the
previous session.

## License

GPL-3.0. See [LICENSE](LICENSE).
