# Changelog

All notable changes to this project are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.18.3] — 2026-08-08

This release folds a long-running batch of fixes, audits, and features
(previously tracked as `[Unreleased]`) into the official 0.18.3 build.
Items carried over from an in-progress local audit are marked
**(unconfirmed)** — reported fixed by the reporting session, not yet
independently re-verified against a real build on this machine.

### Added

- **`--replace` and previous-WM window adoption (EWMH).** New
  `maverick --replace` flag: if another WM already manages the screen,
  Maverick first tries to grab `SUBSTRUCTURE_REDIRECT` directly; if another
  WM holds it, it locates that WM's `_NET_SUPPORTING_WM_CHECK` window
  (EWMH 1.4 § WM Attributes) and sends it `WM_DELETE_WINDOW` (never
  `SIGKILL`), retrying the grab until it yields. On startup over a previous
  WM, existing windows are adopted (`scan_windows`) and previously-floating
  windows have their geometry restored.
- **`_NET_CLIENT_LIST_STACKING`.** The root window now publishes real
  stacking order (visible tiles → floats → the rest, by focus recency)
  via `_NET_CLIENT_LIST_STACKING`, updated on every `flush_client_list`.
  `_NET_SUPPORTED` gained `_NET_WM_WINDOW_OPACITY`, `_NET_CLOSE_WINDOW`,
  and `_NET_WM_BYPASS_COMPOSITOR`.
- **Floating-window geometry persistence.** The custom `_MAVERICK_FLOAT` /
  `_MAVERICK_GEOM` window properties are written/cleared according to
  floating state (`sync_window_prefs`), restoring a window's floating
  geometry across a restart or `--replace`.
- **New rule-matching criteria.** `[[rules]]` now accepts, in addition to
  `class`/`title`: `instance`, `window_type`
  (normal/desktop/dock/toolbar/menu/utility/splash/dialog), and the
  actions `sticky` (implies floating), `size`, and `position` (clamped to
  the workarea). Matching is case-insensitive substring; `ws` accepts
  numeric aliases plus `request` for the current workspace.
- **`maverick-msg` (dwm-style CLI).** New control binary: any
  non-administrative line is forwarded verbatim per the wire protocol
  (`maverick-msg focus-right`; `maverick-msg query tree`). Shares the CLI
  engine (`maverick-sys::ctl`) with `maverickctl`.
- **Structured `query` over the control socket.** `maverickctl query
  workspaces|tree|focused|state` queries live WM state (the WM thread
  replies over the reply channel): IDs/geometry/layout per workspace and
  column, plus the focused window.
- **Mouse support for floating windows.** `Mod+drag` on a floating window
  now resizes it based on which quadrant the pointer is in (top/left
  halves resize); dropping a floating window on top of an already-tiled
  one inserts it into that column (drop-to-tile), with a preview border
  shown on the destination column during the drag.
- **`Rule::ignore_initial_state` ("don't trust the client" mode for
  demanding apps) (unconfirmed).** GTK apps (Firefox chief among them)
  remember whether their last window was maximized/fullscreen and request
  it again via `_NET_WM_STATE` on map; Maverick previously honored that
  unconditionally, so the window landed straight in the `core::present`
  overlay (full workarea/screen, no gaps, zero border), skipping tiling
  entirely. New boolean field on `Rule` (TOML: `ignore_initial_state` /
  aliases `no_initial_state`, `no_maximize`): when a rule matches,
  `apply_rules` clears `MAXIMIZED`/`FULLSCREEN` on the client and rewrites
  its `_NET_WM_STATE` on the spot (the client isn't in `state.clients` yet
  at that point, so `write_net_wm_state` isn't used there) so the property
  stops lying. A default compiled rule is shipped for `class = "firefox"`.
  Note: if you already have your own `[[rules]]` in `config.toml`, those
  fully replace the compiled ones (`merge_config`, pre-existing behavior)
  — add `ignore_initial_state = true` to your own Firefox rule to keep it.
- **Per-rule fullscreen policy: `deny_fullscreen` and `true_fullscreen`.**
  New boolean fields on `Rule` (TOML: `deny_fullscreen` / alias
  `no_fullscreen`; `true_fullscreen` / alias `exclusive_fullscreen`).
  - `deny_fullscreen` rejects fullscreen requests made *by the app itself*
    over EWMH (e.g. a browser's F11, indistinguishable from any other
    `_NET_WM_STATE_FULLSCREEN` arriving via `on_client_message`), while
    leaving the user's own `Mod4+F` intact (that goes through
    `Effect::SetFullscreen`, not the client-request path). A default
    compiled rule is shipped for `class = "firefox"`.
  - `true_fullscreen` gives real, exclusive fullscreen outside the ribbon,
    for games. It's special-cased in exactly **one** place (`fs_ctx` in
    `core::layout`) so `ribbon_geom` / `ideal_scroll` /
    `column_screen_extents` never disagree; `core::present` paints it as
    an overlay in *any* layout and `best_focus` counts it as presented.
    `true_fullscreen` wins over `deny_fullscreen` if both are set on the
    same rule. The ribbon camera is not re-centered for it.
- **Viewport (inspection zoom + page-snap).** A new workspace *display*
  axis, orthogonal to per-window fullscreen and to Overview. `Workspace`
  gained `viewport_mode: ViewportMode { Normal, Zoomed }` plus `page_zoom`
  / `page_zoom_target` (both animated by the `tick_animations` spring).
  `Mod4+=` / `Mod4+-` zoom the ribbon in/out (`alpha > 1` in
  `ribbon_geom`, so columns grow — no upper clamp, unlike Overview's
  `zoom`, which only shrinks); dropping below `1.0` returns to `Normal`.
  `Mod4+]` / `Mod4+[` do a `PageSnap` (scroll the camera by one screen,
  reusing `ideal_scroll`/`camera`, without changing focus). This is
  workspace state, not window/EWMH state, so it's never confused with
  fullscreen.
- **`ToggleMaximize` reachable from keyboard/IPC (bug C18).** New
  `Action::ToggleMaximize` + command + `SetMaximized` handler +
  `MaximizeToggled` event, parsed over IPC as `toggle-maximize`, with
  default keybind `Mod4+Shift+m`. Previously the *peek* presentation
  model only activated via a client's own `_NET_WM_STATE`.
