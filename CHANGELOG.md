# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Fixed

- **A stopped session claimed every process on the machine.** `process list` on a
  stopped session reported 200+ processes beginning at pid 1, and `process kill`
  would terminate any of them, reporting success. Two independent causes, both
  now closed:
  - A stopped session's roots are `ProcRef::default()` — pid 0, documented as
    "no process" — and the process-tree walk seeded from them. Since pid 1's
    `ppid` is literally 0, walking from 0 adopted init and then the whole
    process table. Roots now come from one accessor that requires the recorded
    pid *and* start time to still match, so an unproven root contributes
    nothing, and the walk itself refuses a non-positive root.
  - Registered process groups (`pgrps`) survived `stop`, `kill` and `restart`,
    so a session that named no live process could still authorise a signal
    through a group id. They are now cleared on every down-transition and at
    the start of each generation, and consulted only while a live root exists.

  `process list` and `process kill` now derive ownership from a single shared
  function, so the agreement `docs/sessions.md` describes is structural rather
  than a convention two call sites have to keep matching.

  Not claimed: a process group the kernel reissues *while the session is still
  running* is still trusted. Closing that needs the group leader's start time
  recorded alongside the id, which is a record-format change.

- **`maverickctl window list` and `process list` reported failure with exit
  status 0.** Both printed the error to stderr and returned, so the group
  dispatcher saw success. Under `--json` they printed nothing at all and still
  exited 0 — a silent empty stream a script could not distinguish from a
  session with no windows. They now propagate, matching their own sibling
  verbs, and `process list` no longer discards the reason a session name could
  not be resolved.

- **A server-side refusal was printed to stdout with exit status 0.**
  `maverickctl query` for an unknown topic wrote `error unknown-query: …` to
  stdout and exited 0, so the diagnostic was data rather than a failure. An
  `error `-prefixed reply is now classified as a failure, on stderr, with a
  non-zero status — the same rule `msg` already applied to the same protocol.
  A peer that closes without answering is also now a failure rather than a
  silent success.

### Changed

- **The installer installs for the current user by default.** A bare
  `./install.sh` now installs into `$HOME/.local` and needs no privileges;
  `--system` selects `/usr/local`, and any other location is reachable with
  `--prefix`. The installer never invokes `sudo` and never runs as root: an
  unwritable prefix is reported as a permission error naming the path, rather
  than escalated around. A system-wide install therefore needs write access
  arranged by the caller, and says so if it is missing.
- **The prefix is a hard boundary.** The installer no longer redirects the X11
  session file to `/usr/share/xsessions` when another prefix was selected.
  Everything installed derives from the chosen prefix, and publishing the
  session file to a system location a display manager reads is now the
  explicit, documented `--xsessions-dir` option.
- **Installation is staged before it is committed.** The previous loop
  installed one binary and renamed it into place before starting the next, so a
  failure partway through left a prefix holding a new `maverick` beside an old
  `maverickctl` — a version-skewed pair. The complete set is now staged,
  verified, and only then moved into the prefix, with a destination that is a
  directory caught before any rename.
- **Post-install verification executes the installed binaries.** The final
  check ran `maverick --version` and tested `-x` on files the install loop had
  just written, so a 53-byte stub exiting 42 was reported as
  `2/2 binary functional` with a zero status. The installer now runs
  `maverick --version`, `maverickctl --help` and `maverickctl session --help`
  by absolute path from the prefix. The last of these needs no display, no
  running instance and no network, and an older `maverickctl` that predates
  Sessions answers it as an unknown command with a non-zero status, so it
  doubles as a stale-install probe without hard-coding a version string.
- **`CARGO_TARGET_DIR` is honoured as given.** It was overwritten with
  `$APP_DIR/target` and stripped from the environment on the privileged
  re-exec, so an inherited value was ignored, roughly 70 MB of build output was
  left in the checkout, and `--no-build` was unusable for anyone building to a
  custom directory. When unset it now defaults to a cache directory under
  `$XDG_CACHE_HOME`; an existing directory with prior artifacts is fine.
