# Maverick

Maverick is a tiling window manager for the X Window System, written in Rust. It
combines a scrollable, column-based tiling layout with a built-in OpenGL/GLX
compositor, and is designed to keep its logical window state and the real X11
state in agreement through an explicit, reconciled state pipeline.

> Note: Maverick targets X11 only. There is no Wayland backend.

## Overview

Maverick manages windows on an X11 display. Its core is a pure-Rust state model
that decides where every window should be; a backend layer translates those
decisions into X11 protocol calls and into compositor draw commands. The window
manager and its compositor share a single X11 connection.

Design priorities:

- Minimal external dependencies. The window manager core uses `x11rb` (pure
  Rust) and links no windowing toolkit. Configuration parsing, PNG decoding, and
  the TOML parser are implemented in-tree with no third-party crates. GL/GLX
  access is provided by a hand-written FFI layer.
- A single dispatch path from input and IPC to state change
  (`Engine::dispatch(Action) -> Vec<Effect>`), so behaviour is identical whether
  a change originates from a keypress or a control command.
- Explicit ownership of applied geometry, with invariants checked at runtime, to
  avoid divergence between the logical layout and the actual X11 window
  positions.
- A built-in compositor, so no external compositor is required
  for translucency, rounded corners, or a drawn wallpaper.

## Features

### Window Management

- **Tiling** of managed windows across a scrollable column layout and a grid
  layout.
- **Floating** windows, clamped to the monitor workarea, with drag-to-move and
  drag-to-resize via the pointer.
- **Fullscreen and Maximize** implemented as distinct presentation modes:
  - *Maximize* fills the workarea (screen minus reserved regions), keeps a zero
    border, is per-axis (horizontal and vertical independently), and is applied
    only while the window is focused. On focus loss it returns to its tiling
    slot.
  - *Fullscreen* covers the whole screen (or, under the Column layout's "Normal"
    policy, participates in the scrolling ribbon), independent of focus.
- **Focus** model with directional navigation, optional focus-follows-mouse
  (`focus_mouse`), and optional cursor warping (`warp_cursor`).
- **Workspaces** are tracked per monitor (each monitor has its own active
  workspace), numbered and optionally named. Default count is 9.
- **Multi-monitor** support through RandR. The workarea of each monitor accounts
  for reserved space published by docks and bars via EWMH struts.
- **Window rules** matched by `WM_CLASS` (class/instance), window type, and title
  (case-insensitive substring), supporting floating, sticky, workspace
  assignment, opacity, border, and fullscreen-policy overrides.
- **EWMH/ICCCM** compliance, including `_NET_WM_STATE`, `_NET_WM_DESKTOP`,
  `_NET_ACTIVE_WINDOW`, and window-type handling.
- **`override_redirect` isolation**: external bars, menus, and overlays are not
  managed as tiled windows, and their `ConfigureRequest`s pass through directly.
- **Client lifecycle**: managed windows are discovered, tracked, and cleaned up on
  unmap/destroy; a self-unmap guard prevents the WM from unmanaging its own
  redirected surfaces.
- **Resize semantics**: tiled-window geometry is owned by the WM. A tiled
  window's own `ConfigureRequest` is ignored (the WM echoes its own rect back),
  while floating windows honor `ConfigureRequest` and are clamped to the
  workarea. Tiled resizing is done by changing the focused column's width
  (`GrowCol`/`ShrinkCol`).

### Layouts

Maverick ships two layout modes, switchable at runtime and tracked **per
workspace**.

- **Column** (default): a horizontally scrollable ribbon of columns. Each column has a `weight` expressed as a
  fraction of the workarea width; adding, growing, or removing a column does not
  resize its neighbours. A spring-driven camera scrolls the ribbon to keep the
  focused column centred. An optional *accordion* factor can expand the focused
  column. Rows within a column are uniform height and do not reflow on focus
  change.
- **Grid**: a deterministic engine that partitions the window set into balanced
  rows (longer rows on top) and assigns windows stably across inserts and
  removals.

Cycle between modes with the layout-cycle binding; set a specific mode with the
column/grid bindings. Floating windows are preserved and clamped in both
layouts.

