# MOVEMENT AUDIT — Agent D

**Scope:** the exact coordinate flow from input to pixels in `maverick`, and where
precision is lost. Read-only audit; no repo file was modified. The only artefact is
this document.

**Repo:** `/path/to/Maverick-reconstructed` @ `db622f3`

**Experiments:** three standalone `rustc` programs transcribed from the real code
(§11 appendix lists them and the exact source lines each was transcribed from).

---

## 1. Summary + the governing answer

### One-sentence answer

> **The compositor neither solves nor hides a movement bug: the movement problem is
> not a precision problem at all — it is that the compositor is the *only* thing
> that animates at all, and while it animates, X11 is parked at the destination and
> the screen is at the source, a full travel distance out of step.**

### The three findings that matter

1. **`Phase::Live` never reaches `ConfigureWindow`.** The only production caller of
   the projection is `arrange_full` → `arrange_full_phase(…, Phase::Settled)`
   (`src/backend/x11/render.rs:592-598`). `Phase::Live` is used exactly once in
   production, by the *compositor's* `live_placements`
   (`src/backend/x11/compositor_gl.rs:4179-4192`). With the compositor removed,
   `WindowManager::run_once` calls `State::snap_animations()` instead of ticking
   anything (`src/backend/x11/mod.rs:915-927`), so the camera spring is never
   integrated. **The X11-only build has no animation, by construction.**

2. **X11 is configured once, at the destination, at t = 0; the screen then glides
   from the source.** On a Mod4+wheel notch, `Effect::ArrangeMonitor` runs
   `Phase::Settled` immediately, writing `x = -1519`. The compositor then draws the
   same window from `x = -31.57` to `x = -1519.20` over 44 frames. Peak divergence
   measured: **1487.4 px** (§8). They agree only when the spring settles. That is
   precisely the reported "visually synchronized only after X11 catches up", with
   the direction reversed: X11 is *ahead*, the screen catches up.

3. **The compositor hides a ≤ 0.5 px divergence it introduces itself.** X11 geometry
   is `round()`ed at `src/core/layout.rs:649`; the compositor's drawn position comes
   from a *separate, unrounded* projection
   (`column_screen_extents_into`, `src/core/layout.rs:750`) fed in as
   `visual_x_cache` (`src/backend/x11/compositor_gl.rs:4139-4172`). The two
   disagree by up to 0.5 px forever — measured peak 0.4707 px (§8) — and nothing
   reconciles them, because the fractional value has no authority: X11 hit-testing,
   `client.geom`, `find_client`, the pointer warp and the client's own
   `ConfigureNotify` all read the integer.

### Would movement be correct with the compositor deleted?

**Yes — geometrically exact, and instantly.** The no-compositor path snaps the
camera to `target` and arranges at `Phase::Settled`, so the integer `ConfigureWindow`
rect *is* the final rect. The only thing lost is the animation, not the precision.
The exact code that would have to change to get animation back is listed in §10.

### What the 1-pixel symptom actually is

Not a camera bug, and not a scroll-accumulation bug. `Mod4+wheel` does not free-scroll
at all — it maps a button code straight to a focus-column step
(`src/backend/x11/pointer.rs:674-686`), so there is **no 120-unit wheel delta and
no per-event truncation to accumulate** (§5). The 1-pixel stepping is the *X11 record*
of a continuously moving camera being `round()`ed to a pixel grid
(`src/core/layout.rs:649`): over one notch, 16 % of frames advance the recorded x by
exactly 1 px and 14 % by 0 px (§8.2). The on-screen position is sub-pixel throughout
and is never 1-px-quantised by the WM. See §6 and §8 for the full disproof of the
scroll-accumulation theory.

---

## 2. Pipeline stage table

`input → camera → layout → desired → reconciler → X11 ConfigureWindow → compositor`

| # | Stage | Authoritative type | Symbols | file:line | Formula / note |
|---|-------|--------------------|---------|-----------|----------------|
| 0 | **X input** | `u8` button code, `i16` X/Y | `ButtonPressEvent.detail`, `e.root_x/root_y` | `src/backend/x11/pointer.rs:139-160` | `if e.detail >= 4 { … }`; no delta, no accumulation |
| 0b | **Wheel → intent** | `Dir` enum (no numbers) | `scroll_camera_with_wheel` | `src/backend/x11/pointer.rs:668-686` | `match detail { 7 \| 5 => Dir::Right, _ => Dir::Left }` — **discrete, lossless** |
| 0c | **Wheel → focus** | `f32` (hit-test only) | `focus_column_at` | `src/backend/x11/pointer.rs:691-728` | `px as f32 >= l && px as f32 <= r`, `l/r` from `column_screen_extents` (fractional) |
| 1 | **Camera** | `f32` published, **`f64` integrated** | `Camera{position,target,velocity,stiffness,damping}`, `Camera{x,v}` | `maverick-core/src/types.rs:431-464` | `position/target/velocity: f32`; private continuation `x,v: f64` |
| 1a | Camera target | `f32` | `ideal_scroll` | `src/core/layout.rs:762-798` | `want = x + w/2 - waw/2`, clamped to `[cx/α, total_w-(waw-cx)/α]` |
| 1b | Camera step | `f32` in, **`f64`** solve | `Camera::step` | `maverick-core/src/types.rs:556-639` | closed-form damped oscillator in `f64`; publishes `self.position = self.x as f32` (line 626) |
| 1c | Camera substep | `f32` | `substep_bounds` | `compositor_gl.rs:4196-4205`, stub `compositor.rs:204-213` | `n = ceil(dt/0.008)`, `step = dt/n` — **lossless partition**, `sum(step) == dt` |
| 2 | **Ribbon world geometry** | **`f32`** | `RibbonGeom{alpha,cx,cy,gap,total_w}`, `cols: Vec<(f32,f32)>` | `src/core/layout.rs:397-498` | `cx = wa.w*(1-α)/2`; `w = (weight + boost_total*boost).min(1.0) * usable_w`; `x += w + gap_f` (**f32 running sum**, line 485) |
| 3 | **World → screen** | `f32` → **`i32` (quantised)** | `arrange_columns` | `src/core/layout.rs:649` | **`screen_col_x = (wa.x + (world_x - cam)·α + cx).round() as i32`** |
| 3b | Row y | `f32` → `i32` | `arrange_columns` | `src/core/layout.rs:664` | `screen_y = (wa.y + (row_y_world - wa.y)·α + cy).round() as i32` |
| 3c | Width | `f32` → **`u32` (truncated)** | `arrange_columns` | `src/core/layout.rs:614` | `inner_w = ((col_w·α) - 2·bw).max(1.0) as u32` — **truncation, not round** |
| 3d | Height | `f32` → `u32` (truncated) | `arrange_columns` | `src/core/layout.rs:663,670` | `screen_h = (row_h·α).max(1.0) as u32`, then `(screen_h - 2·bw).max(1) as u32` |
| 3e | Phase select | enum | `Phase::Live` / `Phase::Settled` | `src/core/layout.rs:46-59`, used at `layout.rs:554-558` | `cam = if live { camera.position } else { camera.target }` |
| 4 | **Presentation overlay** | **`i32` only — zero floats** | `present_into`, `maximized_rect` | `src/core/present.rs:43-120` | integer rect substitution (`mon.screen` / `maximized_rect`); **no coordinate transform, no rounding** |
| 5 | **Desired** | `Rect` (`i32`/`u32`) | `DesiredState::from_placements` | `src/core/desired.rs:25-69` | pure copy of `Placements` |
| 6 | **Reconciler** | `Rect` | `wire_geometry`, `AppliedState::diff`, `reconcile` | `src/backend/x11/reconciler.rs:110-156, 199-246` | `w/h.clamp(1, u16::MAX)`, `bw.min(u16::MAX)`; **x/y passed through unchanged** |
| 7 | **X11 ConfigureWindow** | `i32` → wire | `emit_geometry` | `src/backend/x11/render.rs:972-1068` | `configure_window(win, .x(wire.x).y(wire.y).width(..).height(..).border_width(..))` |
| 7b | Synthetic `ConfigureNotify` | **`i16`/`u16`** | `ConfigureNotifyEvent{…}` | `src/backend/x11/render.rs:1011-1023` | `x: geom.x.clamp(i16::MIN, i16::MAX) as i16` — **a second, 16-bit quantisation** |
| 7c | `client.geom` mirror | `i32` | `c.geom = wire` | `src/backend/x11/render.rs:1049-1056` | authoritative for hit-testing and pointer warp |
| 8 | **Compositor live projection** | `Rect` (i32) | `live_placements` | `compositor_gl.rs:4179-4192` | `arrange(…, Phase::Live, …)` → **same `.round()` as X11** |
| 9 | **Compositor fractional x** | **`f32` — presentation only** | `refresh_visual_x_cache`, `visual_x_cache` | `compositor_gl.rs:4139-4172` | `l = wa.x + (x - camera.position)·α + cx` — **identical formula, no `.round()`** |
| 10 | **Compositor transform** | `Rect` (int) + `VisualRect` (f32) + `[f64;5]` | `set_transform_with_visual`, `CompWin::visual_transform` | `compositor_gl.rs:498-601, 135-140` | `live_x = visual_x.unwrap_or(transform.x as f32) as f64`; `visual_transform = presentation_value as f32` |
| 11 | **GPU draw** | `f32` (float vertex attribute) | `visual_draw_dst` → `u_dst` | `compositor_gl.rs:142-144, 3194-3207`; `maverick-gl/src/renderer.rs:66-81, 1281` | `dst = [x, y, x+w, y+h]`, all `f32`, no quantisation in the shader |

