# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

Version note: the entries under `[0.18.4]` describe the window-manager
rewrite that forms the current `main` history. Earlier releases
(`[0.18.2]`, `[0.18.1]`) are retained as historical records of the
pre-rewrite codebase.

## [Unreleased]

Pending work that is not yet part of a release:

- **Video wallpaper source.** `WallpaperSource::Video` is reserved in the
  configuration schema but has no decoder yet; image and GLSL-shader
  sources are the only ones currently implemented.
- **Compositor partial redraw** (scissor to the damaged region) is only
  active when the GLX `GLX_EXT_buffer_age` extension is available. On
  drivers without it, the compositor falls back to full-frame redraws.
- The built-in compositor can be disabled per session with the
  `MAVERICK_NO_COMPOSITOR` environment variable or by setting
  `compositor_enabled = false` under `[general]` in the configuration.

## [0.18.4] - 2026-08-13

This release is a comprehensive rewrite of the window manager: a
backend-agnostic domain model, a new GL compositor, a reworked layout
engine, an explicit desired-state/Reconciler pipeline, native wallpaper,
session persistence, and per-session instance isolation.

### Window Management

- **Tiling column layout.** Columns scroll horizontally; each column holds
  a uniform-height stack of windows. Window width is expressed as a
  fraction of the workarea (`column_width`, default `0.6`) rather than a
  fixed pixel count, so columns scale with the monitor.
- **Floating windows.** Floating windows are positioned by the window
  manager: centered on the transient parent's stored geometry when one
  exists, otherwise centered in the assigned monitor's workarea. Geometry
  from client requests is clamped to the workarea.
- **Fullscreen and maximize.** Fullscreen covers the whole screen with no
  border; maximize fills the workarea (respecting dock/bar struts) and
  keeps the border. Maximize is modeled as independent vertical and
  horizontal bits (`MAXIMIZED_V` / `MAXIMIZED_H`); `is_maximized()`
  requires both. The presented (fullscreen/maximized) window is a
  persistent per-workspace overlay layer that stays in place regardless
  of focus, so moving focus no longer triggers `ConfigureWindow` traffic
  on the presented window.
- **Focus model.** Focus follows a single choke point; all focus changes
  publish a `FocusChanged` domain event. Keyboard navigation, pointer
  enter events, and EWMH `_NET_ACTIVE_WINDOW` requests all converge on the
  same logic. A short guard suppresses pointer `EnterNotify` events
  immediately after a keyboard-driven focus move, so keyboard navigation
  does not "slip" to a window under the cursor.
- **Per-rule window policies.** Rules gained `ignore_initial_state`
  (applications such as GTK-based browsers remember and re-request
  maximized/fullscreen state on map; this rule clears those states so the
  window enters the tile layout normally), `deny_fullscreen` (rejects
  client-requested fullscreen via EWMH while leaving the user's
  `Mod4+F` binding intact), and `true_fullscreen` (exclusive fullscreen
  outside the ribbon, for games). Rules also accept `instance`,
  `window_type`, `sticky`, `size`, and `position` criteria, matched by
  case-insensitive substring.
- **EWMH compliance.** Published stacking order via
  `_NET_CLIENT_LIST_STACKING` (visible tiles → floating → the rest, by
  recent focus). Extended `_NET_SUPPORTED` with `_NET_WM_WINDOW_OPACITY`,
  `_NET_CLOSE_WINDOW`, and `_NET_WM_BYPASS_COMPOSITOR`. Preserved
  unmanaged atoms when writing `_NET_WM_STATE` so properties set by other
  tools are not discarded.
- **`--replace` and prior-window adoption.** `maverick --replace` takes
  over from an existing window manager: it locates the current WM via
  `_NET_SUPPORTING_WM_CHECK`, requests exit through `WM_DELETE_WINDOW`
  (never `SIGKILL`), and adopts already-managed windows, restoring the
  geometry of previously floating ones.
- **Floating-window persistence.** `_MAVERICK_FLOAT` / `_MAVERICK_GEOM`
  properties record floating state and geometry so they survive a
  restart or `--replace`.
- **Mouse interaction with floating windows.** `Mod`-drag resizes a
  floating window according to the pointer quadrant; dropping a floating
  window over a tiled column inserts it there, with a preview border
  shown on the target column.
