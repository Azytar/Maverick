# Maverick's architecture

Maverick is a columnar tiling window manager for X11. This document describes
what the window manager *is*, which subsystem owns what, and where composition
belongs when it is somebody else's job.

It is a description of the code as it stands, not an aspiration. Every claim
here has a `file:line` behind it.

---

## The shape

```
              maverick (bin)
                    │
      ┌─────────────┴──────────────┐
      │                            │
  maverick-core               maverick-x11
  pure domain types:          Xlib/XCB bootstrap:
  State, Monitor,             one shared connection
  Workspace (a View),          (XInitThreads, open_x,
  ViewId, Carousel,            XGetXCBConnection handoff)
  Column, Client, Rect
      │                            │
      └─────────────┬──────────────┘
                    │
                   X11
```

The workspace holds six crates — the root package `maverick` plus the five members
at `Cargo.toml:2-8` — and every one of them is built from this tree. Four are
libraries the window manager links; two of them are the `maverick-core` and
`maverick-x11` in the diagram above, and two more sit alongside that path rather
than inside it:

- **`maverick-toml`** — a zero-dependency TOML-subset parser. Configuration is
  read at startup and on `reload`; it never influences a frame.
- **`maverick-sys`** — the OS boundary: signals, `poll`, uid/gid, process-tree
  signalling, instance identity, and the Unix-socket control protocol.

**`maverickctl`** is the sixth crate, and it is separate from the correctness
path: it ships the control client binary, with CLI parsing, the control-socket
client, instance discovery and session orchestration. It links `maverick-sys`
for the shared protocol surface and never links the window manager.

Configuration is data. It is validated, normalised, and reported on at load
time (`src/userconfig.rs`), and it is never consulted to decide what a window's
geometry is.

### The runtime dependency set

Two external crates, five workspace-local ones:

```
maverick → maverick-core, maverick-x11, maverick-toml,
           maverick-sys, libc, x11rb
```

`maverick-core` and `maverick-toml` have **zero** dependencies
(`maverick-core/Cargo.toml:13` and `maverick-toml/Cargo.toml:13` both open an
empty `[dependencies]` table), and `maverick-x11`'s only one is `x11rb`. That is
the property that keeps the layout and command logic testable without an X
server, a GPU, or a config file.

---

## There is no compositor

Maverick draws through X11 and nothing else. There is no `default` feature, no
optional renderer, and no second presentation path: `[features]` in
`Cargo.toml:59-63` declares `input-trace` and `window-trace` and nothing else,
neither is on by default, and both are empty feature sets that only turn on
logging. `cargo build` and `cargo build --no-default-features` therefore select
the same code, and the runtime dependency list is exactly `Cargo.toml:43-54`:
`maverick-core`, `maverick-sys`, `maverick-toml`, `maverick-x11`, `libc` and
`x11rb`. No GL, Vulkan or graphics-library binding appears in any manifest.

Every state change therefore reaches the server as one configure at its final
position. The properties that govern *presentation* are the window manager's
protocol surface, not a drawing layer:

- `render::emit_geometry` (`src/backend/x11/render.rs:924`) is the single X11
  geometry sink, and `reconciler::wire_geometry`
  (`src/backend/x11/reconciler.rs:100`) is the single clamping function. No other
  code positions a window.
- The camera is a plain `f32` scroll offset (`maverick-core/src/types.rs:367-370`)
  and `Workspace::overview_scale` / `Workspace::page_zoom` are plain `f32`
  values that a mutator assigns outright. Nothing eases toward a target, so
  there is no second projection for a window's size to differ between.
  `overview_scale` is *fixed* when Overview is entered —
  `layout::overview_entry_scale_for` takes `Cfg::overview_scale` as the target
  (reducing it only when the focused tile would not fit even there, never below
  `Cfg::overview_zoom_min`) — and no navigation, resize, map or unmap path
  writes it again, so the scale cannot accumulate across presses; navigation
  only pans the camera.
- `Column` carries `windows`, `weight` and `focused` and nothing else
  (`maverick-core/src/types.rs:300-307`); a column's focus status is read off
  those fields rather than tracked as a separate boost.
- Corner radius is an X11 `Shape` mask applied by `render::sync_rounded_frame`
  (`src/backend/x11/render.rs:1020`) — server-side window geometry, not a drawn
  effect, and it is dropped for fullscreen because there is no desktop behind
  the window to reveal.
- The event loop blocks on X11 plus the control self-pipe with no frame
  deadline, no heartbeat and no timer (`src/backend/x11/mod.rs:662`). An idle
  session costs no CPU.