- **The X11 link pre-flight no longer requires `libXcomposite`.** No shipped
  binary links it — the `#[link]` attributes and `ldd` agree on `libX11` and
  `libX11-xcb` alone — so requiring its `-dev` package blocked ordinary
  installs over a dependency used solely by the C test client in `tests/`.
- **The generated fallback config no longer fails its own validation.** When
  `config/config.toml` is absent the installer wrote `[autostart] commands = []`,
  which the config loader discards as the wrong shape, so `--check-config`
  could never report the config as clean. The fallback omits the table instead,
  which keeps the compiled defaults.

### Added

- **Maverick Sessions**: a whole graphical unit — a real nested X server, a
  Maverick, the applications launched into it, a control socket, logs and a
  lifecycle — that is named, reproducible and controllable from `maverickctl`
  alone. `session create` takes a resolution, a refresh rate, a nested-server
  backend, the Maverick binary, a working directory, a debug mode, a
  compositor on/off switch, and passes everything after `--` to Maverick
  verbatim. `exec`, `shell` and `attach` run a program inside a session with its
  `DISPLAY`, `XAUTHORITY`, `MAVERICK_SESSION` and `MAVERICK_INSTANCE` already
  set. `process list|inspect|kill` describe and control the session's process
  graph, and `window list|inspect|focus|close|move|float|fullscreen` plus
  `camera`, `resize` and `layout` control it semantically. `inspect`, `logs` and
  `debug` report what the session is doing; every listing has a `--json` form.
  The user's own session is addressable as `main` by the same commands. See
  `docs/sessions.md`.
- **Window-targeted actions** in the window manager: `focus_window`,
  `close_window`, `float_window`, `fullscreen_window` and `move_window`, plus a
  percentage form of the column resize. Each dispatches to the *same* command
  the corresponding keybinding uses, so a tool and a keypress cannot reach
  different code.
- **A nested-X-server backend** with a documented comparison of the
  alternatives. Xephyr is the default because it is the only one that is both a
  real X server and visible; Xvfb is available for headless use. Display
  allocation, the per-session MIT-MAGIC cookie, readiness, teardown and cleanup
  are backend-independent.
- **`--session-id`, `--debug` and `--log-level` on `maverick`**: an instance can
  be published under a fixed name, which is what lets a session directory and a
  control socket be addressed by session name, and a session started with
  explicit arguments can set its own log level.
- **`query inspect`** and a per-monitor `screen`/`workarea` in the state
  snapshot, so a tool can report what an instance is running without an X
  connection of its own.
- **`tests/session-suite.sh`** and **`tests/session-security.sh`**: end-to-end
  coverage against a real X server, real windows and real applications, and
  checks of the four boundaries that keep one user's session out of another's
  reach. Neither simulates anything, and neither reports success when it could
  not run.

### Security

- **The control socket verifies its peer.** `SO_PEERCRED` reports the
  credentials the kernel recorded at connect time — the only part of a peer's
  identity a peer cannot assert — and a connection from any other uid is closed
  before a handler thread exists and before a byte is read. The owner is read
  from the kernel inside the server; there is no way to declare one's own.
- **The runtime directory is `0700`.** It was created with the process umask,
  which left it at `0754`/`0755` while each session directory was tightened to
  `0700` — leaking the *names* of sessions, and a name is the address of a
  control socket.
- **The control socket and session logs are `0600`**, set at creation rather
  than after it, so a session's stderr is never briefly world-readable.
- **Per-session X11 authentication.** Each display gets its own
  MIT-MAGIC cookie, written straight into the `.Xauthority` format in a `0600`
  file inside the session directory, and the X server runs with `-nolisten tcp`.
  No cookie appears in any command output, JSON document or log.

### Fixed

- **`maverick-msg` is gone.** Its capability — forwarding any line verbatim —
  is now `maverickctl`'s, with the same engine underneath. A word
  `maverickctl` does not recognise as a command is passed to the window
  manager. The installer, the test suite, the config comments and both READMEs
  name one client.
- **`MoveWindow(win, dir)` moved the focused window, not `win`.** It called
  `apply_move_dir`, which acts on the *focused* window in `sel_mon`'s active
  workspace, so a named window tiled somewhere else was moved in a tree it was
  not in. It now resolves the monitor, workspace and column the named window is
  actually in.