### Compositor

Maverick brings up its own OpenGL/GLX compositor when the display supports it.
The compositor is enabled by default and falls back automatically to the plain
X11 (`ConfigureWindow`) path when OpenGL is unavailable, another compositor
already owns the screen, or context creation fails. It can also be disabled
explicitly (see Configuration).

Architecturally, the compositor:

- Uses **XComposite** manual redirection (`RedirectManual`) on the root's
  subwindows and draws into the **composite overlay window**
  (`CompositeGetOverlayWindow`), whose input region is emptied so input passes
  through.
- Claims the **compositor selection** (`_NET_WM_CM_S0`); it refuses to start if
  another compositor already owns it.
- Creates a **GLX** context at **OpenGL 3.3 core profile** and requires
  `GLX_EXT_texture_from_pixmap`, `GLX_ARB_create_context`, and
  `GLX_ARB_create_context_profile`.
- Imports window contents via **texture-from-pixmap** (`glXBindTexImageEXT`),
  which is a driver-side rebind with **no CPU copy** of pixel data; there is no
  readback/copy fallback path.
- Tracks damage via **XDamage** (`ReportLevel::NON_EMPTY`) and accumulates a
  `DirtyReason` bitmask (damage, geometry, surface, focus, wallpaper) describing
  why a frame is dirty.
- Maintains a **scene** of one texture per client plus a z-order stack, rebuilt
  from the X window stack on restack/configure.
- Performs **occlusion-aware damage**: windows fully covered by a single opaque,
  square-cornered, on-screen window above them are skipped in the draw pass.
- Performs **viewport culling**: off-screen windows (with a small margin) are
  skipped.
- Performs **partial redraw only when possible**: a partial redraw limited to the
  accumulated damage region is used when the `GLX_EXT_buffer_age` extension is
  available *and* the back buffer still holds the previously presented frame
  (observed age 1). Otherwise the compositor performs a **full redraw**. Do not
  assume partial redraw is always available.
- Schedules frames with a **frame scheduler** that decides whether a frame is
  needed and when, and presents with **vsync** through swap-interval 1
  (`GLX_EXT_swap_control` / `GLX_MESA_swap_control` / `GLX_SGI_swap_control`).
  `GLX_SGI_video_sync` is used only for instrumentation, not for pacing.

Effects currently implemented are **per-window opacity** (via
`_NET_WM_WINDOW_OPACITY`), **rounded-corner SDF** rendering, and the drawn
wallpaper. There is **no blur or shadow** effect.

### Animation and Presentation

Presentation is driven by a **spring-based camera** (configurable stiffness and
damping). The same spring model drives the column scroll, the Overview
film-strip zoom, the viewport zoom, and page-snap scrolling. Animation is
advanced by the **frame delta** and **substepped** internally so the integrator
stays stable; the compositor redraws at the current spring value each frame.
Presentation timing is governed by `glXSwapBuffers` with swap interval 1.
Maverick does not claim independence from the display refresh rate beyond
vsync-aligned presentation.

The **wallpaper** is drawn by the compositor and can be:

- a still image (PNG decoded natively by `maverick-img`, with other formats
  delegated to an external converter), or
- a live **GLSL fragment shader** receiving `u_time`, `u_resolution`, and
  `u_delta_time`.

A `Video` wallpaper source is **reserved but not implemented** (it is ignored
with a warning).

### IPC and Session Management

Maverick exposes a control plane over a Unix socket. Two thin clients share one
engine:

- **`maverickctl`** — the general-purpose administration and query tool.
- **`maverick-msg`** — a verbatim command forwarder: any line it
  receives is forwarded to the control socket's `dispatch` action.

Both talk to a per-instance control server implementing the protocol commands:
`ping`, `identify`, `state`, `dispatch`, `quit`, `restart`, `reload`,
`subscribe`, and `query`.