- **`maverick-msg` control client.** A verbatim command client that
  forwards lines verbatim over the control protocol (for example
  `maverick-msg focus-right` or `maverick-msg query tree`), sharing the
  CLI engine with `maverickctl`.
- **Structured state queries.** `maverickctl query workspaces|tree|focused|
  state` returns live workspace, column, window, and focus information
  from the window manager thread.

#### Fixed

- **Scroll-culling destroyed windows.** Hiding off-screen columns unmaps
  them, and the resulting `UnmapNotify` on the root was being treated as a
  client unmanage, deleting windows after the third tiled column scrolled
  out. The window manager now tracks its own unmap operations
  (`ignore_unmaps`) and ignores the reflected root notification while
  still processing client-directed ones.
- **`GrowColumn` panic with 21+ columns.** The clamp upper bound fell
  below the `0.05` floor past 20 columns, violating `f32::clamp`'s
  precondition. The bound is now `max(0.05, …)`.
- **Fullscreen of a floating window broke the window.** Promoting a
  floating client to fullscreen now shares one `apply_fullscreen_topology`
  path (keyboard and EWMH) that promotes the window into the ribbon,
  saves its floating rect, and is idempotent.
- **`_NET_WM_STATE_MAXIMIZED_VERT` no longer implied full maximize.**
  Vertical and horizontal maximize are independent bits.
- **Viewport (inspection zoom + page-snap).** A per-workspace viewport
  mode (`Normal` / `Zoomed`) with spring-animated `page_zoom`, separate
  from window fullscreen and Overview; `Mod4+=`/`Mod4+-` zoom the ribbon
  and `Mod4+]`/`Mod4+[` page-snap the camera by one screen without
  changing focus.
- **Transients mapping before their parent.** A dialog whose
  `WM_TRANSIENT_FOR` points at a not-yet-managed window is recorded with
  its desired parent and queued in `pending_transients`; when the parent is
  managed, the transient is relocated to the parent's monitor/workspace
  and re-centered.
- **Keyboard `ToggleFullscreen` did not apply state.** The command no
  longer mutates the flag directly; it lets the effect handler perform the
  transition, so `_NET_WM_STATE`, `_NET_WM_BYPASS_COMPOSITOR`, and
  `saved_geom` are all updated correctly.
- **Overview did not move real focus.** `OverviewNav` / `OverviewEnter`
  now emit `FocusWindow` on the selected window instead of only moving an
  index.
- **Camera animation on window open was dead.** Opening a window with
  others present now animates the camera instead of snapping it.
- **`GrowColumn` stole width from neighbors.** It now adjusts only the
  focused column's weight and clamps the bound so a single column can
  still be resized.
- **`MoveToWorkspace` and `ToggleFloat` desynchronized the camera.** Both
  now recompute the ideal scroll via `scroll_to_focused`.
- **Camera scroll with the wheel.** `Mod4` + wheel moves column focus one
  slot per notch, re-centering the camera.
- **Per-frame stacking storm.** `stack_overlay` no longer re-raises every
  floating/sticky window each animation frame; it caches the desired order
  per monitor and only re-emits on change. The dead `restack` path was
  removed.
- **`ToggleMaximize` accessible via keyboard/IPC.** New
  `Action::ToggleMaximize` with a default `Mod4+Shift+m` binding and an
  `toggle-maximize` IPC command.
- **Column widths animate on focus change.** Each column carries an
  animated `boost` value (replacing a single global scalar) so the ribbon
  glides rather than jumps when focus moves.
- **Unified new-column policy.** `NewColumn`, orphan re-homing on hotplug,
  and every `add_tiled` create columns at the configured workarea fraction,
  eliminating several divergent width policies. Ribbon geometry no longer
  subtracts gaps from the usable width, so adding a column no longer
  shrinks the others.
- **`FocusDirection` / `MoveWindow` blocked in fullscreen.** The
  keyboard guard that made both commands no-ops while the focused window
  was fullscreen was removed; the intentional click/drag lock on a
  fullscreen window in pointer handling is retained.
