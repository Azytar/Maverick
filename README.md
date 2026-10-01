# Maverick

Maverick is an experimental X11 window manager for Linux, written in Rust.
It combines a horizontally scrolling ribbon of tiled columns with per-monitor
workspaces and independent floating windows. It talks to X11 and nothing else:
no compositor, no GPU renderer, no GL or Vulkan dependency.

[Overview](#overview) · [Design](#design) · [Installation](#installation) ·
[Configuration](#configuration) · [Testing](#testing) · [Screenshots](#screenshots)

Read this in Spanish: [README.es.md](README.es.md).

## Overview

Maverick explores a spatial alternative to fitting every window onto one screen.
New tiled windows normally enter new columns; each column has a width relative
to the monitor's workarea and can contain a vertical stack of windows. Adding
columns extends the ribbon instead of continually shrinking its neighbours.
Navigation moves the viewport through that ribbon, keeping the focused column
in view. Overview provides a zoomed-out film-strip for selecting a column.

Floating windows belong to a monitor and workspace but not to the ribbon. They
keep screen-space geometry while the tiles scroll. Fullscreen and maximize are
presentation policies layered over the logical placement, rather than separate
layouts.

This is an X11 systems project, not a desktop environment or a Wayland compositor.
It uses X11 client properties, RandR monitors, and a reconciliation loop to connect
a testable state model to real application windows. It does not ship a panel,
notification service, lock screen, or application launcher.

## Features

### Window management and navigation

- One tiling layout: horizontally scrolling columns with vertical window stacks.
- Directional focus and movement, adjustable column widths, new-column and
  collapse-column operations.
- Per-workspace cameras, viewport zoom, page-snap scrolling, and Overview selection.
- Configurable borders, gaps, themes, keybindings, and application rules.
- Unix-socket commands, JSON state queries, and event subscriptions.

### Floating windows and fullscreen

- Floating placement through window types/transients, rules, or an explicit toggle.
- Modifier-drag movement and resizing of **already-floating** windows.
- Sticky windows, per-rule geometry, and client size-hint handling.
- Fullscreen, workarea maximize, and an exclusive `true_fullscreen` policy for
  applications that should leave the ribbon presentation entirely.
- Rules for accepting or refusing a client's initial and subsequent fullscreen state.

### Multi-monitor

- RandR monitor discovery and topology updates, with workspaces per monitor.
- Focus and window movement between monitors.
- Workareas derived from dock reservations (`_NET_WM_STRUT_PARTIAL` / `_NET_WM_STRUT`).

### Rendering and session behavior

- X11 only. Every state change is written to the server in one configure at its
  final position; there is no frame loop and no interpolation.
- The scroll camera is a plain offset: scrolling rewrites it and re-projects.
  No window is ever resized frame by frame, because no frames are drawn.
- `_NET_WM_BYPASS_COMPOSITOR` and `_NET_WM_WINDOW_OPACITY` published on managed
  windows, so an external compositor (picom, compton) can honour them.
- In-place restart, adoption of existing windows with `--replace`, and isolated
  control sockets for multiple Maverick instances.
- Unit/regression tests in Rust and separate real-X11 integration harnesses.

## Design

The central distinction is between **logical placement**, **desired presentation**,
and **the state last applied to X11**.

```text
Key / pointer / IPC actions       X11 lifecycle events
             |                           |
             v                           v
        Engine / Command ----------> State + Cfg
             |                           |
          Effects                 layout::arrange
             |                  + present::present_into
             |                           |
             +----> X11 backend <--- DesiredState
                         |
                     Reconciler ----> AppliedState
                         |
                         v
                        X11

```

`maverick-core` defines the domain types, including clients, columns, workspaces,
cameras, and rectangles. The `src/core/` modules implement the engine, commands,
layout, presentation, and desired-state hand-off. Keeping protocol handles out of
the domain model allows geometry and transitions to be tested without an X server.

`layout::arrange` projects a workspace into rectangles; `present::present_into`
applies fullscreen/maximize presentation. The backend's `Reconciler` compares
`DesiredState` with its `AppliedState` bookkeeping and emits geometry/border changes.
The surrounding X11 backend handles visibility, stacking, and focus separately.
Applied state is a backend cache, not a claim that asynchronous X11 requests can
never fail.

The layout has one projection. The camera is a plain scroll offset, and the
rectangles it produces are the rectangles X11 is told about — there is no
interpolated view alongside the settled one, and no window whose size is rewritten
on the way there.

The ribbon is a logical coordinate system, not another X11 screen. Its projection
includes the monitor's global origin and workarea; X11 still sees ordinary windows
in the root coordinate space. Floating windows are deliberately outside the ribbon
transform. This separation matters on multi-monitor desktops and when scrolling,
zooming, or restoring fullscreen geometry. Scrolling is an internal layout
transform over the physical desktop: `_NET_DESKTOP_GEOMETRY` and `_NET_WORKAREA`
stay physical, and Maverick does not publish `_NET_DESKTOP_VIEWPORT`.

## Floating windows

A window can float because it is a transient/dialog, matches floating heuristics
(such as fixed-size hints) or a rule, or is toggled with `Super+Shift+Space`. Rules
can specify size and position; rule positions are relative to the workarea origin.
Initial placement normally centres a float over its transient parent's stored
geometry, or in its assigned monitor's workarea. Persisted float geometry can take
precedence on adoption. Each managed window belongs to either a column or the
workspace's float list, never both.

- **Spatial isolation:** floats use global X11 coordinates. Ribbon scrolling,
  column resizing, and Overview projection do not scale or translate them.
  Ordinary floats follow workspace visibility; sticky floats remain visible
  across workspace switches on their monitor.
- **Geometry ownership:** when the WM places or moves a float into a new context,
  it settles geometry against size hints and the workarea. A client-claimed
  floating rectangle is retained rather than repeatedly re-normalized by layout.
  Dragging, rules, monitor/workspace moves, or workarea changes can reclaim that
  geometry for WM placement. This avoids competing resize authorities.
- **Movement:** `Super+Left-drag` moves and `Super+Right-drag` resizes a float.
  Releasing it over a tile does not insert it into the column. Modifier-dragging
  a tiled window does nothing; use keyboard movement for tiles.
- **Stacking:** ordinary floats are layered above ordinary tiles, but this is not
  a universal “always on top” guarantee. Presented fullscreen/maximize windows,
  transient relationships, focus ordering, and unmanaged X11 windows also affect
  the final stack.
- **Fullscreen:** entry temporarily promotes a float into tiled topology and records
  its previous mode and geometry. Exit restores floating membership and the saved
  rectangle, subject to placement normalization. An ordinary tile-to-float toggle,
  in contrast, starts from the current tile rectangle.
- **Keyboard focus:** directional focus follows columns/rows; `focus:next` and
  `focus:prev` can include floats through focus history. Directional `move` does
  not provide pixel movement for floats.

Tiled clients do not control their layout rectangle through `ConfigureRequest`;
the WM answers with its assigned geometry. Client-claimed floating requests are
bounded for X11 protocol safety, not continuously constrained to the workarea.
During an active drag, the WM retains geometry authority.

### Fullscreen and navigation

Fullscreen entry currently selects exclusive presentation, covering the monitor
without a border. Explicit left/right navigation to another column yields that
overlay back to ribbon fullscreen while retaining its fullscreen flag and restore
snapshot: it can scroll out of view and fill the monitor again on return. An
exclusive overlay is not simply raised or dismissed whenever focus changes.
Maximize instead uses the workarea (also without a border), with independent
horizontal and vertical state bits. These policies are still evolving; fullscreen
is neither a permanently pinned ribbon tile nor a universal input lock.

## Compositing

Maverick has no compositor and draws through X11 alone: no frame loop, no GPU,
no GL or Vulkan library, no extra runtime dependency. Compositing is somebody
else's job, and it composes with a window manager the ordinary way — over the
wire.

Two EWMH properties make that work, and Maverick publishes both:

- `_NET_WM_BYPASS_COMPOSITOR` is set to `2` on a window that takes a true
  exclusive fullscreen, and deleted when it leaves fullscreen. An external
  compositor reads it and steps aside for that window.
- `_NET_WM_WINDOW_OPACITY` is written on managed windows from
  `[[rules]] opacity`. It is a per-window property, not a rendering mode.

Both are ordinary EWMH, so `picom`, `compton` or anything else that speaks it
works unmodified, and the WM is no larger for it. An external compositor owns
its own effects; Maverick does not delegate or emulate them.

If you want blur, shadows, fading or animated window transitions, run a
compositor in your session — from your display manager, or from
`[autostart] commands` — and Maverick gets out of the way.

## Installation

### Dependencies (Arch Linux)

Linux, an X11 server, a C linker, and Rust are required. Workspace manifests declare
edition 2021 and Rust **1.82** as the minimum; current CI uses stable Rust.

```bash
sudo pacman -S --needed base-devel rust libx11 libxcb
# For an X11 session started with startx:
sudo pacman -S --needed xorg-server xorg-xinit
```

The compiled launch bindings use `alacritty` and `rofi`; install those or override
the bindings. Compiled autostart launches `xdg-desktop-portal` and
`xdg-desktop-portal-gtk`; use an explicit autostart list to change or disable it.
These applications are not required by the layout engine.

### Build

```bash
git clone https://github.com/Azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

The runtime binaries are `maverick` (the window manager) and `maverickctl` (its
control and session tool), under `target/release/`. Those two are the whole
user-facing surface. No Rust installer crate is part of the workspace.

### Install

Run the installer as your normal user. The default is a per-user install that
needs no privileges:

```bash
./installer/install.sh
```

That builds and installs into `$HOME/.local`. When that directory is not
already on `PATH`, the installer asks before adding a marked block to the
startup files your shell reads (`~/.profile` plus the rc file of your login
shell) and then tells you to open a new terminal:

```bash
# >>> maverick (install.sh) >>>
# Added by install.sh — delete these lines to undo.
case ":$PATH:" in
  *":/home/you/.local/bin:"*) ;;
  *) PATH="/home/you/.local/bin:$PATH"; export PATH ;;