Compositing is an external program's job, and it composes with a window manager
over the wire. Maverick publishes the two EWMH properties that let it:

- `_NET_WM_BYPASS_COMPOSITOR` is interned at `src/backend/atoms.rs:130` and set
  to `2` while a window holds a true exclusive fullscreen, then deleted when it
  leaves (`src/backend/x11/manage.rs:1266-1273`). `ToggleFullscreen`
  (`src/core/commands.rs:1137`) promotes the window's policy to
  `FullscreenPolicy::True` before it emits the effect, so the bypass is never
  published for a window that is not really exclusive. The comment there names
  the consumer: an external compositor such as picom skips its effect pass for
  that window.
- `_NET_WM_WINDOW_OPACITY` is interned at `src/backend/atoms.rs:127` and written
  per window at manage time from a rule's `opacity`
  (`src/backend/x11/manage.rs:390-396`).

Both are addressed at compositors running *outside* Maverick. Nothing in the
tree draws pixels of its own, so there is no such consumer for them here.

---

## Ownership boundaries

These are the rules the code is arranged around. Each is stated with the thing
that would break if it were violated.

**A View is a logical container, and its identity is not its position.**
`Workspace` *is* a View (`maverick-core/src/types.rs:693`); it owns no window,
requests no pixmap and needs no rendering surface to exist. Its `id` is a
`ViewId` (`maverick-core/src/types.rs:431`) — monotonic, per-monitor, and never
reused, which is what makes a stale `Client::workspace` detectable instead of
silently re-pointing at whichever View inherited the old position. Existence and
order belong to `Monitor::workspaces`
(`maverick-core/src/types.rs:1330-1351`); a `ViewId` survives a reorder, and a
removal shifts every later position without invalidating any reference.

**The Carousel owns selection, and nothing else.** `Carousel`
(`maverick-core/src/types.rs:483`) holds `current`, `origin` and the id counter,
and it is handed the monitor's list on every call rather than keeping a second
copy of it. It has no `match layout` and no `LayoutKind` in it
(`maverick-core/src/types.rs:461-464`): navigation must answer the same question
whichever layout is installed. Both pointers are repaired by `attach`/`detach`
(`maverick-core/src/types.rs:559,580`), so `return_to_origin` is total.

**Scroll does not own View navigation.** `arrange`
(`src/core/layout.rs:210-236`) is the layout's only entry point, consumes
exactly one View — the active one, resolved through `mon.carousel.current()` —
and returns geometry. It never creates, removes, selects or re-orders a View, and
it never mutates View membership. Adding a second layout therefore has nothing
to negotiate with the Carousel.

**Floating clients are outside the layout.** A floating client lives in
`Workspace::floats`, not in `Workspace::columns`
(`maverick-core/src/types.rs:698,705`), which is exactly why the layout's input
excludes it; `present_into` projects it from `Client::geom` instead. A client is
referenced from exactly one of the two lists, on exactly one monitor (invariant
A in `maverick-core/src/lib.rs:30-33`).

**One geometry sink.** Every `ConfigureWindow` for a client is issued by
`render::emit_geometry`, driven by the `Reconciler` diffing `DesiredState`
against `AppliedState`. No other code positions a window.

**One projection, and it is the geometry.** `arrange` takes a state, a monitor
index, a config and an output buffer (`src/core/layout.rs:210-216`) — no frame
delta and no interpolation target. The camera is a number the layout reads, not
a value easing toward another. The integer rect the reconciler writes is
therefore the only rect there is, with no second geometry model to disagree
with it.

**One loop, and it is idle.** The event loop drains the X queue, flushes the
geometry it owes, and blocks on X11 plus the control self-pipe with no frame
deadline. There is no heartbeat and no frame timer: an idle session costs no
CPU.

**`maverick-core` is pure.** No X11, no clock, no filesystem, no environment.
`State`, `Carousel`, `Camera`, layout and the command layer are deterministic
functions of their inputs, which is why most of the test suite needs no display.

**`maverick-sys` owns the OS boundary and nothing above it.** Signal
disposition, `poll`, credentials and process-group signalling live in
`maverick-sys/src/lib.rs`; the line-oriented control protocol server in
`control.rs`; the instance identity record in `identity.rs`; the control hub's
self-pipe in `hub.rs`; minimal JSON document helpers in `json.rs`. The crate
holds no rendering, layout or X11-protocol concern, and session orchestration
lives in `maverickctl`. Signal disposition stays on `libc` because rustix does
not implement `sigaction`; the manifest records that reason at
`maverick-sys/Cargo.toml:14-15`, and the claim is checkable in the dependency
itself — `rustix-1.1.4/src/not_implemented.rs:72` is
`not_implemented!(sigaction);`. The crate is pinned to that resolution in
`Cargo.lock:290-292`.

