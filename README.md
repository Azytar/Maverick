# 🦅 maverick

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

**maverick** is a lightweight X11/XLibre tiling window manager built around a scrollable column layout inspired by [niri](https://github.com/YaLTeR/niri).
Written entirely in Rust using `x11rb 0.13` — no Cairo, no Pango, no heavy runtimes.

Designed around:

- 🦅 horizontally scrollable columns (niri-inspired)
- ⚡ extremely low memory footprint (~3–4 MB)
- 🧠 direct X11/XLibre via `x11rb` — zero bloat
- 🔲 three layout modes: Column · Monocle · Grid
- 🖥 true multi-monitor support via RandR
- 🧩 floating + fullscreen window support
- 📐 configurable gaps, borders, bar, and split bias
- 🔧 declarative window rules
- 🚀 autostart programs
- 🎨 fully themeable status bar and borders with clickable workspaces
- 📋 EWMH compliant

---

## 📸 Preview

<p align="center">
  <img src="assets/preview.png" alt="maverick preview" width="900">
</p>

---

## 🚀 Installation

### Build from source

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release

```

### Add to PATH

```bash
cp target/release/maverick ~/.local/bin/

```

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

maverick ships three layout modes switchable at runtime:

| Mode | Shortcut | Description |
| --- | --- | --- |
| **Column** | `Super+T` | Scrollable columns. Each window lives in its own column by default. |
| **Monocle** | `Super+M` | One window at a time, fullscreen within the workarea. |
| **Grid** | `Super+G` | All windows in a uniform grid. |

Cycle through all modes with `Super+Space`.

> Layout is global across all monitors. Switching it rearranges all monitors simultaneously.

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
| `Super+B` | Toggle bar visibility |

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

> Workspace tags are also **clickable** in the bar.

### WM Control

| Shortcut | Action |
| --- | --- |
| `Super+Shift+Escape` | Quit (with confirmation dialog) |
| `Super+Shift+R` | Hot restart maverick in-place |
| `Super+F5` | Hot restart maverick in-place |

### Mouse (floating windows)

| Action | Result |
| --- | --- |
| `Super+Left-drag` | Move floating window |
| `Super+Right-drag` | Resize floating window |
| Click on bar tag | Switch to that workspace |

---

## 🔧 Configuration

maverick is configured in `src/config.rs` (recompile to apply).

### Core Options

```rust
border_w:      2,      // border width in pixels
gaps:          6,      // gap between windows and screen edges (px)
bar_height:    22,     // status bar height in pixels
top_bar:       true,   // bar at top (false = bottom)
n_tags:        9,      // number of workspaces
default_col_w: 700,    // default column width when created (px)
split_bias:    0.6,    // focused-row size bonus in a split column (0.0–1.0)
focus_mouse:   false,  // focus window on mouse enter
warp_cursor:   false,  // warp cursor to focused window center

```

**`split_bias`** controls how much taller the focused window is compared to its siblings within a split column. `0.0` = equal heights, `1.0` = maximum bias.

### Colors

Default palette: Catppuccin Mocha. All colors are 24-bit hex `0xRRGGBB`:

```rust
col_normal:  0x45475a,  // unfocused window border   (Surface1)
col_focused: 0x89b4fa,  // focused window border      (Blue)
col_urgent:  0xf38ba8,  // urgent window border       (Red)
col_bar_bg:  0x1e1e2e,  // bar background             (Base)
col_bar_fg:  0xcdd6f4,  // bar foreground / text      (Text)
col_bar_sel: 0x89b4fa,  // selected workspace badge   (Blue)
col_bar_occ: 0xa6e3a1,  // occupied workspace dot     (Green)

```

### Workspace Names

```rust
tag_names: ["1", "2", "3", "4", "5", "6", "7", "8", "9"].to_vec(),

```

---

## 📋 Window Rules

Rules let you assign windows to specific workspaces or force them to float automatically, matched by WM_CLASS or title substring.

```rust
rules: vec![
    Rule { class: Some("xdg-desktop-portal"), title: None,                    float: true,  ws: None },
    Rule { class: Some("gpick"),              title: None,                    float: true,  ws: None },
    Rule { class: Some("pinentry"),           title: None,                    float: true,  ws: None },
    Rule { class: None, title: Some("file upload"),    float: true,  ws: None },
    Rule { class: None, title: Some("open file"),      float: true,  ws: None },
    Rule { class: None, title: Some("save file"),      float: true,  ws: None },
    Rule { class: None, title: Some("qt file dialog"), float: true,  ws: None },
],

```

**Rule fields:**

| Field | Type | Description |
| --- | --- | --- |
| `class` | `Option<&str>` | Match against `WM_CLASS` (case-insensitive substring) |
| `title` | `Option<&str>` | Match against window title (case-insensitive substring) |
| `float` | `bool` | Force floating mode |
| `ws` | `Option<usize>` | Send to workspace index (0-based) |

---

## 🚀 Autostart

Programs to launch when maverick starts:

```rust
autostart: vec![
    vec!["setxkbmap", "us", "-variant", "dvorak"],
    vec!["rviv", "--bg", "/home/example.png"],
    vec!["picom", "--active-opacity", "0.8", "--inactive-opacity", "0.8"],
    vec!["alacritty"],
],

```

Each entry is a `Vec<String>` — the first element is the binary, remaining elements are arguments. Processes are spawned with `setsid` in the background.

---

## 🏗 Technical Details

maverick avoids unnecessary abstraction layers wherever possible:

* **X11 / XLibre via `x11rb 0.13**` — type-safe protocol bindings, no libx11.
* **Custom XFT wrapper** (`xft.rs`) — fonts via FFI instead of cairo-rs (~18MB saved).
* **`HashMap` client map** — O(1) window lookups by XID.
* **Bar batching** — queue is drained before each `flush()` to avoid O(N) redraws per event burst.
* **O(N) column layout** — row heights precomputed in a single forward pass, not re-summed per row.
* **RandR monitor detection** — correct workarea accounting for each monitor's bar.
* **EWMH support** — `_NET_WM_STATE`, `_NET_WM_DESKTOP`, `_NET_ACTIVE_WINDOW`, `_NET_WM_STRUT_PARTIAL`, client list.
* **`exec`-based restart** — replaces the process in-place, no X11 grab race condition.
* **`override_redirect` isolation** — bars and overlays are invisible to the WM itself.
* **Float centering guard** — prevents the float-center heuristic from misfiring on fullscreen capture tools (≥90% workarea coverage = skip centering).

Memory footprint: **~3–4 MB** resident with a typical desktop open.

---

## 📂 Project Structure

```text
maverick/
├── src/
│   ├── main.rs          entry point, signal handling, autostart
│   ├── config.rs        configuration, keybinds, window rules
│   ├── types.rs         core types: State, Monitor, Workspace, Column, Client
│   ├── log.rs           lightweight logging
│   ├── core/
│   │   ├── mod.rs
│   │   ├── engine.rs    pure logic layer (layout engine)
│   │   ├── layout.rs    arrange_columns / arrange_monocle / arrange_grid
│   │   ├── events.rs    AppEvent enum
│   │   ├── commands.rs  Command enum (MoveResize, SetBorderColor, …)
│   │   └── tests.rs     unit tests (layout, move_dir, cycle_layout, …)
│   └── backend/
│       ├── mod.rs
│       ├── atoms.rs     EWMH / ICCCM atom cache
│       ├── bar.rs       status bar rendering via XFT
│       └── x11.rs       X11 event loop, window management, RandR
├── Cargo.toml
├── Cargo.lock
└── README.md

```

---

## 🌌 Philosophy

> one window, one column
> scroll, don't stack
> low memory, high control
> rust all the way down

maverick was built because most tiling WMs either carry decades of C legacy, rely on Lua runtimes, or ship a 20 MB Cairo dependency just to draw a bar. maverick uses none of that. Just Rust, x11rb, and XFT.

---

## 🤝 Related

* **[mavshot](https://github.com/azytar/mavshot)** — screenshot + annotation tool built specifically for maverick (`override_redirect` aware, zero WM interference).

---

## 📜 License

MIT

---
