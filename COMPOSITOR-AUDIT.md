# Maverick compositor ↔ WM architecture audit

Scope: every path between the compositor code (`src/backend/x11/compositor*.rs`,
`maverick-gl/`, `maverick-render/`, `maverick-vk/`, `src/compositor_policy.rs`,
`src/backend/renderer.rs`) and the window manager
(`src/core/**`, `src/backend/x11/{mod,render,reconciler,manage,events,input,teardown,pointer}.rs`,
`maverick-core/`, `maverick-x11/`).

Read-only audit. No `.rs`, `Cargo.toml` or `Cargo.lock` was modified; no
`cargo build` / `cargo test` was run. Every claim below is anchored to a
`file:line` or a `grep`/`wc` command excerpt; anything I could not confirm by
reading is marked `[UNVERIFIED]`.

Classification key:

| | |
|---|---|
| **A** | genuinely independent of the WM |
| **B** | modifies WM correctness |
| **C** | masks a WM bug |
| **D** | duplicates geometry/state ownership |
| **E** | compensates for integer X11 geometry |
| **F** | purely visual |

---

## 1. Summary verdict

**Dominant: D — duplicates geometry/state ownership. Secondary: E — compensates for integer X11 geometry.**

The compositor is *not* entangled with WM correctness in the way the campaign
premise implies. Concretely:

* **The WM owns every rectangle that X11 ever sees.** `render::emit_geometry`
  (`src/backend/x11/render.rs:972-1068`) is the single geometry sink, and
  `reconciler::wire_geometry` (`src/backend/x11/reconciler.rs:110-120`) is the
  single clamping function. The compositor **never issues a `ConfigureWindow`
  for a client and never writes a client rectangle to X** — the only X requests
  it makes on client windows are `ShapeSelectInput`
  (`src/backend/x11/compositor_gl.rs:1661`), `DamageCreate`
  (`compositor_gl.rs:1685`) and `CompositeNameWindowPixmap`
  (`compositor_gl.rs:3886`). It writes a shape to *its own overlay*
  (`compositor_gl.rs:2780-2784`), never to a client.
* **No WM correctness decision depends on the compositor.** Focus, stacking,
  monitor topology, window lifecycle, geometry diffing, float authority, and
  hit-testing are all computed from `State` alone. `applied.diff`
  (`reconciler.rs:130-156`) and `classify_configure`
  (`reconciler.rs:304`) never consult the compositor. Verified by reading
  `render.rs`, `reconciler.rs`, `events.rs`, `manage.rs`, `pointer.rs` in full
  for compositor references (the complete list is §3).
* **But it keeps a second, parallel geometry model** (§5): six distinct
  rectangles per window, of which four are compositor-owned, and a fifth
  (`visual_x_cache`) is a *re-derivation of the core layout projection in
  floating point* living inside the compositor. That is the duplicate-ownership
  finding: the compositor re-computes geometry the WM already computed (in
  integer) and re-projects it (in float) so it can draw between the integers.
* **And the compositor is the only clock for WM animation state**
  (`src/backend/x11/mod.rs:791-802`): `State::tick_animations_multi` — which
  advances `camera.position/velocity`, `column.boost`, `ws.zoom`, `ws.page_zoom`
  — is called **only** inside the `if let Some(comp) = self.compositor.as_mut()`
  arm. Without a compositor the loop calls `State::snap_animations()` every turn
  (`mod.rs:923`). That is B-flavoured, but the difference is unobservable
  because with no compositor nothing would draw the animation anyway.
* **The movement symptom is a real, documented, and *partially masked* integer
  quantization** (§6). The mechanism is that the core layout rounds both axes
  to `i32` at the X11 boundary (`src/core/layout.rs:580,584,649,664`) and the
  compositor keeps a fractional re-projection (`visual_x_cache`) that repairs
  **X only, for tiled columns only, excluding floats, maximized and
  fullscreen-overlay windows**. Everything the cache does not cover moves in
  whole-pixel steps.

Entanglement count (§3): **≈70 distinct call/data edges**, of which
**exactly 2 write into WM `State`** (`compositor_gl.rs:2315,2411`) and
**exactly 1 changes what X11 requests the WM issues**
(`render.rs:1071,1077` — the XShape rounded-corner mask is skipped when a
compositor is present). **Zero** change geometry, focus, stacking, or lifecycle
outcomes.

---

## 2. Compositor surface inventory

### 2.1 Feature-gate sites (`compositor-opengl` / `compositor-vulkan`)

Complete list — `grep -rn 'cfg(feature = "compositor' src/ --include=*.rs`:

| file:line | item |
|---|---|
| `src/core/mod.rs:85-86` | `#[cfg(feature="compositor-opengl")] pub use wallpaper::WallpaperGpu;` |
| `src/core/wallpaper.rs:29-42` | `#[cfg(feature="compositor-opengl")] pub trait WallpaperGpu` |
| `src/backend/x11/compositor.rs:21-23` | `#[cfg] #[path="compositor_gl.rs"] mod compositor_gl;` |
| `src/backend/x11/compositor.rs:25-26` | `#[cfg] pub(crate) use compositor_gl::*;` |
| `src/backend/x11/compositor.rs:28-232` | `#[cfg(not(...))] mod placeholder` (zero-cost stub) |
| `src/backend/x11/compositor.rs:234-235` | `#[cfg(not(...))] pub use placeholder::*;` |
| `src/backend/x11/compositor.rs:237-295` | `#[cfg(all(test, not(...)))] mod placeholder_substep_tests` (59 lines) |
| `src/backend/x11/mod.rs:416-417` | `#[cfg] Event::DamageNotify(e) => self.on_damage_notify(e)?` |
| `src/backend/x11/mod.rs:418-419` | `#[cfg] Event::XfixesSelectionNotify(e) => self.on_xfixes_selection_notify(e)?` |
| `src/backend/x11/mod.rs:420-421` | `#[cfg] Event::ShapeNotify(e) => self.on_shape_notify(e)?` |
| `src/backend/x11/events.rs:65-66` | `#[cfg] use damage::NotifyEvent as DamageNotifyEvent` |
| `src/backend/x11/events.rs:700-716` | `#[cfg] fn on_xfixes_selection_notify` |
| `src/backend/x11/events.rs:718-727` | `#[cfg] fn on_shape_notify` |
| `src/backend/x11/events.rs:749-758` | `#[cfg] fn on_damage_notify` |
| `src/backend/x11/compositor_gl.rs:264` | doc comment only (Vulkan placeholder) |

Non-`#[cfg]` compile-time probes (`cfg!`, not attribute gates):

| file:line | effect |
|---|---|
| `src/config.rs:543` | neither `compositor-opengl` nor `compositor-vulkan` ⇒ `validate_compositor_backend` is a no-op `Ok(())` |
| `src/config.rs:554` | `cfg!(feature="compositor-opengl")` selects the "opengl" branch |
| `src/config.rs:567` | `cfg!(feature="compositor-vulkan")` selects the "vulkan" branch |

Total `#[cfg(feature=...)]` sites in `src/`: 41 (17 `input-trace`, 12
`window-trace`, 11 `compositor-opengl`, 1 `compositor-vulkan`).

### 2.2 `src/backend/x11/compositor_gl.rs` (5763 lines) — module map