**Configuration is data, not control flow.** A table Maverick has no field for
— `[compositor]`, `[animations]` — is not parsed, and it is skipped without a
diagnostic, because that is the shape a config written for a different Maverick
takes and refusing it would break loading for no gain
(`src/userconfig.rs:445-455`). A key inside a table Maverick *does* know is the
opposite case: it is reported as unknown, because a silently ignored key is a
setting the user believes is in force and is not
(`src/userconfig.rs:493,648-651`).

---

## Coordinate spaces

Two spaces, and the boundary between them is a single `round()`:

| space | type | owner |
|---|---|---|
| world | `f32`, fractional | `maverick-core` layout and camera |
| screen / X11 | `i32` | `render::emit_geometry` |

World coordinates stay fractional through the whole layout pass. The conversion
happens once, at the X11 boundary, at `src/core/layout.rs:602`:

```rust
let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
```

`Camera` holds a plain `f32` that is rounded exactly once, here, so
quantisation cannot accumulate in the camera. The one running `f32` sum is
the per-column `x += w + gap_f` (`src/core/layout.rs:439`); it measures 0.028 px
at 50 columns, 0.41 px at 200 and 0.93 px at 500, and it is the only
accumulation in the pipeline.

Rearranged, that line is a camera: `screen = (wa.x + wa.w/2) + alpha * (world -
(cam + wa.w/2))`. `Camera::position` is the pan and `alpha` is the scale, both
View state and neither per window. The consequence on X11: **the camera's scale
is necessarily written to `ConfigureWindow`.** There is no compositor to render
a client at a size the server does not hold, so "zoom the view out" and "make
every tile's rectangle smaller" are the same physical operation here. That is
what makes Overview the one view that changes geometry visibly: the entry scale
is a real reduction (`Cfg::overview_scale`, `0.76` by default) taken once on
entry, never derived per projection. What keeps it from degenerating is that
rule — the scale reads the *focused tile*, never the ribbon, so the client count
shapes the scrollable content and not the size of a step — plus the two floors
(`overview_zoom_min` at the entry, `ALPHA_MIN` at the read-back) and the fact
that navigation pans the camera through `overview_scroll` without ever writing
the scale again. There is no "real" graphical camera that pans over unscaled
pixels; the viewport is a WM-native approximation: reduced tiles plus a camera
that scrolls the ribbon, and leaving the mode restores the settled rectangles
exactly because the layout's own geometry was never written.

The consequence: **the integer rectangle in X11 is the only rectangle.**

---

## Deliberate non-goals

- **No compositor.** Stated above, with the file and line behind each part of it.
- **No renderer abstraction.** No manifest in the workspace declares a graphics
  backend, no `Renderer` or `Texture` trait is defined anywhere in the tree, and
  the six local crates listed above are the whole of what can be built. Where a
  source comment says "the renderer", it means `src/backend/x11/render.rs` — the
  module that writes geometry to X11 — not a graphics backend. Its whole
  presentation surface is a `Shape` mask and a set of `ConfigureWindow` calls.
- **No Vulkan backend.** `maverick-vk` is not a member of the workspace, not a
  directory in the tree, and not named by any manifest. There is no
  `compositor-vulkan` feature to select: `[features]` in `Cargo.toml:59-63` holds
  exactly the two diagnostic trace features.
- **No second layout.** `LayoutKind` (`maverick-core/src/types.rs:1584-1587`) has
  exactly one variant, `Column`, and `layout_from`
  (`src/core/action.rs:181-188`) accepts no other name. There is no Mosaic layout
  in this tree; the Carousel already answers navigation independently of
  `LayoutKind`, so adding one would not touch it.
- **No feature that selects a backend.** A build has one shape. `cargo build`
  and `cargo build --no-default-features` compile the same code, and the
  installer's `--no-default-features` and `--with-compositor` spellings are
  rejected with a message saying there is nothing to select
  (`installer/install.sh:79-85`).

---

## Checking these claims

```bash
cargo fmt --all -- --check                            # read-only formatting gate
cargo build                                            # the only configuration
cargo check --workspace --all-targets                  # includes #[cfg(test)] code
cargo test --workspace                                 # the whole suite, no display required
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

A visual layer, if the window manager ever needs one, belongs behind the seam
this document describes: authoritative window manager state, one published
rectangle per window, and presentation that reads that state and writes none of
it back. The failure mode to avoid is re-projecting geometry the window manager
has already settled, so that what is on screen and what X11 holds disagree.