**Single authoritative rectangle type at each point:** `maverick_core::Rect {x,y: i32, w,h: u32}`
(`maverick-core/src/types.rs:72-82`) is the authority from stage 5 through stage 8.
There is exactly **one** float rectangle in the whole system:
`compositor_gl::VisualRect {x,y,w,h: f32}` (`compositor_gl.rs:134-140`), which exists
only from stage 9 onward and is explicitly documented as presentation-only.

**Coordinate transforms in the pipeline** (all of them):

| transform | formula | file:line |
|---|---|---|
| zoom-around-centre | `x' = wa.x + (x - cam)·α + wa.w·(1-α)/2` | `layout.rs:649` (X11), `layout.rs:750` (visual) |
| camera translation | `x' = x - cam` where `cam` is the spring position/target | `layout.rs:554-558, 649, 750` |
| outer-frame inflation | `outer = (x, y, w + 2·bw, h + 2·bw)` | `compositor_gl.rs:511-516` |
| enclosing (damage/cull) | `x0 = floor(x)`, `x1 = ceil(x + w)` | `compositor_gl.rs:164-182` |
| damage map | `floor(local.x·visual.w/source.w)`, `ceil(...)` | `compositor_gl.rs:4088-4093` |
| NDC | `clip = (p.x/res.x·2-1, 1 - p.y/res.y·2)` | `maverick-gl/src/renderer.rs:73-74` |

---

## 3. Precision-loss inventory

Error direction is stated for the *value*; "max error" is the worst case the
expression can introduce, in pixels, for realistic geometry.

### 3.1 Position quantisation (the dominant loss)

| # | file:line | expression | direction | max error |
|---|---|---|---|---|
| P1 | `src/core/layout.rs:649` | `(wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32` | ±, unbiased | **0.5 px** |
| P2 | `src/core/layout.rs:580` | same, for a ribbon fullscreen column | ± | 0.5 px |
| P3 | `src/core/layout.rs:664` | `(wa.y as f32 + (row_y_world - wa.y as f32) * alpha + cy).round() as i32` | ± | 0.5 px |
| P4 | `src/core/layout.rs:584` | `(screen.y as f32 + screen.h as f32 * (1.0 - alpha) / 2.0).round() as i32` | ± | 0.5 px |

These four are the *only* places a screen coordinate leaves the float world. They
run once, at the X11 integer boundary, and the layout's own comment
(`layout.rs:645-648`) states the intent: round here and nowhere earlier.

### 3.2 Size quantisation

| # | file:line | expression | direction | max error |
|---|---|---|---|---|
| S1 | `src/core/layout.rs:614` | `((col_w_world * alpha) - 2.0 * bw as f32).max(1.0) as u32` | **truncates toward zero → always ≤ 0** | **< 1 px** (measured: 1140.4 → 1140, −0.4 px) |
| S2 | `src/core/layout.rs:663` | `(row_h_world * alpha).max(1.0) as u32` | truncates | < 1 px |
| S3 | `src/core/layout.rs:670` | `(screen_h as i32 - 2 * bw as i32).max(1) as u32` | inherits S2 | < 1 px + double-truncation |
| S4 | `src/core/layout.rs:585-586` | `(screen.w as f32 * alpha).max(1.0) as u32` | truncates | < 1 px |
| S5 | `src/backend/x11/reconciler.rs:115-118` | `g.w.clamp(1, u16::MAX)`, `g.h.clamp(1, u16::MAX)`, `bw.min(u16::MAX)` | clamps | 0 in practice; the `u16` bound is **over-conservative** — the `ConfigureWindow` *request* carries 32-bit w/h (see §12) |
| S6 | `src/backend/x11/render.rs:1017-1021` | `geom.x.clamp(i16::MIN,i16::MAX) as i16`, `geom.w.clamp(0,u16::MAX) as u16` | clamps, on the **synthetic ConfigureNotify** | x: up to 65535 px beyond ±32767; w/h: 0 for > 65535 |

**S1 is a real (small) bug-shaped site:** `.round()` is used for position and plain
`as u32` for size in the *same function, three lines apart* (`layout.rs:649` vs
`layout.rs:614`). Every tiled window is systematically up to 1 px narrower than the
fractional geometry says, and the error is one-signed (always shrinking).

### 3.3 Accumulated error (repeated adds in a lower-precision type)

| # | file:line | expression | direction | max error |
|---|---|---|---|---|
| A1 | `src/core/layout.rs:485` | `x += w + gap_f;` — f32 running sum over all columns | biased, sign follows the partial sums | **measured: 0.0003 px @ 5 cols, 0.028 px @ 50, 0.106 px @ 100, 0.409 px @ 200, 0.925 px @ 500** (§8.4) |
| A2 | `maverick-core/src/types.rs:485` | `x += w + gap_f` is the *only* such accumulation; the camera's own state is the one accumulator that was **fixed** — see §5.2 | — | 0 |
| A3 | `src/core/layout.rs:633-635` | `base_h = total_h / n as f32`, `extra_last = total_h - base_h * n as f32` | bounded by construction (last row absorbs) | < 1/n px per row |

A1 is the one genuinely *accumulating* coordinate error in the system. It is worth
noting that it is **not** the camera: the camera carries an f64 continuation
(`types.rs:462-463`) precisely so that this class of error cannot happen there.

### 3.4 Repeated / round-trip conversion

| # | file:line | what happens |
|---|---|---|
| R1 | `layout.rs:649` → `compositor_gl.rs:511` → `compositor_gl.rs:522-524` → `compositor_gl.rs:595` | `f32 → i32 (round) → f32 → f64 → f32`. The `as f32 → as f64` at 524 is a widening round-trip that is *lossless*; the loss is the `round()` at 649, and it is only avoided on the x axis, and only because `visual_x_cache` supplies a *separately computed* unrounded value. |
| R2 | `compositor_gl.rs:560-571` | `presentation_value: [f64;5]` → `.round() as i32` into `transform` **and** the same `[f64;5]` → `as f32` into `visual_transform`. One animation, two different quantisations of the same number, in the same function. |
| R3 | `compositor_gl.rs:4088-4093` | `local` (i32) → `f64` scale → `floor`/`ceil` → `i32` → added to `drawn.x` (already an `enclosing()` of a float). Two quantisations stacked. |
| R4 | `pointer.rs:711-716` | `i32` pointer → `as f32` → compared against **fractional** extents from `layout.rs:750`, while `find_client` (`manage.rs:1224-1243`) lets the **X server** hit-test against the **integer** `ConfigureWindow` geometry. The two hit-tests disagree by up to 0.5 px — and by the full travel distance during a scroll (§8.3). |