| range | item | role |
|---|---|---|
| 86-89 | `SUBSTEP_MS` | spring substep ceiling, **shared with WM core** |
| 91-112 | `ProjSig`, `proj_signature` | cache key over `ws.zoom/zoom_target/page_zoom/…` |
| 114-130 | `LiveProjectionInputs`, `live_projection_needs_rebuild` | live-projection cache predicate |
| 132-183 | `VisualRect`, `visual_draw_dst`, `enclosing()` | **the compositor's fractional rect** |
| 185-255 | `overlay_coverage`, `subtract_rect` | screen-minus-bypass-holes |
| 262-292 | `CompositorRenderer` | backend enum, `Deref` to `GlRenderer` |
| 294-372 | `struct CompWin` | **the per-window duplicate geometry model** (§5) |
| 374-421 | `PresentationTransition`, `presentation_value`, `rounded_radius_for`, `border_rgba` | |
| 423-656 | `impl CompWin` | `new`, `observe_configure`, `set_transform_with_visual`, `tick_presentation`, `can_occlude`, `current_visual_rect`, `offscreen` |
| 658-664 | `rects_overlap` | |
| 666-773 | `DamageRegion` (CAP = 32) | bounded damage set |
| 775-787 | `FrameMode` | Idle/Full/Partial |
| 789-821 | `DirtyReason` | 5 bits |
| 823-834 | `decide_redraw` | pure |
| 836-863 | `plan_aged_damage` | pure, buffer-age journal |
| 865-890 | `visual_moved`, `anim_damage_rects` | |
| 892-899 | `fully_covered_by` | |
| 909-924 | `DrawItem` | |
| 926-937 | `require_x_extension` | |
| 939-1143 | `struct Compositor` | 40+ fields incl. all caches |
| 1144-1151 | `vsync_active` | **feeds the WM frame clock** |
| 1154-1578 | `Compositor::init` | CM selection, redirect, overlay, GLX |
| 1579-1760 | `track`, `retry_pending_tracks` | per-window GL resources + Damage object |
| 1761-1998 | `on_create`, `on_restack`, `on_destroy`, `on_map`, `on_unmap`, `set_hidden` | WM event sinks |
| 2000-2066 | `on_configure` | |
| 2068-2155 | `on_damage`, `finish_damage` | XDamage |
| 2157-2186 | `on_opacity`, `on_border_color`, `on_shape` | |
| 2188-2277 | `set_transforms` | **WM placements → per-window transform** |
| 2279-2433 | `prepare_frame` | **takes `&mut State`** (§3) |
| 2435-2490 | `presentation_animating`, `invalidate`, `wait_vblank`, `set_debug_floats`, `dirty_reasons_bits`, `needs_frame`, `dirty_reasons` | scheduler inputs |
| 2491-2656 | `set_wallpaper`, `set_outputs`, `tick_wallpaper`, `wallpaper_animating`, `impl WallpaperGpu` | |
| 2657-2934 | `damage_region`, `mark_full`, bypass engine (`engage/disengage/bypass_window/resume_window/update_overlay_shape/bypass_covers`), `dbg_res_ids`, `debug_dump` | |
| 2936-3241 | `compute_scene` | occlusion cull + draw-list build |
| 3243-3620 | `render` | fence, scene, `decide_redraw`, scissor, wallpaper, draw, `end_frame`, damage commit |
| 3622-3978 | `selection_owner_changed`, `disable`, `abandon`, `refresh_stack`, `refresh_wallpaper`, `format_for_depth`, `scan_existing`, `rename_and_bind`, `release_texture`, `Drop` | |
| 3979-4177 | `stack_restack/add_top/remove`, `screen_visuals`, `map_damage_rect`, `set_empty_input_region`, `intern_cm_atom`, `selection_owned`, `refresh_visual_x_cache` | |
| 4179-4205 | `live_placements`, `substep_bounds` | **the compositor's entry into `core::layout` / `core::present`** |
| 4207-5763 | `#[cfg(test)]` modules: `stack_tests`, `damage_tests`, `coverage_tests`, `frameplan_tests`, `bench`, `lifecycle_tests`, `substep_tests` — **1557 lines** | |

### 2.3 `src/backend/x11/compositor.rs` (295 lines)

* 1-18 module doc; 21-26 the feature gate; 28-232 the placeholder
  (`Compositor` with 30 no-op methods, `DirtyReason`, `substep_bounds`,
  `live_placements`, `FrameScheduler`); 237-295 placeholder tests.
* The placeholder's `live_placements` (`compositor.rs:215`) and `FrameScheduler`
  (`compositor.rs:226-230`) are **dead**: the only live `FrameScheduler` is
  `framesched::FrameScheduler`, imported explicitly at `mod.rs:85`.

### 2.4 `src/backend/x11/framesched.rs` (795 lines) — WM-side, not compositor-only

`FrameReason` (35-84), `ONE_REFRESH` (89), `MAX_ANIMATION_DT` (96),
`should_wait_after_swap` (100-102), `needs_endpoint_frame` (106-108),
`clamp_frame_dt` (113-119), `FrameScheduler` (126-251), tests 253-795.

`from_compositor` (142-172) takes the WM `animating` flag plus the compositor's
`DirtyReason`. It is used by the WM in **both** configurations
(`mod.rs:840` and `mod.rs:926`), so this file is **not** a deletion candidate.

### 2.5 `src/compositor_policy.rs` (970 lines; 204 production + 766 tests)

Pure, no X/GL (asserted in the header, 15-17).
`CompositionMode` (55-74), `mode_for` (81-89), `bypass_candidate` (106-157),
`occluding_window_present` (162-186), `covers_screen` (193-201).
Only caller: `mod.rs:753-770`, inside the compositor arm.

### 2.6 `maverick-gl` (5043 src + 740 test lines)

| file | lines | public items |
|---|---|---|
| `src/lib.rs` | 95 | `dl`, `gl`, `glx`, `renderer` mods (54-57); `XConn` (72); `probe()` (78) |
| `src/dl.rs` | 290 | `Lib` (22), `open_gl` (63), `sym` (122), `sym_opt` (131) |
| `src/glx.rs` | 214 | ~40 GLX constants (22-59), `has_extension` (212) |
| `src/gl.rs` | 245 | hand-written GL entry-point table |
| `src/renderer.rs` | 4199 | `ShaderId` 38, `Rect` 45, `Texture` 146, `VisualFormat` 214, `Filter` 264, `TextureHandle` 284, `DrawQuad` 288, `RendererBackend` 307, `VsyncMode` 323, `Acceleration` 331, `RendererInfo` 349, `classify_acceleration` 370, `VisualReport` 430, `Renderer` 701, and methods `new` 788, `new_with_vsync` 813, `begin_frame` 1177, `back_buffer_age` 1201, `set_scissor` 1224, `scissor_clear` 1237, `clear_scissor` 1248, `draw` 1262, `draw_raw` 1303, `end_frame` 1349, `wait_vblank` 1365, `texture_from_pixmap` 1388, `bind` 1523, `destroy_raw` 1569, `upload_rgba` 1594, `compile_fragment` 1658, `draw_shader` 1778, `destroy_shader` 1810, `destroy_texture` 1834, `root_format` 1902, `format_report` 1913, `fbconfig_report` 1929, `destroy` 1989 |

Only `maverick-gl` importers in the tree: `src/backend/x11/compositor_gl.rs`,
`src/core/wallpaper.rs` (the `WallpaperGpu` trait signature),
`src/backend/renderer.rs` (a comment + an unused re-export path).

### 2.7 `maverick-render` (264 src + 141 test lines) — **dead seam**

`Rect` 63, `DrawQuad` 79, `Filter` 92, `TextureHandle` 102, `VisualDesc` 109,
`Acceleration` 119, `RendererInfo` 141, `trait Renderer` 189, `trait Texture` 261.

Its only consumer is `src/backend/renderer.rs:26-29`, a pure re-export whose own
doc comment states (lines 14-21): *"The seam is declared but not yet spanned …
nothing in the tree implements `Renderer` or `Texture` and no production code
imports the types below."* Verified: `grep -rn 'maverick_render' --include=*.rs`
matches only `src/backend/renderer.rs:26` (plus comments).

### 2.8 `maverick-vk` (2190 src + 1299 test lines) — **unwired**

`grep -rn 'maverick-vk|maverick_vk' --include=Cargo.toml --include=*.rs` finds
**no crate depending on it**. It appears only in the workspace member list
(`Cargo.toml:10`) and in the source-scan allowlist of the structural test
(`tests/no_wait_in_wm.rs:63`). The `compositor-vulkan` feature is empty
(`Cargo.toml`, `compositor-vulkan = []`) and has zero `#[cfg]` users in `src/`.

### 2.9 `src/backend/renderer.rs` (29 lines)

Pure `pub use maverick_render::{…}` re-export, `#[allow(unused_imports)]`
(line 25). Nothing imports `crate::backend::renderer`.

### 2.10 `src/backend/x11/rootwall.rs` (231 lines) — the **non**-compositor path

`apply_root_wallpaper` early-returns when a compositor is present
(`rootwall.rs:36-37`). This is the *fallback*, so it must survive any deletion
of the compositor.

---

## 3. Adjacency list: compositor ↔ WM subsystem edges

Produced by reading every `compositor` reference outside `compositor_gl.rs`
(`grep -rn 'compositor' src/ | grep -v compositor_gl`) and every
`compositor_gl` reference inside it, then classifying each call site.

Legend: `→` WM calls compositor, `←` compositor touches WM, `⇄` both.

### 3.1 `core` / `maverick-core` (layout, camera, presentation, engine)