- **Rounded corners in fullscreen.** Corner rounding is suppressed
  (radius `0`) when a window is fullscreen, since there is nothing to
  round toward.
- **`WM_TAKE_FOCUS` uses a real ICCCM timestamp.** `send_proto` sends the
  last input event time instead of `CurrentTime`, so strict toolkits
  accept focus correctly.
- **MapRequest under an active overlay (anti-focus-steal).** A transient
  dialog for the presented window takes focus and raises above the
  overlay; any other new window enters the tile tree silently and is
  marked `_NET_WM_STATE_DEMANDS_ATTENTION` until focused.
- **Floating windows opened off-screen.** Position is now computed by the
  window manager rather than trusting the raw X geometry captured at
  creation.
- **RandR monitor hot-plug.** The root now selects RandR events; monitor
  add/remove and geometry-only changes are detected and trigger
  re-arrangement with an "actually changed" guard.
- **`ConfigureRequest` preserved `above_sibling`.** Restack requests that
  position a window above a specific sibling are passed through.
- **Maximized frame overflow.** The maximized overlay applies border `0`
  over the workarea, so a bordered maximized window no longer encroaches
  on reserved/adjacent pixels.
- **Unmapped overlay left stale stack state.** A presented window that is
  unmapped while not focused is now purged immediately, removing
  `BadWindow` risk.
- **Focus fallback ignored the overlay.** `best_focus` prefers the most
  recently presented fullscreen/maximized window on the workspace.
- **`_NET_WM_BYPASS_COMPOSITOR` for fullscreen.** Set to `2` on
  enter and cleared on exit, so external compositors stop shadowing
  fullscreen video or games.

#### Keyboard

- **Keys stolen from applications under multi-group / AltGr layouts.**
  Grab and dispatch now share a strict group-1 policy (the only
  unambiguous part of the keymap). Binds whose group-1 keysym is
  unreachable still resolve via a keysym-directed fallback that scans the
  whole row and records the keycodes it lands on, so nothing is grabbed
  that dispatch would then drop. The shifted-column fallback is clamped to
  group 1, and a bind whose keysym does not exist in the current layout is
  logged and ignored rather than silently swallowing the key.
- **Keyboard refresh is no longer fatal.** A failed keymap re-read keeps
  the previous keymap and retries on the next notification instead of
  propagating an error and exiting the window manager.
- **XKB keyboard-change subscription with coalescing.** Maverick selects
  XKB `MapNotify` / `NewKeyboardNotify` (falling back to core
  `MappingNotify` when XKB is unavailable) and coalesces a burst of
  notifications into a single ungrab-and-regrab within a short window, so
  remaps and USB keyboard hotplug are picked up without a full regrab per
  event.
- **Keyboard stutter with the compositor enabled.** The event loop now
  drains the X event queue before blocking on `poll`, eliminating a
  key→action latency spike (measured worst case near 90 ms with the
  compositor active, reduced to roughly 14 ms) caused by GLX round-trips
  drying the socket between polls.

### Layouts

- **Grid layout engine rewritten.** `grid.rs` is a pure layout engine with
  no X11, state, focus, or event-loop dependencies. Geometry is a
  deterministic function of the window set, workarea, gaps, and border;
  candidate partitions are enumerated in a fixed order with explicit
  tie-breaks, and an optional previous snapshot only nudges the cost
  function to avoid reshuffling.
- **Pluggable layout trait.** Layouts implement a `Layout` trait
  (`name`, `arrange`) and register in a `LayoutRegistry` that `LayoutKind`
  maps into, replacing the monolithic `match` in the arrange path.
- **`Monocle` layout removed.** It remained experimental and duplicated
  `Grid` with little benefit. Only two layout modes ship: **Column** (the
  scrollable tiling layout) and **Grid**. `cycle_layout()` wraps
  Column → Grid → Column.

### State Architecture

- **Explicit desired-state pipeline.** `State → layout::arrange →
  present::present_into → DesiredState → Reconciler → AppliedState → X11`
  is now an explicit hand-off. `DesiredState` is a pure snapshot of every
  desired placement; the `Reconciler` (`backend/x11/reconciler.rs`) is the
  single owner of "what geometry/stack has actually been written to X11",
  replacing scattered change-detection in render, manage, and events. It
  diffs each desired placement against the last `AppliedState` and emits
  `configure_window` only for what changed, while still forcing a
  reconfigure on pending state transitions. Floating geometry requests are
  clamped to the workarea, and transient chains are walked with a depth
  limit and cycle/destroyed-parent guards.
