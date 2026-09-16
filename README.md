# Maverick

A scrollable, column-based tiling window manager for X11, written in Rust.

Maverick tiles windows into a horizontally scrolling ribbon of columns
(niri-style), with a built-in OpenGL/GLX compositor, per-monitor workspaces,
and a Unix-socket control plane. The core layout logic is pure Rust with no
X11 in it; a backend layer translates decisions into X11 protocol calls and
compositor draw commands.

> X11 only. There is no Wayland backend.

## Why Maverick

- **One layout, done well.** A scrollable column ribbon. Every new window
  gets its own column at a fixed fraction of the workarea width, so adding
  a column never resizes its neighbours. A spring camera keeps the focused
  column centred.
- **No external compositor required.** Opacity, rounded corners and a native
  wallpaper (image or live GLSL shader) work out of the box, with automatic
  fallback to the plain X11 path when GL is unavailable.
- **One dispatch path.** Keypresses, mouse actions and IPC commands all go
  through the same action vocabulary, so behaviour is identical no matter
  what triggered it.
- **Explicit applied state.** A reconciler owns "what is actually on X11"
  and only emits `ConfigureWindow`/restack/focus calls for what changed.
- **Minimal dependencies.** The WM core speaks X11 via `x11rb` (pure Rust).
  TOML parsing, PNG decoding and GL/GLX access are hand-written in-tree
  crates — no toolkit, no async runtime, no `build.rs`, no `pkg-config`.

## Quick start

Requirements: Linux with an X server (X.Org / XLibre), a Rust toolchain
(MSRV **1.82**), a C linker, and the X11 client libraries (linked by the GLX
FFI). OpenGL 3.3 is optional — `libGL.so.1` is `dlopen`ed at runtime, and
Maverick runs as a plain X11 WM without it.

System packages per distro (build + test clients + suite):

| Purpose | Arch (`pacman`) | Debian/Ubuntu (`apt`) | Fedora (`dnf`) |
|---|---|---|---|
| Build: toolchain, linker, X11/Xcomposite libs | `base-devel rust libx11 libxcb mesa libxcomposite` | `build-essential cargo libx11-dev libxcb1-dev libgl1-mesa-dev libxcomposite-dev` | `gcc cargo libX11-devel libxcb-devel mesa-libGL-devel libXcomposite-devel` |
| Integration suite (`tests/xephyr-*.sh`) | `xorg-server-xephyr xdotool xterm xorg-xwininfo xorg-xev` | `xephyr x11-utils xdotool xterm` | `xorg-x11-server-Xephyr xdotool xterm xorg-x11-utils` |
| Default keybinds/runtime | `alacritty rofi xdg-desktop-portal xdg-desktop-portal-gtk` + `xorg-xinit` (for `startx`) | `alacritty rofi xdg-desktop-portal xdg-desktop-portal-gtk` + `xinit` | `alacritty rofi xdg-desktop-portal xdg-desktop-portal-gtk` + `xorg-x11-xinit` |
| Wallpaper fallbacks (JPEG/WebP/…) | `ffmpeg` (or `imagemagick`) | `ffmpeg` (or `imagemagick`) | `ffmpeg` (or `ImageMagick`) |

```bash
# Arch example (all of the above):
sudo pacman -S --needed base-devel rust libx11 libxcb mesa libxcomposite \
  xorg-server-xephyr xdotool xterm xorg-xwininfo xorg-xev \
  alacritty rofi xdg-desktop-portal xdg-desktop-portal-gtk xorg-xinit ffmpeg
```

```bash
git clone https://github.com/azytar/Maverick.git
cd Maverick
cargo build --release --workspace
```

Binaries land in `target/release/`:

| Binary | Role |
| --- | --- |
| `maverick` | the window manager |
| `maverickctl` | admin / query CLI (list, state, dispatch, quit, …) |
| `maverick-msg` | verbatim forwarder — any line goes to the control socket |
| `maverick-dialog` | standalone quit-confirmation window |