- **Rounded corners, no compositor required.** New `general.corner_radius`
  (default `0`, disabled) shapes every managed window's outer edges
  (content + border) via the X11 Shape extension's bounding mask —
  `x11rb`'s `shape` feature, no cairo/pango, no new runtime dependency.
  Implemented as an O(radius) list of `Rectangle`s (one middle band plus
  one 1px row per corner pixel, inset by that row's circle chord), applied
  in `apply_geom` only when `corner_radius > 0` — with the default, not a
  single Shape request is ever sent. Composes fine with picom if you're
  already running one for shadows/opacity/animations.
- **Split inner/outer gaps, plus smart gaps.** `gaps` is now `gaps_inner`
  (between windows/columns) and `gaps_outer` (screen edges), configurable
  independently; the legacy `general.gaps` TOML key still sets both at
  once. New `general.smart_gaps` collapses gaps to `0` when a workspace
  has exactly one tiled window (border width is untouched). Column layout
  only applies `gaps_outer` on the vertical axis, since it scrolls
  horizontally and has no fixed left/right screen edge; Grid, which
  doesn't scroll, applies it on all four sides.
- **Named color-theme presets.** `general.theme` in the TOML config
  (`catppuccin-mocha`, `catppuccin-latte`, `gruvbox`, `nord`, `dracula`,
  `everforest`, `solarized`) fills `col_normal`/`col_focused`/`col_urgent`
  from `config::theme_palette`. Applied before `[colors]` in the merge
  order, so an explicit `[colors]` entry always wins field-by-field over
  the theme — pick a preset and tweak just one color if you want.
- **Per-app cosmetic rule overrides.** `Rule` gained `opacity: Option<f32>`
  (written once at manage time as `_NET_WM_WINDOW_OPACITY`, a no-op
  without a compositor, applies to tiled and floating windows alike) and
  `border_w: Option<u32>` (overrides border width for that app —
  **floating windows only**; tiled/column geometry keeps one uniform
  border width across the whole layout, since the column-width/row-height
  formulas in `core/layout.rs` assume a single shared value per column).
  Both are settable per-rule in `config.toml` (`opacity`,
  `border_width`/`border_w`).
- **Pluggable layout trait + `LayoutRegistry`.** The monolithic
  `match layout { Column => ..., Grid => ... }` in `core::layout::arrange`
  is gone. Layouts implement the `Layout` trait (`name`, `arrange`) and
  register themselves in `LayoutRegistry`, which `LayoutKind` maps into.
  `LayoutKind` derives `Hash` for the registry's `HashMap`. Adding a
  layout still needs a `LayoutKind` variant, a parser entry, and a short
  name in `ipc::layout_name()`, but the arrangement logic itself is now a
  single trait implementation instead of a growing match.
- **Optional TOML configuration.** Maverick now reads
  `$XDG_CONFIG_HOME/maverick/config.toml` (falling back to
  `~/.config/maverick/config.toml`) layered over the compiled defaults,
  with per-section overrides for `[general]`, `[colors]`,
  `[[keybindings]]`, `[[rules]]`, and `[autostart]`. Loading is fail-safe:
  a missing file is ignored silently, one that fails to parse is rejected
  whole (falls back to the compiled config), and an individual entry with
  an unknown key or a broken action string is dropped with a warning — a
  bad config can never prevent the WM from starting. User keybinds that
  contain a digit disable the default `super+1..9`/`super+shift+1..9`
  workspace auto-bindings for that digit; everything else keeps
  auto-generating as before.
- **Real configuration hot-reload.** `ControlCommand::Reload` (via
  `maverickctl reload`) now re-reads the TOML from disk through the same
  fail-safe loading path, swaps the engine config, regrabs the keymap, and
  re-arranges every monitor — no restart needed. A tag-count change
  reconciles every monitor's workspace list (grows/truncates, clamping any
  windows left on a removed workspace) before the redraw.
- **`--config <path>` and `--check-config [path]` CLI flags.**
  `--config` loads the config TOML from an explicit path instead of the
  default location (and is reused on `maverickctl reload`, so a custom
  config survives a hot restart); `--check-config` parses a config and
  exits (`0` = clean, `1` = warnings/errors reported) without ever
  starting the WM, for CI/lint gates.
- **The typed EventBus now drives the `subscribe` wire.** `maverickctl
  subscribe` `focus`/`workspace` lines are produced by a `HubEventSink`
  subscribed to the typed `EventBus`, so pointer-driven focus changes and
  window manage/unmanage announce themselves there too. `publish_state`
  no longer string-diffs a hand-built protocol; it only publishes the
  JSON snapshot when it actually changed. All focus changes funnel
  through the backend's single `focus()` choke point, which publishes
  `FocusChanged` (and `manage()`/`unmanage()` publish window
  mapped/unmapped).
- **Capability Layer (`core::capability`).** A public **read-only** API for
  external consumers (bars, hooks, tests): `Engine::query()` exposes
  `focused_window()`, `active_workspace()`, `visible_windows()`,
  `current_layout()`, `window(id)` → `WindowInfo`. Decoupled from the
  internal `State`/`Client` (the public DTOs are stable) and read-only —
  writes still go exclusively through `Engine::execute(Command)`. A bar
  asks, it doesn't mutate.
- **Typed EventBus (`core::event`).** Implements the audit's
  `Command → Domain Event → Effect` model. Each command declares the
  domain event it represents (`FocusChanged`, `WorkspaceChanged`,
  `LayoutChanged`, `WindowMoved`, `GapsChanged`, etc.) without ever
  knowing its consumers. `Engine` publishes to an `EventBus` that the
  renderer, IPC, future bars, hooks, and tests subscribe to. A new
  consumer subscribes and receives incremental changes instead of polling
  state.
- **Batched transactions (`Engine::execute_batch`).** N commands run as a
  single transaction: they mutate `State`/`Cfg`, domain events publish at
  the end, and **one** `PublishIpcState` coalesces the whole batch —
  fixing "a macro publishing 50 times."
- **Overlay popups are never hidden.** `Client` now remembers
  `transient_parent` (`WM_TRANSIENT_FOR`, captured in `manage()`), and the
  renderer's stacking (`stack_overlay`) raises any dialog/popup whose
  transient chain reaches a fullscreen/maximized window **above the
  overlay** — a menu or file picker from a fullscreen app no longer gets
  stuck behind it. Stacking is now unified in a single helper shared by
  `arrange`, `restack`, and `focus`.
- **Typed command system.** New `core::commands` module defining pure
  commands (`FocusDirection`, `MoveWindowToMonitor`, `SetGaps`,
  `ToggleFloat`, etc.). Each command is a pure transformation over
  `State`/`Cfg` that returns `Effect`s and, optionally, the domain event
  it represents, with no knowledge of X11. Run via `Engine::execute()`.
  `Engine::dispatch(Action)` remains — re-documented as the **canonical
  mapper** from the wire DSL (keyboard, IPC, TOML) to commands:
  `Action::MoveDir` now delegates to the `MoveWindow` command instead of
  building effects by hand, removing a duplicate implementation. Adding a
  new action now touches `types.rs` (variant), `core/commands.rs`
  (command), `core/engine.rs` (dispatch arm), and the parsers
  (`core/ipc.rs`, `userconfig.rs`) — not a single file.
- **Transient windows that map before their parent are relocated to the
  right monitor.** A popup (KakaoTalk / Telegram / a file picker) with
  `WM_TRANSIENT_FOR` pointing at a not-yet-managed window records its
  intended parent and queues in `State::pending_transients`; once the
  parent is managed, `relink_pending_transients` moves it to the parent's
  monitor/workspace, re-floats it, and re-centers it (without duplicating
  it in the ribbon).
- **Presentation overlay decoupled from focus (`core::present`).**
  Fullscreen/maximized is now a *persistent* per-workspace layer: a
  presented window covers the screen (`fullscreen`, border 0) or the
  workarea (`maximized`, border 0) **while its flags are set**, whether or
  not it has focus. `focus()` no longer recomputes geometry on a focus
  change: a focus change now produces **zero** `ConfigureWindow` calls on
  the presented window (removes X11 lag and cascading resizes). Tiles
  underneath still compute the same way (pure layout, no focus-triggered
  reflow), so leaving the overlay returns exactly where things were. New
  *peek* behavior: focusing a normal tile while an overlay is active
  raises that tile above the presented window — without resizing anyone
  — so the focus location stays visible.
- **Uniform row heights within a column (`core/layout`).** `split_bias`
  is gone from the vertical split: all windows in a column now share the
  same height, and focus is marked with border/color only. Moving
  focus up/down between tiles no longer reflows (previously every move
  resized every row in the focused column — the source of navigation
  lag).

### Changed

- `Cfg::gaps` → `Cfg::gaps_inner` + `Cfg::gaps_outer` (breaking for anyone
  constructing `Cfg` directly instead of going through `config.toml` or
  `compiled_config()`).
- `Rule` now derives `Default`; existing struct literals need
  `..Default::default()` to pick up the new `opacity`/`border_w` fields.
- Floating-window border width in `core/layout.rs` is now read from the
  client's own `border_w` (so `Rule::border_w` overrides take effect)
  instead of always using the global `Cfg::border_w`. Tiled windows are
  unaffected — they still use the uniform `Cfg::border_w`.