- **State invariants.** `State::check_invariants()` / `assert_invariants()`
  enforce internal consistency (fullscreen overlay ownership, presented
  window bookkeeping, wallpaper layer state).
- **Capability layer (`core::capability`).** A read-only public API
  (`Engine::query()`) exposes `focused_window()`, `active_workspace()`,
  `visible_windows()`, `current_layout()`, and `window(id)`, decoupled
  from internal `State`/`Client` types. Writing remains exclusively through
  `Engine::execute(Command)`.
- **Typed command and event system.** `core::commands` defines pure
  commands (each a transform over `State`/`Cfg` returning effects and an
  optional domain event) executed via `Engine::execute()`. `Action` is the
  canonical mapper from the wire DSL (keyboard, IPC, TOML) to commands.
  A typed `EventBus` carries domain events (`FocusChanged`,
  `WorkspaceChanged`, `LayoutChanged`, …) to renderer, IPC, and future
  consumers; `Engine::execute_batch` runs several commands as one
  transaction with a single coalesced IPC state publish.

### Compositor

The built-in compositor is enabled by default and uses XComposite (manual
redirection, the `_NET_WM_CM_S0` selection, and the compositor overlay
window), GLX/OpenGL 3.3 rendering with vsync, and texture-from-pixmap
(TFP) for zero-copy window textures.

- **Damage tracking.** XDamage notifies drive a fixed-capacity
  (`DamageRegion`, 32 rects, zero-allocation) screen-space damage
  accumulator rebuilt every frame; structural changes force a full
  repaint.
- **Scene buffer and viewport culling.** A reusable `Vec<DrawItem>` scene
  is rebuilt each frame, re-binding only damaged TFP textures and culling
  windows fully outside the screen.
- **Occlusion-aware damage.** Windows fully covered by opaque windows
  above them are skipped (`fully_covered_by`); animating/scrolling windows
  contribute the union of their previous and current rects
  (`anim_damage_rects`) so they leave no trailing artifacts without
  over-damaging the frame.
- **Partial redraw.** When `GLX_EXT_buffer_age` is available, frames are
  classified by an explicit `FrameMode` (`Idle` / `Full` / `Partial`);
  partial frames scissor to the bounding box of accumulated damage and
  preserve the back buffer, falling back to full redraw on overflow or
  missing buffer age.
- **Frame scheduling.** `framesched.rs` provides a pure frame scheduler
  mapping damage reasons to "needs frame" / timeout decisions; the render
  loop is driven by `vsync` (swap interval 1).
- **Native wallpaper.** `WallpaperSource` supports `None`, `Image`
  (decoded by the dependency-free `maverick-img` crate, PNG and common
  formats, with an external converter fallback for others), and `Shader`
  (GLSL fragment shader compiled through `maverick-gl`). `WallpaperMode`
  is `Fill` / `Fit` / `Stretch` / `Center`. Live control is available via
  `maverick-msg wallpaper set|clear|mode`. (`Video` is reserved and not
  yet implemented.)
- **Opacity.** `_NET_WM_WINDOW_OPACITY` is honored in the compositor
  (also settable per-rule at manage time) and applied as a premultiplied
  blend.
- **Rounded corners without a compositor.** `general.corner_radius`
  (default `0`, disabled) shapes every managed window's outer edges via
  the X11 Shape extension's bounding mask; with the default it sends no
  Shape requests at all.
- **Performance.** The per-frame projection path reuses pre-allocated
  caller-owned buffers (zero heap allocations during normal animation,
  asserted by an allocation-counting benchmark); the per-window transform
  lookup in the draw loop was reduced from an O(N²) scan to a single
  hash; and the GL texture filter state is cached and only re-issued on
  transition.

### Animation and Presentation

- **Spring-driven camera, column boost, and viewport zoom** advance in
  `tick_animations` and feed the compositor's live placement path, so
  layout transitions, focus glides, and zoom animate smoothly and stay in
  sync with presentation.
