# Maverick

Maverick is an experimental X11 window manager for Linux, written in Rust.
It combines a horizontally scrolling ribbon of tiled columns with per-monitor
workspaces and independent floating windows. An optional OpenGL/GLX compositor
adds animated presentation without owning the window-management state.

[Overview](#overview) · [Design](#design) · [Installation](#installation) ·
[Configuration](#configuration) · [Testing](#testing) · [Screenshots](#screenshots)

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

- Plain X11 operation without the built-in compositor; geometry changes settle
  immediately, without spring animation.
- Optional OpenGL/GLX rendering: scroll/zoom animation, window opacity, rounded
  corners, image wallpaper, and GLSL wallpaper.
- Static image wallpaper through a root pixmap even without the compositor.
- In-place restart, adoption of existing windows with `--replace`, and isolated
  control sockets for multiple Maverick instances.
- Unit/regression tests in Rust and separate real-X11 integration harnesses.

Vulkan is an **unintegrated experimental bootstrap**, not an alternative working
WM compositor. See [Compositor](#compositor) and [Current status](#current-status).

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

State + Cfg -- layout Phase::Live --> optional OpenGL compositor
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

The layout has two phases. `Phase::Settled` uses camera targets for the geometry
sent to X11. `Phase::Live` uses interpolated camera values for compositor drawing.
The same projection math serves both: animations move textures rather than resize
X windows on every spring frame. Without the compositor, state changes go straight
to settled geometry.

The ribbon is a logical coordinate system, not another X11 screen. Its projection
includes the monitor's global origin and workarea; X11 still sees ordinary windows
in the root coordinate space. Floating windows are deliberately outside the ribbon
transform. This separation matters on multi-monitor desktops and when scrolling,
zooming, or restoring fullscreen geometry.

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

## Compositor

Window management does not require the built-in compositor. Disabling it is a
supported operating mode, useful for a simpler rendering path, nested-X testing,
or running an external X11 compositor. It does not disable tiling, floating,
workspaces, or fullscreen.

### Without the built-in compositor

```bash
MAVERICK_NO_COMPOSITOR=1 maverick
```

Alternatively, set `[compositor] enabled = false`, or build the WM with
`--no-default-features`. Geometry changes are immediate. Static wallpaper is
painted through the X11 root pixmap; GLSL wallpaper requires the GL path. Rounded
corners can use the X Shape path. An external compositor owns its own effects;
Maverick's GPU animations are not delegated to it.

### OpenGL

The default **Cargo build** includes `compositor-opengl`. The implementation uses
OpenGL 3.3, GLX texture-from-pixmap, and the WM's shared X11 connection. `libGL.so.1`
is loaded at runtime. Initialization can fall back to plain X11 if GL is unavailable,
context creation fails, or another compositor owns the screen selection.

Implemented effects are window opacity (`_NET_WM_WINDOW_OPACITY`, also settable
by rule), rounded corners, and wallpaper—not blur or shadows. Partial redraw needs
`GLX_EXT_buffer_age` and a usable back buffer; otherwise frames are fully redrawn.
Fullscreen bypass is conditional on the actual presentation and stacking state,
not guaranteed for every fullscreen client.

The installer treats this path as experimental and defaults to a non-composited
build. Driver and nested-server compatibility need testing; a configuration flag
alone is not evidence that the GL compositor successfully started.

### Vulkan

`maverick-vk` contains instance/device/surface/swapchain infrastructure and a
clear/present path. The root `compositor-vulkan` feature is a placeholder; it does
not wire that crate into the WM. Setting `backend = "vulkan"`, even with the feature,
does **not** provide a working Vulkan desktop compositor. Use OpenGL or plain X11.

## Installation

### Dependencies (Arch Linux)

Linux, an X11 server, a C linker, and Rust are required. Workspace manifests declare
edition 2021 and Rust **1.82** as the minimum; current CI uses stable Rust.

```bash
sudo pacman -S --needed base-devel rust libx11 libxcb
# For an X11 session started with startx:
sudo pacman -S --needed xorg-server xorg-xinit
# For the optional OpenGL path:
sudo pacman -S --needed mesa libxcomposite
```

The compiled launch bindings use `alacritty` and `rofi`; install those or override
the bindings. Compiled autostart launches `xdg-desktop-portal` and
`xdg-desktop-portal-gtk`; use an explicit autostart list to change or disable it.
These applications are not required by the layout engine. Non-PNG image wallpaper
may need `ffmpeg` or ImageMagick as a converter.

### Build

```bash
git clone https://github.com/Azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

For a WM build without GL, select the runtime packages explicitly:

```bash
cargo build --release --no-default-features \
  -p maverick -p maverick-sys
```

The normal runtime binaries are `maverick`, `maverickctl`, and
`maverick-msg`, under `target/release/`. Cargo also discovers the separate
`maverick-setup` utility in `src/bin/`; the shell installer installs the three
runtime binaries, not that utility. No Rust installer crate is part of the workspace.

### Install

Run the installer as your normal user, not from a root shell:

```bash
./install.sh --prefix "$HOME/.local" --yes --without-compositor
# Or system-wide (the script requests sudo for installation):
./install.sh --yes --without-compositor
# Explicitly opt into the experimental GL build:
./install.sh --prefix "$HOME/.local" --yes --with-compositor
```

The default prefix is `/usr/local`, including for a non-root caller. System installs
write the session entry to `/usr/share/xsessions`; a user prefix writes it under
`$prefix/share/xsessions`, which display managers may not discover. Ensure
`$HOME/.local/bin` is on `PATH` for a user install.

The installer builds release binaries, offers/seeds configuration, and retains an
existing config with `--yes`. Use `--no-config` to avoid config creation. Its first
build attempt uses `-C target-cpu=native`, so use a normal Cargo build when producing
artifacts for other machines. See `./install.sh --help` for the remaining options.

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
MAVERICK_NO_COMPOSITOR=1 ./target/debug/maverick --config /path/to/test.toml \
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

[compositor]
enabled = false
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
- `[animations] enabled = false` snaps camera/zoom transitions even when GL is active.
  `stiffness` and `damping` tune the spring.
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
| `Super+Shift+Q` | Quit with confirmation |

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

### Wallpaper and autostart

```toml
[wallpaper]
path = "~/Pictures/wallpaper.png"
mode = "fill"
```

Image modes are `fill`, `fit`, `stretch`, and `center`. PNG, PPM/PNM, QOI, basic BMP,
and farbfeld decoding are in-tree; other formats (or native decoding failures) use
external conversion. With GL active, `.glsl`/`.frag`
wallpapers can use `u_time`, `u_resolution`, and `u_delta_time`. Video wallpaper
has no implementation. The sample's older “requires compositor” image comment
does not apply to the current static root-pixmap path.

`[autostart] commands` is a list of argument lists, for example
`commands = [["polybar", "main"]]`. Supplying a non-empty command list replaces the
compiled one; there is no documented empty-list override, and an empty entry is
discarded with a warning. Use X11-compatible applications; a Wayland-only
panel is not made compatible by listing it here. Docks that publish struts reserve
workarea. Session startup and restart are not a general-purpose service supervisor.

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

`maverick-msg` also forwards action lines, for example `maverick-msg view 3` or
`maverick-msg wallpaper clear`. Each instance has a private runtime directory and
Unix socket under `$XDG_RUNTIME_DIR/maverick/<session-id>/`. Discovery checks
process identity and socket liveness. Selection prefers `--session`, then `--name`,
then inherited `MAVERICK_INSTANCE`, then display/TTY context; a global singleton
can also be selected. Use explicit targeting when testing alongside a live session.

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
action parsing, IPC/session discovery, image decoding, and renderer helpers.
They do not constitute a real-driver compositor or application compatibility test.
Some Vulkan integration tests require explicit opt-in and an X11/Vulkan environment.

[CI](.github/workflows/ci.yml) runs workspace tests, strict Clippy, and installer
Bash syntax checks. It does not run the Xephyr scenarios.

### Real X11 and installer tests

```bash
cargo build -p maverick -p maverick-sys
python3 tests/xvfb-stacking.py
python3 tests/install-smoke.py
```

The Xvfb stacking regression compiles an Xlib probe and checks real X window order
in a private server (requires `xorg-server-xvfb`, a C compiler, and X11 libraries).
The installer smoke test uses isolated temporary directories and stubbed privileged
commands; it is not a system installation.

`tests/xephyr-*.sh` covers fullscreen/pointer interactions, client death, restart,
shutdown, IPC edge cases, wallpaper, compositor damage, and monitor scenarios.
`tests/xephyr-suite.sh` is a separate manual integration harness with optional real
applications; it forces the built-in compositor off because of known nested-GLX
failures. These scripts are **not all isolated to the same standard**: some older
helpers in `tests/common.sh` kill processes by name or use fixed displays. Inspect
a script before running it, and run the legacy suite only in a disposable graphical
session, not alongside work you need to preserve.

The screenshot harness below is separate: it owns its server and clients and never
uses that global cleanup helper. Captures demonstrate selected states, not full
application compatibility or animation correctness.

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
When changing compositor code, validate on a real driver as well as any nested
server that supports the required GLX path.

## Architecture

| Location | Responsibility |
| --- | --- |
| `src/main.rs` | CLI, configuration selection, signals, instance/control lifetime, backend startup |
| `maverick-core/` | Dependency-free domain types and wallpaper source model |
| `src/core/` | Engine, actions/commands/effects/events, layout, presentation, desired state, session recovery |
| `src/backend/x11/` | Event handling, client management, input, EWMH, struts, reconciliation, frame scheduling, root wallpaper |
| `src/config.rs`, `src/userconfig.rs` | Compiled defaults, config merging, validation |
| `maverick-x11/` | Shared Xlib/XCB connection bootstrap |
| `maverick-sys/` | Instance identity/discovery, control socket/hub, `maverickctl` and `maverick-msg` |
| `maverick-render/` | Renderer-facing abstraction |
| `maverick-gl/` | OpenGL/GLX renderer and in-tree FFI/loading |
| `maverick-vk/` | Experimental Vulkan device/surface/swapchain code, not integrated into the WM |
| `maverick-toml/`, `maverick-img/` | TOML-subset parser and PNG decoder/external image conversion |
| `tests/` | Real-X11 probes and integration scripts, installer smoke tests |
| `scripts/showcase/` | Isolated documentation capture harness |

The domain crate is not the entire state machine: the executable's `src/core/`
contains much of that logic. X11 access uses `x11rb` with an XCB FFI connection;
this is not a wholly pure-Rust protocol stack. The implementation has no GUI-toolkit
or async-runtime requirement, but still depends on native X11 libraries.

## Current status

Maverick is under active development and is not declared production-ready. The
plain X11 path implements the current tiling, navigation, floating, fullscreen,
and workspace model; the regression suite exists to harden those interactions.

- **Experimental rendering:** OpenGL is implemented but remains optional and
  driver-sensitive. Vulkan is not connected to window compositing.
- **Scope:** Linux/X11 only; no Wayland backend, built-in desktop shell, blur,
  shadows, or video wallpaper.
- **Layout:** Column is the only implemented layout. Workspace indices are limited
  to 1–9; names are cosmetic.
- **Compatibility:** ICCCM/EWMH support is implemented for the WM's needs, not a
  blanket claim of complete protocol or application compatibility.
- **Monitors:** focus/move cycles monitor enumeration order, not physical direction.
  Topology recovery uses rectangles/indices, not stable connector identities; do not
  assume arbitrary hotplug/reordering preserves assignments.
- **Geometry:** X11 has a global root coordinate space and protocol size/coordinate
  bounds. Scroll projection and multi-monitor workareas must respect those limits.
- **Interfaces:** configuration, internal APIs, presentation policy, and experimental
  renderer behavior can change. The in-tree TOML parser supports a subset, not all
  of the TOML specification.

## Roadmap

Directions supported by the current code and test scaffolding, without promised
release dates:

- Extend real-client regression coverage for floating geometry, focus, fullscreen,
  restart, and monitor/workarea changes.
- Harden OpenGL startup, damage handling, fullscreen bypass, and driver coverage.
- Evaluate integrating the Vulkan bootstrap with real window textures and the
  renderer contract before calling it a supported backend.
- Keep image/shader wallpaper reliable; video remains reserved until a decoder
  and resource-lifecycle design exist.

## Screenshots

All images below were captured from real Maverick sessions: the WM running
inside Xephyr at 2880×1800, managing real applications, captured with
`import`, downsampled with Lanczos to 1440×900 (`sRGB`, stripped metadata),
and wrapped in a presentation frame (1616×1076 final). Every pixel inside the
frame is authentic application rendering through Maverick's layout. No
mock-ups, no terminal imitations of apps, no post-processing effects.

### Everyday desktop

![Maverick everyday desktop](docs/screenshots/real-desktop.png)

A developer's workspace: Neovim in Alacritty on real source
(`src/core/layout.rs`), Firefox on a local offline documentation page
(`file://`, no network), Zed on the repository, and an Alacritty shell with
`git status`. Four Maverick columns; the ribbon already extends past the
viewport, with Neovim peeking in at the left edge.

### Scrolling

![Maverick scrolling, viewport A](docs/screenshots/real-scroll-a.png)

![Maverick scrolling, viewport B](docs/screenshots/real-scroll-b.png)

Six columns (the desktop above plus a file manager and a system monitor);
two viewports of the same desktop reached through Maverick's own directional
focus: viewport A sits on Zed (`scroll=2764`), viewport B on the file manager
(`scroll=4454`). The workspace is larger than the visible mosaic, and the
blue border always marks the focused column. A
[focus walk](docs/screenshots/real-focus.png) ending on Firefox is also
captured.

### Floating windows

![Maverick floating window](docs/screenshots/real-floating.png)

A real Alacritty scratch shell floated by the WM (`960×540` at `960,540`)
stays put while the tiles scroll two columns beneath it: floats keep
screen-space geometry outside the ribbon transform.

### Compositor

![Maverick compositor](docs/screenshots/real-compositor.png)

The same desktop and float with the real OpenGL compositor ON (`0.78`
opacity, rounded corners; Mesa software renderer under Xephyr): Firefox and
Zed content shows through the float. Compare with the floating shot above,
which is the same state with the compositor OFF.

### Technical (synthetic clients)

The `xterm` source-viewer scenes remain for implementation detail
(`tiling`, `navigation`, `floating`, `fullscreen`, `compositor`, plus the
rounded/fullscreen regression set under `docs/screenshots/`). They no longer
carry the burden of being the primary representation of Maverick.

### Reproducing the screenshots

The showcase runs Maverick inside Xephyr with controlled clients, private
configuration/runtime directories (including isolated Firefox profiles and
Zed data directories; the user's `~/.config` is never touched), and bounded
process cleanup. It does not replace the WM on the parent display or modify
the user's configuration. Everything works offline: Firefox opens a local
`file://` page and no scene touches the network.

```bash
./scripts/showcase/run.sh real-desktop
./scripts/showcase/run.sh real-scroll-a
./scripts/showcase/run.sh real-scroll-b
./scripts/showcase/run.sh real-focus
./scripts/showcase/run.sh real-floating
./scripts/showcase/run.sh real-compositor
./scripts/showcase/run.sh tiling
./scripts/showcase/run.sh all
```

Each run renders Maverick inside Xephyr at 2880×1800, captures the root
window, downsamples with Lanczos to 1440×900 (`sRGB`, stripped metadata),
and wraps the authentic pixels in a presentation frame (1616×1076 final).
It verifies every stage's dimensions, checks that the GL scenes really
initialized (`Backend: OpenGL/GLX` plus a submitted frame in the WM log; a
plain-X11 fallback fails the scene instead of producing a misleading capture),
reaps every process it created, and removes its temporary state. Dependencies:
`Xephyr`, `xdpyinfo`, `xdotool`, `xsetroot`, ImageMagick's `import`/
`identify`/`magick`, a reachable host X11 display, and — for the real
desktop scenes only — `alacritty`, `firefox`, `zeditor`, `nvim`
(`nautilus` and `htop` for the scrolling pair). GL clients are forced onto
X11 inside the harness, so a Wayland host session does not break them.

## License

GPL-3.0. See [LICENSE](LICENSE).