### 3.5 `+ 0.5` / `floor` / `ceil` / `trunc` idioms

`grep` for `.floor()|.ceil()|.trunc()|+ 0.5|- 0.5` over `src/core`,
`compositor_gl.rs`, `render.rs` returns **no** `+0.5` rounding idiom anywhere. The
`floor`/`ceil` uses are all in the compositor's *damage* path
(`compositor_gl.rs:165-168, 4088-4093`) where outward rounding is correct, plus
`substep_bounds` (`:4201`) where it is a count, not a coordinate. `rem_euclid` is
used in `maverick-core/src/types.rs` only for wrapping focus indices, not coordinates.

### 3.6 Integer rectangle types and their producers

| type | file:line | produced by |
|---|---|---|
| `maverick_core::Rect {x,y: i32, w,h: u32}` | `maverick-core/src/types.rs:72-82` | `arrange_columns`, `present_into`, `normalize_float_geom`, `parked_rect` |
| `compositor_gl::VisualRect {f32×4}` | `compositor_gl.rs:134-140` | `set_transform_with_visual` only |
| `compositor_gl::CompWin::transform: Rect` | `compositor_gl.rs:342` | `set_transform_with_visual:511-516` |
| `x11rb ConfigureWindowAux{x,y: i32, w,h,bw: u32}` | `x11rb-protocol-0.13.2/src/protocol/xproto.rs:8978-8985` | `emit_geometry` |
| `x11rb ConfigureNotifyEvent{x,y: i16, w,h,bw: u16}` | `x11rb-protocol-0.13.2/src/protocol/xproto.rs` | `emit_geometry` (synthetic), real server |
| `maverick-render::DrawQuad.dst: [f32;4]` | `maverick-render/src/lib.rs:79`, `maverick-gl/src/renderer.rs:288-290` | `visual_draw_dst` |

---

## 4. f32 vs f64 map of every coordinate-bearing type

| type | fields | f32 | f64 | i32/u32 | file:line |
|---|---|---|---|---|---|
| `Rect` | `x, y, w, h` | — | — | **i32 / u32** | `maverick-core/src/types.rs:72-82` |
| `Monitor.screen`, `.workarea` | `Rect` | — | — | i32/u32 | `types.rs:1300+` |
| `Client.geom`, `.saved_geom` | `Rect` | — | — | i32/u32 | `types.rs` (`Client` ~1020-1140) |
| `DesiredWindow.rect` | `Rect` | — | — | i32/u32 | `src/core/desired.rs:25-32` |
| `AppliedWindow.rect` | `Rect` | — | — | i32/u32 | `reconciler.rs:77-88` |
| `Effect::ConfigureWindow.geom` | `Rect` | — | — | i32/u32 | `src/core/effect.rs:41-45` |
| `Camera.position/target/velocity` | camera | **f32 (published)** | — | — | `types.rs:433-437` |
| `Camera.x, .v` | camera | — | **f64 (integrated)** | — | `types.rs:462-463` |
| `Camera.stiffness/.damping` | spring | f32 (sanitised) | — | — | `types.rs:439-442` |
| `RibbonGeom.alpha/cx/cy/gap/total_w` | layout | f32 | — | — | `layout.rs:338-355` |
| `RibbonGeom.cols: [(f32,f32)]` | layout (world x, world w) | f32 | — | — | `layout.rs:352` |
| `Column.weight` | column | f32 | — | — | `types.rs:343` |
| `Column.boost` | column (animated) | f32 | — | — | `types.rs:351` |
| `Workspace.zoom/.zoom_target/.page_zoom/.page_zoom_target` | workspace | f32 | — | — | `types.rs:716-730` |
| `SizeHints.min_aspect/.max_aspect` | hints | f32 | — | — | `types.rs:268-270` |
| `Placements` | `Vec<(WindowId, Rect, u32)>` | — | — | i32/u32 | `layout.rs:31` |
| `VisualRect` | compositor, presentation-only | **f32** | — | — | `compositor_gl.rs:134-140` |
| `CompWin.outer/.transform` | compositor | — | — | i32/u32 | `compositor_gl.rs:297, 342` |
| `CompWin.presentation_value/.presentation_target` | compositor | — | **f64[5]** | — | `compositor_gl.rs:351-352` |
| `PresentationTransition.from` | compositor | — | **f64[5]** | — | `compositor_gl.rs:375` |
| `PresentationTransition.progress` | compositor | **f32** (a reused `Camera`) | (carries f64 internally) | — | `compositor_gl.rs:376` |
| `visual_x_cache` | compositor | f32 | — | — | `compositor_gl.rs:1133` |
| `visual_extents` | compositor | `Vec<(f32,f32)>` | — | — | `compositor_gl.rs:1134` |
| `Compositor.opacity` | compositor | f32 | — | — | `compositor_gl.rs:301` |
| `DrawQuad.dst/.size` | GL | f32 | — | — | `maverick-gl/src/renderer.rs:288-290` |
| `frame_dt` | loop | f32 | — | — | `compositor_gl.rs:1127` |

**There is no coordinate anywhere in the WM core that is `f64` except the camera's
private continuation and the compositor's `[f64;5]` presentation value. The layout
world, the camera's published state, the compositor's visual rect and the GL
destination are all `f32`.**

---

## 5. Camera and scroll

### 5.1 What a wheel event actually does

```rust
// src/backend/x11/pointer.rs:668-686
fn scroll_camera_with_wheel(&mut self, detail: u8, px: i32, py: i32) -> ... {
    let dir = match detail {
        7 | 5 => Dir::Right, // wheel right / down → next column
        _ => Dir::Left,      // wheel left / up → previous column (and any other)
    };
    let effects = self.engine.dispatch(crate::types::Action::FocusDir(dir));
    self.run_effects(effects)?;
    self.focus_column_at(px, py);
    Ok(())
}
```

There is **no wheel delta anywhere in the codebase**. `detail` is a `u8` button code
(4/5/6/7), never a signed axis value; `ButtonReleaseEvent` has no delta field; there
is no `XI2`/`XI_Scroll`/`XI_SmoothScroll` subscription, no
`raw: f32` accumulation buffer, and no `rem_euclid`. Verified by exhaustive grep of
`src/` and `maverick-core/src/` for `BUTTON_4|BUTTON_5|Wheel|scroll_delta|ScrollAmount|Precise|ButtonIndex::[45]`
— the only hits are the `detail >= 4` branch above and prose.

**Therefore the classic "120-unit wheel delta truncated per event" bug cannot occur
in this codebase.** A notch is not a distance at all; it is a focus-column step.
The camera then retargets to a whole-column distance:

```
ideal_scroll (layout.rs:762) for focus 0 -> 0.0000
ideal_scroll                  for focus 1 -> 1527.2000     (one notch = 1527.2 px)
```

The question "does a 120-unit delta move a whole number of pixels?" has no answer
here, because the question does not apply. The camera *target* is a real f32 that
is usually fractional (1527.2), and the fractional part is preserved all the way to
`Camera::target` and from there into `position` via `snap` (`types.rs:642-649`),
which assigns `position = target` with no rounding.

### 5.2 Quantisation of the camera

* **Type:** `position`/`target`/`velocity` are `f32` (`types.rs:433-437`). The
  *integrator state* is `f64` (`types.rs:462-463`).