Each running instance has a **random per-session identity** (derived from PID,
nanosecond time, and 8 bytes from `/dev/urandom` — not from PID alone) and an
**isolated runtime directory** under `$XDG_RUNTIME_DIR/maverick/<session-id>/`
(mode `0700`), with the socket at `control.sock`. An identity record (a JSON
file) is written as `<session-id>.json`. Instance **discovery**
(`maverickctl list`) scans these directories, enriches each record with live
`/proc` data, and computes liveness by requiring both a responding socket and a
matching process start time — which protects against PID recycling. A real login
session and a Xephyr test instance therefore never share or fight over a socket.

### Reliability

- **Hard restart**: `restart` re-executes the WM binary in place with the exact
  launch arguments (including `--config`/`--name`/`--replace`) it was started
  with, rebuilding all state from scratch.
- **Deterministic shutdown**: quit runs with a bounded timeout (3 seconds). When
  the deadline is reached, any remaining clients are force-killed; shutdown does
  not depend on client cooperation to finish.
- **Cooperative close**: before forcing, the WM asks clients that support
  `WM_DELETE_WINDOW` to close.
- **Compositor fallback**: if GL is unavailable, already owned, or fails at init,
  Maverick stays on the non-composited X11 path.
- **Client death handling**: `DestroyNotify`/`UnmapNotify` remove windows and
  drop their compositor textures; a self-unmap guard avoids unmanaging the WM's
  own surfaces.
- **Stale state cleanup**: discovery prunes dead instances; the session snapshot
  drops references to windows that are no longer alive; the control server safely
  removes stale socket files on startup.

Maverick does not claim to be crash-proof; these measures reduce the blast
radius of failures and speed recovery.

## Architecture

Maverick separates a pure-logic core from the X11 backend. The flow of a state
change is:

```text
        User action / IPC command
                  │
                  ▼
        Engine::dispatch(Action)  ──►  Vec<Effect>
                  │
                  ▼
        State  ──►  layout::arrange  ──►  present::present_into
                  │
                  ▼
        DesiredState  (explicit hand-off)
                  │
                  ▼
        Reconciler  ──►  AppliedState  ──►  X11 protocol calls
                  │
                  ▼
        Compositor / presentation (OpenGL)
```

- `Engine::dispatch(Action) -> Vec<Effect>` is the **only** path from a keybind
  or IPC command to state mutation. `Effect` is the vocabulary (e.g.
  `ArrangeMonitor`, `FocusWindow`, `SetFullscreen`) that the backend turns into
  X11 calls. A future non-X11 backend would implement the same effect execution
  without changing the core.
- The **Reconciler** (`backend/x11/reconciler.rs`) is the single owner of "what
  is actually on X11". It decides whether a `ConfigureWindow` (or restack/focus)
  is actually needed, centralizing applied-state change detection.
- Maverick keeps **explicit state ownership and invariants**:
  `State::check_invariants()` verifies internal consistency, and runtime geometry
  is always reconstructed against the windows that are actually alive rather than
  trusted from disk.
- The window manager and the GLX compositor share **one X11 connection** (the
  `x11rb` XCB connection is passed to the hand-written GL FFI), avoiding a second
  display connection.
- The core (`src/core/`) contains no X11 code; all protocol interaction lives in
  `src/backend/x11/`.

## Requirements

- A Linux (or other X11-compatible) system with an **X server** (such as X.Org). Maverick is an X11 window manager, not a standalone display server.
- A **Rust toolchain**, MSRV **1.82** (`rust-version` in `Cargo.toml`), and
  Cargo.
- The system **X11 and X11-xcb client libraries** are linked by the GLX FFI
  (`maverick-gl`); a C toolchain (linker) is therefore required to build the
  workspace.
- **OpenGL 3.3** capable GPU and driver for the compositor. `libGL.so.1` is
  loaded at runtime via `dlopen` and is **optional**: if absent, the compositor
  is disabled and Maverick runs on the plain X11 path.
- No `build.rs`, no `pkg-config`; all Rust dependencies are either pure-Rust
  (`x11rb`) or implemented in-tree.

## Building

Maverick is a Cargo workspace. The root `Cargo.toml` is itself the `maverick`
package; `--workspace` is required to also build the library and binary crates
(`maverick-sys`, `maverick-gl`, `maverick-dialog`, `maverick-toml`,
`maverick-img`, `maverick-installer`).

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

