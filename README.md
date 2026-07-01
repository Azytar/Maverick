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
- ⚡ Minimal footprint (~3–4 MB memory usage).
- 🔲 Three layout modes: Column (stable), Monocle & Grid (experimental).
- 🖥 Multi-monitor support via RandR.
- 🧩 Floating + fullscreen window support.
- 📐 Highly configurable (gaps, borders, bar, split bias).
- 🔧 Declarative window rules.
- 🚀 Autostart programs.
- 📋 EWMH compliant.


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

maverick ships three layout modes switchable at runtime. 

*Note: The `Monocle` and `Grid` modes are currently experimental and still under active development.*

| Mode | Shortcut | Description |
| --- | --- | --- |
| **Column** | `Super+T` | Scrollable columns (default). Each window lives in its own column. |
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
| `Super+Shift+Q` | Quit maverick |
| `Super+Shift+R` | Hot restart maverick in-place |
| `Super+F5` | Hot restart maverick in-place |
| `Super+Space` | Cycle layout modes |
| `Super+T` | Set Column layout |
| `Super+M` | Set Monocle layout |
| `Super+G` | Set Grid layout |

### Mouse (floating windows)

| Action | Result |
| --- | --- |
| `Super+Left-drag` | Move floating window |
| `Super+Right-drag` | Resize floating window |
| Click on bar tag | Switch to that workspace |

---

## 🔧 Configuration

**Note:** maverick is configured entirely in `src/config.rs`. **You must recompile the project after making any changes to this file to apply them.**

```bash
cargo build --release
# Then restart maverick
```


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

### Startup

```rust
compositor: vec!["picom", "--vsync"],            // compositor launched before the WM
compositor_delay_ms: 180,                        // ms to wait after compositor spawns
startup_sound: None,                             // optional WAV/OGG chime on startup
autostart: vec![
    vec!["feh", "--bg-fill", "/path/to/wallpaper.png"],
    vec!["alacritty"],
],

```

The compositor starts **before** the WM so every window gets compositing from its first frame. Autostart programs launch after both compositor and WM are ready. `startup_sound` accepts a path to a `.wav` or `.ogg` file; it tries `pw-play → paplay → canberra-gtk-play → mpv → aplay` in order.

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

## 🏗 Technical Details

maverick minimizes abstraction layers by avoiding unnecessary dependencies:

* **X11 / XLibre via `x11rb 0.13`** — Type-safe protocol bindings, no libx11.
* **Raw X11 bar rendering** — Status bar drawn with `image_text8` and `poly_fill_rectangle`, no external font libraries.
* **`HashMap` client map** — O(1) window lookups by XID.
* **Bar batching** — Queue is drained before each `flush()` to avoid O(N) redraws.
* **O(N) column layout** — Row heights precomputed in a single forward pass.
* **RandR monitor detection** — Correct workarea accounting for each monitor.
* **EWMH support** — Including `_NET_WM_STATE`, `_NET_WM_DESKTOP`, `_NET_ACTIVE_WINDOW`, etc.
* **`exec`-based restart** — Replaces the process in-place, preventing X11 grab race conditions.
* **`override_redirect` isolation** — Bars and overlays remain invisible to the WM.
* **Float centering guard** — Skips centering for fullscreen apps (≥90% workarea coverage).

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
│       ├── bar.rs       status bar rendering
│       └── x11.rs       X11 event loop, window management, RandR
├── Cargo.toml
├── Cargo.lock
├── LICENSE
├── README.md
└── README.es.md

```

---

## 📜 License

GPL-3.0 license 

---