* **Where quantised:** nowhere during the trajectory. `Camera::step` solves the
  damped oscillator in `f64` and publishes `self.position = self.x as f32`
  (`types.rs:626-627`), and `analytic_state` (`types.rs:667-672`) *rejects* the
  published f32 as the next step's initial condition, so the f32 rounding is a pure
  publication step. The design note at `types.rs:443-461` is explicit that feeding
  the rounded value back would inject a speed floor of `k/c · ½ · ulp(position)` —
  which at 12 000 px is ~0.013 px/s, an order of magnitude over
  `CAMERA_SETTLE_VELOCITY = 0.01` (`types.rs:483-484`), making the camera unable to
  ever satisfy the settle predicate.
* **Settle:** `|position - target| <= 0.5` **or** `|velocity| <= 0.01` triggers
  `snap(target)` (`types.rs:573-576, 633-638`), installing the exact endpoint. The
  `0.5` threshold is a *deliberate* half-pixel, matching the `.round()` at
  `layout.rs:649` — i.e. the camera is guaranteed to stop before the X11 record
  could differ by more than the rounding bound.
* **External writes:** `grep` for `camera.position =` / `camera.snap(` across
  `src/` and `maverick-core/src/` finds **only** test and invariant-fixture writers
  (`src/core/invariants.rs:115-116, 702-703, 1214`, `src/core/framebench.rs:146`,
  `src/core/tests.rs` many). No production code overwrites the published fields, so
  the f64 continuation can never be desynchronised in a release build.

**Verdict: the camera is the best-behaved coordinate in the system.** It is the one
accumulator that was already hardened against exactly this bug class.

### 5.3 Substepping

```rust
// src/backend/x11/compositor_gl.rs:4196-4205
pub fn substep_bounds(dt: f32) -> impl Iterator<Item = f32> {
    let (n, step) = if !dt.is_finite() || dt <= 0.0 { (0, 0.0) }
        else { let max = SUBSTEP_MS / 1000.0;              // 8.0 ms
               let n = (dt / max).ceil().max(1.0) as usize;
               (n, dt / n as f32) };
    (0..n).map(move |_| step)
}
```

`n` steps of `dt/n` — a **lossless partition**, `sum(step) == dt` to within one f32
ULP. It is not a truncation of a remainder. The stub implementation
(`src/backend/x11/compositor.rs:204-213`) is numerically identical, and the stub's
own test at `compositor.rs:265` asserts `|sum - dt| < 1e-6`.

---

## 6. Presentation / animation analysis

### 6.1 `src/core/present.rs` contains **no animation and no floats**