Built binaries are placed in `target/release/`:

- `maverick` — the window manager.
- `maverickctl` — the control/query CLI.
- `maverick-msg` — the verbatim command forwarder.
- `maverick-dialog` — the quit-confirmation dialog.
- `maverick-installer` — the installer (see Development).

## Running

Add the binaries to your `PATH`, or invoke them by absolute path.

From `~/.xinitrc`:

```bash
exec maverick
```

For a display manager, install an X session file at
`/usr/share/xsessions/maverick.desktop` (the `maverick-installer` can create one
for you):

```ini
[Desktop Entry]
Name=maverick
Comment=Tiling window manager with built-in compositor
Exec=maverick
Type=XSession
```

Maverick accepts the following command-line options (all optional, in any order):

| Option | Description |
| --- | --- |
| `--config <path>` | Load the config TOML from `<path>` instead of the default location. The same path is reused on `maverickctl reload` and on restart, so a custom config survives both. |
| `--check-config [path]` | Parse the config (the `--config` path if given, otherwise the default) and exit. Exit code `0` means clean (no warnings or errors); `1` means warnings or errors were reported. The WM is never started. |
| `--replace` / `-r` | Replace an already-running window manager, adopting its windows. |
| `--name <id>` | Instance name used for control and discovery (so `maverickctl` can target the right instance). |
| `-v` / `--version` | Print version and exit. |
| `-h` / `--help` | Print usage and exit. |

Validate a config before starting:

```bash
maverick --check-config ~/.config/maverick/config.toml
maverick --config ~/.config/maverick/config.toml
```

## Configuration

Configuration is optional. If no config file is found, Maverick runs on
compiled-in defaults.

- **Default location**: `$XDG_CONFIG_HOME/maverick/config.toml`, or
  `$HOME/.config/maverick/config.toml` when `XDG_CONFIG_HOME` is unset.
- **Format**: TOML. Sections: `[general]`, `[colors]`, `[autostart]`,
  `[wallpaper]`, and array sections `[[keybindings]]` and `[[rules]]`.
- **Fail-safe loading**: a file with invalid TOML syntax is rejected whole and
  the compiled defaults are used; a single bad entry (unknown key, wrong type,
  bad action string) is dropped with a warning and the rest of the file still
  loads. Maverick never fails to start because of a bad config.

A full commented sample is at `config/config.toml`. Copy it as a starting point:

```bash
mkdir -p ~/.config/maverick
cp config/config.toml ~/.config/maverick/config.toml
```

Apply changes without restarting:

```bash
maverickctl reload
```

### General options

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `border_width` | u32 | `2` | Border width in pixels (`border_w` is accepted as an alias). |
| `gaps_inner` | u32 | `6` | Gap between tiled windows. |
| `gaps_outer` | u32 | `6` | Gap at the screen edges. (`gaps` sets both.) |
| `smart_gaps` | bool | `false` | Collapse gaps to 0 when a workspace has a single tiled window. |
| `corner_radius` | u32 | `0` | Rounded corner radius via X11 Shape. |
| `n_tags` | usize | `9` | Number of workspaces (clamped to 1–9). |
| `column_width` | f32 | `0.6` | Width of a new column as a fraction (0.1–1.0) of the workarea. |
| `accordion_boost` | f32 | `0.0` | Extra fraction (0.0–0.9) the focused column expands. |
| `overview_zoom_min` | f32 | `0.25` | Minimum Overview film-strip zoom (0.05–1.0). |
| `focus_mouse` | bool | `false` | Focus a window when the pointer enters it. |
| `warp_cursor` | bool | `false` | Warp the cursor to the centre of the focused window. |
| `auto_workspace_binds` | bool | `true` | Auto-generate `Super+1..9` / `Super+Shift+1..9` workspace binds. |
| `theme` | string | — | Named colour preset (defaults to Catppuccin Mocha when unset). |
| `tag_names` | list | `["1".."9"]` | Cosmetic workspace names (addressed by index). |
| `compositor_enabled` | bool | `true` | Master switch for the compositor. |
| `camera_stiffness` | f32 | `220.0` | Scroll-camera spring stiffness. |
| `camera_damping` | f32 | `30.0` | Scroll-camera spring damping. |