esac
# <<< maverick (install.sh) <<<
```

`--yes` accepts that block, `--add-path` writes it without asking, and
`--no-path` leaves your startup files alone and prints the `export` line
instead. A startup file that already names the directory — Debian and Ubuntu
ship such a line in `~/.profile` — is reported rather than rewritten.

Other forms:

```bash
# System-wide install into /usr/local.
./installer/install.sh --system
# Any explicit prefix.
./installer/install.sh --prefix /opt/maverick
# Additionally publish the session file where a display manager reads it.
./installer/install.sh --system --xsessions-dir /usr/share/xsessions
# Leave shell startup files alone and get the export line printed instead.
./installer/install.sh --no-path
```

The prefix is a hard boundary: the installer writes nothing outside the prefix
it was given, and it never runs `sudo`. A prefix you cannot write is reported
as a permission error rather than escalated around, so a system-wide install
needs write access to `/usr/local` arranged by you — the installer will tell
you so plainly if it does not have it.

Outside the prefix it touches only your own files: `~/.config/maverick/config.toml`,
and — only when the bin directory is missing from `PATH`, and never under
`--no-path` — the marked block above inside a startup file under `$HOME`.

The session entry is written inside the prefix (`$prefix/share/xsessions`).
Display managers generally read only system locations, which is why publishing
it elsewhere is the explicit `--xsessions-dir` option rather than something the
installer does on its own.

The installer builds release binaries, offers/seeds configuration, and retains an
existing config with `--yes`. Use `--no-config` to avoid config creation. It runs
each installed binary before reporting success, so a partial or stale install
fails loudly instead of being announced as complete. Its first build attempt uses
`-C target-cpu=native`, so use a normal Cargo build when producing artifacts for
other machines.

`CARGO_TARGET_DIR` is honoured as given, including when it already holds
artifacts from an earlier build. When unset, the build directory is a cache
directory under `$XDG_CACHE_HOME`; the checkout is never used as a build
directory. See `./installer/install.sh --help` for the remaining options.

To remove an installation, delete the two binaries from `$prefix/bin` and the
session file from `$prefix/share/xsessions`. Nothing else is installed into the
prefix; the only other files the installer may write are your own
`~/.config/maverick/config.toml`, the marked PATH block in a startup file
(remove the lines between the `# >>> maverick` markers to undo it), and a
retained build log under `~/.local/share/maverick/` if you pass `--keep-log`.