```
$ grep -c "as f32\|as f64\|f32\|f64" src/core/present.rs
0   (only in `#[cfg(test)]` and `mod proptests`)
```

`present_into` (`src/core/present.rs:43-90`) is a pure integer rect *substitution*:
`fullscreen > maximized`, substituting `mon.screen` or `maximized_rect(tile,
workarea, client)` (`present.rs:108-120`) and forcing `border = 0`. There is no
interpolation, no clock, no time, no rounding. The mission brief's hypothesis that
`present.rs` holds the f32 sub-pixel animation is **incorrect** for this codebase.

### 6.2 There are exactly two animations, with different owners

| animation | state lives in | advanced by | clock | clock owner |
|---|---|---|---|---|
| **scroll camera**, per-column `boost`, workspace `zoom`/`page_zoom` | `maverick_core::State` (`Workspace::camera`, `Column::boost`, `Workspace::zoom`) — **authoritative WM state** | `State::tick_animations_multi` (`types.rs:2582-2630`) called from `mod.rs:792-797` | `dt` from `Instant::now()` (`mod.rs:714-725`), clamped by `framesched::clamp_frame_dt` | **value: WM. rate: compositor** (see §6.4) |
| **presentation transition** (fullscreen/maximize open/close), per window | `CompWin::presentation: Option<PresentationTransition>` (`compositor_gl.rs:349, 374-377`) — **presentation-only, GPU-owned** | `CompWin::tick_presentation` (`compositor_gl.rs:603-622`) called from `prepare_frame` (`compositor_gl.rs:2300-2302`) | the same `dt` | same |

The camera is authoritative WM state: `Workspace::camera` lives in `maverick-core`
and is documented as such (`types.rs:405-414`: *"The camera is never the source of
truth for logical geometry: arrangement derives each window's x from `target` for
settled geometry and from `position` for live rendering, so animation can never
mutate the layout."*). The presentation transition is the opposite: it is
`CompWin`-private, has no `State` representation, and `prepare_frame`'s comment at
`compositor_gl.rs:2279-2281` says so — *"GPU presentation state is owned by the
compositor, so the WM core only ever hands over placements."*

**Ownership of the current animation value:**
* camera: `maverick_core::Camera::x` (`f64`), with `position` (`f32`) as its published
  rounding. Owned by `State`.
* presentation: `CompWin::presentation_value: [f64;5]` (`compositor_gl.rs:351`).
  Owned by the compositor. **Nobody in the WM core can read it.**

### 6.3 The presentation transition's arithmetic

```rust
// src/backend/x11/compositor_gl.rs:560-572
let progress = f64::from(transition.progress.position.clamp(0.0, 1.0));
for (i, value) in self.presentation_value.iter_mut().enumerate() {
    let to = if i == 4 { goal[i] } else { live[i] };
    *value = transition.from[i] + (to - transition.from[i]) * progress;
}
self.transform = Rect::new(
    self.presentation_value[0].round() as i32,   //  <-- integer
    self.presentation_value[1].round() as i32,
    self.presentation_value[2].round() as u32,
    self.presentation_value[3].round() as u32,
);
```

The `[f64;5]` interpolation is carried in f64 and then **rounded to an integer
`Rect`** for `transform` (X11-facing caches, damage, culling, scissor — see the
field comment at `compositor_gl.rs:342-346`) while the *same* `[f64;5]` is cast to
`f32` for `visual_transform` (GPU draw). This is the R2 double-quantisation: a
single animation value, two different grids, in one function.

Note the `progress` spring reuses `maverick_core::Camera` as a 0→1 scalar
(`compositor_gl.rs:544-551`, `544`: `Camera::new(0.0); progress.target = 1.0;`),
so it inherits the f64-continuation hardening for free.

### 6.4 Is the animation clock compositor-owned?

**Partly — and this is the finding the brief was looking for, with a correction.**

* The clock *value* is **WM-owned** and is a plain monotonic wall clock, not vblank
  and not the X `Present` extension (which is not used anywhere in the repo — grep
  for `xcb_present|PresentExtension` returns nothing):

  ```rust
  // src/backend/x11/mod.rs:714-725
  let now = Instant::now();
  let raw_dt = (now - self.last_frame).as_secs_f32();
  let dt = crate::backend::x11::framesched::clamp_frame_dt(raw_dt, was_animating);
  self.last_frame = now;
  ```

* The clock *rate* is **compositor-owned**. With VSync on, the loop deliberately
  adds **no** software wait and blocks inside `glXSwapBuffers`:

  ```rust
  // src/backend/x11/framesched.rs:100-102
  pub(crate) const fn should_wait_after_swap(vsync_on: bool) -> bool { !vsync_on }
  // src/backend/x11/mod.rs:942-951
  let vsync_on = self.compositor.as_ref().is_some_and(Compositor::vsync_active);
  self.animation_due = if sched.is_continuous() && framesched::should_wait_after_swap(vsync_on) {
      Some(Instant::now() + self.frame_period) } else { None };
  ```

  and `timeout_ms()` is `0` whenever a frame is pending
  (`framesched.rs:248-250`), so the loop re-enters `run_once` immediately, ticks the
  springs, renders, and blocks in the swap. `Compositor::vsync_active` is
  `self.renderer.vsync` (`compositor_gl.rs:1148-1150`) — a GL context property.
  The comment at `mod.rs:906-913` states the dependency explicitly:
  *"The frame clock is deliberately *not* re-seeded here… with swap interval 1 the
  present is almost the entire frame, leaving the springs advanced by only the loop
  overhead of each 16.7 ms frame."*

* The integrator itself is **structurally inside the compositor branch**:

  ```rust
  // src/backend/x11/mod.rs:739, 791-802
  if let Some(comp) = self.compositor.as_mut() {
      ...
      if anim_enabled {
          for sub in compositor::substep_bounds(dt) {
              anim |= self.engine.state.tick_animations_multi(sub, &mut self.anim_per_mon);
          }
      }
  ```

  `tick_animations_multi` is called from **exactly one place in the tree**, and it
  is inside `if let Some(comp)`. With no compositor, the `else` branch is
  `snap_animations()` (`mod.rs:915-927`).

**So: the WM core has no animation clock of its own. Its spring integrator is
reachable only through the compositor, and its tick rate is set by the GLX swap.**
That is a real compositor dependency in the WM core, and it is the *cause* of the
movement problem, not a side effect of it.

---

## 7. Compositor presentation transforms

### 7.1 The transform math

Two rectangles exist per window, from the same projection:

```rust
// compositor_gl.rs:511-531  (integer: X11-facing, damage, cull, scissor)
self.transform = Rect::new(geom.x, geom.y,
                           geom.w.saturating_add(bw.saturating_mul(2)),
                           geom.h.saturating_add(bw.saturating_mul(2)));
self.transform_radius = rounded_radius_for(self.transform, radius, screen_union, screens);
let live_x = visual_x.filter(|x| x.is_finite())
              .unwrap_or(self.transform.x as f32) as f64;     // <-- the escape hatch
let live = [live_x, self.transform.y as f64,
            self.transform.w as f64, self.transform.h as f64,
            self.transform_radius as f64];
```

```rust
// compositor_gl.rs:594-599  (fractional: GPU only)
self.visual_transform = VisualRect {
    x: self.presentation_value[0] as f32,   // <- visual_x_cache, UNROUNDED
    y: self.presentation_value[1] as f32,
    w: self.presentation_value[2] as f32,
    h: self.presentation_value[3] as f32,
};
```

**The compositor applies no scale, no alpha and no translation that X11 does not.**
The only transform that differs from X11 geometry is a **pure x translation of at
most ±0.5 px**, sourced from a second, independent evaluation of the same formula:

```rust
// src/core/layout.rs:750  (X11 path, ROUNDED)
let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
// src/core/layout.rs:750  (visual path, NOT rounded)  [column_screen_extents_into]
let l = g.wa.x as f32 + (x - ws.camera.position) * g.alpha + g.cx;
```

Both read the same `ribbon_geom` table and the same `camera.position`; only
`.round()` differs. `refresh_visual_x_cache` (`compositor_gl.rs:4139-4172`) fills
`visual_x_cache[win] = left` for every **tiled** window on a **Column** layout,
skipping maximized, fullscreen-overlay and presented-maximize windows
(`compositor_gl.rs:4163-4168`) — those fall back to `transform.x` and are therefore
integer.

### 7.2 Is there a sub-pixel offset X11 cannot represent, and does the compositor render it?

**Yes, and the code proves it renders it.** The repo's own unit test pins it:

```rust
// src/backend/x11/compositor_gl.rs:5174-5190
fn visual_transform_preserves_fraction_until_draw() {
    cw.set_transform_with_visual(TransformInput {
        geom: Rect::new(500, 8, 313, 584), bw: 0, radius: 18,
        visual_x: Some(499.75),            //  <-- sub-pixel
    }, Rect::new(0, 0, 800, 600), &[], cw.transform_gen + 1);
    assert!((cw.visual_transform.x - 499.75).abs() < 1e-5);   // fraction kept
    assert_eq!(cw.transform.x, 500);                          // integer kept
}
```

and the fraction survives to the vertex shader unmodified:

```rust
// compositor_gl.rs:142-144
fn visual_draw_dst(visual: VisualRect) -> [f32; 4] {
    [visual.x, visual.y, visual.x + visual.w, visual.y + visual.h]
}
```
```glsl
// maverick-gl/src/renderer.rs:67, 73-74
uniform vec4 u_dst;   // destination rect in pixels: x0,y0,x1,y1 (origin top-left)
vec2 p = mix(u_dst.xy, u_dst.zw, a_pos);
vec2 clip = vec2(p.x / u_res.x * 2.0 - 1.0, 1.0 - p.y / u_res.y * 2.0);
gl_Position = vec4(clip, 0.0, 1.0);
```

`u_dst` is a `vec4` of GL `float`, interpolated into a float `gl_Position`. There is
no `floor`/`round` anywhere between `VisualRect` and rasterisation. **The GL
fragment grid will of course sample at pixel centres, so a hard window edge still
advances one pixel column at a time — but the *phase* is sub-pixel correct, which is
exactly what X11's `round()` destroys.**

**With the numbers from §8: X11 says `x = -1519`, the compositor draws at
`x = -1519.199951`. That 0.2 px is real, is on screen, and is invisible to every
other consumer of the WM's geometry.**

### 7.3 A side effect of the fraction: a permanent filter flip

```rust
// src/backend/x11/compositor_gl.rs:3188-3193
let smooth = tex.width as u32 != outer.w || tex.height as u32 != outer.h;
let filter = if smooth { Filter::Linear } else { Filter::Nearest };
```

`outer = visual.enclosing()` (`compositor_gl.rs:3105`, definition at `:164-182`), and
`tex.width == cw.outer.w` (`compositor_gl.rs:3857-3864`). `enclosing()` uses
`floor(x)` / `ceil(x + w)`, so **any non-zero fractional part inflates the width by
one**. Measured (§8.5): `visual.x = -1519.25 → enclosing.w = 1143 ≠ tex.width 1142`
→ `Filter::Linear`; `visual.x = -1519.0 → enclosing.w = 1142` → `Filter::Nearest`.

Consequence: **during every scroll, every tiled window is drawn with bilinear
filtering (blurred text, resampled client content), and snaps back to nearest at the
exact endpoint.** 44/44 frames measured. The sub-pixel rendering the compositor
provides is therefore delivered *by accident*, as a side effect of a damage-rect
inflation, not as a designed capability. Reported, not fixed (§12).

### 7.4 Bypass

`compositor_policy::mode_for` (`src/compositor_policy.rs:81-104`) can return
`CompositionMode::Bypass` for a fullscreen output, and
`Compositor::engage_bypass` (`compositor_gl.rs:2696-2719`) un-redirects that one
window. **A bypassed window's on-screen position is its X11 integer geometry**, with
no sub-pixel offset at all. So the sub-pixel rendering is not even uniform across
the desktop.

---

## 8. Worked numerical trace of the 1-pixel symptom

Setup, all values from the shipped defaults: 1920×1080 monitor, no struts,
`gaps_inner = 4`, `gaps_outer = 8`, `border_w = 1`, `column_width = 0.6`,
`accordion_boost = 0.0`, camera `stiffness = 220.0`, `damping = 30.0`
(`src/config.rs:111-120, 237-238`).

`workarea = (0,0,1920,1080)` → ribbon `wa = (8,8,1904,1064)` (`layout.rs:417-422`).
`α = 1`, `cx = 0`, `cy = 0`. Columns (first is weight 1.0 per
`Workspace::add_tiled`, `types.rs:789`; the rest 0.6):

```
col0 world_x=0.0000    world_w=1904.0000
col1 world_x=1908.0000 world_w=1142.4000
col2 world_x=3054.3999 world_w=1142.4000
col3 world_x=4200.7998 world_w=1142.4000
col4 world_x=5347.1997 world_w=1142.4000
total_w = 6489.5996
```

One Mod4+wheel notch: `ideal_scroll` focus 0 → **0.0000**, focus 1 → **1527.2000**.
A notch is a **1527.2 px** camera travel.

### 8.1 The camera trajectory (60 Hz, `dt = 1/60`)

```
 frm    cam.position   cam.target   drawn x (visual)   X11 x (steady)   |gap|    filter
   0        39.574417     1527.2000       -31.574417           -1519    1487.4255  Linear
   1       134.756378     1527.2000      -126.756378           -1519    1392.2437  Linear
   2       259.111847     1527.2000      -251.111847           -1519    1267.8882  Linear
   3       395.211823     1527.2000      -387.211823           -1519    1131.7882  Linear
   4       531.923218     1527.2000      -523.923218           -1519     995.0768  Linear
   5       662.464172     1527.2000      -654.464172           -1519     864.5358  Linear
   6       783.016541     1527.2000      -775.016541           -1519     743.9835  Linear
   7       891.742432     1527.2000      -883.742432           -1519     635.2576  Linear
  10      1145.182373     1527.2000     -1137.182373           -1519     381.8176  Linear
  20      1470.101807     1527.2000     -1462.101807           -1519      56.8982  Linear
  30      1519.741577     1527.2000     -1511.741577           -1519       7.2584  Linear
  40      1526.274170     1527.2000     -1518.274170           -1519       0.7258  Linear
  ..  peak |X11 x - drawn x| = 1487.4255 px at frame 0 (of 44)
```

`damping² = 900 > 4k = 880`, so this is the **overdamped** branch
(`types.rs:590-599`), which is why the profile is a slow S-curve with no overshoot.
The camera converges in 44 frames ≈ **733 ms**.

**Three things this table proves:**

1. **The on-screen position is never 1-px-quantised by the WM.** It takes 75 distinct
   fractional values across 75 frames. `d(visual_x)` per frame runs
   `−136.71 … −0.0002` px, monotonically, with **0 frames of exactly 0** and
   **0 frames of an exactly-integer delta ≥ 1**.
2. **X11 does not move at all.** It was written once, with `x = -1519`, at the moment
   the focus changed (`Effect::ArrangeMonitor` → `arrange` → `Phase::Settled`,
   `actions.rs:140` → `render.rs:597`). `run_once`'s compositor branch
   (`mod.rs:739-914`) issues no `arrange` at all, and `AppliedState::diff`
   (`reconciler.rs:146-147`) suppresses a repeat because the desired rect is
   unchanged. X11 is therefore **1487 px away from the screen for three quarters of
   a second**.
3. They agree only when `Camera::snap` installs the exact endpoint
   (`types.rs:636`), at which point X11 = `-1519` and the compositor would draw
   `-1519.199951` — still **0.2 px apart**, permanently.

### 8.2 The 1-pixel increments, located

If a consumer watches the **X11 record** rather than the screen, the per-frame delta
of the `ConfigureWindow` x during the same animation is:

```
frames = 44
  |dx| =   0 px :  6 frames      <- the record is frozen
  |dx| =   1 px :  7 frames   ┐
  |dx| =   2 px :  2 frames   │
  |dx| =   3 px :  2 frames   │
  |dx| =   4 px :  2 frames   ├  the 1-px-step regime
  |dx| =   6..16 px : 6 frames│
  |dx| =  18..45 px : 4 frames│
  |dx| =  53..136 px : 10 frames
  exactly 1 px: 7/44 = 16 % of frames;  exactly 0 px: 6/44 = 14 %
```

**That is the symptom.** "Movement appears to occur in approximately one-pixel
increments" is exactly what `round()` of a smoothly-varying camera looks like when
sampled per frame: the record is a staircase whose riser is 1 px, separated by
frames where it does not move at all. It is a **presentation/observability**
artefact of the integer X11 record, not a defect in the movement itself.

The `filter` column is `Linear` on **44/44** frames (§7.3).

### 8.3 Why the compositor genuinely "hides" this

Because `visual_x_cache` supplies a **separately computed, unrounded** x
(`compositor_gl.rs:4154-4169` from `column_screen_extents_into`,
`layout.rs:729-754`), the *drawn* ribbon is smooth while the *authoritative*
ribbon is a staircase. Nothing in the system reconciles the two:

* X11 server hit-testing uses the integer `ConfigureWindow` geometry
  (`find_client`, `manage.rs:1224-1243`, walks the tree from the server's own
  hit-test) — so **a click landing on a window during a scroll resolves against a
  position up to 1487 px from where the pixels are**.
* `focus_column_at` (`pointer.rs:691-728`) uses the **fractional** extents.
  Two hit-tests, two different geometries, disagreeing for the whole animation.
* `client.geom` (`render.rs:1054`) holds the integer settled rect, so the pointer
  warp (`render.rs:1372-1375`) and `hide_offscreen` (`render.rs:745-761`) read the
  destination, not the drawn position.
* The client itself receives a `ConfigureNotify` echoing `-1519` immediately and
  never again (`render.rs:1011-1028`).

**So the compositor hides the integer X11 record — including its own ≤0.5 px
divergence — by drawing a float the WM never publishes.**

### 8.4 The one genuine accumulated coordinate error

`ribbon_geom_into`'s f32 running sum (`layout.rs:485`), measured against an f64
reference, same ribbon formula:

```
ncol=   5   total_w=   6489.60   world_x[last] f32=  5347.1997  f64=  5347.2000   err= 0.00029 px
ncol=  20   total_w=  23685.60   world_x[last] f32= 22543.2031  f64= 22543.2000   err= 0.00312 px
ncol=  50   total_w=  58077.57   world_x[last] f32= 56935.1719  f64= 56935.2000   err= 0.02813 px
ncol= 100   total_w= 115397.49   world_x[last] f32=114255.0938  f64=114255.2000   err= 0.10625 px
ncol= 200   total_w= 230038.02   world_x[last] f32=228895.6094  f64=228895.2000   err= 0.40938 px
ncol= 500   total_w= 573958.50   world_x[last] f32=572816.1250  f64=572815.2000   err= 0.92500 px
```

At ~200 columns the drift reaches 0.41 px — comparable to the 0.5 px rounding bound
at P1 — and at 500 columns it exceeds it. This is a genuine, unbounded,
one-signed accumulation in `f32` on the horizontal axis only. Vertical geometry is
computed per row from `wa.y` directly (`layout.rs:662`) and does not accumulate.

### 8.5 `enclosing()` inflation → filter flip (measured)

```
  visual.x=-1519.000000 -> enclosing.w= 1142  smooth=false => Filter::Nearest
  visual.x=-1519.250000 -> enclosing.w= 1143  smooth=true  => Filter::Linear
  visual.x=-1519.500000 -> enclosing.w= 1143  smooth=true  => Filter::Linear
  visual.x=  500.000092 -> enclosing.w= 1143  smooth=true  => Filter::Linear
  inner_w (layout.rs:614 `as u32` trunc) = 1140   (f32 value 1140.4000)
```

`inner_w` is `1140.4 → 1140` — a **one-signed −0.4 px** width error (site S1).

### 8.6 Disproof of the scroll-accumulation theory — with evidence

Three independent checks, all negative:

1. **No wheel delta exists.** `src/backend/x11/pointer.rs:674-677` maps button codes
   to `Dir::Left`/`Dir::Right`; there is no numeric delta, no accumulator field, no
   `XI2` smooth-scroll subscription. A per-event truncation *requires* a delta; there
   is none.
2. **The camera cannot drift.** `Camera::step` integrates `f64` and never reads the
   rounded `position` back as its initial condition (`types.rs:586, 621-627, 667-672`).
   Over 75 frames from 0 → 1527.2 the residual at snap is exactly 0 (the endpoint is
   `snap(target)`, `types.rs:636`).
3. **The substep partition is lossless.** `substep_bounds` produces `n` steps of
   `dt/n` (`compositor_gl.rs:4196-4205`); `sum(step) == dt`.

**Therefore the 1-pixel symptom is not in the camera, not in scroll accumulation,
and not in the spring. It is the integer `ConfigureWindow` record of a
continuously-moving camera (§8.2), and the X11 record is what stays 1-px-stepped
while the compositor shows something else.**

---

## 9. Authoritative vs presentation geometry ownership

| quantity | type | owner | authoritative for | never used for |
|---|---|---|---|---|
| `Camera{position,target,velocity}` + `Camera{x,v}` | f32 + f64 | `maverick_core::State` | the *destination*; `target` feeds X11 | the drawn position |
| `Column.boost`, `Workspace.zoom/.page_zoom` | f32 | `State` | nothing on X11 | the live projection |
| `Client.geom` | `Rect` (i32) | `State`, written by `emit_geometry` (`render.rs:1054`) | hit-testing, pointer warp, `hide_offscreen`, `parked_rect`, `normalise_float_geom` | the drawn position |
| `Placements` / `DesiredState` | `Rect` (i32) | core | the X11 intent | the drawn position |
| `AppliedWindow.rect` | `Rect` (i32) | backend | suppress-vs-emit of `ConfigureWindow` | anything else |
| `CompWin.transform` | `Rect` (i32) | compositor | damage, culling, scissor, occlusion | the draw (`dst` uses `visual_transform`) |
| `CompWin.presentation_value/_target/_goal` | `[f64;5]` | compositor | the presentation transition only | anything in `State` |
| **`CompWin.visual_transform` (`VisualRect`, f32)** | f32 | compositor | **nothing** | the GPU draw only |
| **`visual_x_cache` (`HashMap<Window, f32>`)** | f32 | compositor | **nothing** | the fractional x of `visual_transform` |

**The separation is explicit and, with one exception, correct.** The `VisualRect`
doc comment (`compositor_gl.rs:132-133`) says it outright: *"Fractional compositor
geometry. `Rect` remains the WM/X11 authority; this type is used only after the live
projection reaches the compositor."* The `CompWin::transform` doc
(`compositor_gl.rs:343-346`) says the same for the integer copy.

### Where the two are conflated — findings

**C1. The compositor's own live projection uses the X11-rounded integer.**
`live_placements` (`compositor_gl.rs:4179-4192`) calls the *same* `arrange(Phase::Live)`
that feeds X11, so `CompWin::transform` is `round()`ed. The compositor then has to
re-derive a *second* projection (`refresh_visual_x_cache` →
`column_screen_extents_into`) just to recover the fraction it threw away one line
earlier. Two evaluations of one formula, kept in sync by nothing but a comment.

**C2. The two hit-tests disagree during every scroll.** `focus_column_at`
(`pointer.rs:711-716`) uses the fractional extents; `find_client`
(`manage.rs:1224`) uses the server's integer hit-test. Not just 0.5 px — the full
travel distance, per §8.3.

**C3. `filter` is decided by damage geometry.** The Nearest/Linear choice at
`compositor_gl.rs:3188` reads `outer = visual.enclosing()`, i.e. a *damage* rect, to
decide how the *draw* samples its texture. Damage geometry and draw geometry are
conflated, and the consequence (perpetual bilinear resampling during scroll) is a
side effect nobody chose.

**C4. `[f64;5]` → two grids in one function** (`compositor_gl.rs:560-599`): the
presentation value is rounded into `transform` and truncated into
`visual_transform` from the same source, in adjacent statements, with no statement
of which one is the truth.

**Not conflated (checked, clean):** `Client.geom` never takes a compositor value;
`State` has no `f32` geometry field; `present.rs` has no float at all; the reconciler
has no float at all (`grep -c "as f32\|as f64" reconciler.rs` → 0).

---

## 10. What must change for movement to be correct without the compositor

Movement is **already** correct without the compositor. §8.6/experiment 3 measured it:

```
camera snaps to target: position=1527.2000 target=1527.2000
X11 ConfigureWindow x = -1519
```

That is the exact same integer the compositor path eventually produces, with zero
animation. So the answer to "would the movement be correct with the compositor
deleted?" is **yes, and instantly** — the compositor buys motion, not accuracy.

If the goal is animation *without* the compositor, these are the exact sites:

1. **`src/backend/x11/mod.rs:915-927`** — the `else` branch. Replace
   `self.engine.state.snap_animations(); … self.animating = false;` with the same
   `substep_bounds(dt)` + `tick_animations_multi(sub, &mut self.anim_per_mon)` loop
   the compositor branch runs at `mod.rs:791-802`. This single change is what makes
   the integrator reachable; today it is lexically inside `if let Some(comp)`.
2. **Frame pacing** — `mod.rs:946-951` sets `animation_due` from
   `should_wait_after_swap(vsync_on)`; with `self.compositor == None`, `vsync_on` is
   `false` and `should_wait_after_swap(false) == true`, so `animation_due` is
   `Some(Instant::now() + self.frame_period)`. That part already works: the
   refresh-derived `frame_period` comes from RandR (`mod.rs:1081,
   2220-2240`), not from GL. **No change needed**, but the reason it works is that
   `vsync_on` happens to be `false` — this is a latent coupling, not a design.
3. **Per-frame arrange** — call
   `self.arrange_full_phase(mi, /*do_hide=*/false, Phase::Live)`
   (`render.rs:604`) once per animating monitor per frame. The function exists and is
   correct; `hide_offscreen` must be skipped per frame (it is a full re-scan) and
   `apply_geom`/`reconcile` will suppress the no-op writes (`reconciler.rs:146-147`).
   This path is documented at `render.rs:600-603` — *"`Live` (the X11-only animation
   path, per-frame)"* — **but `arrange_full_phase` has exactly one production caller
   and it passes `Phase::Settled` (`render.rs:597`).** The documented X11 animation
   path does not exist.
4. **Reconciler throughput** — `reconcile` (`reconciler.rs:199-246`) allocates a
   `HashMap` per call and a `Vec` per call; at 60 Hz × N monitors with N windows this
   is the only thing that would need attention for a per-frame path.
5. **The rounding is correct as-is.** `layout.rs:649`'s `.round()` is the right
   X11 boundary; `Column.weight` / `boost` / `camera.position` in f32 are adequate for
   a screen-width ribbon. The one thing worth reconsidering is
   `layout.rs:485`'s f32 running sum (§8.4) if ribbons are expected past ~200 columns.

**What would *not* be correct without the compositor, and is not fixable there:**
the sub-pixel drawn position. X11 `ConfigureWindow` cannot express it, so an
X11-native build shows a 1-px-staircase ribbon. That is the honest answer to
"does the compositor hide it": **yes — and it is the only way to hide it on X11.**

---

## 11. Evidence appendix

### 11.1 The experiments

Three standalone programs in `/tmp/kilo/mov/`, compiled with `rustc 1.98.1`,
transcribed line-for-line from the real code. No repo file was touched.

| file | transcribes |
|---|---|
| `trace.rs` | `ribbon_geom_into` (`layout.rs:397-498`), `arrange_columns` screen x / `inner_w` (`layout.rs:614, 649`), `column_screen_extents_into` (`layout.rs:750-752`), `ideal_scroll` (`layout.rs:762-798`), `Camera::{new,retarget,needs_update,analytic_state,step,snap}` (`types.rs:498-673`), `substep_bounds` (`compositor_gl.rs:4196-4205`) |
| `trace2.rs` | same + the X11-delta histogram, the visual-vs-rounded census, the f32 running-sum error sweep |
| `trace3.rs` | `VisualRect::enclosing` (`compositor_gl.rs:164-182`), the filter decision (`compositor_gl.rs:3188-3193`), the X11-vs-drawn divergence, and the compositor-removed comparison |

`ideal_scroll` and the camera integrator were transcribed including both the
overdamped and underdamped branches; the shipped `stiffness = 220.0` /
`damping = 30.0` is overdamped (`c² = 900 > 4k = 880`) and takes the `types.rs:590-599`
branch. Both programs agree on every shared quantity.

### 11.2 Reproduce

```
cd /tmp/kilo/mov && rustc -O -o trace trace.rs && ./trace
cd /tmp/kilo/mov && rustc -O -o trace2 trace2.rs && ./trace2
cd /tmp/kilo/mov && rustc -O -o trace3 trace3.rs && ./trace3
```

### 11.3 Grep results this audit rests on

```
$ grep -c "as f32\|as f64" src/core/layout.rs                 33
$ grep -c "as f32\|as f64" src/core/present.rs                 0
$ grep -c "as f32\|as f64" src/backend/x11/reconciler.rs       0
$ grep -c "as f32\|as f64" src/compositor_policy.rs            0
$ grep -c "as f32\|as f64" src/backend/x11/render.rs           2
$ grep -c "as f32\|as f64" src/backend/x11/compositor_gl.rs   35
$ grep -c "as f32\|as f64" maverick-core/src/types.rs         11

$ grep -rn "BUTTON_4|BUTTON_5|Wheel|scroll_delta|ScrollAmount|Precise|ButtonIndex::[45]" src maverick-core/src
  -> only pointer.rs:156 (`if e.detail >= 4`) and prose

$ grep -rn "xcb_present|PresentExtension|present::" src maverick-gl/src
  -> only core::present (a different thing)

$ grep -rn "arrange_full_phase" src
  -> render.rs:597 (Phase::Settled) and render.rs:604 (definition). No Phase::Live caller.

$ grep -rn "camera.position = |camera.snap(" src maverick-core/src | grep -v tests
  -> invariants.rs:115-116,702-703,1214 and framebench.rs:146 only (test fixtures)
```

### 11.4 The repo's own test that proves the compositor renders sub-pixel

`src/backend/x11/compositor_gl.rs:5174-5190`, quoted in §7.2. Its assertions
(`visual_transform.x == 499.75`, `transform.x == 500`) are the cleanest available
statement of the split.

### 11.5 X11 transport width — a correction to the code's own comments

`reconciler.rs:98-99` and `render.rs:990-996` both assert that *"`ConfigureWindow`
takes an INT16 origin and a CARD16 extent"*. **That is true of the events, not of
the request.** Verified two ways:

* `x11rb-protocol-0.13.2/src/protocol/xproto.rs:8978-8985` types
  `ConfigureWindowAux{x, y: Option<i32>, width, height, border_width: Option<u32>}`
  and serialises 4 bytes each;
* serialising a real request (x = `0x7FFF1234`) produces 32 bytes with the value in
  bytes 12-15 — `0c 00 08 00 | 78 56 34 12 | 1f 00 00 00 | 34 12 ff 7f | …`, i.e.
  length 8 words, **not** the 6 words a 16-bit encoding would need;
* libxcb agrees: `xcb_configure_window_value_list_t { int32_t x, y; uint32_t
  width, height, border_width; … }` (`/usr/include/xcb/xproto.h:1737-1745`).

The **events** *are* 16-bit: `ConfigureRequestEvent { x: i16, y: i16, width: u16,
height: u16, border_width: u16 }` (`xproto.rs:4167-4180`). So
`emit_geometry`'s synthetic-notify clamp (`render.rs:1017-1021`) is correct and
necessary, and `wire_geometry`'s `u16::MAX` width clamp (`reconciler.rs:115-118`) is
*over*-conservative — it rejects 65536-px windows the wire can carry. The
`parked_rect` comment (`render.rs:398-404`) reasons about `INT16` for the *notify*,
which is right. **[UNVERIFIED against a live X server]** — established from the
protocol headers and a wire dump only.

### 11.6 GL rasterisation note

`VERTEX_SRC` (`maverick-gl/src/renderer.rs:66-81`) is a `vec4` float destination
rect interpolated into a float `gl_Position`; there is no quantisation in the
shader. The fragment grid still samples at pixel centres, so a hard edge advances one
column at a time — the fraction is honoured as *phase*, not as a smooth translate.
This is normal for any compositor; it is stated here so the "sub-pixel" claim in §7.2
is not over-read.

---

## 12. Out-of-scope bugs and unverified claims

### Bugs found, not fixed (out of the movement pipeline's scope)

| # | site | issue |
|---|---|---|
| B1 | `src/core/layout.rs:614` vs `:649` | Position uses `.round()`, size uses bare `as u32` (truncation) three lines apart in the same function. Every tiled window is up to 1 px narrower than the fractional geometry, one-signed. Measured −0.4 px on a 0.6-weight column at 1904 px workarea. |
| B2 | `src/core/layout.rs:485` | f32 running sum over columns. Unbounded, one-signed. 0.41 px @ 200 columns, 0.93 px @ 500. |
| B3 | `src/backend/x11/compositor_gl.rs:3188-3193` | `Filter::Nearest`/`Linear` is chosen from `visual.enclosing()` — a *damage* rect. Any sub-pixel `visual.x` inflates the enclosing width by 1, so every window is bilinearly resampled for the entire scroll and snaps to nearest at the endpoint. Measured 44/44 frames. |
| B4 | `src/backend/x11/render.rs:1087-1089` | Stale comment: *"`emit_geometry` fires on every Configure effect, including pure moves (camera scroll re-Configures every visible window's x each animation frame)"*. Since `Phase::Live` never reaches the reconciler (§3), `emit_geometry` fires **once** per settle, not per frame. The `SHAPE` guard below it is therefore guarding a case that cannot occur. |
| B5 | `src/backend/x11/reconciler.rs:40-41` | Stale comment: *"repeated calls from `arrange_full_phase` (once per animating monitor per frame)"*. `arrange_full_phase` has no per-frame caller. |
| B6 | `src/backend/x11/render.rs:600-603` | Stale doc: describes `Phase::Live` as *"the X11-only animation path, per-frame"*. There is no X11-only animation path. |
| B7 | `src/backend/x11/pointer.rs:711-716` vs `manage.rs:1224-1243` | Two hit-tests over two different geometries (fractional extents vs the server's integer hit-test). Disagree by up to the full scroll travel. |
| B8 | `src/backend/x11/render.rs:1011-1028` + `compositor_gl.rs:526` | A `ConfigureNotify` synthesised with the *destination* geometry is sent to a client that is currently being *drawn* at the source geometry, and then never updated for ~733 ms. Toolkits that cache geometry from the notify will be a full travel distance stale. |
| B9 | `reconciler.rs:98-99`, `render.rs:990-996` | Comments claim `ConfigureWindow` is INT16/CARD16. The request is 32-bit; only the events are 16-bit (§11.5). `wire_geometry`'s `u16::MAX` width clamp is consequently over-conservative. |

### Unverified claims

* **[UNVERIFIED]** Every number in §8 comes from a transcription, not from a live
  X server. The transcription was checked line-by-line against the cited sources and
  the two independent programs agree, but no end-to-end Xephyr run was performed
  (out of scope; Agent E owns the test harness).
* **[UNVERIFIED]** The claim that GL's fragment grid samples at pixel centres and
  therefore a hard edge still advances one column at a time is standard rasteriser
  behaviour, argued from `maverick-gl/src/renderer.rs:66-81`; not measured on this
  GPU. The *filter flip* that accompanies it (B3) **is** measured from the code.
* **[UNVERIFIED]** "A click during a compositor scroll resolves against the wrong
  window." Derived from the X11 protocol (server hit-tests against the window's own
  geometry, which the compositor does not move) plus the measured 1487 px gap. Not
  observed on a live display.
* **[UNVERIFIED]** The `u16::MAX` clamp in `wire_geometry` is over-conservative
  rather than load-bearing. Established from the protocol headers and a wire dump
  (§11.5); a live server that rejected >65535-px configures would contradict this.
* **[NOT APPLICABLE]** The brief's hypothesis that `src/core/present.rs` is the
  f32 sub-pixel animation owner. `grep` for `f32`/`f64` in that file returns
  nothing outside `#[cfg(test)]`; it is a pure integer rect-substitution layer.
* **[NOT APPLICABLE]** The brief's hypothesis that a 120-unit wheel delta is scaled,
  accumulated lossily, or truncated per event. No wheel delta exists in the codebase
  (§5.1, §8.6).
