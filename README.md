# Maverick

Maverick is a tiling window manager for X11, written in Rust. It arranges
windows on an X11 display, publishes the EWMH properties a desktop expects, and
stops there. It follows Unix conventions and is deliberately narrow in scope: no
compositor, no panel, no launcher, no notifications daemon, no wallpaper
subsystem, and no animation system.

Its model is built around **logical Views**. Each View holds a set of tiled
columns and its own floating windows; a **Carousel** selects which View is
current; the **Scroll layout** decides where the current View's tiled clients go;
and a `DesiredState`/`Reconciler` pair turns that decision into X11 requests.
Views are logical containers, not X11 windows: creating, selecting and dropping
one is a state transition, and nothing about it requires a round trip to the
server.

Maverick ships two binaries. `maverick` is the window manager.
`maverickctl` is a separate control client that never links the window manager
and never touches X11; it talks to a running instance over that instance's Unix
control socket. Installation is source-based: there are no release archives or
prebuilt binaries.

[Overview](#overview) · [What Maverick is not](#what-maverick-is-not) ·
[Architecture](#architecture) · [Project structure](#project-structure) ·
[Requirements](#requirements) · [Installation](#installation) ·
[Running Maverick](#running-maverick) · [Keybindings](#keybindings) ·
[Configuration](#configuration) · [maverickctl](#maverickctl) ·
[Sessions](#sessions) · [Troubleshooting](#troubleshooting) ·
[Building from source](#building-from-source) · [Testing](#testing) ·
[Status](#status) · [License](#license)

Read this in Spanish: [README.es.md](README.es.md).

## Overview

- **A tiling window manager for X11, written in Rust.** Linux, X11, ICCCM and
  EWMH for the parts a window manager needs. There is no Wayland backend.
- **Built around logical Views.** A View is a container of windows, not an X11
  window. The Rust type behind a View is `Workspace`.
- **Navigated with a Carousel.** Each monitor owns a `Carousel` recording which
  View is `current` and which is `origin`, and moves around them one step at a
  time. Navigation is logical and instantaneous.
- **Arranged by layouts.** A layout turns a View's tiled clients into
  rectangles. Scroll is the only layout provided.
- **Floating clients sit outside the layout.** A floating window keeps its own
  screen-space geometry; the tiled layout neither places nor moves it.
- **Materialised through a DesiredState and a Reconciler.** The engine computes
  a pure intent, the reconciler diffs it against what X11 already holds, and
  only the difference becomes `ConfigureWindow` calls.

Five things are deliberately kept apart, and the whole design turns on keeping
them apart:

| concept | what it is | what it is not |
|---|---|---|
| **View identity** (`ViewId`) | A stable, monotonically minted, never-reused name for a View on a monitor | a position, an X11 window id, or a layout tag |
| **View order** | The position a View occupies in a monitor's list | the View's identity: removing a View shifts every later one |
| **Carousel selection** | Which View is `current`, and which is `origin` | layout state; the Carousel knows nothing about layouts |
| **Layout geometry** | The rectangles a View's tiled clients are given | View membership; no layout adds, removes or re-homes a client |
| **X11 materialisation** | The `ConfigureWindow` calls that make the server agree | a second source of truth; applied state is a backend cache |

### Views and View identity

- Every monitor owns a list of Views. A monitor starts with `n_tags` of them
  (default 9, maximum 9 — the value is clamped at 9).
- Each View carries a `ViewId`: a `u32` minted by that monitor's Carousel,
  strictly increasing and never reused. A deleted View frees its id permanently,
  so a stale `ViewId` is *detectable* rather than silently re-pointing at
  whatever inherited the old position.
- A View is either tiled (its columns) or floating (its own float list). Every
  managed window is referenced from exactly one of the two, on exactly one
  monitor.
- `view_create` adds a View, up to 9 per monitor. `view_remove` is refused
  while the View it names still holds clients: where they would go is a policy
  decision, so the removal is refused rather than guessed.

### Carousel navigation

- `view_next` / `view_prev` step one place around a **circular** carousel: next
  from the last View is the first, previous from the first is the last.
- `view_return` selects the `origin` — the View the carousel was pinned to when
  that monitor's first View was created. It is a total operation: `origin` is
  repaired on every removal, so it can never name a deleted View.
- `view <n>` selects a View by its position in the monitor's list, resolved
  through the Carousel, so the switch is a change of View identity rather than a
  positional index.
- Creating a View while others exist does **not** change the current View: only
  the empty → non-empty transition makes a new View current and pins the origin.
- Removing a View repairs `current` and `origin` independently, each adopting
  the successor at the freed position, or the new tail when the last View goes.
- Both Carousel pointers are `Some` exactly when the monitor has at least one
  View, and are both `None` only when it has none.

### Scroll layout

Scroll is the only layout. It is a horizontally scrolling ribbon of columns,
each column a vertical stack of windows, with a scroll camera that keeps the
focused column in view.

- Columns have a width expressed as a fraction of the monitor's workarea. Adding
  a column extends the ribbon instead of shrinking its neighbours.
- `ideal_scroll` derives the camera offset that makes the focused column fully
  visible, and it is recomputed after every change to the column tree, so the
  camera cannot be stranded past the end of a shorter ribbon.
- Viewport zoom (`viewport_zoom`) enlarges the ribbon for close inspection, and
  `page_snap` scrolls the camera one screen-width at a time.
- Overview (`toggle_overview`, `overview_nav`, `overview_enter`) is a zoomed-out
  projection of the current View for picking a column. It changes the
  projection, not the layout and not View membership.
- `grow_col` resizes the focused column by a pixel amount; `maverickctl resize`
  addresses the same operation as a percentage. `new_column` and
  `collapse_column` add and remove columns.
- Fullscreen and maximize are **presentation**, applied after the layout
  (`present::present_into`), not separate layouts. A window taking a real
  exclusive fullscreen also gets `_NET_WM_BYPASS_COMPOSITOR` published, so an
  external compositor steps aside for it.
- `LayoutKind` has a single variant, `Column`. `set_layout` accepts only
  `column`, and no second layout exists in this repository.

### Floating clients

- A window floats because it is a transient or dialog, matches a
  floating-heuristic or a rule, or is toggled with `Super+Shift+Space`.
- Floating windows are projected from their own `Client::geom` and are never
  placed by the layout. Scrolling the ribbon, resizing a column and entering
  Overview leave them where they are, in global X11 coordinates.
- Sticky floats stay visible on every View of their monitor. Ordinary floats
  follow the visibility of the View they belong to.
- `[[rules]]` can force a float's size and position (relative to the workarea
  origin), its opacity, its border width, and whether a client's own fullscreen
  requests are honoured, normalised or refused.
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

Compositing, a panel, a launcher, notifications and a wallpaper are other
programs' work. Maverick starts them when they are listed in `[autostart]`,
reads the struts they publish, and never speaks to them again.

The installer reflects the same scope: there is no switch for a compositor, a
wallpaper, an animation component or a demo asset, and `--with-compositor` and
`--no-default-features` are rejected with exit status 2 because there is nothing
to select.

## Architecture

The high-level flow, from the active View to the geometry X11 holds:

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

- **The Carousel does not know about layouts.** It holds two `ViewId`s and moves
  between them. There is no layout match in it, and navigation answers the same
  question whichever layout is installed.
- **Scroll does not own View navigation.** `layout::arrange` is handed one View
  — the active one, resolved through the Carousel — and returns geometry. It
  never creates, removes, selects or re-orders a View, and it never mutates
  View membership: it reads `columns` and `floats` and computes rectangles.
- **View order and View identity are separate.** Membership is keyed by
  `ViewId`, so removing a View shifts positions without invalidating any client
  reference.
- **Floating clients are outside the layout.** They live in the View's `floats`
  list, which is why they are excluded from the layout's input; the presentation
  stage projects them from their own geometry.
- **One projection, and it is the geometry.** There is no interpolated view
  alongside the settled one. A scroll rewrites the camera and the next arrange
  *is* the final geometry.
- **One geometry sink.** Every `ConfigureWindow` for a client is issued from the
  reconciler's diff; no other code positions a window. Applied state is a
  backend cache, not a claim that asynchronous X11 requests cannot fail.
- **The engine is pure.** `Engine::dispatch(Action)` executes a `Command`, which
  mutates `State` and emits `Effect`s. Only the backend performs effects, and
  only the backend touches X11.

| crate or path | responsibility |
| --- | --- |
| `maverick` (root package) | the window manager binary: CLI, configuration selection, signals, instance identity, backend startup |
| `maverick-core/` | Dependency-free domain types: `State`, `Monitor`, `Workspace` (a View), `ViewId`, `Carousel`, `Column`, `Client`, `Rect`, `Action` |
| `src/core/` | Engine, actions/commands/effects/events, layout, presentation, `DesiredState` |
| `src/backend/x11/` | Event handling, client management, input, EWMH, struts, reconciliation |
| `src/config.rs`, `src/userconfig.rs` | Compiled defaults, configuration merging, validation |
| `maverick-x11/` | Shared Xlib/XCB connection bootstrap; links `X11` and `X11-xcb` |
| `maverick-sys/` | OS/FFI boundary, instance identity, control socket protocol server, minimal JSON |
| `maverick-toml/` | Zero-dependency TOML-subset parser |
| `maverickctl/` | Control client: CLI, control-socket IPC client, discovery, session lifecycle |
| `installer/` | The installer, its shell library, and its test suite |
| `tests/` | Real-X11 probes, integration scripts, installer smoke test |

`maverick-core` has no X11, no clock, no filesystem and no environment
dependency, which is why most of the test suite needs no display. The
implementation does depend on native X11 libraries, and `x11rb` is used with an
XCB FFI connection rather than as a wholly pure-Rust protocol stack. There is no
async runtime and no GUI toolkit.

`docs/architecture.md` describes the same boundaries with `file:line` anchors.

## Project structure

```text
Maverick/
├── Cargo.toml            workspace root, and the `maverick` binary
├── Cargo.lock
├── config/
│   └── config.toml       commented sample configuration
├── docs/
│   ├── architecture.md   module boundaries with file:line anchors
│   └── sessions.md       the session model and its security boundary
├── installer/
│   ├── install.sh        the installer
│   ├── lint.sh           bash -n, plus shellcheck when available
│   ├── lib/              i18n, setup and terminal-UI shell libraries
│   ├── tests/            installer behaviour suite
│   └── golden/           expected installer output, English and Spanish
├── maverick-core/        pure domain types; no X11, clock or filesystem
├── maverick-sys/         libc FFI, instance identity, control protocol server
├── maverick-toml/        zero-dependency TOML-subset parser
├── maverick-x11/         Xlib/XCB connection bootstrap
├── maverickctl/          the `maverickctl` control client
├── src/
│   ├── main.rs           CLI, startup, signals, backend wiring
│   ├── config.rs         compiled defaults
│   ├── userconfig.rs     configuration parsing, merging, validation
│   ├── types.rs          window-manager type re-exports
│   ├── log.rs            log-level handling
│   ├── core/             engine, actions, layout, presentation, IPC
│   └── backend/x11/      events, clients, input, EWMH, struts, reconcile
├── tests/                real-X11 probes and integration scripts
├── CHANGELOG.md
├── README.md
├── README.es.md
└── LICENSE
```

## Requirements

Linux, an X11 server, a C linker and Rust 1.82 or newer (`rust-version` in
`Cargo.toml`). Maverick links `libX11` and `libX11-xcb`.

```bash
# Arch Linux
sudo pacman -S --needed base-devel rust libx11 libxcb
# Debian / Ubuntu
sudo apt install --no-install-recommends build-essential cargo \
  libx11-dev libx11-xcb-dev libxcb1-dev
# Fedora
sudo dnf install -y cargo gcc libX11-devel libxcb-devel
```

`libX11-xcb` is a separate development package on Debian and Ubuntu:
`libx11-dev` does not depend on it, so `-lX11-xcb` is missing without it. On
Arch the single `libx11` package ships both `libX11.so` and `libX11-xcb.so`,
which is why that line names neither.

For an X11 session started with `startx`, `xorg-server` and `xorg-xinit` are
also required (Arch: `xorg-server xorg-xinit`).

The compiled default keybindings launch `alacritty` and `rofi`, and the compiled
autostart launches `/usr/lib/xdg-desktop-portal` and
`/usr/lib/xdg-desktop-portal-gtk` by absolute path. Those are conveniences, not
requirements: the bindings can be overridden and the `[autostart] commands` list
replaced. The layout engine needs none of them.

Relevant environment variables:

| variable | effect |
| --- | --- |
| `DISPLAY` | the X display Maverick connects to |
| `XDG_RUNTIME_DIR` | parent of the control-socket directory; falls back to `/run/user/$UID`, never `/tmp` |
| `XDG_CONFIG_HOME` | parent of `maverick/config.toml` |
| `MAVERICK_INSTANCE` | default instance selector for `maverickctl` |
| `MAVERICK_SESSION` | session selector used by the session tooling |
| `MAVERICK_LOG` | log level; `--debug` is equivalent to `MAVERICK_LOG=debug` |

## Installation

Installation is source-based. The installer builds both binaries from this
workspace with `cargo build --release -p maverick -p maverickctl`.

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
./installer/install.sh
```

The default prefix is `$HOME/.local`, which needs no privileges. The installer
refuses to run as root, never invokes `sudo`, and never enables a service.

```text
--system               install into /usr/local instead
--prefix DIR           install into DIR
--xsessions-dir DIR    also install the session file into DIR (the only write
                       that leaves the prefix; off by default because display
                       managers usually read only system locations)
--lang LANG            force the installer language: en | es | auto
--yes, -y              skip installer confirmations
--no-config            do not create a configuration file
--no-build             skip the build and use existing $CARGO_TARGET_DIR/release
--add-path             add the bin directory to PATH without asking
--no-path              never modify shell startup files; print the export line
--no-anim              disable the installer's own terminal animation
--keep-log             keep the build log even on success
-h, --help             show every option
```

What it installs, and where:

| path | what |
| --- | --- |
| `<prefix>/bin/maverick` | the window manager |
| `<prefix>/bin/maverickctl` | the control client |
| `<prefix>/share/xsessions/maverick.desktop` | the X11 session entry |

Outside the prefix the installer writes only files belonging to the invoking
user, and only after confirmation:

- `${XDG_CONFIG_HOME:-$HOME/.config}/maverick/config.toml`, seeded from
  [`config/config.toml`](config/config.toml) unless one is already present
  (`--no-config` skips this; declining the overwrite keeps the existing file);
- one marked, self-guarding block in a shell startup file under `$HOME`, offered
  only when the bin directory is missing from `PATH`, and never with `--no-path`.

The prefix is a hard boundary: nothing outside it is created or modified except
those two files. A prefix that is not writable is reported as a permission error
rather than escalated around, so a system-wide install needs write access to
`/usr/local` arranged in advance.

Each step fails loudly. The installer runs the binaries it just installed —
`maverick --version`, `maverickctl --help`, `maverickctl session --help` — and a
partial, stale or broken set is reported as a failure rather than as a successful
install. It is safe to run repeatedly: a second run converges, fixes
umask-hostile permissions and does not duplicate the `PATH` block.

`CARGO_TARGET_DIR` is honoured as given. When it is unset the build happens in a
cache directory under `$XDG_CACHE_HOME`, and the checkout is never used as a
build directory. The first build attempt passes `-C target-cpu=native` and falls
back to a plain build if that fails, so the installed binary is tuned for the
machine that built it; use a normal `cargo build` when artifacts for a different
CPU are needed.

Verify an installation with:

```bash
maverick --version
maverickctl --version
```

To uninstall, delete the two binaries and the session file from the prefix, and
remove the block between the `# >>> maverick (install.sh) >>>` markers from any
startup file it touched.

See [`installer/README.md`](installer/README.md) for the installer's own
documentation and its test suite.

## Running Maverick

For a `startx` session, put this at the end of `~/.xinitrc`:

```sh
exec maverick
```

Alternatively, select the installed Maverick session in the display manager.

```bash
maverick --check-config ~/.config/maverick/config.toml   # validate, start nothing
maverick --config ~/.config/maverick/config.toml --name desktop
maverick --help
```

| flag | effect |
| --- | --- |
| `--name <id>` | label the instance for control and identification |
| `--session-id <id>` | publish the instance under a fixed session id (`[A-Za-z0-9_-]`), which is what `maverickctl session` uses; the default is random |
| `--replace` | take over from a running window manager, adopting its windows |
| `--debug` | log at debug level, equivalent to `MAVERICK_LOG=debug` |
| `--log-level <level>` | `off`, `error`, `warn`, `info`, `debug` or `trace`; overrides `--debug` |
| `--config <path>` | read the configuration from `<path>` instead of `$XDG_CONFIG_HOME/maverick/config.toml`; reused by reload and restart |
| `--check-config [path]` | validate a configuration and exit: `0` clean, `1` on warnings or errors. Starts no window manager and opens no display |
| `-v`, `--version` | print the version and exit |
| `-h`, `--help` | show the built-in help |

## Keybindings

`Super` is Mod4, normally the Windows key. The table below is the **compiled
default** binding set: 33 explicit bindings plus one `Super+<digit>` and one
`Super+Shift+<digit>` per View, 51 in total.

| action | binding | behaviour |
| --- | --- | --- |
| Spawn a terminal | `Super+Return` | runs `alacritty` |
| Launcher, run a command | `Super+Shift+P` | runs `rofi -show run` |
| Launcher, run a desktop entry | `Super+P` | runs `rofi -show drun` |
| Close the focused window | `Super+Shift+C` | asks the client to close |
| Toggle floating | `Super+Shift+Space` | moves the window in or out of the layout |
| Toggle fullscreen | `Super+Shift+F` | real exclusive fullscreen overlay |
| Toggle maximize | `Super+Shift+M` | presentation-only maximize |
| Focus left / down / up / right | `Super+H` / `J` / `K` / `L` | moves focus within the View |
| Move window left / down / up / right | `Super+Shift+H` / `J` / `K` / `L` | moves the client to a neighbouring column |
| New column | `Super+Shift+Return` | appends a column to the ribbon |
| Shrink column | `Super+Ctrl+H` | `grow_col:-50` |
| Grow column | `Super+Ctrl+L` | `grow_col:50` |
| Collapse column | `Super+Ctrl+J` | removes the focused column |
| Set the layout | `Super+T` | `layout:column`; the only accepted value |
| Quit | `Super+Shift+Q` | orderly shutdown: ask clients to close, wait, force-kill the remainder, clean up |
| Restart | `Super+Shift+R`, `Super+F5` | re-executes in place with the same arguments |
| Focus the next monitor | `Super+Tab` | cycles monitor enumeration order |
| Move window to the next monitor | `Super+Shift+Tab` | cycles monitor enumeration order |
| Overview toggle / enter / next / previous | `Super+O` / `Super+E` / `Super+N` / `Super+Shift+O` | zoomed-out projection for picking a column |
| Viewport zoom in / out | `Super+=` / `Super+-` | enlarges or restores the ribbon |
| Page-snap right / left | `Super+]` / `Super+[` | scrolls the camera one screen-width |
| Select View | `Super+1` … `Super+9` | generated per View, up to `n_tags` |
| Send window to View | `Super+Shift+1` … `Super+Shift+9` | generated per View |

The generated digit bindings follow `n_tags`. Setting
`auto_workspace_binds = false` in `[general]` suppresses them, leaving the digit
row entirely unmanaged.

Carousel stepping (`view_next`, `view_prev`, `view_return`) and View lifecycle
(`view_create`, `view_remove`) are **not** bound by default. They are reachable
through `maverickctl view`, or through a `[[keybindings]]` table.

Bindings are resolved through the active XKB layout, so a binding matches what
the keyboard actually produces rather than the nominal keycode. A key combination
written in a configuration file uses X keysym names with `Mod4`/`Mod1`-style
modifiers, for example `Mod4+Shift+Return` or `Mod4+Control+h`.

The sample configuration in [`config/config.toml`](config/config.toml) is a
preset rather than a copy of the compiled defaults: it binds viewport zoom and
page-snap to different keys and adds rules, a theme and an autostart list, so
using it as a starting point replaces the compiled bindings.

## Configuration

Configuration is optional. Maverick reads `$XDG_CONFIG_HOME/maverick/config.toml`,
falling back to `~/.config/maverick/config.toml`, and uses compiled defaults for
anything the file does not set.

A minimal working file:

```toml
[general]
column_width = 0.5
gaps_inner = 10
gaps_outer = 14
focus_mouse = false
```

Compiled defaults for the most relevant `[general]` keys:

| key | default | meaning |
| --- | --- | --- |
| `n_tags` | `9` | Views per monitor at startup; clamped to a maximum of 9 |
| `column_width` | `0.6` | column width as a fraction of the monitor workarea; must be between `0.1` and `1.0` |
| `gaps_inner` | `4` | gap between adjacent tiles |
| `gaps_outer` | `8` | gap between tiles and the screen edge |
| `border_width` | `1` | tile border width in pixels |
| `focus_mouse` | `false` | whether moving the pointer changes focus |
| `honor_initial_state` | `false` | honour a client's map-time maximized/fullscreen state instead of normalising it |
| `auto_workspace_binds` | `true` | generate the `Super+<digit>` View bindings |
| `smart_gaps` | `false` | suppress outer gaps where only one tile is visible |
| `corner_radius` | `0` | tile corner radius in pixels |
| `theme` | `catppuccin-mocha` | built-in theme name |
| `tag_names` | `["1"]` … `["9"]` | View labels published as `_NET_DESKTOP_NAMES` |

Other accepted `[general]` keys: `gaps`, `accordion_boost`, `overview_zoom_min`
and `warp_cursor`. `border_w` is an alias for `border_width`.

Two keys are deprecated aliases kept for compatibility. Both still load, and both
emit a warning:

| deprecated key | alias | type | superseded by |
| --- | --- | --- | --- |
| `default_col_width` | `default_col_w` | pixels | `column_width`, converting against a fixed 1920px workarea |
| `split_bias` | — | fraction, `0.0`–`1.0` | `column_width` |

`[colors]` accepts `normal`, `focused` and `urgent`, each also readable as
`col_normal`, `col_focused` and `col_urgent`.

### How a file is combined with the defaults

- Ordinary settings are merged field by field.
- `[[keybindings]]` and `[[rules]]` **replace** the compiled list outright when
  they are declared. Supplying one `[[rules]]` drops the compiled
  per-application float policy, so the required entries must be repeated.

### Behaviour on a malformed or partial file

- Malformed TOML falls back to the compiled defaults.
- An invalid individual entry is diagnosed and ignored; the rest of the file
  still loads.
- An unknown table is skipped silently, so a file written for a different
  Maverick does not produce noise.
- An unknown key inside a table Maverick does know is reported as a warning and
  ignored.

Validate before relying on any of this:

```bash
maverick --check-config ~/.config/maverick/config.toml
```

[`config/config.toml`](config/config.toml) is a commented sample covering the
wider vocabulary. It is a preset, not a copy of the compiled defaults.

### Application rules

`class`, `instance` and `title` match case-insensitive substrings of the
window's own strings; `window_type` (alias `type`) matches one complete
normalised `_NET_WM_WINDOW_TYPE` name. Every criterion present in a rule must
match.

```toml
[[rules]]
class = "calculator"
float = true
size = [480, 360]
position = [120, 100]
```

| rule key | aliases | effect |
| --- | --- | --- |
| `class`, `instance`, `title` | — | case-insensitive substring match |
| `window_type` | `type` | one complete normalised `_NET_WM_WINDOW_TYPE` name |
| `float` | — | manage the window as a floating client |
| `sticky` | — | show the window on every View of its monitor |
| `workspace` | `ws` | place the window on this 1-based View |
| `size` | — | `[width, height]` for a floating window |
| `position` | — | `[x, y]` relative to the workarea origin |
| `opacity` | — | window opacity |
| `border_width` | `border_w` | per-window border width |
| `honor_initial_state` | — | opt this window out of initial-state normalisation |
| `ignore_initial_state` | `no_initial_state`, `no_maximize` | the inverse of the above |
| `deny_fullscreen` | `no_fullscreen` | refuse the client's own fullscreen requests, not the `Super+Shift+F` binding |
| `true_fullscreen` | `exclusive_fullscreen` | ask for a real exclusive overlay; takes precedence over `deny_fullscreen` |

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

## maverickctl

`maverickctl` is a separate control client. It never links the window manager and
never opens an X display; it speaks to a running instance over that instance's
Unix control socket, which is peer-checked with `SO_PEERCRED` inside a private
`0700` runtime directory.

```bash
maverickctl --version
maverickctl list
maverickctl state --name desktop
maverickctl query tree --name desktop
maverickctl msg view 3 --name desktop
maverickctl subscribe --name desktop
maverickctl reload --name desktop
maverickctl restart --name desktop
maverickctl quit --name desktop --confirm
```

Global options may appear anywhere on the line and are never forwarded to the
instance as part of an action:

| option | effect |
| --- | --- |
| `-v`, `--version` | print the version and exit |
| `-j`, `--json` | machine-readable output where a command produces a document |
| `-y`, `--yes` | skip a confirmation prompt |
| `-s`, `--session <sid>` | explicit session id, from `list` |
| `-n`, `--name <id>` | instance label, or session id |

Instance selection prefers `--session`, then `--name`, then
`$MAVERICK_INSTANCE`, then the sole instance on the current `DISPLAY`/TTY. When
several candidates match, discovery refuses to guess.

Views and windows are addressed semantically, and every operation is the same
action a keybinding runs:

```bash
maverickctl view debug next
maverickctl view debug goto 3
maverickctl window list debug --json
maverickctl window focus debug firefox
maverickctl window float debug 0x42003
maverickctl resize debug +10%
maverickctl process list debug --json
maverickctl inspect debug
```

A window is addressed either by its X11 id (`0x42003`) or by a name matched
against the class, the instance name and the title — an exact match is tried
before a substring match, and an ambiguous name is refused with the candidate ids
rather than guessed. Omitting the window acts on the focused one.

Any word `maverickctl` does not recognise as a command is forwarded verbatim to
the window manager, which is the only component that can tell an action from a
query topic from a typo.

`maverickctl <group> --help` documents one group in full: `session`, `window`,
`process`. `maverickctl --help` gives the complete command list.

## Sessions

`maverickctl session` manages whole graphical sessions: a nested X server, a
Maverick, the programs launched into it, a cookie, logs and a lifecycle.

```bash
maverickctl session create work --resolution 1920x1080
maverickctl session status work --json
maverickctl session logs work -f
maverickctl session stop work
maverickctl session remove work
```

Each session runs a nested X server — `xephyr` by default, which is visible, or
`xvfb` with `--backend xvfb`. The session's identity, socket and logs live under
the private runtime directory, and a session refuses to start on a display it has
not claimed exclusively.

Quitting a session asks its clients to close through `WM_DELETE_WINDOW` and
force-closes the survivors after a bounded wait, so pending work should be saved
first.

See [`docs/sessions.md`](docs/sessions.md) for the full session model, its
limitations and its security boundary.

## Troubleshooting

**`maverickctl` cannot find the instance.** Discovery refuses to guess when more
than one candidate matches the display. List the candidates with
`maverickctl list` and select explicitly with `--name` or `--session`.

**A configuration change had no effect.** `reload` is a no-op when the running
binary was built with compiled-in configuration only; restart the instance
instead. `--check-config <path>` reports whether a file parses and whether any
key is unknown.

**A keybinding does not fire.** Bindings resolve through the active XKB layout.
A `[[keybindings]]` table replaces the compiled list outright, so declaring one
removes every default that is not repeated in it.

**A window opens maximized or fullscreen when it should not.** Map-time
`_NET_WM_STATE` is normalised by default. Set `honor_initial_state = true` in
`[general]`, or per rule, to keep the client's own state.

**A stale `ViewId` is reported.** View ids are never reused, so a reference to a
deleted View is detectable rather than silently redirected. Re-select the View by
position with `maverickctl view <session> goto <n>`.

**The installer exits with status 2.** `--with-compositor`,
`--without-compositor`, `--no-compositor` and `--no-default-features` are
rejected: Maverick has no compositor and the build has no default feature, so
there is nothing to select.

**The installer fails on a prefix it cannot write.** The prefix is a hard
boundary and is never escalated around with `sudo`. Use `--prefix` with a
writable location, or arrange write access to `/usr/local` in advance for
`--system`.

**More detail is needed about input or window state.** Build with the opt-in
`input-trace` and `window-trace` features to add structured traces. Both are off
by default and only add logging:

```bash
cargo build -p maverick --features input-trace,window-trace
./target/debug/maverick --config /path/to/config.toml 2> /tmp/maverick-debug.log
```

Run that only on a `DISPLAY` intended to be handed to a window manager.

**Installer animation garbles piped output.** Use `--no-anim`, or export
`MAVERICK_NO_ANIM=1`. It reaches no cargo build: Maverick draws through X11 and
has no animation subsystem.

## Building from source

```bash
cargo build --release -p maverick -p maverickctl
cargo check --workspace --all-targets
cargo fmt --all -- --check      # read-only form; `cargo fmt --all` to apply
```

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Layout, Carousel and command work is covered by pure state tests, which need no
display. Protocol, stacking and focus behaviour use an isolated X server.

The installer has its own checks:

```bash
bash installer/lint.sh                  # bash -n, and shellcheck when present
python3 installer/tests/partition.py    # the installer behaviour suite
```

Real-X11 harnesses live in `tests/`. `tests/xvfb-stacking.py` is the automated
regression, and the `tests/xephyr-*.sh` scripts are manual integration
scenarios. They are **not** all isolated to the same standard — some older
helpers in `tests/common.sh` kill processes by name or use fixed displays — so a
script should be read before it is run, and the legacy suite reserved for a
disposable graphical session.

CI (`.github/workflows/ci.yml`) runs three jobs: the workspace with strict
Clippy and both feature sets, the installer checks, and an Xvfb stacking smoke
test.

## Status

The canonical version is declared once, in `[workspace.package]` in
`Cargo.toml`, and every package inherits it. The tree currently carries
**1.1.1**, which is the current release. Changes made since the last release
are collected under `[Unreleased]` in [`CHANGELOG.md`](CHANGELOG.md). The
most recent release is **1.1.1**; the full history is there too.

Maverick is in preview. It is not declared production-ready, and the integration
scripts are regression checks rather than a certification of application
compatibility.

- **Scope:** Linux and X11 only. No Wayland backend, compositor, animation
  subsystem, desktop shell, blur or shadows.
- **Layouts:** Scroll is the only layout. `LayoutKind` has one variant; a second
  layout is not implemented and is not documented as if it were.
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