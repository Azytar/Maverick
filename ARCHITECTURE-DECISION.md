# Maverick — Compositor Removal & Unix Architecture Decision

Status: **decision record, no implementation performed**
Baseline: `db622f3` · 86,434 Rust LOC · 1,094 `#[test]` attributes
Inputs: `CARGO-AUDIT.md`, `COMPOSITOR-AUDIT.md`, `SYS-AUDIT.md`, `MOVEMENT-AUDIT.md`, `TEST-AUDIT.md`
All claims below were independently re-verified by the lead against source; where an
agent's evidence did not survive verification it is struck and marked.

---

## 0. The campaign premise, tested

The brief asked whether `maverick-gl`, `maverick-render`, `maverick-img` and
`maverick-sys` should remain. Four independent audits, and my own verification of
their load-bearing claims, produced a materially different answer than the brief
anticipated:

| Premise | Verdict | Evidence |
|---|---|---|
| Cargo surgery will yield a much smaller graph | **False** | `--no-default-features` is already 15/15 justified; the delta is **one** crate |
| `maverick-img` is compositor infrastructure | **False** | `rootwall.rs` is the *feh-style* path, ungated, 5 call sites |
| `maverick-sys` is a removable OS wrapper | **False** | ships `maverickctl`; 68.6% is app logic, but 225 LOC of FFI is irreducible |
| The compositor masks a movement bug | **Partly true, wrong mechanism** | it masks a *divergence*, not a precision loss |
| The WM needs the compositor to be correct | **False** | removal is **geometrically exact and instant** |

The campaign's real value was not deletion volume. It was that the audits
localized **four genuine architectural defects**, none of which a feature flag
fixes. Those are the subject of this document.

---

## 1. Can Maverick operate correctly without `maverick-gl`?

**Yes. It already does, and the result is not merely acceptable — it is exact.**

Verified: `render.rs:597` — `arrange_full` calls `arrange_full_phase(..., Phase::Settled)`.
The only production call to `arrange(..., Phase::Live, ...)` in the entire tree is
`compositor_gl.rs:4189`, the compositor's `live_placements`. Every other `Phase::Live`
site is `core/present.rs`, `core/invariants.rs`, `core/framebench.rs`, `bench_arrange.rs`,
or `layout.rs` tests.

With the compositor removed, `mod.rs:915-927` calls `State::snap_animations()`, the
camera spring is never integrated, and the integer `ConfigureWindow` rect *is* the
final rect. Measured: both paths converge to `x = -1519`.

**This is the load-bearing result of the entire campaign.** The success criterion —
"no visual subsystem required to make the WM correct" — is **already satisfied**.

## 2. Is `maverick-gl` hiding a WM defect?

**It hides a divergence, not a precision loss — and the divergence is one it creates.**