- **`ToggleFullscreen(Some(win))` rearranged the wrong monitor.** It used
  `sel_mon` for the camera recentre, the pending-focus consumption and the
  arrange, so on a multi-monitor setup the camera moved and another screen
  re-arranged. `ToggleFloat` gained the same targeted monitor resolution, which
  is also why it no longer rejects a target on a different monitor than
  `sel_mon` — that guard exists for a stale *focus* slot, and an explicit
  request carries no such ambiguity.
- **The nested X server's readiness wait had an inverted predicate**, so a
  healthy server was reported as having exited during startup and every
  `session create` failed with a reason pointing at an empty log.
- **`maverickctl` consumed the arguments of the program it launched.**
  `maverickctl exec debug alacritty --json` ran `alacritty` without `--json`.
  Everything after the program word is now the program's, and a bare `--` hands
  the rest over untouched.
- **The verbatim-forwarding path ignored the line's selection options**, so
  `maverickctl ping --session debug` resolved the target from context *and* sent
  the literal text `ping --session debug` as a protocol line. A global option
  may also now precede the verb.
- **A signed amount was read as an option**, so `maverickctl resize debug -10%`
  reported no argument at all. Flags are now recognised from the list this tool
  actually has; a dash alone is a value.
- **A session view could report a working directory of "null".** A `null` field
  read as the four characters `null` rather than as absent.

### Fixed (earlier in this release)

- **Floating windows no longer "jump around by themselves"** (two-authorities
  ping-pong). Root cause: the `ConfigureRequest` sink adopted the client's
  rect verbatim, but every `arrange` re-projected floats through
  `normalize_float_geom` (hint snap + workarea clamp), rewriting what was just
  promised — the client re-requested, the WM re-wrote, forever. The fix is a
  single-authority policy: a new `Client::float_client_authority` seal marks
  the float's rect as client-claimed when the WM adopts a request, and the
  arrange projection then re-emits that rect verbatim (protocol sanity only,
  `adopt_client_float_geometry`) instead of re-normalizing it. The seal is
  cleared exactly where the WM reclaims the geometry (drag start, window
  rules, relink with recentering, `ToggleFloat`, workspace/monitor moves,
  RandR/strut workarea changes — `reposition_floats`), and every "float
  gained a new context" path now settles the rect through the new pure helper
  `layout::settle_float_in_workarea` so the first arrange of the new context
  has nothing to correct (one configure, zero visible jumps).

### Changed

- **Pointer drags no longer change tiling membership** (niri-style drop
  removed by design decision): only already-floating windows are draggable
  (Button1 = move, Button3 = resize), a float released over a column stays
  floating, and a Mod4 drag on a tiled window is a no-op — tiles are moved
  with the keyboard (`Mod4+Shift+h/l/j/k`). The drag-to-tile preview
  highlight (`drag_target`) and the `drop_candidate` machinery were removed;
  `MoveResize` no longer sets the `FLOAT` flag.
- **No-compositor mode is now dwm-style**: zero animation. The per-frame
  X11 reconfiguration path (`arrange_live`) is removed; with the compositor
  off (env var, config, or a `--no-default-features` build) every state change
  lands on its final geometry in a single pass and the loop goes idle. The
  compositor path animates exactly as before.
- **Installer**: the Rust `maverick-installer` crate is replaced by a single
  `install.sh` bash script (same behaviour: release build with native-CPU
  optimization, binary installation to `/usr/local/bin` or `~/.local/bin`,
  X session desktop file, First Flight config, PATH check). Removed from the
  workspace; CI now syntax-checks `install.sh`.
- **`Mod4+Shift+Q` quits natively.** The default quit binding dispatches the
  WM's own `Action::Quit` (`Effect::Quit` → `begin_shutdown`): cooperative
  client close, one global budget, force-kill of stragglers, then `cleanup()`
  before the process exits 0. The shipped sample config now writes
  `action = "quit"` instead of spawning
  `maverickctl quit --confirm`, so no auxiliary process or prompt sits on the
  keyboard quit path.

### Removed