- **Default column width is now a fraction of the workarea, not pixels
  (breaking, visual).** The old `default_col_w = 700` (pixels) and
  `split_bias` keys are deprecated aliases for the new `column_width` (a
  fraction, `0.1–1.0`, of the workarea width, default `0.6`). `700px` on a
  1920px-wide screen was ~`0.36`; the new default `0.6` makes fresh
  columns noticeably wider. Migration: set `column_width` explicitly in
  `[general]`, or keep `default_col_w` (converted via a 1920px fallback) /
  `split_bias` for now — both now emit a deprecation warning.
- **Fase-0 bug-fix batch (B1–B12).** Unified TOML/IPC action vocabulary
  (`core::action`); workspace binds are auto-generated and only claim the
  slots you override (`auto_workspace_binds`); first-wins keybinding
  conflict policy; X11 keysym lookup fixed to column 0 with a
  shifted-column fallback; expanded keysym name table (F-keys, keypad,
  XF86 media/brightness, symbol keys) with a `0x<hex>` escape. Config is
  never fatal: diagnostics are logged and startup proceeds regardless.
- **`core`: `PublishIpcState` emitted after every state-mutating
  `dispatch`.** Previously the effect existed but was never produced; now
  pushed automatically so IPC subscribers (bars, `maverickctl subscribe`)
  receive fresh snapshots without explicit per-action wiring.
- **`core`: `focus_mon`/`move_mon` now accept directional variants.**
  `focus-mon left`/`right` and `move-mon left`/`right` move in the
  expected direction instead of always wrapping to the next monitor
  (previously the behavior of `next`).
- **`cargo build --release` no longer ships a status bar.** The
  `internal-bar` feature (previously on by default) is gone, so a default
  build now expects an external bar (polybar/waybar/eww) launched from
  `autostart`, relying on the WM's strut reservation. This is a breaking
  change for anyone who relied on the built-in bar — point your
  `autostart` at an external bar (see README).
- **Default config genericized for distribution.** `config.rs`'s
  `load_config()` carried a maintainer's personal machine setup — a
  hardcoded Dvorak `setxkbmap` autostart entry, a wallpaper launched from
  a home-directory path, and an unrelated personal DNS tool — none of
  which mean anything on a fresh install. All three are removed; the
  shipped `autostart` now only launches the `xdg-desktop-portal(-gtk)`
  pair needed for file-picker dialogs to work, with a commented example
  showing where to add your own wallpaper command. Also dropped the
  `polybar` autostart entry, which duplicated the `internal-bar` feature
  that used to be on by default.

### Fixed

- **(unconfirmed) Scroll-culling was deleting windows (opening 3+ lost
  one).** `hide_offscreen` unmaps columns outside the viewport, but since
  `SUBSTRUCTURE_NOTIFY` is selected on root, the WM itself received that
  `UnmapNotify`; `on_unmap` interpreted it as the client withdrawing and
  called `unmanage`, so the window was deleted from the layout and
  invisible forever. With 3 tiled windows, column 0 already falls more
  than `cull_margin` offscreen and vanished. The WM now keeps an
  `ignore_unmaps` counter (incremented before each self-initiated
  `unmap_window`), and `on_unmap` discards the `UnmapNotify` reflected on
  root (`e.event == self.root`) without touching the client; the
  duplicate event targeted at the window itself is still processed.
  Regression test: `three_columns_push_first_offscreen_under_cull_margin`.
- **(unconfirmed) `GrowCol` panicked with 21+ columns.** The clamp's upper
  bound was `1.0 - 0.05*(n-1)`, which drops below the `0.05` floor past 21
  columns; `f32::clamp` asserts `min <= max` and the WM died (debug and
  release) on the default `Mod4+Ctrl+H/L` keybind. The bound is now
  `(1.0 - 0.05*(n-1)).max(0.05)`, so `min <= max` always holds. Regression
  test: `grow_column_does_not_panic_with_many_columns` (25 columns, both
  directions).
- **(audit, fullscreen/transient) Fullscreening a floating window broke
  everything.** When a floating client entered fullscreen, `set_fullscreen`
  didn't promote it into the tiling tree before setting the `FULLSCREEN`
  flag (the keyboard path did this via `apply_fullscreen_topology`, but the
  EWMH path did not), so the floating window ended up with a zero `geom`
  and video players (mpv, etc.) simply disappeared. Both paths (keyboard
  and EWMH) now share `apply_fullscreen_topology`, which promotes the
  float into the ribbon, saves its floating rect in `saved_geom` (at the
  right moment, before `arrange` overwrites `geom`), and is idempotent.
  Regression tests: `ewmh_fullscreen_promotes_float`,
  `fullscreen_topology_is_idempotent`,
  `tiled_window_entering_fullscreen...`.
- **(audit) `_NET_WM_STATE_MAXIMIZED_VERT` no longer promotes to full
  maximize.** `MAXIMIZED` used to be a single bit; a vertical-only
  maximize request ended up filling the whole workarea. It's now two
  independent bits, `MAXIMIZED_V` / `MAXIMIZED_H` (with `MAXIMIZED` =
  both), and `is_maximized()` requires both; `core::present` only
  stretches the requested axes via `maximized_rect`, and the overlay only
  activates for the focused window. `ToggleMaximize` still toggles both
  axes together. Tests: `maximize_vertical_only_stretches_y`,
  `maximize_horizontal_only_stretches_x`.
- **(audit) `ConfigureRequest` / `WM_NORMAL_HINTS` reviewed, no change
  needed.** Current handling was already correct and conservative: a
  tile's `ConfigureRequest` is swallowed and answered synthetically
  (`on_configure_request`), and a client with fixed-size
  `WM_NORMAL_HINTS` is already marked `FIXED`+`FLOAT` at map time
  (`manage.rs`). Left untouched to avoid risking resize loops; acceptance
  criterion was "no regressions," and behavior remains pinned by the
  existing `present`/`arrange` tests.
- **(unconfirmed) `ToggleFullscreen` from the keyboard didn't apply the
  state.** The command mutated `WinFlags::FULLSCREEN` and then emitted
  `SetFullscreen`, but the `set_fullscreen` handler early-returns when the
  flag already matches — so `_NET_WM_STATE` was never written,
  `_NET_WM_BYPASS_COMPOSITOR` was never set (picom kept shadowing
  fullscreen windows), `saved_geom` was never saved, and a floating window
  got stuck at screen size on exit. The command no longer mutates the
  flag itself; that's left to the effect (bug C3). Same fix applied to the
  new `ToggleMaximize`.
- **(unconfirmed) Overview didn't move real focus (bug C1/C4).**
  `OverviewNav` / `OverviewEnter` moved `ws.focus.column_idx` and only
  emitted `ArrangeMonitor`, leaving `ws.focus.column_idx` out of sync with
  `mon.focused` (which also broke the anti-culling protection for the
  focused column). Both commands now emit `FocusWindow` for the selected
  window.
- **(unconfirmed) Camera animation on window open was dead (bug C5).**
  `manage` computed `was_empty ? snap : target` and then did an
  unconditional `snap` that overwrote it; the second call is gone, so
  opening a window while others are present now animates the camera
  instead of teleporting it.
- **(unconfirmed) `GrowColumn` stole width from neighboring columns (bug
  C7).** In a scrolling layout, column weights are independent and
  growing one shouldn't touch the others (the ribbon should get longer and
  the camera scroll). The command now only adjusts the focused column's
  `weight`, clamped to `max >= 0.05`; it also no longer early-returns with
  a single column, so a lone window can be resized too.
