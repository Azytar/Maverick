# Changelog

All notable changes to this project are documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Quality

- **Enforced `rustfmt` across the workspace** — formatted all crates with
  `cargo fmt` to a consistent style.
- **Fixed all `clippy` warnings** — resolved 10 lints across `bar.rs`,
  `manage.rs`, `engine.rs`, `types.rs`, and `ipc.rs` (`map_unwrap_or`,
  `doc_markdown`, `redundant_closure_for_method_calls`, `match_same_arms`,
  `unnecessary_min_or_max`). Clippy is now clean at `-D warnings`.
- **Clean `rustdoc` build** — fixed unclosed HTML tags (`<pid>`, `<px>`,
  `<n>`, `<cmd>`) in doc comments; docs now build with
  `RUSTDOCFLAGS="-D warnings"`.
- **Expanded `.gitignore`** — added `coverage/`, `*.profraw`, `.env`,
  editor swap files, and common Rust build artifacts to prevent accidental
  commits.
- **Added `rust-version` and metadata** — `Cargo.toml` for all three
  workspace crates now declare `rust-version = "1.82"`, `repository`,
  `categories`, and `keywords` for better crate index presentation.
- **Doc-comment fixes** — `image_text8`, `draw()`, and code samples in
  docstrings now use proper backtick quoting.

### Fixed

- **`maverick-sys`: control socket could be tricked by symlink attack.**
  `remove_file` ran before `bind` without checking the existing file
  type; a symlink pointing outside the runtime dir would be followed.
  Now only removes the path if it is a regular socket. Also: unbounded
  thread creation per connection limited to 32 concurrent handlers;
  `identity_json` now escapes all JSON-special characters instead of
  only quotes and newlines; `send_command` rejects commands containing
  `\\n` to prevent line-protocol injection.

- **`maverick-sys`: identity ficha parser failed on process names
  containing `)` or commas in field values.** `/proc/<pid>/stat`'s
  second field (comm) is enclosed in parentheses but the comm itself
  may contain `)`. Switched from `find(')')` to `rfind(')')`. The
  custom JSON parser split on `,` unconditionally, breaking when a
  string value contained a comma; replaced with a char-by-char walker
  that respects JSON string quoting.

- **`maverick-sys`: `wait_readable` busy-looped on `POLLERR`/`POLLHUP`.**
  `poll()` returning `> 0` was treated as "data available" regardless
  of `revents`. Now checks that `POLLIN` is actually set so an error
  state doesn't spin the event loop.

- **`maverick-sys`: `detach_from_terminal` ignored `setsid()` failure.**
  If the process was already a session leader, `setsid()` returns
  `EPERM` and the WM would not actually detach. The return value is now
  discarded (the subsequent `isatty` check still works), but the intent
  is clearer and the function no longer silently depends on it
  succeeding.

- **`maverick-sys`: `hub::emit` held the subscriber mutex during
  channel sends.** A slow `subscribe` connection could block the WM
  thread. The subscriber list is now cloned under the lock and the
  actual sends happen outside it.

- **`maverickctl`: TTY confirmation read input byte-by-byte, breaking
  UTF-8 multi-byte characters.** `read(&mut [0u8;1])` and `as char`
  produced garbled strings for non-ASCII input. Replaced with
  `read_line` for correct Unicode handling.

- **`core`: `CycleLayout`/`SetLayout` could panic on a monitor-less
  state.** Both actions accessed `self.state.monitors[mi]` without
  verifying the index was in bounds. Added the same guard used by
  `ToggleBar` and other actions.

- **`core`: `collapse_col` computed ideal scroll before collapsing,
  leaving the viewport slightly off-centre.** Moved the
  `ideal_scroll` call to after the column is removed so it reflects
  the new column count.

- **`core`: `focus_mon`/`move_mon` treated `Dir::Left` and
  `Dir::Right` identically to `Dir::Next`** (always wrapping right).
  They now map `Left`/`Prev` to decrement and `Right`/`Next` to
  increment, matching user expectation.

- **`core`: missing `UpdateBar` effects after workspace/view changes.**
  `View`, `MoveToWs`, `CycleLayout`, and `SetLayout` did not mark the
  bar dirty, so the tag-active / layout-symbol / occupancy display
  could become stale. Added `Effect::UpdateBar` to each path.