- **Presentation overlay decoupled from focus.** A presented
  fullscreen/maximized window stays put while focus moves underneath; a
  normal tile focused under an active overlay is raised above the
  presented window (peek) without resizing anything.

### IPC and Session Management

- **Per-session identity and isolation.** Each instance derives an isolated
  runtime directory and control socket from a unique session id, so
  multiple instances (for example a real session and a Xephyr test
  instance) no longer collide. Runtime-dir permissions are locked down,
  and discovery distinguishes a live instance from a stale one using PID
  start time and the attached X server.
- **`maverick-sys` control plane.** A Unix-socket protocol
  (`ping` / `identify` / `state` / `dispatch` / `restart` / `reload` /
  `subscribe` / `quit`) bridged to the single-threaded X11 event loop,
  with `maverickctl` as the CLI (`list` / `state` / `msg` / `subscribe` /
  `quit[--confirm]` / `quit-all` / `restart` / `reload` / `prune`) and
  `maverick-dialog` as a standalone confirmation window used by
  `Mod4+Shift+Q`.
- **Session persistence and recovery.** `core/session.rs` saves and
  restores desktop topology (workspace/column/weight layout, active
  workspace, focus) across reload/restart through a staged pipeline
  (`PersistedSession → validate → commit`). Runtime-only data (geometry,
  camera springs, zoom/overview animation, grid caches, compositor state,
  presented windows) is never trusted from disk and is always
  reconstructed.
- **Restart preserves launch arguments.** `main` captures `argv` up front
  so `restart` re-execs with the exact same arguments, and the control
  socket and instance metadata are removed before `exec`.
- **Cooperative shutdown.** Quit requests send `WM_DELETE_WINDOW` to
  clients and apply a shutdown deadline with a forced kill of remaining
  clients.

#### Fixed

- **Control-socket symlink attack.** The socket path is only removed if it
  is a regular socket; concurrent handler count is bounded; identity JSON
  escapes all JSON-special characters; and `send_command` rejects
  newlines to prevent line-protocol injection.
- **Identity parser failures.** `/proc/<pid>/stat` comm parsing uses
  `rfind(')')` to handle parentheses in process names, and the JSON parser
  respects string quoting when splitting fields.
- **`wait_readable` busy-loop.** The poll loop now checks `POLLIN` rather
  than treating any non-zero `revents` as readable.

### Reliability

- **Stable restart and lifecycle.** Restart cleans up the control socket
  and identity ficha before `exec`; `argv` is preserved; shutdown applies
  a deadline with forced kill of survivors; the identity ficha is removed
  on init failure.