| # | edge | site | class |
|---|---|---|---|
| C1 | ⇄ | `mod.rs:792` `compositor::substep_bounds(dt)` drives `state.tick_animations_multi(sub, …)` at `mod.rs:793-796` | **D** — the spring *integrator* is core code, the *step-size function* is defined twice in the compositor module (`compositor_gl.rs:4196-4205` and the placeholder `compositor.rs:201-212`, with a comment at 201-205 demanding they "must match exactly"). Any divergence silently changes every animation's trajectory. |
| C2 | → | `compositor_gl.rs:4189` `arrange(state, mon_idx, cfg, registry, Phase::Live, out, scratch)` | **A** — a pure call into `core::layout::arrange`; identical function the WM calls with `Phase::Settled` (`render.rs:619-627`). |
| C3 | → | `compositor_gl.rs:4191`, `2342-2347` `present_into(state, mon, out, raise)` | **A** — the same pure function the WM calls at `render.rs:631-636`. |
| C4 | → | `compositor_gl.rs:4153` `fs_ctx(&state.clients, ws, mon.screen)`; `4154` `column_screen_extents_into(ws, cfg, mon.workarea, &fs, …)` | **D** — a second, *floating-point* re-projection of the same table `arrange` just produced in integers. This is the fractional-X compensation (§6). |
| C5 | ⇄ | `compositor_gl.rs:882-887` `prepare_frame(&mut self.engine.state, &self.engine.cfg, &self.layout_registry, &self.anim_per_mon, dt)` | **D** — the compositor is handed `&mut State`. |
| C6 | ← | `compositor_gl.rs:2411` `state.monitors[i].layout_dirty = false;` | **D** — the compositor *clears a WM cache-invalidation flag*. See §3.7 for the reader audit. |
| C7 | ← | `compositor_gl.rs:2315` `m.layout_dirty = true;` (on monitor-count change) | **D** — same flag, opposite direction. |
| C8 | ← | `compositor_gl.rs:103-112` `proj_signature(ws, cfg)` reads `ws.zoom/zoom_target/page_zoom/page_zoom_target/columns[].boost` | **A** (read-only) |
| C9 | → | `compositor_gl.rs:425`, `544-547`, `612-614` construct and step `crate::types::Camera` for the per-window presentation transition | **D** — a `maverick_core` type instantiated and integrated inside the compositor. |
| C10 | → | `compositor_gl.rs:2617-2656` `impl WallpaperGpu for Compositor` (the core trait at `core/wallpaper.rs:30`) | **F** |
| C11 | ⇄ | `mod.rs:739-914` the whole compositor block runs **inside** `run_once` and owns the branch that decides whether WM springs are ticked at all | **B** (see §3.8) |
| C12 | ⇄ | `compositor_gl.rs:2298` `self.frame_dt = dt;` — the compositor's animation clock is the *WM loop's* `dt`, and its presentation springs run off it (`compositor_gl.rs:2300-2302`) | **B** — one clock, two consumers (WM springs + compositor springs). Deliberate and documented (`compositor_gl.rs:2290-2292`). |

**`maverick-core` decisions affected: 0.** No `Command`, no `Effect`, no
`Engine::dispatch` path, no `Client` flag, and no invariant check
(`core/invariants.rs`) reads the compositor.

### 3.2 `backend::x11::render` — geometry

| # | edge | site | class |
|---|---|---|---|
| G1 | → | `render.rs:670-672` `c.invalidate()` at the end of `arrange_full_phase` | **F** — one compositor frame after a WM arrange. |
| G2 | → | `render.rs:747-749` `c.set_hidden(win, true)`; `762-764` `c.set_hidden(win, false)` in `hide_offscreen` | **F** — but note `set_hidden` also drops the presentation transition and zeroes `transform_gen` (`compositor_gl.rs:1990-1997`). |
| G3 | → | `render.rs:1280`, `1427`, `1599`, `1618` `compositor.on_border_color(win, col)` (focus / urgent / normal) | **F** |
| G4 | ⇄ | `render.rs:1071` `(cfg.corner_radius > 0 && self.compositor.is_none())` and `1077` `if is_fullscreen \|\| self.compositor.is_some() { 0 }` in `sync_rounded_frame` | **F** — **the only place where compositor presence changes the X11 request stream.** With a compositor, no `ShapeRectangles` mask is uploaded; without one, the WM rounds the client window via XShape. Same visual result, different protocol. |
| G5 | → | `render.rs:10-18` (pipeline doc) `… → emit_geometry → stack_overlay → compositor.invalidate` | — (documentation of G1) |

### 3.3 `backend::x11::reconciler` — **no edges**

`grep -n 'compositor' src/backend/x11/reconciler.rs` → no match other than the
doc reference in the `Phase::Live` comment at line 40. `reconcile`
(`reconciler.rs:199`), `AppliedState::diff` (130), `wire_geometry` (110) and
`classify_configure` (304) are pure functions of `DesiredState` + `State` +
`AppliedState`. **The geometry round-trip is entirely compositor-free.**

### 3.4 `backend::x11::input` (setup_root, keymaps, grabs) — **no edges**

`grep -n 'compositor|damage|Damage|xfixes|Shape' src/backend/x11/input.rs`
matches only `ChangeWindowAttributesAux…event_mask` (72) and
`randr_select_input` (160). Event *selection* for Damage/Shape is done inside
the compositor (see G-row below), but the WM's own root selection is unchanged.

| # | edge | site | class |
|---|---|---|---|
| I1 | ← | `compositor_gl.rs:1661` `shape_select_input(win, true)` on each tracked window | **A/F** — an X subscription on a client window; it enables `Event::ShapeNotify` at `mod.rs:420`. |
| I2 | ← | `compositor_gl.rs:1685` `damage_create(dmg, win, ReportLevel::NON_EMPTY)` on each tracked window | **A/F** |
| I3 | ← | `compositor_gl.rs:4102-4115` `set_empty_input_region(overlay)` — empties the **overlay's** INPUT shape so pointer events fall through | **A** — and it is *required for input correctness*, but the compositor refuses to start if it fails (`compositor_gl.rs:1345-1353` → `return None`), so the WM never runs with a swallowing overlay. |

### 3.5 `backend::x11::events` — event dispatch

| # | edge | site | class |
|---|---|---|---|
| E1 | → | `events.rs:103-105` `c.on_destroy(e.window)` | **F** (resource cleanup) |
| E2 | → | `events.rs:116-118` `c.on_unmap(e.window)` — called **before** the `e.event == self.root` filter (126) and before the unmanage | **A** |
| E3 | → | `events.rs:376-384` `c.on_configure(win, x, y, w, h, bw)` for every `ConfigureNotify` | **F** — but see E3b. |
| E4 | → | `events.rs:385-388` `c.on_restack(win, above)` for the root-targeted copy | **F** — the compositor maintains a *draw* stack; the X stack is owned by `render::stack_overlay` (`render.rs:807-958`). Two independent stacks, one for X, one for GL. |
| E5 | → | `events.rs:583-591` `comp.set_outputs(&outs)` on monitor change; else `apply_root_wallpaper()` at 595 | **F** |
| E6 | → | `events.rs:669-680` `_NET_WM_WINDOW_OPACITY` → `c.on_opacity(...)`, then `return Ok(())` | **A** — the early return is *inside* the `if let Some(c) = self.compositor` arm, so with no compositor the event falls through to 682 (`Property::DELETE` guard) and 686 (name/hints). Neither atom matches, so the fall-through is a no-op. Behaviour-identical, but the control flow differs by feature. |
| E7 | → | `events.rs:705-715` `c.selection_owner_changed(...)`; on `false` → `compositor.take()`, `disable()`, `apply_root_wallpaper()` | **A** — an external compositor taking `_NET_WM_CM_S0` demotes Maverick to the X11 path. No WM state change. |
| E8 | → | `events.rs:723-725` `c.on_shape(win, shaped)` | **F** |
| E9 | → | `events.rs:733-735` `c.on_create(win)` | **A** |
| E10 | → | `events.rs:743-745` `c.on_map(win)` | **A** |
| E11 | → | `events.rs:754-756` `c.on_damage(drawable, &e)` | **F** |
| E3b | ← | `on_configure` → `compositor_gl.rs:2065` `self.mark_full(DirtyReason::GEOMETRY)` on **every** WM-issued configure | **F** — a WM geometry write forces a compositor *full* repaint. This is the WM→compositor invalidation path. The synthetic `SendEvent` echo is filtered at `events.rs:313-315` (`response_type & 0x80`), so only the *server's* real echo reaches `on_configure`. |