- **`maverick-setup`.** The host-probe / starter-config generator in
  `src/bin/` is gone, and `maverickctl` is the single supported control and
  session interface. The utility was built on every install and then discarded
  (the installer installs a fixed list that never included it), and it could
  not succeed: its default keybind table still emitted the removed
  `layout:grid` action, so `--write` always exited non-zero and left the invalid
  config it had already written on disk. `theme_palette`'s doc no longer claims
  a synchronisation obligation with a second hardcoded list of theme names.
- **An orphan session implementation.** `src/core/session.rs` defined
  `PersistedSession`, `ValidatedSession`, `SessionStage` and
  `SESSION_SCHEMA_VERSION` behind a staged `snapshot → parse → validate →
  commit` pipeline, but no `mod session;` ever declared it, so it had never
  been compiled and nothing referenced it. `src/core/mod.rs` nevertheless
  advertised it as a live module. Graphical sessions are modelled in
  `maverick_sys::session`; the module map now says so.

- **Grid layout.** The `Grid` mode described under `[0.18.4]` is no longer
  present. `LayoutKind` now has the single variant `Column`, the layout
  registry registers only `ColumnLayout`, and `LayoutKind::from_str` resolves
  every name to `Column` so existing config strings keep working. The
  `cycle_layout` helper no longer exists. Session records written by earlier
  versions still load: `src/core/session.rs` maps a `"grid"` tag onto
  `LayoutKind::Column`. The `[0.18.4]` entries describing `grid.rs` and the
  two-mode `Column`/`Grid` cycle are kept above as the historical record of
  that release.
- **`maverick-installer`.** The leftover legacy Rust installer directory is
  gone too: it was already out of the workspace and unused (nothing invoked
  it — `install.sh` is the official installer, CI only syntax-checks that
  script). `install.sh` + `tests/install-smoke.py` are unchanged.
- **`maverick-dialog`.** The standalone X11 confirmation client is gone from
  the workspace, `install.sh`, the installer's binary list, `maverickctl`'s
  confirmation fallback and the docs. The `maverickctl quit --confirm` flag
  itself stays: it is an explicit opt-in prompt (`zenity`/`kdialog`/TTY) for
  remote/scripted control, not part of the keyboard quit path. Dated release
  notes further down keep their `maverick-dialog` mentions as historical
  records.

### Fixed

- **Build**: unclosed delimiter in `maverick-sys/src/control.rs` (`query`
  handler) broke compilation of the whole workspace.
- **Floating windows**: `clamp_float_to_workarea` now uses saturating
  arithmetic so a workarea at a large negative origin (multi-monitor) can no
  longer overflow `i32` and park a float at an absurd position; degenerate
  0x0 workareas keep the window at the workarea origin.
- **Floating windows**: self-resizing floats (e.g. PrismLauncher's resource
  download dialog) no longer flicker bigger/smaller on every update. The
  float `ConfigureRequest` sink answered with the raw requested size,
  ignoring the client's own `WM_NORMAL_HINTS`, so a hint-respecting toolkit
  (Qt, Xt) corrected the answer with a follow-up `ConfigureRequest` on each
  update — one corrective bounce per resize. Both float sinks
  (`ConfigureRequest` and the `ConfigureNotify` follow path, which adopted
  the reported rect raw with no clamp at all) now route through
  `normalize_float_request` (hints snap, then workarea clamp, then a final
  settle onto the increment grid), and `WM_NORMAL_HINTS` is re-read on
  `PropertyNotify` so mid-life hint updates are honored. Invariant
  `INV-FLOAT-CONVERGE` (the WM's answer is a fixed point of the toolkit's
  correction) is pinned by unit tests in `src/backend/x11/render.rs`
  (helpers in `src/core/layout.rs`).

### Verified

- No-compositor mode (`MAVERICK_NO_COMPOSITOR=1`, `[compositor] enabled =
  false`, and binaries built `--no-default-features`): scroll layout, tiling,
  float toggle, fullscreen presentation, workspace hiding and focus transitions
  validated end-to-end in Xephyr (also with the release build).

Version note: the entries under `[0.18.4]` describe the window-manager
rewrite that forms the current `main` history. Earlier releases
(`[0.18.2]`, `[0.18.1]`) are retained as historical records of the
pre-rewrite codebase.


### Pending

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
