# 🦅 Maverick

<p align="center">
  <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white">
  <img src="https://img.shields.io/badge/Linux-111111?style=for-the-badge&logo=linux&logoColor=white">
  <img src="https://img.shields.io/badge/XLibre-222222?style=for-the-badge&logo=x.org&logoColor=white">
  <img src="https://img.shields.io/badge/x11rb_0.13-444444?style=for-the-badge">
</p>

<p align="center">
  <a href="README.es.md">
    <img src="https://img.shields.io/badge/Language-Español-blue?style=for-the-badge&logo=translate&logoColor=white">
  </a>
</p>

<p align="center">
  <b>Columnar tiling window manager with niri-style scrollable layout, written in Rust</b>
</p>

<p align="center">
  🦅 columnar • 🦀 rust • 🖥 xlibre • 🧩 tiling • 🌙 minimal
</p>

---

## ✨ About

**maverick** is a lightweight, columnar tiling window manager written in Rust. It features a scrollable column layout inspired by [niri](https://github.com/YaLTeR/niri), and is built directly on top of `x11rb 0.13` to minimize dependencies and bloat.

### Key Features
- 🦅 Horizontally scrollable column-based layout.
- ⚡ Lean footprint — no cairo/pango/Xft, no async runtime, single static binary.
- 🔲 Two layout modes: Column (scrollable, default) & Grid.
- 🖼 Real maximize (workarea-fill, keeps border) alongside fullscreen.
- 🖥 Multi-monitor support via RandR.
- 🧩 Floating + fullscreen window support.
- 🧱 External dock/bar support (Waybar, Polybar, …) via EWMH struts.
- 🔌 `maverickctl` control socket — list/state/dispatch/restart/reload/quit any running instance.
- 📐 Highly configurable (gaps, borders, split bias).
- 🔧 Declarative window rules.
- 🚀 Autostart programs.
- 📋 EWMH compliant.


---



## 🚀 Installation

### Build from source

Maverick is a Cargo workspace with three binaries: `maverick` (the WM
itself), `maverickctl` (control CLI), and `maverick-dialog` (the quit
confirmation prompt). Build all of them together:

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

(`--workspace` is needed because the root `Cargo.toml` is itself the
`maverick` package — without it, Cargo only builds `maverick` and
skips the `maverick-sys`/`maverick-dialog` binaries.)

### Add to PATH

```bash
cp target/release/maverick target/release/maverickctl target/release/maverick-dialog ~/.local/bin/
```

`maverick-dialog` only needs to be on `PATH` if you want the
`Super+Shift+Q` quit prompt to appear; without it, `maverickctl`
falls back to `zenity`/`kdialog`/a TTY prompt.

### Start with `.xinitrc`

```bash
exec maverick

```

### Display manager — `maverick.desktop`

Create `/usr/share/xsessions/maverick.desktop`:

```ini
[Desktop Entry]
Name=maverick
Comment=Columnar tiling WM
Exec=maverick
Type=XSession

```

---

## 🔲 Layouts

maverick ships two layout modes switchable at runtime.

| Mode | Shortcut | Description |
| --- | --- | --- |
| **Column** | `Super+T` | Scrollable columns (default). Each window lives in its own column. |
| **Grid** | `Super+G` | All windows in a uniform grid. |

Cycle through all modes with `Super+Space`.

> Layout is set **per workspace**, not globally — switching it only rearranges the active workspace on the selected monitor.

---

## ⌨️ Keybindings

`Super` = Windows key (`Mod4`)

### Spawn

| Shortcut | Action |
| --- | --- |
| `Super+Return` | Open terminal (`alacritty`) |
| `Super+P` | App launcher (`rofi -show drun`) |
| `Super+Shift+P` | Command runner (`rofi -show run`) |

### Window Operations

| Shortcut | Action |
| --- | --- |
| `Super+Shift+C` | Kill focused window |
| `Super+Shift+Space` | Toggle floating |
| `Super+Shift+F` | Toggle fullscreen |

### Focus Navigation

| Shortcut | Action |
| --- | --- |
| `Super+H` | Focus column to the left |
| `Super+L` | Focus column to the right |
| `Super+K` | Focus window above (within column) |
| `Super+J` | Focus window below (within column) |
| `Super+Tab` | Focus next monitor |

### Window Movement

| Shortcut | Action |
| --- | --- |
| `Super+Shift+H` | Move window left |
| `Super+Shift+L` | Move window right |
| `Super+Shift+K` | Swap window upward within column |
| `Super+Shift+J` | Swap window downward within column |
| `Super+Shift+Tab` | Move window to next monitor |

> **Move semantics:** if the focused column has one window, `Shift+H/L` swaps the entire column with its neighbour (fully reversible). If the column has multiple windows, the focused window is extracted into its own new adjacent column.

### Column Operations

| Shortcut | Action |
| --- | --- |
| `Super+Shift+Return` | Move window to a new column |
| `Super+Ctrl+H` | Shrink current column (−50 px) |
| `Super+Ctrl+L` | Grow current column (+50 px) |
| `Super+Ctrl+J` | Collapse column into the one to its left |

### Workspaces

| Shortcut | Action |
| --- | --- |
| `Super+1` … `Super+9` | Switch to workspace 1–9 |
| `Super+Shift+1` … `Super+Shift+9` | Move focused window to workspace 1–9 |

### WM Control

| Shortcut | Action |
| --- | --- |
| `Super+Shift+Q` | Ask for confirmation, then quit maverick |
| `Super+Shift+R` | Hot restart maverick in-place |
| `Super+F5` | Hot restart maverick in-place |
| `Super+Space` | Cycle layout modes |
| `Super+T` | Set Column layout |
| `Super+G` | Set Grid layout |

> `Super+Shift+Q` spawns `maverickctl quit --confirm` (falls back to `zenity`/`kdialog`/TTY if `maverick-dialog` isn't installed) so a stray keypress can't kill the session. The whole WM is also controllable from outside over a Unix socket via `maverickctl` — see [Technical Details](#-technical-details).

### Mouse (floating windows)

| Action | Result |
| --- | --- |
| `Super+Left-drag` | Move floating window |
| `Super+Right-drag` | Resize floating window |

---

## 🔧 Configuration

Maverick is configured in **`$XDG_CONFIG_HOME/maverick/config.toml`** (or
`~/.config/maverick/config.toml` when `XDG_CONFIG_HOME` is unset). The file is
**fully optional** — if it's missing, maverick runs on sensible compiled
defaults with no complaint. Missing fields fall back to those defaults, so you
only write what you want to override.

Loading is **fail-safe by design**: a file with invalid syntax is rejected
whole and the compiled defaults are used, while a wrong-typed value, unknown
key name or broken action string is dropped with a warning and the rest of
the file still loads. Maverick never fails to start because of a bad config.

There's a full, commented example at [`config/config.toml`](config/config.toml):

```bash
mkdir -p ~/.config/maverick
cp config/config.toml ~/.config/maverick/config.toml
```

```toml
# ~/.config/maverick/config.toml

[general]
border_width = 2
gaps = 6
n_tags = 9

[colors]
normal  = 0x45475a
focused = 0x89b4fa
urgent  = 0xf38ba8

[[keybindings]]
key = "super+return"
action = "spawn:alacritty"

[[keybindings]]
key = "super+shift+q"
action = "kill"

[[rules]]
class = "mpv"
float = true

[autostart]
commands = [["nm-applet"]]
```

Apply changes without restarting:

```bash
maverickctl reload
```

If you'd rather keep everything compiled in, just don't create the file —
nothing changes from before.

### Core Options

```rust
border_w:       2,        // border width in pixels
gaps:           6,        // gap between windows and screen edges (px)
n_tags:         9,        // number of workspaces
column_width:   0.6,      // width of a freshly created column, as a
                          //   fraction (0.1–1.0) of the workarea width
accordion_boost: 0.0,     // focus-expansion factor for the focused column (0.0–0.9)
overview_zoom_min: 0.25,  // minimum Overview film-strip zoom (0.05–1.0)
focus_mouse:    false,    // focus window on mouse enter
warp_cursor:    false,    // warp cursor to focused window center
auto_workspace_binds: true, // auto-generate Super+1..9 / Super+Shift+1..9
```

`column_width` is the fraction of the workarea given to a newly created
column (0.1–1.0). It replaces the old `default_col_w` (pixels) and
`split_bias` keys, which are now deprecated aliases that map onto it.

### Colors

Default palette: Catppuccin Mocha. All colors are 24-bit hex `0xRRGGBB`:

```rust
col_normal:  0x45475a,  // unfocused window border   (Surface1)
col_focused: 0x89b4fa,  // focused window border      (Blue)
col_urgent:  0xf38ba8,  // urgent window border       (Red)

```

### Workspace Names

```rust
tag_names: (1..=9).map(|n| n.to_string()).collect(),

```

### Startup

```rust
autostart: vec![
    vec!["/usr/lib/xdg-desktop-portal-gtk"],
    vec!["/usr/lib/xdg-desktop-portal"],
    vec!["picom", "--vsync"],                    // compositor, if you want one
    vec!["polybar", "main"],                     // external bar
    vec!["feh", "--bg-fill", "/path/to/wallpaper.png"],
    vec!["alacritty"],
],

```

maverick doesn't orchestrate any external tool specially — compositor, bar,
wallpaper, portals are all just autostart entries, launched once the WM is
ready. There's no startup ordering/delay logic to configure; if a tool needs
a moment before it's usable, that's on the tool itself.

> The shipped default `autostart` also launches `/usr/lib/xdg-desktop-portal` and `/usr/lib/xdg-desktop-portal-gtk` — without them, GTK/portal-based file pickers (browser upload dialogs, etc.) never appear.

### Using an external bar

maverick ships **no status bar** — drawing one isn't the WM's job. Use
polybar, waybar, eww, or similar; the WM reserves screen space for any dock
that publishes `_NET_WM_STRUT_PARTIAL`/`_NET_WM_STRUT`, so tiled windows never
overlap it (see `backend/x11/struts.rs`). Launch your bar from `autostart`:

```rust
autostart: vec![
    vec!["polybar".into(), "main".into()],
    // …
],
```

For the status text, maverick reads the root window's `WM_NAME` (set with
`xsetroot -name "…"` or `xsetroot -name "$(date)"`) and exposes it through
`maverickctl state` / `maverickctl subscribe`, so a bar or script can read it
without scraping X properties itself.

---

## 📋 Window Rules

Rules let you assign windows to specific workspaces or force them to float automatically, matched by WM_CLASS or title substring. Set them via `[[rules]]` in `config.toml` (see [Configuration](#-configuration)) or, for the compiled baseline, in `config.rs`:

```rust
rules: vec![
    Rule { class: Some("xdg-desktop-portal".into()), title: None,                             float: true, ws: None },
    Rule { class: Some("gpick".into()),              title: None,                             float: true, ws: None },
    Rule { class: Some("pinentry".into()),           title: None,                             float: true, ws: None },
    Rule { class: None, title: Some("file upload".into()),    float: true, ws: None },
    Rule { class: None, title: Some("open file".into()),      float: true, ws: None },
    Rule { class: None, title: Some("save file".into()),      float: true, ws: None },
    Rule { class: None, title: Some("qt file dialog".into()), float: true, ws: None },
],

```

**Rule fields:**

| Field | Type | Description |
| --- | --- | --- |
| `class` | `Option<String>` | Match against `WM_CLASS` (case-insensitive substring) |
| `title` | `Option<String>` | Match against window title (case-insensitive substring) |
| `float` | `bool` | Force floating mode |
| `ws` | `Option<usize>` | Send to workspace index (0-based) |

---

## 🏗 Technical Details

maverick minimizes abstraction layers by avoiding unnecessary dependencies:

* **X11 / XLibre via `x11rb 0.13`** — Type-safe protocol bindings, no libx11. Only the WM and `maverick-dialog` link `x11rb`; the rest of the workspace is pure `std`.
* **One dispatch seam** — `Engine::dispatch(Action) -> Vec<Effect>` is the *only* path from a keybind or IPC command to state mutation. `Effect` is a semantic vocabulary (`ArrangeMonitor`, `FocusWindow`, `SetFullscreen`, …); the X11 backend's `execute()` is the only place that turns those into protocol calls. A future non-X11 backend would implement `execute()` against the same effects without the core changing.
* **Fullscreen/maximize as presentation, not a state-machine block** — `core/present.rs` rewrites only the *focused* window's rect (fullscreen → whole screen, maximize → workarea, both keeping precedence over plain layout) and re-arranges on every focus transition, instead of blocking input while a window is fullscreen.
* **Self-computed float placement** — `manage()` never trusts the raw X geometry a new window reports; floating windows are centered on the transient parent's real stored geometry (or the assigned monitor's workarea, for portal-spawned dialogs with no real parent) and clamped inside it. Only width/height come from the original request.
* **Instance control plane** — `maverick-sys` gives every running instance a PID/display/tty identity and a Unix-socket protocol (`ping`/`identify`/`state`/`dispatch`/`restart`/`reload`/`subscribe`/`quit`). `maverickctl` talks to it: `list`, `state`, `msg <action>`, `subscribe`, `quit[--confirm]`, `quit-all`, `restart`, `reload`, `prune`. Handles several instances on different displays/ttys.
* **Optional TOML config layer** — `userconfig.rs` parses `config.toml` and merges it over `config::compiled_config()` field-by-field; a file that fails to parse is rejected whole, a single bad entry is dropped with a warning. `maverickctl reload` re-reads it live, no restart.
* **External dock/bar struts** — Docks are detected via `_NET_WM_WINDOW_TYPE_DOCK`/`_DESKTOP` (never by process name) and reserve space by reading `_NET_WM_STRUT_PARTIAL`/legacy `_NET_WM_STRUT`, tracked per-monitor and released on destroy/unmap. maverick itself ships no status bar — drive Waybar/Polybar/eww and let the WM reserve space for it.
* **`HashMap` client map** — O(1) window lookups by XID.
* **O(N) column layout** — Row heights precomputed in a single forward pass.
* **RandR monitor detection** — Correct workarea accounting for each monitor.
* **EWMH support** — Including `_NET_WM_STATE`, `_NET_WM_DESKTOP`, `_NET_ACTIVE_WINDOW`, etc.
* **`exec`-based restart** — Replaces the process in-place, preventing X11 grab race conditions.
* **`override_redirect` isolation** — External bars and overlays remain invisible to the WM.

---

## 📂 Project Structure

```text
Maverick/                    # Cargo workspace
├── src/                     # `maverick` — the WM binary
│   ├── main.rs               entry point, signals, autostart, control-plane wiring
│   ├── config.rs              compiled baseline config: Cfg, Rule, keybinds, colors
│   ├── userconfig.rs           optional config.toml: parsing, fail-safe loading, merge
│   ├── types.rs                core data model: State, Monitor, Workspace, Column, Client
│   ├── log.rs                   lightweight stderr logger
│   ├── core/                    pure logic layer — no X11
│   │   ├── engine.rs              Engine::dispatch(Action) -> Vec<Effect>
│   │   ├── effect.rs               Effect enum (the core/backend seam)
│   │   ├── present.rs               fullscreen/maximize presentation layer
│   │   ├── layout.rs                 arrange_columns / arrange_grid
│   │   ├── ipc.rs                     state_json / parse_action for the control socket
│   │   └── tests.rs                   unit tests
│   └── backend/                 X11 backend — the only place that speaks the protocol
│       ├── atoms.rs               EWMH / ICCCM atom cache
│       └── x11/                     the running WindowManager, split by concern
│           ├── mod.rs                 WindowManager, event loop, RandR
│           ├── manage.rs                window discovery, property reads, client setup
│           ├── events.rs                 X event dispatch table
│           ├── ewmh.rs                    EWMH property maintenance
│           ├── actions.rs                  do_action / execute (runs core's Effects), reload
│           ├── input.rs                     keymap, key grabs
│           ├── pointer.rs                    drag-to-move/resize, click focus
│           ├── render.rs                      geometry application, focus, restack
│           ├── struts.rs                       external dock reservation
├── maverick-sys/             # libc FFI + instance identity/control-socket/hub/discover
│   └── src/
│       ├── identity.rs         per-instance PID/display/tty "ficha"
│       ├── control.rs           ControlServer — the Unix-socket protocol
│       ├── hub.rs                 ControlHub — bridge to the WM's event loop
│       ├── discover.rs             list/find/quit instances
│       └── bin/maverickctl.rs       the `maverickctl` CLI
├── maverick-dialog/           # standalone X11 yes/no quit-confirmation window
│   └── src/main.rs
├── examples/
│   └── config.toml            full, commented sample user config
├── CHANGELOG.md
├── Cargo.toml                 # workspace root + the `maverick` package
├── Cargo.lock
├── LICENSE
├── README.md
└── README.es.md

```

---

## 📜 License

GPL-3.0 license 

---