> The compositor is configured under `[general]` via `compositor_enabled` (not a
> separate `[compositor]` table). It can also be disabled for a single run with
> the `MAVERICK_NO_COMPOSITOR` environment variable.

### Colours

All colours are 24-bit hex `0xRRGGBB`. Keys `normal`/`focused`/`urgent` are
accepted alongside the older `col_normal`/`col_focused`/`col_urgent` aliases.

| Key | Default | Meaning |
| --- | --- | --- |
| `normal` | `0x45475a` | Unfocused window border. |
| `focused` | `0x89b4fa` | Focused window border. |
| `urgent` | `0xf38ba8` | Urgent window border. |

### Autostart

Programs to launch once the WM is ready, as a list of argument lists:

```toml
[autostart]
commands = [["nm-applet"]]
```

The compositor and wallpaper are built in and are not autostart entries; bars and
portals are launched here like any other program. Maverick ships no status bar — use any external status bar; it reserves screen space for any dock that
publishes `_NET_WM_STRUT_PARTIAL`/`_NET_WM_STRUT`, so tiled windows never overlap
it. For status text, Maverick exposes the root window name through
`maverickctl state` / `maverickctl subscribe`.

### Wallpaper

```toml
[wallpaper]
path = "~/Pictures/wallpaper.png"   # image, or a .glsl/.frag shader; null disables
mode = "fill"                       # fill | fit | stretch | center
```

- **Images** (PNG decoded natively; other formats via an external converter).
- **GLSL shaders** compiled on the GPU, redrawn every frame with `u_time`,
  `u_resolution`, `u_delta_time`.
- A `Video` source is reserved but not implemented.

### Window rules

Rules match by `WM_CLASS` (class/instance), window type, and title
(case-insensitive substring) and can force floating, sticky, workspace
assignment, opacity, border width, and fullscreen policy.

```toml
[[rules]]
class = "mpv"
float = true
```

| Field | Type | Description |
| --- | --- | --- |
| `class` | string | Match `WM_CLASS` (case-insensitive substring). |
| `instance` | string | Match the `WM_CLASS` instance. |
| `window_type` / `type` | string | Match EWMH window type (e.g. `dialog`). |
| `title` | string | Match window title (case-insensitive substring). |
| `float` | bool | Force floating. |
| `sticky` | bool | Keep visible across workspaces. |
| `workspace` / `ws` | int | Send to a 0-based workspace. |
| `size` | [w, h] | Force size for floats. |
| `position` | [x, y] | Force position for floats. |
| `opacity` | float | Per-window opacity. |
| `border_width` | int | Override border width. |
| `ignore_initial_state` | bool | Ignore the client's initial maximized/fullscreen request. |
| `deny_fullscreen` | bool | Refuse client-requested fullscreen (user toggles still work). |
| `true_fullscreen` | bool | Treat as an exclusive, screen-covering fullscreen. |

## Keybindings

`Super` is the Windows/Mod4 key. All bindings below are the compiled defaults
and are fully overridable in `config.toml`.

### Spawn

| Binding | Action |
| --- | --- |
| `Super+Return` | Terminal (`alacritty`) |
| `Super+P` | App launcher (`rofi -show drun`) |
| `Super+Shift+P` | Command runner (`rofi -show run`) |

### Window operations

| Binding | Action |
| --- | --- |
| `Super+Shift+C` | Kill focused window |
| `Super+Shift+Space` | Toggle floating |
| `Super+Shift+F` | Toggle fullscreen |
| `Super+Shift+M` | Toggle maximize |
| `Super+Shift+Q` | Quit (runs `maverickctl quit --confirm`) |

### Focus and movement

| Binding | Action |
| --- | --- |
| `Super+H` / `L` / `J` / `K` | Focus column left/right, window down/up |
| `Super+Shift+H` / `L` / `J` / `K` | Move window left/right, down/up |
| `Super+Tab` | Focus next monitor |
| `Super+Shift+Tab` | Move window to next monitor |