Easiest install (builds release, installs the four binaries, writes the
X session file, seeds a config when none exists):

```bash
./install.sh                  # /usr/local/bin as root, ~/.local/bin otherwise
./install.sh --prefix ~/.local --yes
./install.sh --without-compositor   # pure-X11 build (--no-default-features)
```

Then, from `~/.xinitrc`:

```bash
exec maverick
```

or pick `maverick` in your display manager (`install.sh` installs
`maverick.desktop` for you).

CLI flags (all optional, any order):

| Flag | Meaning |
| --- | --- |
| `--config <path>` | Use this TOML instead of the default location. Reused on `reload` and `restart`. |
| `--check-config [path]` | Validate config and exit (`0` = clean, `1` = warnings/errors). Never starts the WM. |
| `--replace` / `-r` | Replace the running WM, adopting its windows. |
| `--name <id>` | Instance label for control/discovery. |
| `-v` / `--version` | Print version and exit. |
| `-h` / `--help` | Print usage and exit. |

```bash
maverick --check-config ~/.config/maverick/config.toml
maverick --config ~/.config/maverick/config.toml
```

## Keybindings

`Super` is the Windows/Mod4 key. These are the compiled defaults — every
row is overridable in `config.toml`, and `Super+1..9` /
`Super+Shift+1..9` workspace binds are auto-generated on top (yours win on
conflict).

### Launch

| Binding | Action |
| --- | --- |
| `Super+Return` | Terminal (`alacritty`) |
| `Super+P` | App launcher (`rofi -show drun`) |
| `Super+Shift+P` | Command runner (`rofi -show run`) |

### Windows

| Binding | Action |
| --- | --- |
| `Super+Shift+C` | Kill focused window |
| `Super+Shift+Space` | Toggle floating |
| `Super+Shift+F` | Toggle fullscreen (covers the screen) |
| `Super+Shift+M` | Toggle maximize (fills the workarea) |
| `Super+Shift+Q` | Quit via `maverickctl quit --confirm` |

### Focus and move

| Binding | Action |
| --- | --- |
| `Super+H` / `L` / `J` / `K` | Focus left / right / down / up |
| `Super+Shift+H` / `L` / `J` / `K` | Move window left / right / down / up |
| `Super+Tab` | Focus next monitor |
| `Super+Shift+Tab` | Move window to next monitor |

### Columns

| Binding | Action |
| --- | --- |
| `Super+Shift+Return` | Move window into a new column |
| `Super+Ctrl+H` / `L` | Shrink / grow focused column (±50 px) |
| `Super+Ctrl+J` | Collapse column into the one on its left |
| `Super+T` | Column layout (the only layout; per workspace) |

### Overview and viewport

| Binding | Action |
| --- | --- |
| `Super+O` | Toggle Overview (film-strip) |
| `Super+N` / `Super+Shift+O` | Overview navigate right / left |
| `Super+E` | Enter the Overview selection |
| `Super+=` / `Super+-` | Zoom viewport in / out |
| `Super+]` / `Super+[` | Page-snap scroll right / left |
| `Super+Shift+R` / `Super+F5` | Restart in place |

### Workspaces

| Binding | Action |
| --- | --- |
| `Super+1` … `Super+9` | Switch to workspace 1–9 |
| `Super+Shift+1` … `Super+Shift+9` | Move focused window to workspace 1–9 |

### Mouse

Only already-floating windows are draggable. A `Super`-drag on a tiled
window is a no-op — tiles move with the keyboard.

| Action | Result |
| --- | --- |
| `Super+Left-drag` | Move floating window |
| `Super+Right-drag` | Resize floating window (quadrant-aware) |
| `Super+wheel` | Step column focus one slot per notch |

## Configuration

Optional. No config file means compiled defaults.

- Location: `$XDG_CONFIG_HOME/maverick/config.toml`, else
  `~/.config/maverick/config.toml`.
- Start from the commented sample:

```bash
mkdir -p ~/.config/maverick
cp config/config.toml ~/.config/maverick/config.toml
maverickctl reload   # apply live, no restart
```

- Fail-safe by design: broken TOML falls back to compiled defaults whole;
  a single bad entry (wrong type, unknown action, out-of-range workspace)
  is dropped with a warning and the rest still loads. A bad config never
  prevents startup.
- `[[keybindings]]` and `[[rules]]` **replace** the compiled lists when
  present (workspace numeric binds are still auto-filled into free slots
  unless `auto_workspace_binds = false`).
- Key names: `a–z`, `0–9`, `F1–F12`, `Return`/`Enter`, `Space`, `Tab`,
  `bracketleft/right`, `equal`, `minus`, XF86 names, or a raw `0x<hex>`
  keysym. Modifiers: `Super`/`Mod4`, `Shift`, `Control`/`Ctrl`, `Alt`/`Mod1`.
- Action grammar (`:` or space separated, e.g. `focus:left` ≡ `focus left`):
  `spawn:<cmd>`, `kill`, `toggle_float`, `toggle_fullscreen`,
  `toggle_maximize`, `focus:<dir>`, `move:<dir>`, `focus_mon:<dir>`,
  `move_mon:<dir>`, `layout:column` / `set_layout:column`,
  `grow_col:<px>`, `new_column`, `collapse_column`, `view:<n>`,
  `move_to_ws:<n>`, `viewport_zoom[:<f>]`, `page_snap:<dir>`,
  `toggle_overview`, `overview_nav:<dir>`, `overview_enter`,
  `wallpaper <set|clear|mode> …`, `restart`, `quit`.

### `[general]`

| Key | Default | Notes |
| --- | --- | --- |
| `border_width` | `2` | Pixels (`border_w` alias). `0` disables. |
| `gaps_inner` / `gaps_outer` | `6` / `6` | Between tiles / at screen edges (`gaps` sets both). |
| `smart_gaps` | `false` | Zero gaps with a single tiled window. |
| `corner_radius` | `0` | Rounded corners (Shape without GL, SDF with GL). |
| `n_tags` | `9` | Workspaces, clamped to 1–9. |
| `column_width` | `0.6` | New-column width as workarea fraction (0.1–1.0). |
| `accordion_boost` | `0.0` | Focused-column expansion (0.0–0.9, `0` off). |
| `overview_zoom_min` | `0.25` | Overview film-strip minimum zoom (0.05–1.0). |
| `focus_mouse` | `false` | Focus on pointer enter. |
| `warp_cursor` | `false` | Warp cursor to focused window centre. |
| `auto_workspace_binds` | `true` | Generate `Super+1..9` / `Super+Shift+1..9`. |
| `honor_initial_state` | `false` | `true` honours a client's map-time maximized/fullscreen request; default normalises it to a plain tile. Per-app opt-in via rules. |
| `tag_names` | `"1".."9"` | Cosmetic workspace names. |
| `theme` | `catppuccin-mocha` | `nord`, `dracula`, `gruvbox`, `everforest`, `solarized`, `catppuccin-mocha`, `catppuccin-latte`. Overridden by `[colors]`. |

Deprecated aliases (still parsed, warn in `--check-config`):
`default_col_width` / `split_bias` → `column_width`,
`general.camera_stiffness` / `camera_damping` → `[animations]`,
`general.compositor_enabled` → `[compositor].enabled`.

### `[compositor]` and `[animations]`

```toml
[compositor]
# enabled = true
# backend = "opengl"      # "opengl" | "vulkan" (vulkan needs the feature build)
# vsync = "on"            # "on" | "off" | "adaptive"
# fullscreen_bypass = true

[animations]
# enabled = true          # false = snap instantly, no spring frames
# stiffness = 220.0
# damping = 30.0
```