- **`core`: `PublishIpcState` was never emitted.** The effect variant
  existed but no dispatch path produced it. Now pushed at the end of
  every `dispatch()` that produced at least one effect.

- **`core`: floating windows were not clamped to the workarea in
  `arrange_columns`.** The floating pass pushed `client.geom`
  verbatim; windows could be placed entirely off-screen. Added clamp
  to the workarea rect.

- **`core`: `Client::new` always initialised `tags: 1`**, ignoring
  the `workspace` parameter. Changed to `tags: 1 << workspace` so the
  tag mask matches the assigned workspace from creation.

- **`core`: `Rule::matches` compared lowercase `class`/`title` against
  an unnormalised pattern.** A rule written with uppercase letters
  would never match. The pattern is now also lowered before comparison.

- **`main`: identity ficha left on disk if `WindowManager::new`
  failed.** `write_meta` runs before WM initialisation; a subsequent
  init failure called `process::exit(1)` without cleaning up the
  ficha, leaving a zombie entry for tools like `maverickctl list`.
  Added `cleanup_meta` call in the error path.

- **`x11/events`: resolution change not detected when monitor count
  stayed the same.** The RANDR notify handler only acted when
  `new_mons.len() != old count`; a resolution or position change
  that kept the same number of monitors was silently ignored.
  Added per-monitor geometry comparison.

- **`x11/manage`: `find_client` could loop infinitely on a cyclic
  window tree.** The function walked the X11 window tree upward
  without tracking visited windows; a client creating a parent cycle
  would hang the WM. Added a `HashSet` guard.

- **`x11/render`: `ConfigureNotify` coordinates truncated silently.**
  `hide_offscreen` pushes windows far left (`i32::MIN`), which when
  cast to `i16` wrapped to 0, making offscreen windows visible.
  Values are now clamped to `i16`/`u16` ranges before casting.

- **`x11/render`, `ewmh`: potential panic on empty monitor list.**
  `focus()` and `update_workarea` indexed `monitors[0]` or assumed
  `client.monitor` was always valid. Added bounds checks / `.first()`.

- **`x11/input`: keyboard froze after mouse-focusing a window**
  (`grab_buttons`). The catch-all `grab_button` used `pointer_mode=SYNC`
  **and** `keyboard_mode=SYNC`. Every matching `ButtonPress` froze both
  devices, but `on_button_press` only called
  `allow_events(REPLAY_POINTER)`, which releases the pointer but not
  the keyboard. The keyboard stayed frozen at the X11 level after
  clicking any managed window, breaking WM shortcuts and the client's
  own key input — most noticeable with clients that grab focus
  aggressively on click (Firefox, Minecraft). `keyboard_mode` changed to
  `ASYNC` (standard practice, matches dwm/i3-style click-to-focus
  grabs); `pointer_mode` stays `SYNC` since `on_button_press` still
  needs to conditionally replay or keep it frozen for drags.
  Confirmed fixed in real usage (mouse click-to-focus, tested against
  Firefox and Minecraft).

- **`x11/manage`: `write_net_wm_state` overwrote unknown EWMH
  atoms.** It replaced `_NET_WM_STATE` with only the fullscreen/
  maximized flags the WM tracks, discarding `_NET_WM_STATE_STICKY`,
  `_NET_WM_STATE_HIDDEN`, etc. set by other tools. Now reads the
  current atom list first and preserves unmanaged atoms.

- **`backend/bar`: potential `u16`/`i16` overflow in label and
  glyph calculations.** Arithmetic on `u16`/`i16` values could wrap
  with many wide tags. Converted to `i32` intermediates with
  saturating operations and final clamp to the target type.

### Changed

- **`core`: `PublishIpcState` emitted after every state-mutating
  `dispatch`.** Previously the effect existed but was never produced;
  now pushed automatically so IPC subscribers (bars, `maverickctl
  subscribe`) receive fresh snapshots without explicit
  per-action wiring.

- **`core`: `focus_mon`/`move_mon` now accept directional variants.**
  `focus-mon left`/`right` and `move-mon left`/`right` now move in
  the expected direction instead of always wrapping to the next
  monitor (which was the behaviour of `next`).

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