### Columns

| Binding | Action |
| --- | --- |
| `Super+Shift+Return` | Move window to a new column |
| `Super+Ctrl+H` | Shrink focused column (−50 px) |
| `Super+Ctrl+L` | Grow focused column (+50 px) |
| `Super+Ctrl+J` | Collapse column into the one to its left |

### Layout and overview

| Binding | Action |
| --- | --- |
| `Super+Space` | Cycle layout modes |
| `Super+T` | Column layout |
| `Super+G` | Grid layout |
| `Super+O` | Toggle Overview |
| `Super+N` | Overview navigate right |
| `Super+Shift+O` | Overview navigate left |
| `Super+E` | Enter focused window in Overview |
| `Super+=` / `Super+-` | Zoom viewport in / out |
| `Super+]` / `Super+[` | Page-snap scroll right / left |
| `Super+Shift+R` / `Super+F5` | Restart in place |

### Workspaces

| Binding | Action |
| --- | --- |
| `Super+1` … `Super+9` | Switch to workspace 1–9 |
| `Super+Shift+1` … `Super+Shift+9` | Move focused window to workspace 1–9 |

### Mouse (floating windows)

| Action | Result |
| --- | --- |
| `Super+Left-drag` | Move floating window |
| `Super+Right-drag` | Resize floating window |
| `Super+wheel` | Scroll the column ribbon |
| Drop floating on a tiled window | Re-insert it into the tiling tree |

## maverickctl

`maverickctl` is the primary control client. It connects to the running instance
selected by `--session <id>`, `--name <id>`, the `MAVERICK_INSTANCE`
environment variable, or the current `DISPLAY`/`TTY` context.

| Subcommand | Description |
| --- | --- |
| `list` / `ls` | List discovered instances with identity and liveness. |
| `state` | Print the full window/state snapshot (JSON by default). |
| `query` / `q` | Print a condensed snapshot. |
| `msg` / `dispatch` / `command` | Send a control action (e.g. `msg wallpaper set …`). |
| `subscribe` / `sub` | Stream state-change events. |
| `quit` | Quit the instance. `--confirm` requires confirmation (the prompt uses `maverick-dialog` when available, falling back to `zenity`/`kdialog`/a TTY prompt); `--yes`/`-y` skips confirmation. |
| `quit-all` | Quit all discovered instances. |
| `restart` | Restart the instance in place. |
| `reload` | Re-read the config file live. |
| `prune` | Remove stale instance records. |
| `help` / `-h` / `--help` | Print usage. |

`state`/`query` also accept `-j`/`--json` and `-b`/`--bare` as pass-through
flags.

Examples:

```bash
maverickctl list
maverickctl state
maverickctl msg wallpaper set ~/Pictures/wallpaper.png
maverickctl msg wallpaper mode fit
maverickctl msg wallpaper clear
maverickctl subscribe
maverickctl quit --confirm
maverickctl restart
maverickctl reload
maverickctl prune
```

`maverick-msg` is a thin verbatim forwarder over the same engine. The examples
above also work as:

```bash
maverick-msg wallpaper set ~/Pictures/wallpaper.png
maverick-msg wallpaper mode fit
maverick-msg wallpaper clear
```

## Testing

Unit tests live in the crates (`src/core/tests.rs`, `src/backend/x11/tests.rs`):

```bash
cargo test --workspace
```

Format and lint:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

> Clippy is not entirely clean: two deliberate `clippy::question_mark`
> warnings remain in `maverick-sys/src/control.rs` (documented in
> `CHANGELOG.md`). They are intentionally kept.

An integration harness under `tests/` drives Maverick inside a nested Xephyr
server with real X clients and asserts live X properties. It is a manual/CI
harness and is not part of `cargo test`. Requirements: `Xephyr`, `x11-utils`,
`xdotool`, `xterm` (and optionally `firefox`/`mpv`). Run it with:

```bash
DISPLAY=:1 ./tests/xephyr-suite.sh
```

