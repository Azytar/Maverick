# Maverick's architecture

Maverick is a columnar tiling window manager for X11. This document describes
what the window manager *is*, which subsystem owns what, and why there is no
compositor.

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
  pure State, layout,          Xlib/XCB bootstrap:
  camera, commands,            one shared connection
  IPC document shapes          (XInitThreads, open_display,
      │                        XGetXCBConnection handoff)
      │                            │
      └─────────────┬──────────────┘
                    │
                   X11
```

Three supporting crates sit alongside, not inside, the correctness path:

- **`maverick-toml`** — a zero-dependency TOML-subset parser. Configuration is
  read at startup and on `reload`; it never influences a frame.
- **`maverick-sys`** — the OS boundary: signals, `poll`, uid/gid, process-tree
  signalling, instance identity, the Unix-socket control protocol, and the
  `maverickctl` CLI engine (a second shipped binary).
Configuration is data. It is validated, normalised, and reported on at load
time (`src/userconfig.rs`), and it is never consulted to decide what a window's
geometry is.

### The runtime dependency set

Six crates, of which four are workspace-local:

```
maverick → maverick-core, maverick-x11, maverick-toml,
           maverick-sys, libc, x11rb
```

`maverick-core`, `maverick-x11` and `maverick-toml` have **zero** dependencies.
That is the property that keeps the layout and command logic testable without an
X server, a GPU, or a config file.

---

## There is no compositor

Maverick draws through X11 and nothing else. There is no `default` feature, no
optional renderer, and no second presentation path. `cargo build` and
`cargo build --no-default-features` produce the same binary; the only features
that exist are the two diagnostic trace builds (`input-trace`,
`window-trace`), which add logging and no dependencies.

This was a decision, not an accident, and the reason is worth recording.

### The compositor was not making the WM correct

Maverick shipped an OpenGL/GLX compositor. An audit of every path between the
compositor and the window manager found:

- The WM owned every rectangle X11 ever saw. `render::emit_geometry` was the
  single geometry sink; `reconciler::wire_geometry` the single clamping
  function. The compositor never issued a `ConfigureWindow` for a client.
- Across roughly 70 call and data edges, **exactly two** wrote WM `State` (both
  setting `monitors[i].layout_dirty`, a cache-invalidation flag) and **exactly
  one** changed what X11 requests the WM issued. **Zero** changed geometry,
  focus, stacking, or window-lifecycle outcomes.
- `Phase::Live` — the animated projection — never reached `ConfigureWindow`.
  `arrange_full` arranges at `Phase::Settled`; the only production caller of the
  live projection was the compositor's own `live_placements`. Removing the
  compositor therefore does not make the geometry approximate: it makes the
  integer `ConfigureWindow` rect *the* final rect.

### It masked a divergence it created

The reported symptom — movement that looked like it advanced in one-pixel
steps, or appeared to synchronise only once X11 caught up — was not a rounding
bug. X11 was configured **once, at the destination**, and the compositor then
drew the window from the source over 44 frames. Peak divergence: **1487 px**.

Worse, it manufactured a permanent second source of truth. X11 geometry is
`round()`ed at `src/core/layout.rs:649`; the compositor drew from a *separate,
unrounded* projection (`column_screen_extents_into`, `layout.rs:750`) held in
its own cache. The two disagreed by up to 0.5 px forever, with nothing
reconciling them, because the fractional value had no authority: hit-testing,
`client.geom`, the pointer warp and the client's own `ConfigureNotify` all read
the integer.

It was also a measured cost to the WM, not only to the GPU: a documented 6x
key-latency regression (worst case near 90 ms, down to 14 ms after a fix)
attributed to GLX round-trips drying the X socket between polls.

### What is gone

Animation, opacity, drop shadows, GL-drawn rounded corners, and sub-pixel
motion. Rounded corners survive as the X11 `Shape` mask
(`render::sync_rounded_frame`), which was already implemented as the
non-compositor path and is now the only one.

`_NET_WM_BYPASS_COMPOSITOR` and `_NET_WM_WINDOW_OPACITY` are still **published**
on Maverick's own windows. Those are EWMH interoperability aimed at *external*
compositors, not at Maverick's own, and removing them would be a regression for
users who run one.

---

## Ownership boundaries

These are the rules the code is arranged around. Each is stated with the thing
that would break if it were violated.

**One geometry sink.** Every `ConfigureWindow` for a client is issued by
`render::emit_geometry`, driven by the `Reconciler` diffing `DesiredState`
against `AppliedState`. No other code positions a window.

**One animation policy, and it is the settled one.** `run_once` calls
`State::snap_animations()` on every turn (`backend::x11/mod.rs`). The logical
state is left settled, which is what makes the integer rect the final rect.

**One loop, and it is idle.** The event loop drains the X queue, settles
animation, and blocks on X11 plus the control self-pipe with no frame deadline.
There is no heartbeat and no frame timer: an idle session costs no CPU.

**`maverick-core` is pure.** No X11, no clock, no filesystem, no environment.
`State`, `Camera`, layout and the command layer are deterministic functions of
their inputs, which is why most of the test suite needs no display.

**`maverick-sys` owns the OS boundary and nothing above it.** It holds ~225
production lines of FFI (signal disposition, `poll`, credentials, process-group
signalling) and a control protocol. Signal disposition stays on `libc` because
rustix does not implement `sigaction` — verified at
`rustix-1.1.4/src/not_implemented.rs:72`.

**Configuration is data, not control flow.** `[compositor]` in a config file
still parses, because it is the historical home of the `stiffness`/`damping`
animation aliases, and every other key in it reports itself as ignored rather
than being silently accepted.

---

## Coordinate spaces

Two spaces, and the boundary between them is a single `round()`:

| space | type | owner |
|---|---|---|
| world | `f32`, fractional | `maverick-core` layout and camera |
| screen / X11 | `i32` | `render::emit_geometry` |

World coordinates stay fractional through the whole layout pass. The conversion
happens once, at the X11 boundary, at `src/core/layout.rs:649`:

```rust
let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
```

`Camera::step` integrates in `f64` and never reads the rounded `position` back,
so quantisation cannot accumulate in the camera. The one running `f32` sum is
the per-column `x += w + gap_f` (`layout.rs:485`); it measures 0.028 px at 50
columns, 0.41 px at 200 and 0.93 px at 500, and it is the only accumulation in
the pipeline.

The consequence of there being no compositor: **the integer rectangle in X11 is
the only rectangle.** There is no second geometry model to disagree with it.

---

## Deliberate non-goals

- **No compositor.** Stated above.
- **No renderer abstraction.** A `maverick-render` crate once defined
  `Renderer`/`Texture` traits for "the compositor backends". Nothing implemented
  them; the file that re-exported them carried an `#[allow(unused_imports)]`
  because the compiler had noticed. It was deleted rather than kept as a
  placeholder, because an abstraction held alive by a comment is the thing that
  had already gone wrong once.
- **No Vulkan backend.** `maverick-vk` is an unwired bootstrap. No crate
  depends on it, so it is excluded from the workspace rather than deleted. A
  `compositor-vulkan` feature once named it and selected nothing, which is how
  a user could get an error telling them to rebuild with a flag that would have
  done nothing.

---

## Checking these claims

```bash
cargo build                              # the only configuration
cargo test --workspace                   # 862 tests, no display required
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

If the window manager ever needs a visual layer again, the seam it must be built
behind is the one this document describes: authoritative WM state, one
published rectangle per window, and presentation that reads that state and
writes none of it back. The specific failure mode to avoid is the one the
compositor had — re-projecting geometry the WM had already settled, so that the
screen and X11 disagree.