The compositor is on by default with automatic fallback to the plain X11
path when GL is missing, context creation fails, or another compositor owns
`_NET_WM_CM_S0`. `MAVERICK_NO_COMPOSITOR=1` disables it for one run;
`--no-default-features` builds a pure-X11 binary. Without the compositor
there is no animation — every state change lands on final geometry
immediately (dwm-style).

Partial redraw is conditional: it needs `GLX_EXT_buffer_age` *and* a live
back buffer, otherwise the compositor does a full redraw. Effects are
deliberately small: per-window opacity (`_NET_WM_WINDOW_OPACITY`,
settable per rule), rounded corners, wallpaper. No blur, no shadows.

### `[colors]`

24-bit hex `0xRRGGBB` (`col_*` aliases accepted). Defaults are Catppuccin
Mocha: `normal = 0x45475a`, `focused = 0x89b4fa`, `urgent = 0xf38ba8`.

### `[wallpaper]`

```toml
[wallpaper]
# path = "~/Pictures/wallpaper.png"  # image or .glsl/.frag shader; null disables
# mode = "fill"                      # fill | fit | stretch | center
```

Images: PNG decoded natively, other formats via an external converter.
Shaders: compiled on the GPU, redrawn every frame with `u_time`,
`u_resolution`, `u_delta_time`. `Video` is reserved, not implemented.
Control live without restarting:

```bash
maverick-msg wallpaper set ~/Pictures/wallpaper.png
maverick-msg wallpaper mode fit
maverick-msg wallpaper clear
```

### `[[rules]]`

Matched by `WM_CLASS` class/instance, window type and title — all
case-insensitive substrings (multiple criteria AND). Example:

```toml
[[rules]]
class = "mpv"
float = true

[[rules]]
window_type = "dialog"
float = true
```

| Field | Meaning |
| --- | --- |
| `class` / `instance` / `title` | Substring match. |
| `window_type` / `type` | `normal`, `dialog`, `utility`, `menu`, `toolbar`, `splash`, `desktop`, `dock`. |
| `float` / `sticky` | Force floating / keep visible on all workspaces. |
| `workspace` / `ws` | Pin to workspace (1-based). |
| `size` / `position` | Forced float size `[w, h]` / position `[x, y]`. |
| `opacity` | 0.0–1.0 (needs compositor). |
| `border_width` | Border override (floats). |
| `ignore_initial_state` | Force this app to open as a plain tile. |
| `honor_initial_state` | Let this app keep its launch maximized/fullscreen. |
| `deny_fullscreen` | Refuse the app's own EWMH fullscreen (F11); `Super+Shift+F` still works. |
| `true_fullscreen` | Exclusive screen-covering overlay, outside the ribbon (games). Wins over `deny_fullscreen`. |

### `[autostart]`

A list of command lists, launched once when the WM is ready:

```toml
[autostart]
commands = [["nm-applet"]]
```

The compositor and wallpaper are built in, not autostart entries. The
compiled default launches the XDG portals (needed for GTK file pickers).
Maverick ships no bar — run `polybar`/`waybar`/similar here; any dock
publishing `_NET_WM_STRUT_PARTIAL`/`_NET_WM_STRUT` automatically reserves
space. Status text is exposed via `maverickctl state` / `subscribe`
(root `WM_NAME`).

## Control plane

Each instance gets a random session id and an isolated runtime dir
(`$XDG_RUNTIME_DIR/maverick/<session-id>/`, mode `0700`, socket at
`control.sock`). Discovery checks both socket liveness and `/proc` start
time, so PID recycling can't confuse it — a login session and a Xephyr test
instance never fight over a socket.

Target selection: `--session <sid>` → `--name <id>` →
`$MAVERICK_INSTANCE` (exported by the WM to its children) → the sole live
instance in your `DISPLAY`/`TTY` context.

```bash
maverickctl list
maverickctl state                    # full snapshot (JSON)
maverickctl query workspaces         # state | workspaces | tree | focused
maverickctl msg focus-left
maverickctl subscribe
maverickctl reload                   # re-read config live
maverickctl restart                  # re-exec in place, same launch args
maverickctl quit --confirm           # maverick-dialog → zenity/kdialog → TTY
maverickctl quit --yes
maverickctl quit-all --yes
maverickctl prune                    # drop stale records
```