The harness sets `MAVERICK_NO_COMPOSITOR=1` because the GLX compositor cannot
initialize under a nested Xephyr display; this is a limitation of the test
environment, not of the compositor on a real X server.

## Project Status

Maverick is under active development. The X11 backend and the integrated OpenGL
compositor are implemented and are the project's primary, daily-driven
configuration.

- **Backend**: X11 (RandR, EWMH/ICCCM). No Wayland backend.
- **Compositor**: implemented (OpenGL 3.3 / GLX), enabled by default with
  automatic, safe fallback.
- **Layouts**: Column and Grid, both implemented.
- **IPC**: implemented (`maverickctl` / `maverick-msg`, per-session socket).
- **Session management**: implemented (per-instance identity, discovery,
  isolation, restart/reload).

The project is not declared production-ready; see Known Limitations.

## Known Limitations

- **X11 only.** There is no Wayland backend and Maverick requires a running X
  server.
- **Partial redraw is conditional.** It is only used when `GLX_EXT_buffer_age`
  is present and the back buffer still holds the previous frame; otherwise a full
  redraw is performed.
- **Compositor is optional by design.** When disabled (or when GL is
  unavailable), Maverick runs without compositing; rounded corners then fall
  back to X11 Shape and translucency is not composited.
- **`Video` wallpaper is reserved but not implemented.** Only image and GLSL-
  shader wallpapers work.
- **No blur or shadow effects.** Only per-window opacity and rounded-corner
  rendering are provided.
- **Tiled windows do not honor client `ConfigureRequest` resizes.** Tiled
  geometry is owned by the WM; resizing is done via column-width adjustment or by
  floating the window.
- **Workspace names are cosmetic.** Workspaces are addressed by index.
- `n_tags` is clamped to the range 1–9.

## Development

The workspace is laid out as:

```text
Maverick/
├── src/                     entry point, configuration, and core logic
│   ├── main.rs              signals, autostart, and control wiring
│   ├── config.rs            compiled default configuration
│   ├── userconfig.rs        optional TOML config parsing and merge
│   ├── types.rs             core data model (State, Monitor, Workspace, Column, Client)
│   ├── core/                pure logic layer — no X11
│   │   ├── engine.rs         Engine::dispatch(Action) -> Vec<Effect>
│   │   ├── layout.rs         column and grid layout engines
│   │   ├── present.rs        fullscreen / maximize presentation
│   │   ├── desired.rs        DesiredState hand-off to the Reconciler
│   │   └── session.rs        session persistence and recovery
│   └── backend/
│       └── x11/             the only X11-speaking code
│           ├── mod.rs         WindowManager, event loop, RandR
│           ├── reconciler.rs  single owner of applied geometry
│           ├── compositor.rs  GL damage tracking and draw
│           └── framesched.rs  frame scheduler
├── maverick-sys/            IPC, per-session identity, control server, discovery
├── maverick-gl/             hand-written GLX / OpenGL FFI
├── maverick-img/            dependency-free PNG decode
├── maverick-toml/           zero-dependency TOML parser
├── maverick-dialog/         standalone quit-confirmation window
├── maverick-installer/      installer (workspace member)
├── config/
│   └── config.toml          full, commented sample configuration
├── tests/                   Xephyr integration harness and C test clients
├── CHANGELOG.md
├── Cargo.toml              workspace root and the `maverick` package
├── Cargo.lock
├── LICENSE
└── README.md
```

All crates use **edition 2021** and the workspace MSRV is **1.82**.

**Building and testing** (see above): `cargo build --release --workspace`,
`cargo test --workspace`, `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets`.

**Installing**: the `maverick-installer` binary is the project's install
mechanism. It compiles the workspace, installs `maverick`, `maverickctl`,
`maverick-msg`, and `maverick-dialog` to `/usr/local/bin` (or `~/.local/bin` as
a non-root user), and installs an X session desktop file. There are no
distribution packages (no Arch `PKGBUILD` or Debian `.deb`); run it with:

```bash
cargo build --release --workspace
./target/release/maverick-installer
```

## License

Maverick is distributed under the GNU General Public License version 3
(GPL-3.0). See `LICENSE`.