- **`startx` / `EnterVT` launch crash.** `detach_from_terminal()` no
  longer calls `setsid()` unconditionally (which put the WM in a new POSIX
  session while still a child of the login session's VT/DRM handoff); it
  now only redirects stdin/stdout to `/dev/null` when launched from a real
  tty.
- **Monitor handling.** Hot-plug preserves client monitor/workspace
  assignments where the target still exists; geometry-only changes trigger
  re-arrangement; `_NET_WORKAREA` reports each monitor's own workarea;
  moving a window to a monitor with fewer workspaces clamps the index.
- **Window lifecycle hardening.** `UnmapNotify` no longer removes windows
  from the workspace (iconify/restore preserves tiling state); the
  `FocusIn` handler no longer steals focus from popups and dialogs;
  `find_client` guards against cyclic window trees; `ConfigureNotify`
  coordinates are clamped before casting; focus/stacking index with an
  empty monitor list are bounds-checked; `focus()` no longer computes the
  previously-focused window twice; `focus_dir` Next/Prev filters by the
  active workspace.
- **Input robustness.** Keyboard freeze after click-to-focus was fixed by
  setting `keyboard_mode=ASYNC` on the catch-all `grab_button`
  (previously `SYNC`, which left the keyboard frozen at the X11 level).
  `focus_mouse` no longer performs a `query_tree` round-trip per motion
  event; it uses `EnterNotify` instead. Rejected key/button grabs are now
  detected and logged instead of silently swallowing keys.
- **Misc.** `CycleLayout`/`SetLayout` and `collapse_col` were guarded
  against out-of-bounds/ordering bugs; `Restart`/`reload`/`subscribe`
  wiring and the `PublishIpcState` effect are now emitted consistently;
  `Client::new` initializes tag bits from the assigned workspace; rule
  pattern matching is normalized to lowercase.

### Configuration

- **Optional TOML configuration.** Maverick reads
  `$XDG_CONFIG_HOME/maverick/config.toml` (falling back to
  `~/.config/maverick/config.toml`) layered over compiled defaults, with
  per-section overrides for `[general]`, `[colors]`, `[[keybindings]]`,
  `[[rules]]`, and `[autostart]`. Loading is fail-safe: a missing file is
  ignored, a file that fails to parse falls back to compiled defaults, and
  a single bad entry is dropped with a warning — a malformed configuration
  can never prevent startup.
- **Real configuration hot-reload.** `maverickctl reload` re-reads the
  TOML through the same fail-safe path, swaps the engine config, regrabs
  the keymap, and re-arranges every monitor; a tag-count change reconciles
  each monitor's workspace list.
- **New options.** `column_width` (workarea fraction, replacing the
  deprecated `default_col_w` / `split_bias`); `gaps_inner` / `gaps_outer`
  with `smart_gaps`; named color-theme presets (`catppuccin-mocha`,
  `catppuccin-latte`, `gruvbox`, `nord`, `dracula`, `everforest`,
  `solarized`); per-rule `opacity` and `border_w`; a `[wallpaper]` table
  (`path` + `mode`); and `[general]` keys `compositor_enabled` plus
  `camera_stiffness`/`camera_damping` (scroll-camera spring). `--config <path>` and `--check-config
  [path]` CLI flags were added; automatic workspace bindings can be
  overridden per digit.
- **Zero-dependency `maverick-toml` crate.** The configuration parser was
  rewritten as a local strict TOML-subset crate with no external
  dependencies, replacing `serde` and `toml` (and their transitive
  dependencies). The stripped binary is measurably smaller on the same
  release profile.
- **Generic default configuration.** The shipped `autostart` now launches
  only `xdg-desktop-portal` / `xdg-desktop-portal-gtk` (needed for file
  picker dialogs) and no longer carries a maintainer-specific machine
  setup.

#### Removed

- **Internal status bar removed.** Drawing a status bar is not the window
  manager's responsibility; its removal also dropped the plain X11
  core-font rendering path. External bars are supported through
  `_NET_WM_STRUT_PARTIAL` reservation, and `root` `WM_NAME` is still
  exposed over IPC.
- **Compositor orchestration removed from startup.** `main` no longer
  spawns a compositor, waits for it to attach, or plays a startup sound;
  the compositor is built in and any external program belongs in
  `autostart`. `Cfg::compositor`, `compositor_delay_ms`, and
  `startup_sound` were removed.
- **Dead code and atoms.** Removed unused client flags, never-emitted
  effect variants, ~40 interned-but-unread atoms, and the
  `#[allow(dead_code)]` escape hatch; `_NET_SUPPORTED` now lists only
  atoms the WM acts on. Duplicate string-escape logic was consolidated
  into `maverick_sys::json`.

### Testing and Quality

- **Expanded unit coverage.** `src/core/tests.rs` covers the Grid layout
  engine, the desired-state/Reconciler pipeline, session persistence,
  fullscreen/maximize commands, and wallpaper state. `src/backend/x11/
  tests.rs` adds backend-level tests for the reconciler, focus, and
  struts.
- **Xephyr integration suite.** New `tests/` client programs and
  `xephyr-*.sh` scenarios cover multi-monitor, client death, compositor,
  config + wallpaper, fullscreen pointer, IPC edge cases, partial
  redraw, restart-with-config, shutdown, stress, and wallpaper paths, plus
  a session-isolation script exercising save/restore across restarts.
- **Image-decoder tests.** `maverick-img` includes fixture-based tests
  (palette, RGB, RGBA, paeth-filtered, grayscale+alpha PNGs).
- **Compositor benchmarks.** Frame-projection and damage/`FramePlan`
  benchmarks, plus an allocation-counter test asserting zero per-frame
  heap allocations during animation.
- **Code quality.** `rustfmt` is enforced across the workspace; `rustdoc`
  builds cleanly under `-D warnings`; `.gitignore` was expanded; workspace
  crates declare `rust-version = "1.82"`, `repository`, `categories`, and
  `keywords`. The `clippy` lints present at the relevant points were
  resolved across `manage.rs`, `engine.rs`, `types.rs`, and `ipc.rs`. Note:
  the workspace is not entirely clippy-clean — two deliberate
  `clippy::question_mark` warnings remain in `maverick-sys/src/control.rs`
  and are left in place as out of scope for that cleanup.

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
  `identity` (per-instance PID/display/tty record under the runtime
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
- **External dock support**: docks are detected
  by `_NET_WM_WINDOW_TYPE_DOCK`/`_DESKTOP`, never by process name, and
  reserve space via `_NET_WM_STRUT_PARTIAL`/legacy `_NET_WM_STRUT`,
  tracked per-monitor and released on destroy/unmap.
- `internal-bar` Cargo feature (default on): `cargo build --release
  --no-default-features` builds without the internal status bar for
  people driving an external bar instead.

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
  from `core`/`types.rs`.

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
   was restored byte-for-byte to the 0.18.1 version. Verified no
   other file referenced the removed symbols
   (`backend/x11/bar.rs` and `pointer.rs` only call `Bar::draw`/`Bar::tag_at_x`,
   whose signatures are unchanged, so this is a pure revert with no other
   code affected).

## [0.18.1] — 2026-07-02

### Fixed

- **Quit confirmation dialog was non-functional.** `Action::QuitConfirm`
  set `running = false` directly — identical to `Action::Quit` — with no
  dialog window ever created anywhere in the codebase. Consolidated into a
  single `Action::Quit` bound to `Mod4+Shift+Q`; removed the dead
  scaffolding (`quit_win` field, the raise-above-fullscreen hook, the
  destroy-notify cleanup hook, an orphaned doc comment left dangling above
  an unrelated function).
- **Bar workspace-tag clicks could desync from what was rendered.**
  `tag_at_x()` counted glyphs by filtering out every character above
  U+00FF; `draw()`'s `to_latin1()` counts every character and substitutes
  `?` for anything above U+00FF. Same tag name in, two different glyph
  counts out — the click hitbox drifted from the rendered label the
  moment a tag name held a non-Latin1 character. Invisible with the
  default numeric tag names, but breaks click-to-switch for anyone who
  customizes them with icons, CJK, or emoji. `tag_at_x()` now calls
  `to_latin1()` directly so the two can't diverge again.
- **New-column width was inconsistent depending on how the column was
  created.** `add_tiled` (opening a new window) sized every column past
  the first at 75% of the workarea. `apply_move_dir`'s extract-to-new-
  column branch and `new_column()` (`Mod4+Shift+Return`) instead used a
  fixed `default_col_w` (700px), which doesn't scale with monitor
  resolution and made the same logical action — "put this window in its
  own column" — look very different depending on which keybind triggered
  it. All three paths now compute the same 75%-of-workarea width.
- **Browser file-picker / upload dialogs never appeared.** Root cause of
  a previously-diagnosed issue: neither `xdg-desktop-portal` nor
  `xdg-desktop-portal-gtk` was ever started. `detect_portal()` only
  floats the dialog window once one exists — it can't conjure one if the
  backing service never launched. Added both to `autostart`, with full
  paths since neither binary lives on `$PATH` on Arch.

### Changed

- New-column sizing is now unified around workarea percentage, so
  `default_col_w` in `Cfg` no longer drives column width anywhere in the
  live code path. Left the field in place for now rather than remove
  config surface as a side effect of a bug-fix pass.

### Docs

- README: bar section now describes the raw-X11
  (`image_text8` / `poly_fill_rectangle`) rendering path instead of the
  retired `xft.rs` FFI wrapper; dropped an unverified `~3–4 MB` resident
  memory figure from that section rather than leave it stale; keybind
  table no longer claims a confirmation dialog on quit.
- Restored English-only inline comments — six spots had reverted to, or
  were left in, Spanish (one mixed both languages in the same comment
  block); fixed a compositor config comment that still described an
  opacity flag removed a few commits earlier.

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