- **(unconfirmed) `MoveToWorkspace` and `ToggleFloat` left the camera out
  of sync (bug C8).** Neither recomputed `ideal_scroll`, so the ribbon
  stayed scrolled past its new width. Both now go through the
  `scroll_to_focused` helper.
- **(unconfirmed) Mouse-wheel camera scroll (bug C9).** `on_button_press`
  discarded buttons 4–7; `Mod4 + wheel` now moves the column focus one
  slot per notch (via `FocusDir`), which recenters the camera — the
  paradigm's signature interaction, previously unreachable.
- **(unconfirmed) Per-frame stacking storm / performance (bug C6).**
  `stack_overlay` re-emitted `raise()` for every float/sticky on every
  animation frame (arrange runs on every monitor at ~125fps). It now
  caches the desired order per monitor (`last_stack_order`) and only
  re-emits when it changes. Dead code `restack` / `stack_dirty` /
  `do_restack` (identical to `stack_overlay`, which already runs in
  `arrange_full`) was removed as a consequence.
- **Cleanup (bugs C11–C13).** Corrected the `cleanup_empty_columns`
  comment (no longer says "re-normalize" — `rebalance_weights` only
  repairs weights ≤0); `split_bias` is now documented as "width fraction
  of the workarea for new columns" instead of "extra height for the
  focused row."
- **(unconfirmed) Column widths now animate on focus change (glide, not
  jump) (bug C10).** `Workspace` gained a per-column animated `boost: f32`
  (previously a single global `accordion` scalar that only moved on
  Overview enter/exit); `tick_animations` relaxes each column toward
  `1.0` when focused and `0.0` when not, so `focus-right` now glides the
  ribbon instead of jump-cutting, while the camera glides too.
  `ribbon_geom` reads `c.boost` per column (forced to `0` in Overview so
  every column fits in the strip).
- **(unconfirmed) Unified the "new column" width policy on
  `default_col_w` (bugs C14, C16).** `NewColumn`, orphan re-homing on
  hotplug (`events.rs`), and every `add_tiled` call site now create
  columns at `default_col_w` (a workarea fraction), removing five
  different policies (70/30, 50/50, `split_bias`, inherit, …) that
  previously coexisted. `ribbon_geom` no longer discounts gaps from
  `usable_w`: each column's width is a fraction of the full workarea and
  *independent of the total column count*, so adding a column no longer
  shrinks the others.
- **(unconfirmed) `FocusDirection`/`MoveWindow` blocked during fullscreen
  — regression vs 0.18.2.** The refactor had added a `focused_fs` guard
  that turned both commands into no-ops while the focused window was
  fullscreen; 0.18.2 never had that guard (`engine.rs::focus_dir`/
  `move_dir` never checked `is_fullscreen()`). The guard wasn't just an
  input regression, it also made the *peek* mode already implemented and
  tested in `core::present`/`render.rs`
  (`fullscreen_persists_while_unfocused`,
  `test_fullscreen_unfocused_layering`) unreachable by keyboard — the
  fullscreen overlay is meant to stay put while focus moves around/under
  it, but without being able to move focus, that path could never be
  exercised. The guard is removed from `FocusDirection` and `MoveWindow`
  in `core/commands.rs`; the intentional mouse click/drag block on a
  fullscreen window in `pointer.rs` is unchanged (it existed the same way
  in 0.18.2). Tests updated:
  `test_focus_direction_blocked_in_fullscreen` /
  `test_move_window_blocked_in_fullscreen` →
  `..._allowed_in_fullscreen`.
- **(unconfirmed) Rounded corners on fullscreen windows (niri-style).**
  `round_corners` ignored window state and always applied
  `cfg.corner_radius`; in fullscreen (border 0, geometry = screen) this
  cut the content under a curved mask instead of showing desktop behind
  it — there's nothing to round "toward," so it just looked broken.
  `round_corners` now takes the effective radius as a parameter;
  `apply_geom` captures `is_fullscreen()` before the mutable borrow and
  passes `0` (square mask, edge-to-edge) while fullscreen, returning to
  the configured radius as soon as it exits.
- **`EnterNotify`/keyboard race guard (focus-follows-mouse).** Keyboard
  navigation now arms a 50ms window (`pointer_guard_until`); any
  `EnterNotify` generated by the pointer's position within that window is
  ignored, and only the first real `MotionNotify` lifts the guard.
  Navigating with `Mod+Up/Down` no longer "slips" onto a neighboring
  window the cursor happens to be touching.
- **`WM_TAKE_FOCUS` now uses a real ICCCM timestamp.** `send_proto` now
  sends the last input-event timestamp (`last_event_time`, recorded on
  key/button/enter/motion) instead of `CurrentTime`; strict toolkits
  (Swing, some Emacs builds) now accept focus correctly. Also applies to
  `kill()`'s `WM_DELETE_WINDOW`.
- **`MapRequest` with an active fullscreen/maximized overlay
  (anti-focus-steal).** If the new `MapRequest` is a `WM_TRANSIENT_FOR`
  dialog of the presented window (e.g. Ctrl+S from a fullscreen app), it
  takes focus and raises above the overlay; any other window joins the
  tiling tree silently — without `focus()`, the overlay keeps focus and
  stacking, and the new window is marked
  `_NET_WM_STATE_DEMANDS_ATTENTION` (urgency, highlighted border), cleared
  once it's focused.
- **Floating windows never opened centered, and could land fully
  off-screen if created mid workspace-switch.** `manage()` trusted the raw
  X geometry captured when a window is created. Toolkits center dialogs
  relative to their parent's *current on-screen* position — if the parent
  happened to be off-screen at that exact instant (see `hide_offscreen()`
  in `backend/x11/render.rs`, which parks hidden-workspace windows at a
  negative x rather than unmapping them), the new dialog inherited that
  bogus position, got clamped to a workarea edge, and effectively
  vanished. Portal-spawned file pickers (no real `WM_TRANSIENT_FOR`) never
  had a sane position to begin with. Maverick now computes floating-window
  position itself: centered on the transient parent's real *stored*
  geometry when there is a parent, otherwise centered in the assigned
  monitor's workarea — width/height from the original request are kept,
  only position is recomputed.
- **RandR monitor hotplug could go unnoticed.** Some X servers only
  deliver RandR events, not the root `ConfigureNotify` Maverick was
  relying on, so a plugged/unplugged monitor left stale geometry until a
  full restart. Maverick now calls `RandrSelectInput` on the root and
  handles `RandrNotify` / `RandrScreenChangeNotify` through the same
  topology re-detect path (guarded by an "actually changed" check, so no
  needless reflows).
- **`ConfigureRequest` dropped `above_sibling`.** Restack requests that
  position a window above a specific sibling (used by docks and some
  compositor helpers) were ignored — only `STACK_MODE` was honored. The
  `SIBLING` value-mask bit is now passed through to `configure_window`.
- **A maximized window's border could poke off-screen.** The `maximized`
  presentation kept the client's border: since X11 draws the border
  *outside* the `(x,y,w,h)` rect (`xproto` semantics), a window with
  `border > 0` invaded reserved/adjacent monitor pixels. The `maximized`
  overlay now applies border `0` over the workarea, same as `fullscreen`
  — it never pokes off-screen and respects reserved regions.
- **An unmapped overlay window left its stacking dirty.** If a
  fullscreen/maximized window closed, crashed, or unmapped while
  unfocused, its `WindowId` stayed in the client list until
  `DestroyNotify`; any `restack`/`arrange` could still project or raise
  it as if it existed. `on_unmap` now instantly purges (`unmanage`) any
  presented window that unmaps — tiled/floating windows keep ICCCM
  behavior (they stay withdrawn until destroy/re-map). The `BadWindow`
  risk is gone.