The reported symptom ("~1-pixel increments", "visually synchronized only after X11
catches up") is *not* a rounding artifact. There is no wheel delta in the codebase:
`pointer.rs:668-677` `scroll_camera_with_wheel(detail: u8, ...)` maps `7|5 => Dir::Right,
_ => Dir::Left` and dispatches `Action::FocusDir(dir)` — discrete focus-column movement.
There is no 120-unit accumulation to lose. (The function name is a misnomer.)

The actual mechanism:

> X11 is configured **once**, at the destination, at t=0. The compositor then draws the
> window from the source, over 44 frames. **Peak X11-vs-screen divergence: 1487.4 px.**

They agree only when the spring settles. That is exactly the reported symptom, with the
direction reversed — X11 is *ahead*; the screen catches up.

Worse, the compositor manufactures a second, permanent divergence **from itself**:
X11 geometry is `round()`ed at `layout.rs:649`; the compositor draws from a separate
**unrounded** projection (`column_screen_extents_into`, `layout.rs:750`) fed in as
`visual_x_cache` (`compositor_gl.rs:4139-4172`). Measured peak split **0.4707 px**,
permanent, with nothing reconciling it — because the fractional value has no authority.
X11 hit-testing, `client.geom`, `find_client`, the pointer warp, and the client's own
`ConfigureNotify` all read the integer.

So: the compositor is **masking a defect of its own making**, and the mask is partial
(only tiled columns; floats, maximized and fullscreen overlays are excluded —
`compositor_policy.rs:81`).

Precision loss elsewhere is real but small and *not* the symptom:
`layout.rs:614` truncates (`.max(1.0) as u32`, no `round()`) — ~0.4 px, one-signed;
`layout.rs:485` `x += w + gap_f` is a genuine f32 running sum — 0.028 px @ 50 columns,
0.41 px @ 200, 0.93 px @ 500. The camera itself is clean: `Camera::step` integrates
`f64` and never reads the rounded `position` back.

## 3. Is `maverick-render` required?

**No. Delete it.** 405 LOC + 3 tests. Zero implementors of `Renderer` or `Texture`.
`src/backend/renderer.rs:26` re-exports under `#[allow(unused_imports)]`, and the
file's own header states *"no production code imports the types below"*.
`maverick-gl` carries its own parallel type set and never references it.

This is also a **pre-existing violation of this campaign's own code-quality gate**
(an `#[allow]` already in the tree).

## 4. Is `maverick-img` required?

**Yes, and it is mandatory — the brief's framing is wrong.**

`src/backend/x11/rootwall.rs` is titled *"Root-pixmap wallpaper — the no-compositor,
feh-style path"*, calls `maverick_img::decode`, is compiled **ungated**
(`mod.rs:106`, bare `mod rootwall;`), and is called from five unconditional sites
(`events.rs:595`, `mod.rs:904`, `mod.rs:1320`, `actions.rs:436`, `actions.rs:481`).
Deleting or making it optional **breaks the non-composited WM**. It also has a second
consumer (`compositor_gl.rs`), so it must not be inlined. `KEEP` mandatory.

## 5. Is `maverick-sys` justified?

**Justified in substance, wrong in scope. Split it — do not delete it.**

- Ships a **user-facing binary**: `maverickctl`, auto-discovered from
  `maverick-sys/src/bin/maverickctl.rs`. `install.sh:150`
  `RUNTIME_BINS=(maverick maverickctl)`, with loops at 1603/1617/1624;
  `install.sh:1488` builds `-p maverick -p maverick-sys`. `tests/install-smoke.py`
  asserts the binary set. Deleting the crate deletes a shipped artifact.
- The FFI is irreducible and correctly confined: 4 production `unsafe` blocks, all
  SAFETY-noted. The rustix justification **verifies** —
  `rustix-1.1.4/src/not_implemented.rs:72 not_implemented!(sigaction);` (rustix's
  `runtime::kernel_sigaction` exists at `runtime.rs:503` but its own docs at
  `runtime.rs:41-46` say it is for implementing a libc). `Signal` owns a real invariant:
  SIGCHLD always `SA_NOCLDWAIT|SA_RESTART` (`lib.rs:326-332`), refused dispositions
  reported not swallowed (`lib.rs:396-416`).
- **68.6% is application logic, not an OS boundary.** `session/` + `ctl/` + `discover/`
  = 7,563 of 13,119 production lines. Genuine FFI ≈ **225 lines (1.7%)**.
  `categories = ["os::unix-apis"]` is wrong for two-thirds of the crate.
- API is enormously over-broad: **273 public items, ~32 externally referenced.**
  3 have zero callers workspace-wide: `ctl::dispatch_to` (`ctl/mod.rs:474`),
  `discover::find_by_display` (`discover.rs:121`), `Session::xserver_is_up` (`session/mod.rs:365`).
- **"Removing it removes rustix" is false** — struck. `Cargo.lock:447`: `x11rb 0.13.2`
  already depends on `rustix` non-optionally. `maverick-sys` only adds the `process` and
  `rand` *features*, which add no package. `libc` also stays (root `Cargo.toml:48`).

## 6. Which dependencies does the WM actually require?

**Already correct.** 15/15 crates in the `--no-default-features` graph are justified.
One genuine misconfiguration exists: `x11rb`'s `sync` feature is unconditional, but its
only use site is the XSync fence at `compositor_gl.rs:76`, inside a compositor-only file.
Its siblings `composite`/`damage`/`fixes` **are** correctly gated. Move `sync` into
`compositor-opengl`.

> **Method note:** `cargo tree -e features` does not render `x11rb/composite`-style
> edges in this workspace. All feature claims here come from
> `cargo metadata`'s `resolve.nodes[].features`, which is authoritative.

## 7. Which code exists solely for the compositor?

15,485 lines certain (incl. already-dead `maverick-vk`), 16,750 including partials.
From `COMPOSITOR-AUDIT.md` §9, which I spot-verified: `compositor_gl.rs` (5,763, wholly
`cfg`-gated), all of `maverick-gl/src` (5,043) + tests (740), all of `maverick-vk`
(2,190 + 1,299), all of `maverick-render` (405) + `src/backend/renderer.rs` (29),
`compositor_policy.rs` (204 production), the `WallpaperGpu` trait (16).

**Not deletable:** `rootwall.rs` (the fallback path), `framesched.rs` (used in both
configurations), `core/{layout,present}.rs` (feature-independent), and
`_NET_WM_BYPASS_COMPOSITOR` handling (EWMH interop with *external* compositors — keep).

## 8. What can be deleted?

| target | LOC | confidence | note |
|---|---|---|---|
| `maverick-render` + `src/backend/renderer.rs` | 434 | certain | already dead |
| `maverick-vk` | 3,489 | certain | **unreachable** — zero reverse deps, `cargo tree -i` errors. `exclude`, do not delete |
| `compositor-vulkan = []` feature | 0 | certain | selects nothing |
| `x11rb/sync` → gated | 0 | certain | misfiled |
| `compositor_gl.rs`, `maverick-gl/*`, `compositor_policy` | ~11,750 | certain | only if the compositor is removed |
| 3 zero-caller `maverick-sys` APIs + 2 dead re-exports | — | certain | |
| **Unconditional win, no compositor removal needed** | **~3,929** | | `render` + `vk` + feature hygiene |

## 9. Which abstractions violate the intended Unix architecture?

1. **The compositor is the only clock for authoritative WM state.**
   `State::tick_animations_multi` has **one** call site, lexically inside
   `if let Some(comp) = self.compositor` (`mod.rs:791-802`). It advances
   `camera.position/velocity`, `column.boost`, `ws.zoom`, `ws.page_zoom` — all in
   `State`. `mod.rs:1034` names the paths outright:
   `off_path=snap_animations on_path=analytic_substeps`. A display path drives WM state.
2. **The compositor writes WM `State`.** `compositor_gl.rs:2314` and `:2411`, inside
   functions holding `&mut state`, set/clear `state.monitors[i].layout_dirty`.
   This is precisely the `WM state ↕ compositor state` coupling the target architecture
   forbids. It is a cache flag, not geometry — so not a correctness bug, but forbidden
   as an ownership pattern.
3. **Six rectangles per window, four compositor-owned** — duplicate geometry ownership.
4. **`maverick-sys` is an OS boundary in name, app logic in substance** (§5).
5. **Six rectangles of geometry, and a fractional projection the WM never publishes** —
   the compositor draws a float that nothing else can see (§2).

## 10. The smallest coherent Maverick architecture

**The target architecture is achievable, but not by deletion. It requires one seam.**

```
              MAVERICK (bin)
                     │
       ┌─────────────┴─────────────┐
       │                           │
  authoritative WM state     optional visual
  (pure, feature-free)       presentation
       │                           │
       ▼                           ▼
    X11                      reads settled state
```

Required properties, each traceable to a finding above:

- **P1 — WM state is feature-free.** No `cfg(compositor-opengl)` may gate a module that
  defines or mutates `State`. Fixes §9.1, §9.2.
- **P2 — The WM publishes one settled rectangle per window per arrange.** Presentation
  reads it; it may never re-derive or re-project it. Fixes §2, §9.3, §9.5.
- **P3 — Presentation is a leaf.** It writes no `State`, calls no `ConfigureWindow`
  for a client, and is not the animation clock. Currently true for edges 1–3; **false
  for the clock** (§9.1).
- **P4 — `maverick-core`/`x11`/`toml`/`img` stay mandatory; `gl` optional; `render`
  gone; `vk` excluded; `sys` split.**

### The one decision that needs a human

**Animation is a real user-visible behaviour that only exists when the compositor is
present.** `CHANGELOG.md:330-334` records that the per-frame X11 path `arrange_live`
was *deleted*, making the compositor the sole animation host. Three options:

- **(a) Accept dwm-style.** Delete the compositor. Animation disappears permanently.
  Simplest, and satisfies every stated constraint — but it is a **product regression**
  and must be chosen deliberately, not by default.
- **(b) Restore `arrange_live`.** Per-frame X11 reconfiguration, no compositor. ~795 LOC
  of `framesched.rs` is already configuration-agnostic and reusable. Recovers animation
  at 60 fps only if the X server keeps up — the 1-px stepping would be *real* here, since
  X11 geometry genuinely is the display.
- **(c) Keep the compositor, isolate it.** Achieve P1–P3 behind the existing feature
  flag, deleting nothing. The WM is already correct without it, so (c) costs only
  architectural clarity, not correctness.

**I recommend (b) followed by (a):** make the animation clock WM-owned, which is what
the target architecture requires anyway, then decide the visual question separately.
Options (a) and (c) both leave the clock wrong.

I am not proceeding to implementation until this is decided. (a), (b) and (c) imply
materially different diffs.

---

## Before / After (projected, for option (a))

| | Before | After (a) | After (b) |
|---|---|---|---|
| workspace crates | 8 + root | 6 + root (`vk` excluded) | 6 + root |
| runtime deps, default | 10 | 9 | 9 |
| runtime deps, no-default | 9 | 8 | 8 |
| Rust LOC | 86,434 | ~70,900 | ~71,700 |
| tests (`#[test]`) | 1,094 | ~822 | ~822 |

---

## Out-of-scope defects (recorded, not fixed — per campaign rules)

1. **79 compositor tests sit in ungated modules.** Verified: `src/compositor_policy.rs` (24
   tests, `main.rs:82` `mod compositor_policy;`), `backend/x11/framesched.rs` (24, `mod.rs:99`),
   `backend/x11/trace.rs` (11, `mod.rs:111`) — all declared without a feature gate, so
   they compile and run in a `--no-default-features` build. Only `compositor_gl.rs` is gated.
2. **CI never tests `--no-default-features`.** Verified: `.github/workflows/ci.yml:26` is
   `cargo test --workspace` (default features, whole workspace — which is why
   `maverick-gl`'s 62 and `maverick-vk`'s 36 tests run); line 49 is
   `cargo build --release --no-default-features` — **build only, no test**. The 518
   no-compositor tests are never executed by automation.
3. **Largest coverage gap is the movement/coordinate path.** `manage.rs` (1,367),
   `events.rs` (1,097), `pointer.rs` (729), `actions.rs` (551), `input.rs` (446) —
   **4,911 lines with zero `#[cfg(test)]`**. `pointer.rs:668` is untested, and the only
   "projection is not accumulated" property (`compositor_gl.rs:4944`) **dies with the
   compositor**, leaving no surviving test asserting `camera.target → arrange` is exact
   over many steps. This should be added regardless of which option is chosen.
4. `no_wait_in_wm.rs:57-64` scans `maverick-gl/src` and `maverick-vk/src` as WM sources
   though neither is linked into the WM binary.
5. `maverick-img/tests/properties/mod.rs` is auto-discovered only via its non-canonical
   name; 38 of its tests would vanish silently if cargo tightened discovery.
6. `maverick-sys::json`'s *parser* is used only inside `src/core/ipc.rs`'s own
   `#[cfg(test)]` (`ipc.rs:499`). The WM never parses its own output in production; the
   real consumer is `maverickctl`. Any split must move it with the CLI.
7. `compositor_gl.rs:3886` `CompositeNameWindowPixmap` on client windows — noted by Agent B;
   warrants confirmation before removal.
8. Path error in the agent reports: `compositor_policy.rs` is `src/compositor_policy.rs`,
   not `src/backend/x11/compositor_policy.rs`. Corrected here.

## Corrections struck after verification

- `cargo metadata --offline` does **not** fail; `ash 0.38.0+1.3.281` is in `Cargo.lock:12`
  and fully vendored. The "excluding `maverick-vk` fixes offline" rationale is void;
  `maverick-vk` is still justified as *unreachable workspace weight*.
- `maverick-sys/Cargo.toml` declares **no** `[[bin]]`; `maverickctl` is cargo
  auto-discovery from `src/bin/`. Substance confirmed, mechanism corrected.
- "Removing `maverick-sys` removes rustix" — false; `x11rb` already pulls it.