## Running

### X11 session

For `startx`, put this at the end of `~/.xinitrc` after any session setup:

```sh
exec maverick
```

Alternatively select the installed Maverick session in a display manager. Do not
start a second WM on your live display accidentally. `maverick --replace` deliberately
requests a handover from the existing WM and adopts its windows.

```bash
maverick --check-config "$HOME/.config/maverick/config.toml"
maverick --config "$HOME/.config/maverick/config.toml" --name desktop
maverick --help
```

`--check-config [path]` validates without starting X11 and returns `0` for a clean
configuration or `1` for diagnostics. `--config` is reused by reload/restart.
`--name` labels the instance; `--version` prints the version.

### Nested X11 and debugging

Use the [showcase](#reproducing-the-screenshots) for an isolated Xephyr session with
a private configuration, controlled clients, and automatic cleanup. Xephyr requires
an accessible parent X display; on Wayland this normally means Xwayland.

For diagnostics, build with the opt-in `input-trace` and/or `window-trace` features
and capture the WM's standard error in a test session:

```bash
cargo build -p maverick --features input-trace,window-trace
./target/debug/maverick --config /path/to/test.toml \
  2> /tmp/maverick-debug.log
```

Run the latter **only on your intended test `DISPLAY`**. These features add structured
input/focus or desired/applied/X11-state traces; they are off in normal builds.

## Configuration

Configuration is optional. Maverick looks for
`$XDG_CONFIG_HOME/maverick/config.toml`, falling back to
`~/.config/maverick/config.toml`. Missing settings use compiled defaults.

A small configuration is sufficient:

```toml
[general]
column_width = 0.5
gaps_inner = 10
gaps_outer = 14
focus_mouse = false
```

Leaving out `[autostart]` keeps the compiled defaults; see the autostart notes
below for how list replacement works.

Validate it before applying `maverickctl reload`. Malformed TOML falls back to
compiled defaults; invalid individual entries are diagnosed and ignored. Treat
warnings seriously—falling back to defaults can also change bindings and autostart.

The [commented sample](config/config.toml) lists the wider configuration vocabulary.
It is a preset, **not an exact copy of compiled defaults**: copying it changes
bindings, rules, and autostart. In particular:

- Ordinary settings merge with defaults. `[[keybindings]]` and `[[rules]]` replace
  their respective compiled lists when supplied.
- Numeric workspace bindings are filled into unused slots unless
  `auto_workspace_binds = false`. `n_tags` is limited to 1–9.
- `column_width` is a workarea fraction (0.1–1.0); `accordion_boost` defaults to
  `0.0`, so focused-column expansion is opt-in.
- `[colors]` accepts `0xRRGGBB` values for `normal`, `focused`, and `urgent`, overriding
  a `[general] theme` preset.

### Essential compiled bindings

`Super` means Mod4 (usually the Windows key). The sample and installer-generated
configuration can override these defaults.

| Binding | Action |
| --- | --- |
| `Super+Return` / `Super+P` | Terminal / application launcher |
| `Super+H/J/K/L` | Focus left/down/up/right |
| `Super+Shift+H/J/K/L` | Move window left/down/up/right |
| `Super+Shift+Return` | Put window in a new column |
| `Super+Ctrl+H/L` / `Super+Ctrl+J` | Shrink/grow column / collapse into previous column |
| `Super+Shift+Space` | Toggle floating |
| `Super+Shift+F` / `Super+Shift+M` | Toggle fullscreen / maximize |
| `Super+O` / `Super+E` | Toggle Overview / enter its selection |
| `Super+N` / `Super+Shift+O` | Overview selection right / left |
| `Super+=/-` / `Super+]/[` | Viewport zoom / page-snap right/left |
| `Super+1…9` / `Super+Shift+1…9` | Switch workspace / send window to workspace |
| `Super+Tab` / `Super+Shift+Tab` | Focus next monitor / send window to next monitor |
| `Super+wheel` | Step column focus |
| `Super+Shift+C` | Close focused window |
| `Super+Shift+R` or `Super+F5` | Restart in place |
| `Super+Shift+Q` | Quit immediately (native clean shutdown, no dialog) |

Custom bindings use entries such as `key = "Mod4+Return"` and
`action = "spawn:xterm"` inside `[[keybindings]]`. Remember that supplying one
replaces the compiled non-workspace binding list. The canonical action parser is
[`src/core/action.rs`](src/core/action.rs).

### Application rules

`class`, `instance`, and `title` match case-insensitive substrings. `window_type`
matches a complete normalized type name, such as `dialog` or `utility`. Multiple
criteria in one rule must all match.

```toml
[[rules]]
class = "calculator"
float = true
size = [480, 360]
position = [120, 100]
```

Rules also accept `sticky`, `workspace` (1-based), `opacity`, and `border_width`.
Client-requested map-time fullscreen/maximize is normally normalized;
`honor_initial_state` opts in globally or per rule, while `ignore_initial_state`
forces normalization. `deny_fullscreen` refuses client EWMH fullscreen requests,
not the user's WM toggle. `true_fullscreen` selects an exclusive overlay policy
and takes precedence over that denial.

### Autostart

`[autostart] commands` is a list of argument lists, for example
`commands = [["polybar", "main"]]`. Supplying a non-empty command list replaces the
compiled one; there is no documented empty-list override, and an empty entry is
discarded with a warning. This is also where a compositor or a wallpaper program
belongs — Maverick starts them and never talks to them again. Use X11-compatible
applications; a Wayland-only panel is not made compatible by listing it here.
Docks that publish struts reserve workarea. Session startup and restart are not a
general-purpose service supervisor.

## Control and session lifecycle

```bash
maverickctl list
maverickctl state --name desktop
maverickctl query tree --name desktop
maverickctl msg focus-left --name desktop
maverickctl subscribe --name desktop
maverickctl reload --name desktop
maverickctl restart --name desktop
maverickctl quit --name desktop --confirm
```

`maverickctl` also forwards action lines verbatim, for example
`maverickctl view 3`; a word it does not
recognise as a command is passed to the window manager, which is the only thing
that can tell an action from a query topic from a typo. Each instance has a
private runtime directory and
Unix socket under `$XDG_RUNTIME_DIR/maverick/<session-id>/`. Discovery checks
process identity and socket liveness. Selection prefers `--session`, then `--name`,
then inherited `MAVERICK_INSTANCE`, then display/TTY context; a global singleton
can also be selected. Use explicit targeting when testing alongside a live session.

There is no second control binary: `maverick-msg`'s capability is `maverickctl`'s.

### Maverick Sessions

A **session** is a whole graphical unit — a real nested X server, a Maverick, the
applications launched into it, a control socket, logs and a lifecycle — named,
reproducible and controllable from that one tool:

```bash
maverickctl session create debug --resolution 1280x720
maverickctl exec debug alacritty
maverickctl window list debug --json
maverickctl inspect debug
maverickctl session stop debug
```

A session can run a specific binary, working directory and arguments, which is
what makes it a development and debugging tool rather than a second desktop:

```bash
maverickctl session create debug \
    --binary ./target/debug/maverick --resolution 1280x720 --debug \
    -- --debug --log-level trace

maverickctl session create release --binary ./target/release/maverick
maverickctl session create tiny   --resolution 800x600
```

`maverickctl` controls windows semantically — by id or by name, with the same
actions the keybindings use — and never touches X11 itself, so a tool and a
keypress cannot reach different code:

```bash
maverickctl window focus  debug firefox
maverickctl window float  debug 0x42003
maverickctl camera        debug right
maverickctl resize        debug +10%
maverickctl process list  debug --json
```

The user's own session is `main` and is addressable the same way. Every listing
has a `--json` form, and ownership is by uid: the runtime directory is `0700`,
the socket `0600` and peer-checked with `SO_PEERCRED`, and each display has its
own X cookie. See **[`docs/sessions.md`](docs/sessions.md)** for the model, the
nested-X-server backend comparison, the lifecycle and the limitations.

**Quit closes the session's managed applications**, not just the WM. Shutdown asks
clients through `WM_DELETE_WINDOW`, then force-closes survivors after a bounded
wait (three seconds). Save work before quitting. Restart is a separate in-place
re-execution path with topology/geometry recovery, not a fresh desktop login.

## Testing

### Rust checks

```bash
cargo check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo check` checks the default package/build configuration; workspace tests also
exercise the supporting crates. Tests cover layout and state invariants,
presentation/focus transitions, floating geometry convergence, configuration and
action parsing, and the control-protocol and session/discovery surfaces.
They do not constitute an application compatibility test against real drivers.

[CI](.github/workflows/ci.yml) runs three jobs: workspace tests with strict
Clippy, installer checks (`bash -n` plus `installer/tests/partition.py`, which
runs every `tests/install-smoke.py` suite and the partition-only ones), and an
X11 smoke job that runs the Xvfb stacking regression. It does not run the Xephyr
scenarios.

### Real X11 and installer tests

```bash
cargo build -p maverick -p maverickctl
python3 tests/xvfb-stacking.py
python3 installer/tests/partition.py
```

The Xvfb stacking regression compiles an Xlib probe and checks real X window order
in a private server (requires `xorg-server-xvfb`, a C compiler, and X11 libraries).
The installer smoke test uses isolated temporary directories and stubbed privileged
commands; it is not a system installation.

`tests/xephyr-*.sh` covers fullscreen/pointer interactions, client death, restart,
shutdown, IPC edge cases, and monitor scenarios.
`tests/xephyr-suite.sh` is a separate manual integration harness with optional real
applications. These scripts are **not all isolated to the same standard**: some older
helpers in `tests/common.sh` kill processes by name or use fixed displays. Inspect
a script before running it, and run the legacy suite only in a disposable graphical
session, not alongside work you need to preserve.

The screenshot harness below is separate: it owns its server and clients and never
uses that global cleanup helper. Captures demonstrate selected states, not full
application compatibility.

## Development

The normal development loop is:

```bash
cargo fmt --all
cargo check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Use `cargo fmt --all -- --check` for a read-only formatting check. Existing
formatting drift should be handled independently rather than mixed into a
documentation or behavior change. Keep new layout/policy work covered by pure
state tests; use an isolated X server for protocol, stacking, and focus behavior.
When changing EWMH interop (struts, bypass hints, opacity), validate against a
real session and against a server with an external compositor running.

## Architecture

| Location | Responsibility |
| --- | --- |
| `src/main.rs` | CLI, configuration selection, signals, instance/control lifetime, backend startup |
| `maverick-core/` | Dependency-free domain types |
| `src/core/` | Engine, actions/commands/effects/events, layout, presentation, desired state |
| `src/backend/x11/` | Event handling, client management, input, EWMH, struts, reconciliation |
| `src/config.rs`, `src/userconfig.rs` | Compiled defaults, config merging, validation |
| `maverick-x11/` | Shared Xlib/XCB connection bootstrap |
| `maverick-sys/` | OS/FFI boundary, instance identity, control socket/hub |
| `maverick-toml/` | TOML-subset parser |
| `maverickctl/` | Control client binary: CLI, control-socket IPC client, instance discovery, session lifecycle |
| `tests/` | Real-X11 probes and integration scripts, installer smoke tests |
| `showcase/` | Isolated, reproducible technical presentation harness |

The domain crate is not the entire state machine: the executable's `src/core/`
contains much of that logic. Graphical sessions are not modelled there at all —
the X server, the process graph and the session record belong to
`maverickctl::session`. X11 access uses `x11rb` with an XCB FFI connection;
this is not a wholly pure-Rust protocol stack. The implementation has no GUI-toolkit
or async-runtime requirement, but still depends on native X11 libraries.

## Project Layout

```text
.
├── src/                 # Main window manager
├── maverick-core/       # Shared state and core types
├── maverick-x11/        # X11 integration
├── maverick-toml/       # TOML/config support
├── maverick-sys/        # IPC/control protocol and OS boundary
├── maverickctl/         # External control client and session lifecycle
├── config/              # Example configuration
├── docs/                # Documentation (see sessions.md) and assets
├── showcase/            # Reproducible technical presentation
└── tests/               # Integration and X11 tests
```

## Current status

Maverick is in **preview** and is not yet declared production-ready. It still
needs validation on the target machine and with the applications used in the
session. CI and the integration scripts are regression checks, not a
certification of broad application compatibility or long-running reliability.

- **Scope:** Linux/X11 only; no Wayland backend, compositor, animation,
  built-in desktop shell, blur, or shadows.
- **Layout:** Column is the only implemented layout. Workspace indices are limited
  to 1–9; names are cosmetic.
- **Compatibility:** ICCCM/EWMH support is implemented for the WM's needs, not a
  blanket claim of complete protocol or application compatibility.
- **Monitors:** focus/move cycles monitor enumeration order, not physical direction.
  Topology recovery uses rectangles/indices, not stable connector identities; do not
  assume arbitrary hotplug/reordering preserves assignments.
- **Geometry:** X11 has a global root coordinate space and protocol size/coordinate
  bounds. Scroll projection and multi-monitor workareas must respect those limits.
- **Interfaces:** configuration, internal APIs and presentation policy can
  change. The in-tree TOML parser supports a subset, not all of the TOML
  specification.

### Preview launch checklist

Before using Maverick as the only window manager for important work, validate a
disposable X11 session on the target machine. Confirm login and clean exit,
application launch and close, focus/input, fullscreen, floating/transient dialogs,
workspace switching, display sleep/wake, and monitor changes. Keep a way to return
to the previous session and preserve the user's work before testing shutdown.
Do not treat arbitrary monitor hotplug or unlisted Linux distributions as
supported release targets yet.

## Roadmap

Directions supported by the current code and test scaffolding, without promised
release dates:

- Extend real-client regression coverage for floating geometry, focus, fullscreen,
  restart, and monitor/workarea changes.
- Keep the EWMH interop (`_NET_WM_BYPASS_COMPOSITOR`, `_NET_WM_WINDOW_OPACITY`)
  honest against a real external compositor.
- Keep the workarea and struts honest against docks that reserve space the
  window manager does not own.

## Screenshots

The showcase is a six-scene technical presentation captured from an isolated
Xephyr session. These are authentic root-window captures: Maverick lays out
real X11 clients, the harness dispatches real actions, and the images are not
painted or reconstructed after capture.

### Workspace

![Maverick workspace](docs/screenshots/workspace.png)

A clean three-column start: a real terminal, Neovim reading
`src/core/layout.rs`, and Firefox on a local offline reference page. The first
composition is intentionally small enough to read at a glance.

### Ribbon and scrolling

![Maverick scrolling ribbon](docs/screenshots/ribbon.png)

The same workspace gains another source window, an offline reference viewer
and the live monitor. Maverick's directional focus moves the camera, leaving
the ribbon larger than the viewport. The browser and editor are real
application windows; the source views are real terminals running Neovim.

![Maverick real tools](docs/screenshots/tools.png)

A directional-focus step reveals a different view of the same real
application set. The scene is a navigation state, not a second desktop
mock-up.

### Legibility

![Maverick legibility adjustment](docs/screenshots/legibility.png)

The scene starts from a compact composition and then uses the real `Mod+Ctrl+H`
and `Mod+Ctrl+L` chords on the focused column. The harness checks that the
column narrows, that `Mod+Ctrl+L` restores a more comfortable width, that no
overlap appears, and that the result keeps the real content legible.

### Floating isolation

![Maverick floating monitor](docs/screenshots/floating.png)

`Maverick Monitor` is a small real terminal application that polls
`maverickctl query tree`. It is floated with Maverick's `toggle_float` action
and then compared before and after the tiled camera moves: the float remains
at the same screen-space geometry while the mosaic shifts beneath it.

### Hero composition

![Maverick hero composition](docs/screenshots/hero.png)

The final scene keeps several columns, varied widths, real clients, scrolling
and the independent monitor float in one deliberate composition.

### Reproducing the screenshots

The presentation lives in [`showcase/`](showcase/README.md) and uses a private
Xephyr display, private XDG directories, a private Firefox profile and bounded
process cleanup. It does not replace the host WM or modify the user's
configuration. The local browser fixture is opened through `file://` with
Firefox offline preferences; no network resource is required.

```bash
./showcase/run.sh
./showcase/run.sh floating
./showcase/run.sh --size 1440x900
```

The reference resolution is `1920x1080`; `--size` exists for a different
local development display. The harness verifies capture dimensions, records
JSON state/tree evidence under `/tmp/opencode/mav-showcase-evidence/`, and
reaps every owned process before removing its private runtime. See
[`showcase/README.md`](showcase/README.md) for dependencies, fallbacks and
known limitations.

## License

GPL-3.0. See [LICENSE](LICENSE).