- **Focus fallback ignored the overlay.** Closing a tile in *peek* mode
  (focus on a tile while a fullscreen window covers the screen) used to
  drop focus onto an invisible tile under the overlay. `best_focus` now
  prefers the workspace's most-recent fullscreen/maximized window over
  the column/stack — closing the peeked tile now returns focus to the
  presented window.
- **`_NET_WM_BYPASS_COMPOSITOR` directive for external compositors.**
  Entering fullscreen now writes `_NET_WM_BYPASS_COMPOSITOR = 2` ("bypass
  while fullscreen") and clears it on exit. picom & co. stop
  redirecting/shadowing video or games in fullscreen — less input lag,
  more FPS.
- **Build was broken on `main`: the `Monocle` removal had been done
  halfway and took unrelated code with it.** An in-progress edit had
  deleted `LayoutKind::Monocle` from `types.rs` but left `config.rs`,
  `core/ipc.rs`, and `core/tests.rs` still referencing it (wouldn't
  compile). Worse, the same edit had accidentally deleted `arrange_grid()`
  and `ideal_scroll()` from `layout.rs` entirely along with
  `Workspace.scroll`, and rewrote the column-position formula from
  `wa.x - ws.scroll` to a fixed `wa.x` — silently disabling the Column
  layout's horizontal scrolling. All of the above is restored; `Grid` and
  scrollable `Column` both work again, and Monocle is now fully (not
  partially) gone.
- **`CHANGELOG.md` contained an unresolved git merge-conflict marker**
  (`=======`) followed by a duplicate copy of the keyboard-freeze fix
  entry already documented above it. Removed the marker and the duplicate
  section; no information was lost, the content was a verbatim repeat.
- **`clippy::new_without_default` on `State::new()`.** Added `impl
  Default for State` (`fn default() -> Self { Self::new() }`).
  Pre-existing before the internal-bar removal; caught while re-verifying
  against the exact 1.82 MSRV toolchain.
- **`maverick-sys`: control socket could be tricked by a symlink
  attack.** `remove_file` ran before `bind` without checking the existing
  file's type; a symlink pointing outside the runtime dir would be
  followed. Now only removes the path if it's a regular socket. Also:
  unbounded thread creation per connection is now limited to 32 concurrent
  handlers; `identity_json` now escapes all JSON-special characters
  instead of only quotes and newlines; `send_command` rejects commands
  containing `\n` to prevent line-protocol injection.
- **`maverick-sys`: the identity ficha parser failed on process names
  containing `)` or commas in field values.** `/proc/<pid>/stat`'s second
  field (comm) is enclosed in parentheses, but the comm itself may
  contain `)`. Switched from `find(')')` to `rfind(')')`. The custom JSON
  parser split on `,` unconditionally, breaking when a string value
  contained a comma; replaced with a char-by-char walker that respects
  JSON string quoting.
- **`maverick-sys`: `wait_readable` busy-looped on `POLLERR`/`POLLHUP`.**
  `poll()` returning `> 0` was treated as "data available" regardless of
  `revents`. Now checks that `POLLIN` is actually set, so an error state
  doesn't spin the event loop.
- **`UnmapNotify` no longer removes windows from the workspace.**
  Previously, every `UnmapNotify` (e.g. iconify) called `unmanage()`,
  removing the window from `clients`, the workspace structure, and the
  focus stack. When later remapped, it was re-managed as a brand-new
  window, losing its workspace assignment, floating state, and column
  position. Now, non-synthetic `UnmapNotify` events only clear `WM_STATE`
  and move focus if the window was focused. The window stays in the
  workspace so its tiling state survives iconify/restore.
- **`FocusIn` handler no longer steals focus from popups and dialogs.**
  `on_focus_in` used to re-focus the WM's focused window whenever *any*
  window received `FocusIn`. This caused popups and dialogs (e.g. Firefox
  file pickers, GTK dialogs) to immediately lose focus back to the main
  window. The handler is removed entirely; focus is now managed
  exclusively through keybindings, mouse clicks, and EWMH requests
  (`_NET_ACTIVE_WINDOW`).
- **Moving a window to another monitor no longer panics on workspace
  overflow.** When moving a window to a monitor with fewer workspaces
  than the source, the workspace index could exceed the destination
  monitor's workspace count, causing a panic. The index is now clamped to
  the destination monitor's valid range.
- **`_NET_WORKAREA` now reports all monitors.** Previously only the first
  monitor's workarea was reported for every desktop, giving incorrect
  workarea values to external taskbars/docks on secondary monitors in
  multi-monitor setups.
- **Monitor hotplug preserves client workspace assignments.** When the
  monitor count changes (hotplug), clients are no longer blindly
  reassigned to monitor 0 / workspace 0. Their original monitor and
  workspace assignments are preserved where the target still exists; only
  clients on removed monitors are reassigned to valid targets.
- **Geometry-only monitor changes now trigger a rearrange.** When a
  monitor's resolution or position changes without monitors being
  added/removed, the previous code only updated `screen` and `workarea`
  without calling `arrange()`, leaving windows with stale geometry. All
  affected monitors are now re-arranged after a geometry-only change.
- **`focus_mouse` no longer triggers an X11 `query_tree` round-trip on
  every motion event.** `on_motion` called `find_client()` (which walks
  the window tree via `query_tree`) for every mouse movement when
  `focus_mouse` was enabled, causing noticeable lag. Focus-follows-mouse
  is now handled exclusively via `EnterNotify` in `on_enter`, which fires
  far less often.
- **`focus()` no longer computes `prev_focused` twice.** The
  previously-focused window was computed at the top of the function and
  again just before the unfocus logic. The redundant second computation
  is removed.
- **`focus_dir` Next/Prev now filters by active workspace.** The focus
  stack could contain windows from different workspaces, so cycling
  Next/Prev could jump to a window on a different workspace without
  switching to it, leaving the user unsure which workspace they were on.
  Only windows on the active workspace are considered now.
- **`restart()` now cleans up the control socket before `exec()`.**
  Previously `exec()` ran without removing the Unix socket file or the
  identity ficha, which could prevent the new process from binding the
  socket on restart. Both are now removed before `exec()`.
- **Removed dead code `Focus.window_idx`.** Set in multiple places but
  never read for layout or focus determination — the actual focused
  window in a column is `Column.focused`, not `Focus.window_idx`. The
  field and all references to it are removed.
- **`maverick-sys`: `detach_from_terminal` ignored `setsid()`
  failure.** If the process was already a session leader, `setsid()`
  returns `EPERM` and the WM wasn't actually detaching. The return value
  is now discarded explicitly (the subsequent `isatty` check still
  works), making the intent clearer instead of silently depending on
  success.
- **`maverick-sys`: `detach_from_terminal` no longer calls `setsid()` at
  all.** Under `startx`, Maverick is a child of the same login session
  that owns the VT/seat Xorg is running on; forcing a brand-new POSIX
  session here correlated with Xorg losing its DRM master mid-startup
  (`EnterVT failed`, `Failed to enable any CRTC`) right as autostart
  kicked in — a separate build without the `setsid()` call did not
  reproduce it on the same hardware/Xorg/kernel. Maverick doesn't fork
  away from its parent, so it doesn't need a new session; staying in the
  launching session avoids touching seat/session assignment that Xorg
  depends on. The stdin/stdout redirect (so a display-manager-less
  `startx` launch doesn't hang its shell) is kept, and now only runs when
  stdin is a real tty.
- **`maverick-sys`: `hub::emit` held the subscriber mutex during channel
  sends.** A slow `subscribe` connection could block the WM thread. The
  subscriber list is now cloned under the lock, with the actual sends
  happening outside it.
- **`maverickctl`: TTY confirmation read input byte-by-byte, breaking
  UTF-8 multi-byte characters.** `read(&mut [0u8;1])` plus `as char`
  produced garbled strings for non-ASCII input. Replaced with `read_line`
  for correct Unicode handling.
- **`core`: `CycleLayout`/`SetLayout` could panic on a monitor-less
  state.** Both actions indexed `self.state.monitors[mi]` without
  checking the index was in bounds. Added the same guard used by
  `ToggleBar` and other actions.
- **`core`: `collapse_col` computed ideal scroll before collapsing,
  leaving the viewport slightly off-center.** Moved the `ideal_scroll`
  call to after the column is removed so it reflects the new column
  count.
- **`core`: `focus_mon`/`move_mon` treated `Dir::Left` and `Dir::Right`
  identically to `Dir::Next`** (always wrapping right). They now map
  `Left`/`Prev` to decrement and `Right`/`Next` to increment, matching
  user expectations.
- **`core`: missing `UpdateBar` effects after workspace/view changes.**
  `View`, `MoveToWs`, `CycleLayout`, and `SetLayout` didn't mark the bar
  dirty, so the tag-active/layout-symbol/occupancy display could go
  stale. `Effect::UpdateBar` is now added on each path.
- **`core`: `PublishIpcState` was never emitted.** The effect variant
  existed but no dispatch path produced it. Now pushed at the end of
  every `dispatch()` that produced at least one effect.
- **`core`: floating windows weren't clamped to the workarea in
  `arrange_columns`.** The floating pass pushed `client.geom` verbatim;
  windows could end up placed entirely off-screen. Added a clamp to the
  workarea rect.
- **`core`: `Client::new` always initialized `tags: 1`**, ignoring the
  `workspace` parameter. Changed to `tags: 1 << workspace` so the tag
  mask matches the assigned workspace from creation.
- **`core`: `Rule::matches` compared a lowercased `class`/`title` against
  an un-normalized pattern.** A rule written with uppercase letters would
  never match. The pattern is now also lowered before comparison.
- **`main`: identity ficha left on disk if `WindowManager::new`
  failed.** `write_meta` runs before WM initialization; a subsequent init
  failure called `process::exit(1)` without cleaning up the ficha,
  leaving a zombie entry for tools like `maverickctl list`. Added a
  `cleanup_meta` call on the error path.
- **`x11/events`: resolution change wasn't detected when the monitor
  count stayed the same.** The RandR notify handler only acted when
  `new_mons.len() != old count`; a resolution or position change that
  kept the same monitor count was silently ignored. Added a per-monitor
  geometry comparison.
- **`x11/manage`: `find_client` could loop infinitely on a cyclic window
  tree.** It walked the X11 window tree upward without tracking visited
  windows; a client creating a parent cycle would hang the WM. Added a
  `HashSet` guard.
- **`x11/render`: `ConfigureNotify` coordinates were silently
  truncated.** `hide_offscreen` pushes windows far left (`i32::MIN`),
  which wrapped to `0` when cast to `i16`, making offscreen windows
  visible. Values are now clamped to `i16`/`u16` ranges before casting.
- **`x11/render`, `ewmh`: potential panic on an empty monitor list.**
  `focus()` and `update_workarea` indexed `monitors[0]` or assumed
  `client.monitor` was always valid. Added bounds checks / `.first()`.
- **`x11/input`: keyboard froze after mouse-focusing a window
  (`grab_buttons`).** The catch-all `grab_button` used
  `pointer_mode=SYNC` **and** `keyboard_mode=SYNC`. Every matching
  `ButtonPress` froze both devices, but `on_button_press` only called
  `allow_events(REPLAY_POINTER)`, which releases the pointer but not the
  keyboard — leaving the keyboard frozen at the X11 level after clicking
  any managed window, breaking WM shortcuts and the client's own key
  input (most noticeable with apps that grab focus aggressively on click,
  like Firefox or Minecraft). `keyboard_mode` is now `ASYNC` (standard
  practice, matches dwm/i3-style click-to-focus grabs); `pointer_mode`
  stays `SYNC` since `on_button_press` still needs to conditionally
  replay or keep it frozen for drags. Confirmed fixed against real usage
  (Firefox, Minecraft).
- **`x11/manage`: `write_net_wm_state` overwrote unknown EWMH atoms.**
  It replaced `_NET_WM_STATE` with only the fullscreen/maximized flags the
  WM tracks, discarding `_NET_WM_STATE_STICKY`, `_NET_WM_STATE_HIDDEN`,
  etc. set by other tools. Now reads the current atom list first and
  preserves unmanaged atoms.
- **`backend/bar`: potential `u16`/`i16` overflow in label and glyph
  calculations.** Arithmetic on `u16`/`i16` values could wrap with many
  wide tags. Converted to `i32` intermediates with saturating operations
  and a final clamp to the target type. (Historical fix, kept for the
  record — the internal bar itself is removed in this release, see
  below.)

### Removed

- **`serde` + `toml` dependencies; new zero-dependency `maverick-toml`
  crate.** The config parser is now a local strict TOML-subset crate
  (`maverick-toml`) with **zero external dependencies**, replacing
  `serde 1.0.229` and `toml 0.8.23` (and their transitive `winnow
  0.7.15`, `indexmap 2.14.0`, `serde_spanned`, `toml_datetime`,
  `toml_edit`). `src/userconfig.rs` was rewritten to consume its event
  iterator (`Section` / `ArraySection` / `KeyValue`) instead of `serde`
  derives, preserving the same fail-safe contract (syntax error → whole
  file rejected → compiled defaults; semantic errors dropped per-entry
  with a warning) and all value aliases (`border_w`/`border_width`,
  `col_normal`/`normal`, `type`/`window_type`, `ws`/`workspace`,
  `commands`/`apps`/`programs`, …). Supported syntax:
  `[section]`/`[[tables]]`, plain key = `value`, ints (negative), `0x…`
  hex, floats, booleans, basic strings `"…"`, flat string/int lists, and
  nested `autostart`-style grids; single-quoted strings, dotted keys, and
  exports are rejected. **Binary shrinks ~21%** on the same release
  profile (stripped): the config-parsing layer measured in isolation
  drops from 614.8 KB (serde+toml) to 357.1 KB (maverick-toml), and the
  stripped `maverick` binary with identical functionality measures
  934,160 B.
- **Dead code and dead atoms.** `Client::is_dialog`, `Client::tags`/
  `TagMask`, the empty `on_focus_in` stub, `Layout::handle_action`, never-
  emitted `Effect` variants (`ArrangeAll`, `MapWindow`, `UnmapWindow`,
  `UpdateEwmhDesktops`, `UpdateClientList`), and ~40 atoms that were
  interned and advertised but never read are gone. `_NET_SUPPORTED` now
  lists only the atoms the WM actually acts on, and the
  `#[allow(dead_code)]` escape hatch is removed.
- **Duplicate string-escape logic.** Two private `json_escape`/`unquote`
  copies (CLI and core IPC) are replaced by `maverick_sys::json` —
  including a real `\uXXXX`-aware `json_unescape` that the previous
  implementation mishandled.
- **Compositor orchestration removed from the WM's startup sequence, and
  `startup_sound` dropped entirely.** `main.rs` no longer spawns a
  compositor before `WindowManager::new()`, waits a fixed delay for it to
  attach, or plays a startup chime — that was three phases of bespoke
  process-spawning logic for something `autostart` already does for the
  bar, wallpaper, and everything else. `Cfg::compositor`,
  `compositor_delay_ms`, and `startup_sound` are gone; put your
  compositor in `autostart` like any other program (see README).
- **`Monocle` layout removed entirely.** It never left an experimental
  state and added a third code path to every layout-dispatching site for
  little benefit over `Grid`. Removed `LayoutKind::Monocle` and
  `arrange_monocle()`, the `Super+M` keybind, the `monocle` IPC/CLI
  layout name (`maverickctl dispatch layout monocle` no longer parses),
  and all related tests/docs. `cycle_layout()` now wraps Column→Grid→
  Column. Only two layout modes ship: **Column** (the niri-style
  scrollable layout, stable) and **Grid**.
- **Internal bar removed.** Drawing a status bar isn't the window
  manager's job — it duplicated what polybar/waybar/eww already do well,
  and its removal drops the plain X11 core-font rendering path
  (`open_font`/`query_font`/`image_text8`/`to_latin1`) entirely. Removed
  `src/backend/bar.rs` and `src/backend/x11/bar.rs`, the `internal-bar`
  Cargo feature, the `Bar` struct, `Action::ToggleBar` (+ its `Super+B`
  keybind and `toggle-bar` IPC verb), the
  `Effect::UpdateBar`/`SyncBarVisibility`/`RecalcWorkarea` variants, and
  the `Cfg`/`Monitor` bar fields (`bar_height`, `top_bar`, `col_bar_*`,
  `internal_bar_height`, `show_bar`, `bar_win`, `bar_gc`). Maverick still
  reserves screen space correctly for any external bar via
  `_NET_WM_STRUT_PARTIAL` (`backend/x11/struts.rs`, untouched); root
  `WM_NAME` is still read into `state.status` and exposed over IPC for
  external bars. See README for a polybar example.

### Quality

- **Enforced `rustfmt` across the workspace** — formatted every crate
  with `cargo fmt` to a consistent style.
- **Fixed all `clippy` warnings** — resolved 10 lints across `bar.rs`,
  `manage.rs`, `engine.rs`, `types.rs`, and `ipc.rs` (`map_unwrap_or`,
  `doc_markdown`, `redundant_closure_for_method_calls`, `match_same_arms`,
  `unnecessary_min_or_max`). Clippy is now clean at `-D warnings`.
- **Clean `rustdoc` build** — fixed unclosed HTML tags (`<pid>`, `<px>`,
  `<n>`, `<cmd>`) in doc comments; docs now build with
  `RUSTDOCFLAGS="-D warnings"`.
- **Expanded `.gitignore`** — added `coverage/`, `*.profraw`, `.env`,
  editor swap files, and common Rust build artifacts to prevent
  accidental commits.
- **Added `rust-version` and metadata** — `Cargo.toml` for all workspace
  crates now declares `rust-version = "1.82"`, `repository`,
  `categories`, and `keywords` for better crate-index presentation.
- **Doc-comment fixes** — `image_text8`, `draw()`, and code samples in
  docstrings now use proper backtick quoting.
- **CI workflow added (`.github/workflows/ci.yml`).** Two jobs: the main
  WM workspace (clippy `-D warnings` + `cargo test --workspace`), and a
  separate job for `maverick-installer`, which is intentionally excluded
  from the main workspace (`Cargo.toml`'s `exclude`) so its build/lint
  gate can't slow down or bitrot the WM's own CI.
- **Xephyr integration test harness (`tests/xephyr-suite.sh`).** A manual
  / CI script that spins up a throwaway nested X server (Xephyr), launches
  Maverick on it, and drives real clients (xterm, firefox, mpv, a GL game)
  to verify fullscreen/transient/viewport behavior end-to-end via
  `xprop`/`xwininfo`/`xev`. Needs a real nested X server, so it doesn't run
  under `cargo test` in CI by default; every assertion reads live X
  properties, nothing is faked.
- **Optional `maverick-installer` binary.** A standalone install helper,
  excluded from the main Cargo workspace (see `Cargo.toml` and the CI
  note above) so it never slows down the WM's own build/test loop.

CHANGELOG entries above marked **(unconfirmed)** or **(audit)** carry
over the original session's caveat notation and remain to be re-verified
against a real `cargo build --release` on target hardware.

## [0.18.2] — 2026-07-19

Two prior attempts at the next release (internally called "0.18.4" in
early planning) added a stack of new features — TOML config, a
"Window" floating layout, a predictive prefetch daemon — but both were
abandoned after serious regressions during development, including one
where `backend/x11/mod.rs` was lost outright to an accidental
`git checkout --` and had to be reconstructed from an old blob. Rather
than resume that feature list, this release starts over from `main`
(v0.18.1) with a narrower goal: **pay down the coupling between the
domain model and X11** so a non-X11 backend (Wayland) becomes possible
later, without adding user-facing features. No TOML config, no new
layout modes, no prefetch daemon in this release — that work is
shelved, not lost, and can be revisited once the split below is
further along.

### Added

- **Instance control plane** (`maverick-sys`, new workspace member):
  `identity` (per-instance PID/display/tty "ficha" under the runtime
  dir), `control` (`ControlServer` — a Unix-socket protocol:
  `ping`/`identify`/`state`/`dispatch`/`restart`/`reload`/`subscribe`/
  `quit`), `hub` (`ControlHub`, the MPSC bridge between the socket
  thread and the single-threaded X11 event loop), `discover`
  (list/find/quit instances by name or display). Replaces the old PID
  file + `pkill`-by-name approach from the abandoned line.
- **`maverickctl`** (`maverick-sys/src/bin/`): CLI for the above —
  `list|state|msg|subscribe|quit[--confirm]|quit-all|restart|reload|prune`.
  Instance resolution: `--name` → `$MAVERICK_INSTANCE` → sole live
  instance → refuse/ambiguous list.
- **`maverick-dialog`** (new workspace member): standalone X11
  yes/no confirmation window, the only `x11rb` user outside the WM
  itself. `Mod4+Shift+Q` now spawns `maverickctl quit --confirm`
  instead of calling `Action::Quit` directly, so a stray keypress
  can't kill the session; the raw `Action::Quit` is still reachable
  over the control socket.
- **Maximize** implemented for real: `WinFlags::MAXIMIZED`,
  `Client::is_maximized()`; a maximized-but-not-fullscreen focused
  window fills `workarea` (respects bar/dock struts) and keeps its
  border, vs. fullscreen which covers the whole screen with no
  border. `_NET_WM_STATE_MAXIMIZED_VERT/HORIZ` handled on both read
  (initial `manage()`) and write (`on_client_message`).
- **External dock support**: docks (Waybar/Polybar/etc.) are detected
  by `_NET_WM_WINDOW_TYPE_DOCK`/`_DESKTOP`, never by process name, and
  reserve space via `_NET_WM_STRUT_PARTIAL`/legacy `_NET_WM_STRUT`,
  tracked per-monitor and released on destroy/unmap.
- `internal-bar` Cargo feature (default on): `cargo build --release
  --no-default-features` builds without the internal status bar for
  people driving Waybar/Polybar instead.
  (`1a36561`, `c23087c`)

### Changed

- **`core/` rebuilt around one seam**: `Engine::dispatch(Action) ->
  Vec<Effect>` is now the *only* path from user/IPC intent to state
  mutation. `Effect` is a semantic vocabulary (`ArrangeMonitor`,
  `FocusWindow`, `SetFullscreen`, …) — the backend's `execute()` is
  the only place that turns those into X11 calls. This removes the
  previous split-brain where `backend/x11.rs` reimplemented action
  handling separately from a dead `core/engine.rs::process_event`
  path that only 3 stale unit tests exercised.
- **Fullscreen re-modeled as presentation, not a state machine
  block.** The old approach guarded `do_action`/`on_button_press` to
  refuse input while any window was fullscreen — a patch on the
  symptom that still left stale fullscreen windows on screen when
  focus moved via `map_request` or an EWMH message. `core/present.rs`
  now rewrites *only the focused* window's rect to `mon.screen` when
  it's fullscreen (`layout.rs::arrange` stays pure geometry); `focus()`
  re-arranges on every fullscreen transition. Maximize reuses the same
  seam (fullscreen > maximized > layout precedence).
- **`backend/x11.rs` split** into `backend/x11/{mod,manage,events,
  ewmh,input,pointer,render,struts,bar,actions}.rs` (previously one
  ~2900-line file). No behavioural change, just navigability.
- Dead code removed: `core/engine.rs`'s old `process_event`/`AppEvent`/
  `Command` path, `core/events.rs`, `core/commands.rs`,
  `Workspace::move_window_right()` (flagged unused in 0.18.1).

### Fixed

- **`WindowId` was an alias for x11rb's `Window`, not a real
  backend-agnostic type** (`src/types.rs`). The domain model — the part
  that's supposed to have zero X11 knowledge — imported
  `x11rb::protocol::xproto::Window` directly. `WindowId` is now a
  plain `u32` with no dependency on `x11rb`; since x11rb's `Window` is
  itself a `u32` alias, this is behaviourally a no-op (no cast sites
  needed anywhere in `backend/`) but it removes the last x11rb import
  from `core`/`types.rs`. Confirmed via `grep -rl x11rb src/core/
  src/types.rs` returning nothing after the change.
  (`fe2e766`)

### Known issues — core/backend separation (in progress, tracked here on purpose)

This is the actual roadmap item for the next few passes, not a
finished job. Concrete couplings found while reading through the
current tree, ranked by how much they'd block a Wayland backend:

1. **`backend/x11/manage.rs::manage()` mixes protocol decoding with
   domain decisions in one ~500-line function.** Reading raw
   `_NET_WM_WINDOW_TYPE`/`WM_HINTS`/`WM_NORMAL_HINTS` property bytes
   and *deciding* `is_dialog`, `WinFlags::FLOAT`, `WinFlags::URGENT`,
   tag/workspace placement, etc. are interleaved line-by-line. A
   Wayland backend would have to re-derive all of that decision logic
   from scratch instead of calling one shared function with its own
   protocol-specific extraction feeding in. Next step: extract a
   backend-agnostic `fn classify_client(info: WindowInfo) -> (WinFlags,
   bool /*is_dialog*/, …)` in `core/` that both backends call after
   doing their own (necessarily protocol-specific) property reads.
2. **`Cfg::keybinds: Vec<(u16, u32, Action)>`** stores raw X11
   modifier-mask bits and X keysyms directly as the config's own
   types (`config.rs::load_config` builds them via
   `x11rb::protocol::xproto::ModMask`). Config itself doesn't import
   x11rb (the raw ints are backend-agnostic on their face), but the
   *meaning* of those ints is X11-specific; a Wayland backend using
   `xkbcommon` keysyms would happen to reuse the same keysym space but
   not the modifier-mask bit layout. Not urgent — flagging so it isn't
   assumed to be already-portable.
3. **Rule matching (`config.rs::Rule::matches`) runs on `class`/`title`
   strings that only X11's `WM_CLASS`/`_NET_WM_NAME` naturally
   produce.** Wayland equivalents (`app_id`, xdg-shell title) map
   cleanly onto the same two strings, so this one is low-risk, but it's
   still backend-shaped data flowing through a `core`-owned type.
4. **Bar visual style reverted to 0.18.1.** The rebuild's `backend/bar.rs`
   picked up several cosmetic additions along the way — an active-monitor
   marker block, a bottom accent underline, an extra green "occupied" dot
   drawn next to tags whose label was already colored green for the same
   state, and "…" truncation on long titles/status text. Net effect read as
   visually noisy/cluttered rather than an improvement, so `backend/bar.rs`
   was restored byte-for-byte to the 0.18.1 version (`22a6352`). Verified no
   other file referenced the removed symbols (`COL_LAYOUT_CYAN`,
   `truncate_latin1`, `tag_width`, `separator()`, `START_X`, `ACCENT_H`) —
   `backend/x11/bar.rs` and `pointer.rs` only call `Bar::draw`/`Bar::tag_at_x`,
   whose signatures are unchanged, so this is a pure revert with no other
   code affected.
   (`2602f43`)

## [0.18.1] — 2026-07-02

### Fixed

- **Quit confirmation dialog was non-functional.** `Action::QuitConfirm`
  set `running = false` directly — identical to `Action::Quit` — with no
  dialog window ever created anywhere in the codebase. `quit_win` was
  declared, initialized, and read in two places (`restack()`,
  `on_destroy()`), but never once assigned `Some(_)`. Consolidated into a
  single `Action::Quit` bound to `Mod4+Shift+Q`; removed the dead
  scaffolding (`quit_win` field, the raise-above-fullscreen hook, the
  destroy-notify cleanup hook, an orphaned doc comment left dangling above
  an unrelated function).
  (`3465939`)

- **Bar workspace-tag clicks could desync from what was rendered.**
  `tag_at_x()` counted glyphs by filtering out every character above
  U+00FF; `draw()`'s `to_latin1()` counts every character and substitutes
  `?` for anything above U+00FF. Same tag name in, two different glyph
  counts out — the click hitbox drifted from the rendered label the
  moment a tag name held a non-Latin1 character. Invisible with the
  default numeric tag names, but breaks click-to-switch for anyone who
  customizes them with icons, CJK, or emoji. `tag_at_x()` now calls
  `to_latin1()` directly so the two can't diverge again.
  (`557ba37`)

- **New-column width was inconsistent depending on how the column was
  created.** `add_tiled` (opening a new window) sized every column past
  the first at 75% of the workarea. `apply_move_dir`'s extract-to-new-
  column branch and `new_column()` (`Mod4+Shift+Return`) instead used a
  fixed `default_col_w` (700px), which doesn't scale with monitor
  resolution and made the same logical action — "put this window in its
  own column" — look very different depending on which keybind triggered
  it. All three paths now compute the same 75%-of-workarea width.
  (`6f7a9d6`)

- **Browser file-picker / upload dialogs never appeared.** Root cause of
  a previously-diagnosed issue: neither `xdg-desktop-portal` nor
  `xdg-desktop-portal-gtk` was ever started. `detect_portal()` only
  floats the dialog window once one exists — it can't conjure one if the
  backing service never launched. Added both to `autostart`, with full
  paths (`/usr/lib/...`) since neither binary lives on `$PATH` on Arch.
  (`734e4ec`)

### Changed

- New-column sizing is now unified around workarea percentage, so
  `default_col_w` in `Cfg` no longer drives column width anywhere in the
  live code path. Left the field in place for now rather than remove
  config surface as a side effect of a bug-fix pass.

### Docs

- README / README.es.md: bar section now describes the raw-X11
  (`image_text8` / `poly_fill_rectangle`) rendering path instead of the
  retired `xft.rs` FFI wrapper; dropped an unverified `~3–4 MB` resident
  memory figure from that section rather than leave it stale; keybind
  table no longer claims a confirmation dialog on quit.
  (`4499fb0`)
- Restored English-only inline comments — six spots had reverted to, or
  were left in, Spanish (one mixed both languages in the same comment
  block); fixed a compositor config comment that still described an
  opacity flag removed a few commits earlier.
  (`d452b7b`)

### Known issues

Flagged during this pass, not fixed here — bigger changes, out of scope
for a bug-fix batch:

- `core/engine.rs`'s `process_event` / `AppEvent` / `Command` path is
  never invoked by the running window manager — `backend/x11.rs`
  reimplements `ToggleBar`, `CycleLayout`, `SetLayout`, and window
  creation directly instead of going through it. 3 of the 7 unit tests
  (`test_toggle_bar_hides_and_shows`, `test_cycle_layout_wraps_around`,
  `test_window_created_emits_layout_commands`) exercise only that
  disconnected path and don't protect the code that actually ships.
- `Workspace::move_window_right()` (`types.rs`) has no caller anywhere in
  the tree — dead code, found while fixing the column-width
  inconsistency above.