### 3.6 `backend::x11::manage` / `teardown` / `actions` / `pointer`

| # | edge | site | class |
|---|---|---|---|
| M1 | → | `manage.rs:489-490` `compositor.on_border_color(win, cfg.col_normal)` at map time | **F** |
| M2 | ⇄ | `manage.rs:1263-1275` `set_fullscreen` writes `_NET_WM_BYPASS_COMPOSITOR = 2` (or deletes it) **unconditionally** — the comment says it is for *external* compositors (picom). | **B/D** — see the loop note below. |
| M3 | → | `actions.rs:183-185` `comp.set_wallpaper(&self.engine.state.wallpaper)` on `Effect::SetWallpaper` | **F** |
| M4 | → | `actions.rs:430-455` on config reload: `compositor::Compositor::init(...)` / `compositor.take()` + `disable()` / `set_wallpaper` | **A** — hot-plugging the whole compositor at runtime. |
| M5 | → | `actions.rs:464` `apply_root_wallpaper()` in the no-compositor branch of the reload | **F** |
| M6 | → | `actions.rs:67-74`, `103-110` `needs_frame()` / `dirty_reasons_bits()` — read **only** for the `MAV_FLOAT_TRACE` log at 111-119 | **F** (diagnostic) |
| M7 | → | `actions.rs:351-352` `compositor.is_some()` / `compositor_backend_name(&cfg)` for the IPC `inspect` document (`core/ipc.rs:154-159`, `223-231`) | **F** (observability) |
| M8 | — | `teardown.rs` — no compositor reference except doc text (5, 121, 142). `mod.rs:599-601` `compositor.disable()` and `mod.rs:648-652` `abandon_compositor()` | **A** |
| M9 | — | `pointer.rs` — **no compositor reference at all.** Drag uses `client.geom` (`pointer.rs:399`), which the compositor never writes. | **A** |

**M2 loop (a genuine cross-edge).** The WM writes
`_NET_WM_BYPASS_COMPOSITOR=2` on every fullscreen window
(`manage.rs:1264-1270`). That same atom is read back into `client.bypass_hint`
by the WM's own property handler (`events.rs:624-639`, read at 629-630) and
then fed to Maverick's **own** composition policy
(`compositor_policy.rs:131-135`, which vetoes only on `Some(1)`). So the WM's
EWMH output, written for picom's benefit, becomes an input to Maverick's own
bypass decision — a WM output looped back into compositor policy. Consequence:
with `fullscreen_bypass` on, a fullscreen window is bypassed from Maverick's
own compositor partly because Maverick told it to. Harmless (the value is
semantically "force bypass", which the policy would have chosen anyway), but it
is real entanglement and it is undocumented at the write site.

### 3.7 `mod.rs` — the event loop and startup

| # | edge | site | class |
|---|---|---|---|
| L1 | → | `mod.rs:1192-1208` `Compositor::init(conn, dpy, root, screen_num, check_win, &cfg)` at startup | **A** — returns `None` on any failure, WM continues. |
| L2 | → | `mod.rs:1242-1246` `comp.set_wallpaper(...)` | **F** |
| L3 | → | `mod.rs:1320` `wm.apply_root_wallpaper()` — the no-compositor branch (it self-guards at `rootwall.rs:36`) | **F** |
| L4 | ⇄ | `mod.rs:747-774` per-monitor `comp.engage_bypass/disengage_bypass/disengage_all_bypass` driven by `compositor_policy::mode_for` + `bypass_candidate` | **A** — the policy is a pure function of `cfg` + `state` (`compositor_policy.rs:78-80`). |
| L5 | ⇄ | `mod.rs:791-802` `if anim_enabled { for sub in compositor::substep_bounds(dt) { state.tick_animations_multi(sub, &mut anim_per_mon) } } else { state.snap_animations() }` | **B** — see §3.8. |
| L6 | ⇄ | `mod.rs:807-809` `if framesched::needs_endpoint_frame(was_animating, anim) { comp.invalidate(); }` | **F** (one extra compositor frame). Comment at 803-806 names the artifact: *"the GPU could retain the previous (up to 0.5 px) transform indefinitely."* |
| L7 | ⇄ | `mod.rs:834` `comp.tick_wallpaper(dt)` — shares the WM's clamped `dt` | **F** |
| L8 | ⇄ | `mod.rs:840-844` `FrameScheduler::from_compositor(self.animating \|\| comp.presentation_animating(), comp.wallpaper_animating(), comp.dirty_reasons())` | **B** — compositor-owned per-window spring state (`compositor_gl.rs:349`, `2437-2441`) sets the loop's `Animation` bit, which sets `timeout_ms()` (`framesched.rs:248-250`) and therefore whether the WM loop blocks. |
| L9 | ⇄ | `mod.rs:881-887` `comp.prepare_frame(&mut self.engine.state, …)` | **D** (C5) |
| L10 | ⇄ | `mod.rs:893` `comp.render()`; `896-905` on `false` → `disable()`, `compositor = None`, `apply_root_wallpaper()` | **A** |
| L11 | ⇄ | `mod.rs:935-941` `sched.after_present(self.animating \|\| comp.presentation_animating())` | **B** (L8) |
| L12 | ⇄ | `mod.rs:942-951` `let vsync_on = comp.vsync_active(); self.animation_due = if sched.is_continuous() && framesched::should_wait_after_swap(vsync_on) { now + frame_period } else { None };` | **B** — see §3.8. |
| L13 | ⇄ | `mod.rs:864-877` `comp.set_debug_floats(&fids)` under `comp.float_trace` | **F** |
| L14 | ⇄ | `mod.rs:1085-1090` IPC `compositor_requested` / `compositor_actual` | **F** |
| L15 | → | `mod.rs:599-601` `compositor.disable()` (clean shutdown); `648-652` `abandon_compositor()` → `c.abandon()` (dead-server path) | **A** |

**`layout_dirty` reader audit (for C6/C7).** `grep -rn 'layout_dirty' src/ maverick-core/ --include=*.rs | grep -v tests`:

* **Writers:** `compositor_gl.rs:2315` (true), `compositor_gl.rs:2411` (false), `render.rs:675` (true), `render.rs:1062` (true), `maverick-core/src/types.rs:1347` (init true).
* **Readers:** `actions.rs:57`, `59`, `62` and `actions.rs:102` — and all four are consumed **only** by the `log::info!` at `actions.rs:111-119`, which is itself gated on `comp.float_trace`.
* **No reader in `maverick-core`, `core::layout`, `core::commands`, `core::engine`, or `core::invariants`.**

⇒ `Monitor::layout_dirty` (`maverick-core/src/types.rs:1321`) is a
**write-only field in the current tree**. The compositor's two writes
(compositor_gl.rs:2315,2411) are therefore harmless *today*, but they are the
only place in the codebase where compositor code mutates `State`, and they
would become a correctness bug the moment any reader is added that runs after
`prepare_frame` in the same turn (e.g. an incremental layout cache inside
`arrange`). Report as **D (latent)**.

### 3.8 The two B-edges that touch WM state, stated precisely

**B1 — the compositor is the sole driver of WM animation state.**
`src/backend/x11/mod.rs:739-927`. Inside `if let Some(comp) = self.compositor.as_mut()`:

```
789  let anim_enabled = crate::config::animations_enabled(&self.engine.cfg);
791  if anim_enabled {
792      for sub in compositor::substep_bounds(dt) {
793-796      anim |= self.engine.state.tick_animations_multi(sub, &mut self.anim_per_mon);
         }
     } else { snap_animations(); anim_per_mon.fill(false); }
802  self.animating = anim;
```

`else` branch (`mod.rs:915-926`): `self.engine.state.snap_animations();
self.anim_per_mon.fill(false); self.animating = false;`.

So `Workspace::camera.{position,velocity}`, `Column::boost`, `Workspace::zoom`,
`Workspace::page_zoom` (`maverick-core/src/types.rs:2582-2630`) advance **only**
when `self.compositor.is_some()`. The compositor does not decide the values —
`tick_animations_multi` is pure core code — but it decides *whether they exist*.
This is invisible without a compositor only because the no-compositor path also
snaps the geometry, so there is nothing to animate. It is nonetheless the single
strongest WM-state coupling in the tree, and it means **the camera spring's
numerical behaviour is specified by a function that lives in the compositor
module** (C1/D).