`maverick-msg` is the same engine without subcommands — it forwards the
whole line verbatim (`maverick-msg view 3`, `maverick-msg wallpaper clear`).

Shutdown is bounded (3 s): clients supporting `WM_DELETE_WINDOW` are asked
politely first, survivors are force-killed. Shutdown never hangs on client
cooperation.

## How it works

```
User action / IPC
        │
        ▼
Action ──► pure core ──► layout::arrange ──► present::present_into
        │                                              │
        │                                              ▼
        │                                    DesiredState (explicit hand-off)
        │                                              │
        │                                              ▼
        │                                    Reconciler ──► AppliedState ──► X11
        │                                              │
        └──────────────────────────────────────────────┘
                                    compositor (OpenGL)
```

- `maverick-core` owns all logical state (columns, floats, focus order,
  cameras, fullscreen/maximize policy) with zero X11 types — `WindowId`
  is a plain `u32`. `State::check_invariants()` guards consistency.
- The X11 backend is the only code that touches the protocol. It never
  mutates `State` directly; everything flows through commands/effects.
- The WM and the GLX compositor share one X11 connection.
- Tiled geometry is owned by the WM: a tiled client's own
  `ConfigureRequest` resize is answered with the WM rect; floating windows
  honour requests clamped to the workarea.
- `override_redirect` windows (bars, menus, overlays) are never managed,
  and their configure requests pass through.

## Testing

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

The Xephyr suite drives real X clients under a nested server (manual/CI
harness, not part of `cargo test`). Needs `Xephyr`, `x11-utils`,
`xdotool`, `xterm` (`firefox`/`mpv` exercised when present):

```bash
DISPLAY=:1 ./tests/xephyr-suite.sh
```

It sets `MAVERICK_NO_COMPOSITOR=1` — GLX texture-from-pixmap can't
initialise under nested Xephyr. That is a test-environment limit, not a
compositor defect on a real X server.

## Status and limitations

Actively developed, daily-driven on X11, not declared production-ready.

- X11 only. No Wayland backend.
- One layout: Column. (Grid was removed; layout-cycle bindings are gone —
  `Super+T` pins Column per workspace.)
- Vulkan backend (`maverick-vk`) is an early bootstrap only (clear/present),
  not wired to the WM. `compositor.backend = "vulkan"` without the feature
  build errors actionably and falls back to X11.
- `Video` wallpaper is reserved, not implemented. Only image + GLSL work.
- No blur or shadow effects.
- Partial redraw only with `GLX_EXT_buffer_age` and a live back buffer.
- `n_tags` clamps to 1–9; workspace names are cosmetic (addressed by index).

## Project layout

```text
Maverick/
├── src/                  maverick binary (main, config, userconfig, core/, backend/x11/)
├── maverick-core/        pure logic: types, layout, presentation, wallpaper model
├── maverick-sys/         IPC: identity, control socket, discovery + maverickctl/maverick-msg
├── maverick-x11/         X11 connection bootstrap (shared by WM and renderers)
├── maverick-render/      neutral renderer trait (backends implement it)
├── maverick-gl/          GLX/OpenGL backend (hand-written FFI)
├── maverick-vk/          experimental Vulkan bootstrap (not wired to the WM)
├── maverick-toml/        zero-dependency TOML subset parser
├── maverick-img/         dependency-free PNG decoder
├── maverick-dialog/      quit-confirmation window
├── config/config.toml    commented sample config
├── tests/                Xephyr integration harness + C test clients
├── install.sh            release build + install + session file + seed config
├── CHANGELOG.md
└── Cargo.toml            workspace root (also the `maverick` package, edition 2021)
```

Crates are edition 2021, MSRV 1.82.

## License

GPL-3.0 — see `LICENSE`.