**B2 — GLX swap-interval decides the WM loop's sleep, hence the animation `dt`.**
`mod.rs:942-951` reads `compositor::Compositor::vsync_active`
(`compositor_gl.rs:1148-1150`, returning `self.renderer.vsync`, set from
`glXSwapIntervalEXT/MESA/SGI` at `maverick-gl/src/renderer.rs:2124-2182`).
`framesched::should_wait_after_swap(vsync_on) = !vsync_on` (`framesched.rs:100-102`).
With vsync on, `animation_due = None` and `sched.timeout_ms()` is `Some(0)`
(`framesched.rs:248-250`) while the animation bit is set, so `mod.rs:987`
(`if timeout != Some(ZERO)`) skips the `poll` entirely and the loop is paced
only by the blocking `glXSwapBuffers` inside `Renderer::end_frame`
(`maverick-gl/src/renderer.rs:1349-1356`). The `dt` fed to every WM spring is
then the *present-to-present* interval (`mod.rs:715-725`, comment 906-913).
If the driver silently ignores the interval, `vsync` is `false`
(`renderer.rs:2181`) and the loop switches to a software `frame_period`
deadline — a different `dt` distribution, i.e. a different animation.
**WM animation timing is load-bearing on compositor-owned hardware state.**

---

## 4. Geometry ownership

### 4.1 With the compositor ON

| rectangle | owner | written by | read by |
|---|---|---|---|
| `Client::geom` (model) | WM | `render.rs:1054` `c.geom = wire;` (only when `write_client_geom`) | hit-test (`pointer.rs:399`), float normalisation (`layout.rs:700-703`), `covers_screen` (`compositor_policy.rs:194`) |
| `AppliedState.windows[win]` (wire) | WM reconciler | `reconciler.rs:149-151` | `reconciler.rs:146-147` diff, `events.rs:351-352` echo classification |
| the actual X window rectangle | **WM only** | `render.rs:1000-1008` `configure_window(win, x, y, w, h, border_width)` | X server |
| `CompWin.outer` | compositor, **from X** | `compositor_gl.rs:466-473` `observe_configure` (via `events.rs:377-384`), `1650-1655` at track | `current_visual_rect` fallback (635), culling (2981), occluder (2992), bypass hole (2761) |
| `CompWin.transform` (int) | compositor | `compositor_gl.rs:511-516` (+ `566-571` during a spring) | damage (3152-3163), occlusion (2970-2992), cull (3172), `_TRANSFORM_` trace |
| `CompWin.visual_transform` (float) | compositor | `compositor_gl.rs:594-599` | **the GPU draw** (`3194-3196` `visual_draw_dst(visual)`) |
| `CompWin.prev_visual` / `prev_visual_f` (int+float) | compositor | `compositor_gl.rs:3165-3167` | animation damage (3152-3154) |
| `CompWin.presentation_value/_target/_goal` (5×f64) | compositor | `compositor_gl.rs:559,588,2355-2374` | the spring (560-565) |
| `visual_x_cache[win]` (f32) | compositor | `compositor_gl.rs:2420-2426` → `4139-4173` | `set_transforms` `2203` → `set_transform_with_visual` `522-524` |
| overlay BOUNDING shape | compositor | `compositor_gl.rs:2757-2785` | X server (its own window) |

**The compositor never computes or overrides a window rectangle that X11 sees.**
It computes a *drawing* rectangle, and the X rectangle is written once by
`emit_geometry`.

### 4.2 With the compositor OFF (`--no-default-features`, or `[compositor] enabled=false`, or GL init failure, or `_NET_WM_CM_S0` stolen)

The first four rows are unchanged. The `CompWin.*`, `visual_x_cache` and
overlay-shape rows do not exist (the stub is zero-field,
`compositor.rs:36-38`). Two visible differences:

1. **Rounded corners move from GL to XShape.** `render.rs:1071` flips
   `sync_rounded_frame` from "skip" to "upload a `ShapeRectangles` mask"
   (`rounded_rectangles`, `render.rs:115-130`). Same pixels, different protocol.
2. **The wallpaper moves from the overlay to the root pixmap**
   (`rootwall.rs:36-37` early-return vs `apply_root_wallpaper`).

Neither touches geometry, focus, stacking or lifecycle.

### 4.3 Can the WM and the compositor disagree? Yes — permanently, by design

Three standing disagreements, all in the compositor-on configuration:

* **During a camera scroll**, X11 holds the **settled** rect
  (`render.rs:597` `Phase::Settled`) while the compositor draws the **live**
  rect (`compositor_gl.rs:4189` `Phase::Live`). The two differ for the whole
  scroll. This is the stated design (`compositor_gl.rs:3-8`; `mod.rs:783-784`
  *"The WM's settled geometry was already written by whichever action triggered
  the change, so no per-frame `ConfigureWindow` storm"*). The X window really
  does sit at the destination while its image is drawn somewhere else.
* **The camera settles 0.5 px early.** `CAMERA_SETTLE_POSITION = 0.5`
  (`maverick-core/src/types.rs:483`) and `CAMERA_SETTLE_VELOCITY = 0.01` (484)
  make `Camera::needs_update` (536-542) return false, after which `step` calls
  `snap(target)` (573-576, 633-637). The final live X can therefore be up to
  0.5 px from the settled integer rect. Resolved by exactly one extra
  compositor frame: `framesched::needs_endpoint_frame` (`framesched.rs:106-108`)
  at `mod.rs:807-809`, whose comment (803-806) names the artifact verbatim.
* **Any window not in this frame's placements** falls back to its X geometry:
  `current_visual_rect` (`compositor_gl.rs:631-637`) returns
  `VisualRect::from_rect(self.outer)` when `transform_gen != gen`. That covers
  override-redirect menus (`track` records them, `compositor_gl.rs:1660`) and
  dock/bar windows (not in `state.clients`, so never in placements). Both are
  positioned by the WM, so this is correct — but it means the compositor has
  two independent draw-position sources.

**Is there a reconcile round-trip (WM state ↔ compositor state)?** No, not a
real one. There is a **one-way push**: the WM hands `State` to the compositor
(`mod.rs:881-887`), and the compositor writes back only `layout_dirty`
(`compositor_gl.rs:2315, 2411`) — a field with no readers (§3.7). The
authoritative round-trip in the tree is `DesiredState → AppliedState → X`, and
it lives entirely in `render.rs` / `reconciler.rs` with **no compositor
participation**.

---

## 5. Duplicate geometry / state models

**Six rectangles per window, four of them compositor-owned, and one of those is
a float re-derivation of a core integer projection.** Per §4.1.

The most consequential duplication is `visual_x_cache`:

```
compositor_gl.rs:4139  fn refresh_visual_x_cache(state, cfg, out, extents, scratch)
compositor_gl.rs:4154      column_screen_extents_into(ws, cfg, mon.workarea, &fs, extents, scratch)
layout.rs:750              let l = g.wa.x as f32 + (x - ws.camera.position) * g.alpha + g.cx;
layout.rs:649              let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
```

The compositor recomputes the exact expression the core just rounded, in `f32`,
purely so it can draw between the integers. Coverage is partial and asymmetric:

* X axis: fractional. **Covered** (`compositor_gl.rs:522-524`).
* Y axis: **never** fractional — `compositor_gl.rs:527` `self.transform.y as f64`.
* Tiled columns of the active workspace: covered.
* **Floats: not covered** — `refresh_visual_x_cache` iterates
  `ws.columns[].windows` only (`compositor_gl.rs:4155,4159`); `ws.floats` is
  never visited.
* Maximized / fullscreen-overlay / `presented_maximize` windows: explicitly
  skipped (`compositor_gl.rs:4163-4168`).
* `LayoutKind::Column` only (`compositor_gl.rs:4150`). `LayoutKind` has exactly
  one variant today (`maverick-core/src/types.rs:1506-1509`), so that guard is
  currently free.

A **second** duplication: `compositor_gl.rs:1130-1137` keeps
`live_cache` (per-monitor integer placements), `settled_cache` (per-monitor
settled placements), `cam_cache` (last camera value), and `proj_cache` (layout
signature) — a four-way mirror of core layout state inside the compositor,
rebuilt under `live_projection_needs_rebuild` (`compositor_gl.rs:128-130`).
Its own comment (`compositor_gl.rs:2383-2388`) states the reason:
*"A cache translated by `round(dx)` is not a valid substitute: it discards the
fractional part of a subpixel camera motion and turns a smooth visual state
into one-pixel jumps."*

Also duplicated: the draw stack. `self.stack: Vec<Window>`
(`compositor_gl.rs:1033-1039`) is maintained from `QueryTree` + the
`SubstructureNotify` stream and is completely independent of the WM's
`last_stack_order` (`render.rs:908-917`). Two stacks, two authorities: X order
from the WM, GL draw order from the compositor. They agree only because both
observe the same server.

---

## 6. Movement / presentation symptom — mechanism

**Reported:** movement appears to happen in ~1-pixel increments, or appears
visually synchronized only *after* X11 catches up.

Both halves are real and have distinct causes. **The compositor partially
masks one of them and is the direct cause of the other.**

### 6.1 "~1-pixel increments" — cause: integer rounding at the X11 boundary, incompletely compensated

**Cause (core, not compositor).** The layout projection rounds both axes to
`i32` at the point of the X11 request, deliberately
(`src/core/layout.rs:645-648`: *"Round here, at the integer X11 boundary, and
nowhere earlier"*):

```
layout.rs:580   let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
layout.rs:584   let screen_y = (screen.y as f32 + screen.h as f32 * (1.0 - alpha) / 2.0).round() as i32;
layout.rs:649   let screen_col_x = (wa.x as f32 + (world_x - cam) * alpha + cx).round() as i32;
layout.rs:664   let screen_y = (wa.y as f32 + (row_y_world - wa.y as f32) * alpha + cy).round() as i32;
```

**The compositor's compensation (E).** `visual_x_cache` → `visual_x`
(`compositor_gl.rs:2203`) → `set_transform_with_visual` (`498-601`) keeps X in
`f32` for the draw:

```
compositor_gl.rs:522-524   let live_x = visual_x.filter(|x| x.is_finite()).unwrap_or(self.transform.x as f32) as f64;
compositor_gl.rs:525-531   let live = [live_x, self.transform.y as f64, self.transform.w as f64, self.transform.h as f64, self.transform_radius as f64];
compositor_gl.rs:594-599   self.visual_transform = VisualRect { x: presentation_value[0] as f32, y: …, … };
compositor_gl.rs:3194-3196 let q = DrawQuad { dst: visual_draw_dst(visual), … };
```

and the vertex shader keeps the fraction
(`maverick-gl/src/renderer.rs:73-80`: `vec2 p = mix(u_dst.xy, u_dst.zw, a_pos)`,
`glUniform4f` at `renderer.rs:1332`).

**Where the compensation stops, and movement becomes 1-px-stepped:**

1. **All vertical motion** — `compositor_gl.rs:527` uses
   `self.transform.y as f64`, an `i32`. Overview zoom (`alpha < 1` moves `cy`),
   the fullscreen column's vertical centring (`layout.rs:583-584`), and any
   `row_y_world` fraction all quantise to whole pixels **while the same
   animation's horizontal motion is smooth.** A zoom-to-overview therefore
   looks smooth in X and stair-stepped in Y.
2. **All floating-window motion** — `refresh_visual_x_cache` never visits
   `ws.floats` (`compositor_gl.rs:4155,4159`). A float drag re-`ConfigureWindow`s
   per motion event anyway (`pointer.rs` → `apply_geom`), so a float is both
   X11-quantised *and* has no fractional override: it moves in whole pixels.
3. **Maximize / fullscreen-overlay / presented-maximize** — skipped at
   `compositor_gl.rs:4163-4168`; their transition is the presentation spring,
   which is float during flight but terminates on the integer goal
   (`compositor_gl.rs:575-586`).
4. **Every settled frame** — the camera snaps 0.5 px early
   (`maverick-core/src/types.rs:483`), so the last visual step is a fraction of
   a pixel short of the X destination, corrected by one extra frame
   (`mod.rs:803-809`).

**Assessment: masked, partially.** For tiled-window camera scroll the
compositor *completely* masks the integer X11 quantization by keeping a
fractional draw position; the regression test for exactly this is
`compositor_gl.rs:4945-4967` (`visual_x_projection_preserves_fraction_and_is_not_accumulated`)
and `4930-4942` (`camera_motion_never_reuses_a_translated_integer_cache`).
For Y, floats, and the presentation-spring endpoints it does **not** mask —
those are 1-px-stepped today, with the compositor as the drawing authority.

### 6.2 "visually synchronized only after X11 catches up" — cause: the X window genuinely does not move

With the compositor on, a camera scroll issues **no** `ConfigureWindow`:

* The only per-arrange geometry path is
  `render.rs:597-653` `arrange_full` → `arrange_full_phase(…, Phase::Settled)`
  → `reconcile` → `emit_geometry`, and `AppliedState::diff`
  (`reconciler.rs:130-156`) emits nothing when the settled rect is unchanged.
* `arrange_full_phase` is called **only** with `Phase::Settled`
  (`render.rs:597`). `Phase::Live` on the WM side is dead: the only other
  `Phase::Live` call sites are `compositor_gl.rs:4189` and test helpers
  (`render.rs:2944`, `bench_arrange.rs`, `core/framebench.rs`,
  `core/invariants.rs`). The doc comment at `render.rs:600-603` describing
  "`Live` (the X11-only animation path, per-frame)" is **stale** — that path no
  longer exists.
* `mod.rs:783-784` states it: *"The WM's settled geometry was already written
  by whichever action triggered the change, so no per-frame `ConfigureWindow`
  storm."*

So: the server holds the window at the **destination** for the whole scroll
while the compositor slides a **stale-sized** pixmap across the screen. What
you see is a composited image of a window that, on the server, is somewhere
else. When the camera settles, `arrange` fires once (via
`Effect::ArrangeMonitor`), X11 actually moves the window, the client repaints,
`XDamage` fires (`compositor_gl.rs:2073-2128`) and the image becomes
"correct". That is precisely the reported "visually synchronized only after X11
catches up".

**Assessment: caused by the compositor's design, not masked by it.** The
compositor is the thing that makes the motion smooth *and* the thing that makes
it a lie about where the window is. Any client that reads its own position
(`XGetGeometry`, `_NET_FRAME_EXTENTS` consumers, drag-to-border logic, or a
tooltip that follows the cursor) sees the settled position, not the drawn one.

### 6.3 Mechanisms explicitly checked and ruled out

| candidate | verdict | evidence |
|---|---|---|
| sub-pixel animation stepping | **ruled out for the camera** | `Camera` keeps an `f64` continuation `x`/`v` alongside the published `f32` pair (`maverick-core/src/types.rs:505-506, 624-627`), with the exact-match `analytic_state` handoff (666-673) and a comment (581-585) naming the quantization floor it removes. The presentation spring is a second `Camera` (`compositor_gl.rs:375-377`). |
| per-frame `ConfigureWindow` echoing | **ruled out** (compositor-on) | §6.2. `render.rs:1071-1100` also *caches* the XShape mask key so a pure move does not re-upload it (comment 1086-1094). |
| `Present`/vblank gate on WM state | **partially confirmed** | `glXSwapBuffers` in `Renderer::end_frame` (`maverick-gl/src/renderer.rs:1349-1356`) is the sole synchroniser; `Compositor::wait_vblank` exists but is `#[allow(dead_code)]` and never called (`compositor_gl.rs:2451-2454`). With vsync on it is the only thing pacing the loop (§3.8 B2). |
| damage / buffer-age round-trips as a quantiser | **ruled out** | `DamageRegion` is additive and bounded (`compositor_gl.rs:704-727`); `plan_aged_damage` (840-863) widens the region, never narrows it. Neither feeds a coordinate. |
| throttling/coalescing in `framesched.rs` | **affects rate, not position** | `clamp_frame_dt` (113-119) bounds the idle→animating `dt` to `ONE_REFRESH` (1/60 s) and an in-flight one to 1.0 s. This changes *how far* the spring moves per frame, never a position directly. It is a plausible source of a *visible jump on the first frame after idle* at high refresh rates, but not of 1-px stepping. |
| compositor redraw timing used as a WM clock | **confirmed, by design** | `mod.rs:715-725` and the comment at 906-913: `dt` is explicitly the present-to-present interval because the GLX swap is most of the frame. `compositor_gl.rs:2290-2292` refuses to let `last_present` become a second clock. |

---

## 7. GL sync / XDamage / buffer-age / TFP inventory

| mechanism | implemented at | used for | load-bearing for WM state? |
|---|---|---|---|
| **CM selection `_NET_WM_CM_S<n>`** | `compositor_gl.rs:1302-1306`, `intern_cm_atom` 4117 | ownership handoff | **No.** Losing it demotes the compositor only (`events.rs:705-715`). |
| **`Composite` MANUAL subwindow redirect** | `compositor_gl.rs:1307-1310` | off-screen storage without auto-composite | **No.** |
| **`CompositeGetOverlayWindow`** | `compositor_gl.rs:1326-1342` | the only drawable | **No** (init fails cleanly if unavailable). |
| **Overlay empty INPUT shape (XFixes)** | `set_empty_input_region` 4102-4115, called 1345 | pointer pass-through | **Yes, for input** — but a failure aborts init (`return None` at 1352), so the WM never runs with a swallowing overlay. |
| **Overlay BOUNDING shape (XFixes, bypass holes)** | `update_overlay_shape` 2757-2785 | punch the overlay transparent over a bypassed window | **No** (visual). |
| **TFP `GLX_EXT_texture_from_pixmap`** | `rename_and_bind` 3849-3948 (`NameWindowPixmap` 3886, `glXCreatePixmap`/`glXBindTexImageEXT` after), `Renderer::texture_from_pixmap` `maverick-gl/src/renderer.rs:1388`, `bind` 1523 | zero-copy pixmap → texture | **No** (visual). A bind failure bypasses the window (`compositor_gl.rs:3042-3045`, `3123-3127`). |
| **`XDamage` (ReportLevel::NON_EMPTY)** | `damage_create` 1685 / 2838; `on_damage` 2073-2129; `DamageSubtract`+`XFixesFetchRegion` 2097-2102 | client-repaint regions → partial redraw | **No** (visual). |
| **`GLX_EXT_buffer_age`** | `has_buffer_age` `compositor_gl.rs:270-274` ← `maverick-gl/src/renderer.rs:776,974`; `back_buffer_age` `renderer.rs:1201`; consumed 3300-3314; `plan_aged_damage` 840-863; `damage_history` 3501-3513 | partial redraw | **No** (visual). Debug override `MAVERICK_FORCE_FULL_REDRAW` at 1099. |
| **Scissor + partial clear** | `renderer.rs:1224-1248` (`scissor_box` 653-666), used `compositor_gl.rs:3339-3354` | bounded repaint | **No** (visual). |
| **`XSync` fence** | `sync_initialize`/`sync_create_fence` 1357-1369; `sync_trigger_fence` 3258; `sync_await_fence`+`sync_reset_fence` 3272-3273 | order client rendering before sampling | **No.** A failure *disables* the compositor (`render()` returns `false` at 3276 → `mod.rs:896-905`). |
| **Swap interval / vsync** | `set_swap_interval` `maverick-gl/src/renderer.rs:2124-2182`; consumed `compositor_gl.rs:1148-1150` | the loop's pacing | **YES** — see §3.8 B2. Determines the loop's wait timeout (`framesched.rs:100-102` → `mod.rs:946-951`) and therefore the `dt` fed to every WM spring. |
| **`ShapeSelectInput` (per client)** | `compositor_gl.rs:1661` | subscribe to `ShapeNotify` | **No** (conservative-occluder flag only). |
| **Occlusion culling** | `compute_scene` 2952-2994, `fully_covered_by` 897-899 | skip fully-hidden draws | **No.** Conservative: disabled whenever any edge is fractional (2982-2990) or the window is translucent/shaped (`can_occlude` 627-629). |
| **Rounded-corner SDF (fragment shader)** | `maverick-gl/src/renderer.rs:98-110`, radius from `rounded_radius_for` `compositor_gl.rs:406-412` | corner rounding | **No.** Mirrors the XShape policy exactly (`render.rs:1077-1081` sets radius 0 in the same three cases). |
| **`glXSwapBuffers` (present)** | `maverick-gl/src/renderer.rs:1349-1356` | vblank block + GL error check | **YES**, indirectly: `end_frame`'s return value is the compositor's `render()` result, and its *timing* is the loop's clock. |

**None of the damage/buffer-age/TFP machinery is load-bearing for WM state.**
The only compositor state that is load-bearing for WM behaviour is (a) the
presence of a compositor, which selects the animation branch (§3.8 B1), and
(b) the actual vsync state, which selects the loop's pacing (§3.8 B2).

---

## 8. Behaviour under `--no-default-features`

`default = ["compositor-opengl"]`; `compositor-opengl = ["dep:maverick-gl",
"x11rb/composite", "x11rb/damage", "x11rb/xfixes"]` (`Cargo.toml`).

**Nothing becomes `unimplemented!`/`todo!`/`unreachable!`.** Verified: `grep -rn
'todo!|unimplemented!|unreachable!|panic!(' src/ --include=*.rs` returns only
test-only panics plus `src/core/ipc.rs:508,669` and `src/core/action.rs:608,615`
(IPC test helpers). The stub is total: `Compositor::init` returns `None`
(`compositor.rs:41-49`) and all 30 methods are `#[inline(always)]` no-ops
(`compositor.rs:51-181`).

**What is lost:**

| lost | mechanism | severity |
|---|---|---|
| GL/XDamage/Shape event arms | `mod.rs:416-421`, `events.rs:65,700,718,749` | none — the events are not generated |
| `WallpaperGpu` trait + re-export | `core/wallpaper.rs:29-42`, `core/mod.rs:85` | none — `Effect::SetWallpaper` becomes a no-op (`actions.rs:183-185`) and `rootwall.rs` takes over |
| rounded corners via GL | `render.rs:1071` flips to the XShape path | **visual only** |
| animated / native wallpaper | `compositor_gl.rs:2491-2616` | **visual only** |
| fractional draw position | `visual_x_cache` | **visual only** (there is no drawing) |
| **all animation** | `mod.rs:915-926` `snap_animations()` every turn | WM `State` differs (`camera.position == camera.target` always), but nothing consumes it. `settled_cache`/`live_cache` are never built, so `Phase::Live` is never used. |
| **1557 lines of compositor unit tests** | `compositor_gl.rs:4207-5763` | test-coverage loss only |
| **59 lines of placeholder tests** | `compositor.rs:237-295` | present in this build |
| `substep_bounds` is duplicated | `compositor.rs:201-212` vs `compositor_gl.rs:4196-4205` | maintenance hazard, not a behaviour change — the comment at 201-205 requires exact numerical equality |

**Dead items in the no-compositor build** (present, `pub`, never called):
`compositor.rs:215-224` `live_placements` (no-op body) and
`compositor.rs:226-230` `placeholder::FrameScheduler` — `mod.rs:85` imports the
real `framesched::FrameScheduler` explicitly, so the glob
`pub use placeholder::*` (`compositor.rs:234-235`) never shadows it.

**Are any lost paths reachable from a WM *correctness* decision? No.** With the
compositor compiled out, the WM's correctness surface is:
`Engine::dispatch` → `Effect` → `actions::execute` → `render` / `reconciler` /
`manage` / `events`, none of which reference `compositor` except through
`Option::is_some()` guards for visual side effects. The single non-visual
difference, `mod.rs:915-926`, makes the WM *more* deterministic, not less.

---

## 9. Code that exists solely for the compositor — candidate deletion set

Ordered by confidence. Nothing here was deleted; this is the measurement only.

| # | path | lines | confidence | note |
|---|---|---|---|---|
| 1 | `src/backend/x11/compositor_gl.rs` | **5763** (1557 test) | **certain** | the whole file is `#[cfg]`-gated out |
| 2 | `maverick-gl/src/{lib,dl,gl,glx,renderer}.rs` | **5043** | **certain** | only importer is `compositor_gl.rs` + the trait signature in `core/wallpaper.rs` |
| 3 | `maverick-gl/tests/{loader,props,shared_bootstrap}.rs` | **740** | **certain** | |
| 4 | `maverick-vk/src/*.rs` | **2190** | **certain** | **already dead** — no crate depends on it; `compositor-vulkan = []` has zero users |
| 5 | `maverick-vk/tests/*.rs` | **1299** | **certain** | |
| 6 | `maverick-render/src/lib.rs` + `tests/contract_types.rs` | **405** | **certain** | **already dead** — no `Renderer`/`Texture` implementation exists; `src/backend/renderer.rs` is its only importer |
| 7 | `src/backend/renderer.rs` | **29** | **certain** | pure re-export, `#[allow(unused_imports)]`, no importers |
| 8 | `src/backend/x11/compositor.rs` | **295** | **partial** | the 199-line placeholder could be replaced by an `Option`-free stub, but the 196-line `no-default-features` build still needs the type to exist. Not a clean delete. |
| 9 | `src/compositor_policy.rs` | **970** (204 prod + 766 test) | **partial** | only caller is `mod.rs:753-770` inside the compositor arm, so the 204 production lines go with the compositor; the 766 test lines are pure and would go too. |
| 10 | `src/core/wallpaper.rs:29-42` + `src/core/mod.rs:85-86` | **14 + 2** | certain | the `WallpaperGpu` trait is `compositor-opengl`-only and has one implementor |
| 11 | `src/backend/x11/trace.rs` | **875** | **low** | compiled and called unconditionally, but ~90% of its call sites are inside the compositor arm of `run_once`. It also serves `geometry_applied` (`render.rs:1010`) and `geometry_flush_returned` (`mod.rs:690`), so it would need trimming, not deleting. |
| 12 | `x11rb` features `composite`, `damage`, `xfixes` | 3 | certain | `Cargo.toml` `compositor-opengl` line only |
| — | **NOT deletable:** `src/backend/x11/rootwall.rs` (231) — it is the fallback path; `src/backend/x11/framesched.rs` (795) — used in both configurations; `src/core/{layout,present}.rs` — pure core, feature-independent. |

**Arithmetic.** Certain + already-dead:
5763 + 5043 + 740 + 2190 + 1299 + 405 + 29 + 16 = **15 485 lines**
(of which 1557 + 740 + 1299 + 141 = 3737 are tests).
With the partial items (8, 9) included: **16 750 lines**.
Against a workspace total of ~86 400 lines (86 434 by `wc -l` over all
`*.rs`, excluding `target/`), that is **~18% of the tree, ~19% of
non-test code in the listed paths**.

Against the WM binary crate alone (`src/`, 43 files): items 1, 7, 8, 9, 10
= 5763 + 29 + 295 + 970 + 16 = **7073 lines** of the 22 000-ish in `src/`
excluding `core/tests.rs` (10 875).

---

## 10. Evidence appendix

Commands run (read-only):

```
$ grep -rn 'cfg(feature = "compositor-opengl")\|cfg(feature = "compositor-vulkan")\|…' src/ --include=*.rs
   → 15 hits, all tabulated in §2.1

$ grep -rno 'cfg(feature = "[a-z-]*")' src/ --include=*.rs | sed … | sort | uniq -c
   → 17 input-trace, 12 window-trace, 11 compositor-opengl, 1 compositor-vulkan

$ grep -rn 'compositor' src/ --include=*.rs | grep -v compositor_gl.rs | grep -v core/tests.rs
   → the complete §3.1–3.8 call-site list (184 hits, 0 in reconciler.rs, 0 in input.rs, 0 in pointer.rs)

$ grep -n 'compositor' src/backend/x11/reconciler.rs      → (no matches)
$ grep -n 'compositor|damage|xfixes' src/backend/x11/input.rs   → only event_mask/randr
$ grep -n 'compositor' src/backend/x11/pointer.rs          → (no matches)

$ grep -rn 'layout_dirty' src/ maverick-core/ --include=*.rs | grep -v tests
   → writers: compositor_gl.rs:2315,2411; render.rs:675,1062; types.rs:1347
   → readers: actions.rs:57,59,62,102 (all trace-only, gated on float_trace)
   → NO reader in maverick-core or core::layout/commands/engine/invariants

$ grep -rn 'arrange_full_phase\|Phase::Live' src/ --include=*.rs | grep -v core/tests.rs
   → the only production Phase::Live call is compositor_gl.rs:4189;
     render.rs:604 (fn) is invoked only as Phase::Settled at render.rs:597

$ grep -rn 'live_placements' src/ --include=*.rs | grep -v tests
   → defined at compositor_gl.rs:4179 and the no-op stub at compositor.rs:215;
     only production call is compositor_gl.rs:2398

$ grep -rn 'maverick-vk|maverick_vk' --include=Cargo.toml --include=*.rs .
   → Cargo.toml:10 (workspace member), maverick-render/src/lib.rs:49 (comment),
     maverick-sys/src/lib.rs:9 (comment), tests/no_wait_in_wm.rs:63 (scan list)
   → no dependency edge

$ grep -rn 'maverick_render' --include=*.rs .
   → src/backend/renderer.rs:26 (the only use), plus doc comments

$ grep -rn 'todo!|unimplemented!|unreachable!|panic!(' src/ --include=*.rs
   → no todo!/unimplemented!/unreachable! anywhere; all panics are in tests

$ grep -n 'cfg!(feature' src/config.rs   → 543, 554, 567

$ grep -n 'camera' src/core/layout.rs | grep -n 'round()'
   → 580, 584, 649, 664 (the four integer boundaries)

$ wc -l <each candidate path>   → the §9 table
```

Key code excerpts quoted above:

* WM→compositor animation gate — `src/backend/x11/mod.rs:789-802` vs `915-926`
* vsync→pacing gate — `src/backend/x11/mod.rs:942-951` +
  `src/backend/x11/framesched.rs:100-102`
* compositor writes into WM State — `src/backend/x11/compositor_gl.rs:2311-2317`,
  `2381-2411`
* integer layout rounding — `src/core/layout.rs:645-664`
* fractional compensation and its gaps — `src/backend/x11/compositor_gl.rs:4139-4173`,
  `2203`, `522-531`
* compositor never configures a client — `src/backend/x11/compositor_gl.rs:1661`,
  `1685`, `2792`, `2816`, `3886`
* only X11-request divergence — `src/backend/x11/render.rs:1070-1101`
* EWMH bypass loop — `src/backend/x11/manage.rs:1259-1275` →
  `src/backend/x11/events.rs:624-639` → `src/compositor_policy.rs:131-135`

---

## 11. Out-of-scope bugs / unverified claims

**OUT OF SCOPE BUGS** (found while tracing; not fixed, not compositor-related):

1. **Stale doc comment, `render.rs:600-603`.** Claims
   `Phase::Live` is "the X11-only animation path, per-frame … so windows ease
   smoothly". `arrange_full_phase` is only ever called with `Phase::Settled`
   (`render.rs:597`); the per-frame X11 animation path no longer exists.
   Misleading to the next reader of the geometry pipeline.
2. **`Monitor::layout_dirty` is a write-only field** (`maverick-core/src/types.rs:1321`).
   Five write sites, zero functional read sites. Either the incremental-layout
   cache it was built for was removed, or the readers were lost. Dead state
   that the compositor also writes (§3.7).
3. **Dead placeholder API**, `src/backend/x11/compositor.rs:215-230`:
   `live_placements` (no-op body) and `placeholder::FrameScheduler` are
   unreachable in the only build that compiles them.
4. **Redundant early return**, `src/backend/x11/events.rs:669-680`: the
   `_NET_WM_WINDOW_OPACITY` arm returns `Ok(())` only when a compositor exists.
   Behaviour is currently identical (the fall-through arms match no other atom),
   but the asymmetry is a trap for the next atom added to that function.

**UNVERIFIED:**

* I did not run the binary, an Xephyr harness, or any of the `tests/xephyr-*.sh`
  scripts, so the *observed* magnitude of the 1-px stepping (§6.1) is inferred
  from the code path, not measured. The code path is unambiguous; the
  perceptual severity is not something I can quantify from source.
* Whether any driver in the wild actually ignores `glXSwapInterval` (making
  §3.8 B2 fire) is driver-dependent and untested here. The defensive branch
  exists (`maverick-gl/src/renderer.rs:2181` returns `false`).
* `maverick-vk` is unwired as a *dependency*, but `tests/no_wait_in_wm.rs:63`
  lists `maverick-vk/src` in `WM_SOURCE_DIRS` as if it were linked into the WM
  binary. Whether that list is aspirational or defensive, I did not determine.
* The claim that `Phase::Live` on the WM side is dead rests on the `grep` in
  §10. A macro or trait-object indirection could in principle hide a call; I
  found none, but I did not do a full reachability proof.
* `maverick-render`'s `Renderer`/`Texture` traits having no implementation is
  asserted by `src/backend/renderer.rs:14-21` and confirmed by grep for
  `impl Renderer` / `impl maverick_render` — I did not enumerate every possible
  trait-object coercion site.
