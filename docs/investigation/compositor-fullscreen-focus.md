# Maverick compositor/fullscreen/focus investigation

Status: rounds 1 and 2 complete (static audit). No production logic changed yet.
Method: 7 specialized read-only agents + coordinator cross-verification of every
conflicting claim directly against the source.

## Symptoms
1. mpv breaks compositing.
2. Zen Browser behaves worse than Firefox.
3. Zen steals focus.
4. Zen does not enter tiled mode.
5. Animations are not applied to Zen.
6. Firefox video fullscreen does not fill the tile.
7. `Mod+Shift+F` during video fullscreen breaks compositing.
8. Wine works well and must not regress.

## Reproduction
Not reproduced yet: the investigation round had no X server / shell access.
Everything below marked `CONFIRMED` is a verified code fact (file:line).
Causality against the live symptoms is still `SUPPORTED`, not proven.

## Architecture (verified)
```
X11 event -> src/backend/x11/events.rs -> Engine::execute(Command) -> Effect -> backend
                                       \-> compositor.on_{create,map,unmap,destroy,configure,damage}
Engine state (src/types.rs) -> core::layout::arrange -> core::present::present_into
   -> DesiredState -> backend::x11::reconciler::reconcile -> render::emit_geometry
   -> configure_window
```
- Layout is the sole producer of desired geometry; `AppliedState` (reconciler.rs)
  is the sole record of what was written to X11.
- A tiled/fullscreen client cannot move itself: `events.rs:100-120` ignores the
  ConfigureRequest and replies with a synthetic ConfigureNotify (ICCCM 4.1.5 OK).
- Fullscreen is ONE bit (`WinFlags::FULLSCREEN`) plus a per-rule
  `FullscreenPolicy{Normal,Deny,True}`.
- In `LayoutKind::Column` a fullscreen window is NOT a pinned overlay: it is a
  screen-sized tile of the scrolling ribbon (`present.rs:62-73`,
  `layout.rs:545-577`); at `alpha == 1` its rect is exactly `mon.screen`.
- Compositor: XComposite manual redirect + TFP zero-copy + XDamage; incremental
  `stack` with `QueryTree` resync.

## Confirmed root causes (code facts)

### RC-1 — No retry and no fallback when a window texture bind fails
`compositor.rs:1772-1793`: if `texture_from_pixmap` fails, the pixmap is freed and
`cw.tex` stays `None` forever. `rename_and_bind` is only re-entered from `on_map`
and from `on_configure` on a *resize*. `compute_scene` skips a window with no
texture (`compositor.rs:1260`) -> the window disappears from every frame,
permanently. `renderer.rs:1105-1129` only round-trips/verifies
`glXCreatePixmap` for the FIRST pixmap of each visual (`self.verified.insert`);
later ones skip the sync and `maverick-gl/src/xlib.rs` installs a silent error
handler, so an asynchronous `BadMatch`/`BadDrawable` yields an invalid
`glx_pixmap` treated as valid. `renderer.rs:1383-1396` caches the `Err` from
`choose_tfp_fbconfig` per visual (permanent negative cache).

### RC-2 — The compositor's fallback contract is dead code
`mod.rs:596-607` documents "A GL failure disables the compositor and returns us
to the classic path", but `Compositor::render()` returns `true` unconditionally
(`compositor.rs:1594`). The `catch_unwind` only catches panics; with
`panic = "abort"` (`Cargo.toml:69`) a real panic kills the WM in release. The
compositor therefore cannot degrade: it stays visibly broken.

### RC-3 — `FocusIn`/`FocusOut` do not filter `mode`/`detail`
`events.rs:754-782` writes `x11_input_focus` and calls `reconcile_focus()` for
every focus event, whatever its `mode`/`detail`. `reconcile_focus` re-asserts
`set_input_focus` on any divergence (`render.rs:1086-1100`), with a
`has_protocol` X round-trip per repair. A client that moves focus to a child
window (Gecko: Firefox and Zen) produces `FocusOut(detail=Inferior)` ->
spurious repair -> focus ping-pong. `on_enter` already filters correctly
(`events.rs:727`), which shows the pattern is a known one and simply missing here.

### RC-4 — Fullscreen/initial-state policy is a per-application rule, not an invariant
`config.rs:341-343` (compiled default) and the shipped/user config
(`config/config.toml:238-241`) contain:
`[[rules]] class = "firefox"  deny_fullscreen = true  ignore_initial_state = true`.
`Rule::matches` is a case-insensitive SUBSTRING test over class/instance
(`config.rs:187-206`), so Zen (`zen` / `navigator`) matches neither. There is no
`zen` rule anywhere in the repo.
- `deny_fullscreen` (`events.rs:556-569`) rejects the client's EWMH request and
  rewrites `_NET_WM_STATE`. In Gecko, F11 and the DOM/video Fullscreen API are
  the SAME ClientMessage, so both are denied: the `<video>` enters Gecko's
  internal fullscreen and stretches only to the tile -> symptom 6. The WM's
  fullscreen geometry itself is correct (`layout.rs:558-571`, `present.rs:73`).
- `ignore_initial_state` (`manage.rs:713-758`) strips map-time
  `_NET_WM_STATE_{FULLSCREEN,MAXIMIZED_*}`. Without it (Zen), `manage.rs:209-230`
  trusts the client's map-time state -> Zen opens fullscreen/maximized -> in
  Column that is a screen-filling ribbon tile that hides its siblings
  (`layout.rs:545-577`) -> symptoms 4 and 5 (its rect is `screen`/`workarea`,
  not a ribbon tile, so the tile animation never applies to it).

### RC-5 — `damaged = false` right after the post-resize rebind
`compositor.rs:1777`. `compute_scene` only re-binds when `damaged`
(`:1264-1268`), and the TFP spec leaves texture contents undefined after the
client draws while bound. A client that does not repaint after a resize can show
undefined content.

### RC-6 — `ignore_unmaps` is dead code
Declared `mod.rs:250-252`, initialised `mod.rs:866`, read/decremented
`events.rs:62-65`, and NEVER incremented; there is no `unmap_window` call in
`src/` at all. Harmless today (the WM culls instead of unmapping) but a latent
trap, and the CHANGELOG presents it as the fix for the scroll-culling bug (C1).

### RC-7 — The example config autostarts `picom`
`config/config.toml:279` while the built-in compositor is on by default -> two
compositors redirecting. Not present in the reporting user's own config, so it is
NOT the cause of the observed symptoms, but any user copying the example hits it.
The compiled default has it commented out (`config.rs:388`).

## Broken invariants (the common cause)

INV-A (compositor): every mapped window's GPU/X resources must be valid or
queued for retry, and an unrecoverable failure must degrade visibly, never
silently. Violated by RC-1 + RC-2 + RC-5 -> symptoms 1 and 7.

INV-B (WM): the policy for client-declared state (fullscreen/maximize at map
time and at runtime) is a WM invariant, not a per-`WM_CLASS` rule. Violated by
RC-4 -> symptoms 2, 3, 4, 5, 6. This is why a Firefox fork behaves worse than
Firefox: it does not match the substring `"firefox"`.

INV-C (focus): only real focus transitions (`mode=Normal`, `detail != Inferior`)
describe a focus change. Violated by RC-3 -> symptom 3 and background churn for
every client with child windows.

## Hypotheses

HYPOTHESIS: H1 mpv "breaks compositing" because its visual/pixmap cannot be
TFP-bound and the compositor neither retries nor degrades, so the window is
skipped forever.
EVIDENCE: compositor.rs:1772-1793, :1260; renderer.rs:1105-1129, :1383-1396;
mpv is floated by rule (config.toml:250-252) so its self-resizes are honoured ->
rename_and_bind storm.
COUNTER-EVIDENCE: no runtime trace yet; the warning is emitted once per visual
(compositor.rs:1789).
CONFIDENCE: medium-high
TEST: `MAVERICK_LOG=debug` + mpv; grep "compositor: cannot texture windows of".
RESULT: not run
STATUS: SUPPORTED

HYPOTHESIS: H2 `Mod+Shift+F` during video fullscreen breaks compositing by the
same mechanism, amplified by the state ping-pong `deny_fullscreen` creates (the
WM refuses the client's fullscreen, then grants the same state via
`write_net_wm_state`), which makes Gecko relayout/resize.
EVIDENCE: events.rs:556-569; manage.rs:1108-1123; compositor.rs:893-927; RC-1; RC-2.
COUNTER-EVIDENCE: teardown order in on_unmap/on_destroy is correct (TFP release
before glXDestroyPixmap, renderer.rs:1355-1367): no classic stale pixmap.
CONFIDENCE: medium
TEST: 20x fullscreen toggle under tests/xephyr-compositor.sh with pxsample.
RESULT: not run
STATUS: SUPPORTED

HYPOTHESIS: H3 Firefox video fullscreen does not fill the tile because the
default `deny_fullscreen` rule rejects the EWMH request; F11 and the video
fullscreen API are indistinguishable in Gecko.
EVIDENCE: config.rs:341-343; events.rs:556-569; layout.rs:558-571; present.rs:73.
COUNTER-EVIDENCE: none; the fullscreen geometry math is correct.
CONFIDENCE: high
TEST: `xprop -spy -id <ff> _NET_WM_STATE` while entering YouTube fullscreen.
RESULT: not run
STATUS: SUPPORTED

HYPOTHESIS: H4 Zen does not tile / is not animated because it maps with
`_NET_WM_STATE_FULLSCREEN|MAXIMIZED` and, with no `zen` rule,
`ignore_initial_state` never runs.
EVIDENCE: manage.rs:209-230, :713-758; config.rs:187-206; layout.rs:545-577;
present.rs:74-79.
COUNTER-EVIDENCE: needs xprop confirmation that Zen really requests that state;
alternative not excluded: Zen's early/hidden window unmaps and its remap goes
through unmanage->manage (events.rs:85), losing state.
CONFIDENCE: medium-high
TEST: poll `xprop _NET_WM_STATE` during Zen startup; compare with Firefox.
RESULT: not run
STATUS: SUPPORTED

HYPOTHESIS: H5 "Zen steals focus" is largely Maverick's own churn: unfiltered
FocusIn/FocusOut plus reconcile_focus re-assertion.
EVIDENCE: events.rs:754-782 vs events.rs:727; render.rs:1086-1100.
COUNTER-EVIDENCE: manage.rs:508-536 also focuses every new window unless an
overlay is present, so part of the symptom may be policy, not churn.
CONFIDENCE: medium-high (the filtering defect is CONFIRMED; its weight is not)
TEST: `--features input-trace`; count "reconcile_focus REPAIR" for Zen vs Firefox.
RESULT: not run
STATUS: SUPPORTED

HYPOTHESIS: H6 Zen does not "refuse" animations; Maverick never applies them
because the window is not a ribbon tile (consequence of H4). A tiled client
cannot block its own animation: the reconciler writes intermediate geometry
every frame and re-asserts on divergence.
EVIDENCE: reconciler.rs:117-145; events.rs:254-268; layout.rs:535-539;
manage.rs:463-467.
COUNTER-EVIDENCE: if Zen maps two top-levels, the second one would animate.
CONFIDENCE: medium-high
STATUS: SUPPORTED

HYPOTHESIS: H7 The compositor holds a stale pixmap after a resize (the classic
suspect).
COUNTER-EVIDENCE: compositor.rs:893-927 frees tex+pixmap and rebinds when
(resized && mapped), fed by the real ConfigureNotify (synthetic dropped at
events.rs:205); teardown order is correct.
CONFIDENCE: high (that it is NOT this)
STATUS: REJECTED — the bug is a failed bind with no retry, not an old pixmap.

HYPOTHESIS: H8 Dual compositor (picom + built-in) explains symptoms 1 and 7.
COUNTER-EVIDENCE: the reporting user's own config does not autostart picom.
STATUS: REJECTED for the reported symptoms; CONFIRMED as a repo example bug
(config/config.toml:279).

HYPOTHESIS: H9 A fullscreen window does not fill the screen when the workspace
viewport zoom != 1.0, because the fullscreen rect is scaled by alpha.
EVIDENCE: layout.rs:559-567.
COUNTER-EVIDENCE: `fs_ctx` is empty in Overview; viewport zoom is an explicit
user mode, so this may be intended.
CONFIDENCE: low-medium
STATUS: UNKNOWN — document, do not change without a design decision.

HYPOTHESIS: H10 Late `WM_CLASS`/`_NET_WM_WINDOW_TYPE` (set after MapRequest)
means rules never apply.
EVIDENCE: manage.rs:103-287 classifies once; events.rs:495-506 only re-reads
WM_NAME/_NET_WM_NAME and WM_HINTS.
COUNTER-EVIDENCE: Gecko normally sets WM_CLASS before mapping.
CONFIDENCE: low
STATUS: UNKNOWN — needs an xtrace of Zen's startup.

## Rejected hypotheses
- Stale Composite name-window-pixmap after resize (H7).
- Missing synthetic ConfigureNotify for denied ConfigureRequests: Maverick does
  send it (events.rs:100-120).
- Missed `XDamageSubtract` stalling updates: exactly one subtract per report
  (compositor.rs:931-940).
- Dual compositor as the reported cause (H8).
- A size-based "large window floats/fullscreens" heuristic: none exists in
  manage.rs.
- Wine-specific code paths: none exist; Wine benefits from general invariants.

## Proposed fixes (minimal, invariant-level, no per-app branches)
1. docs(investigation): this document.
2. fix(compositor): retry the texture bind instead of dropping the window
   forever; mark `damaged = true` after a rebind (RC-1, RC-5).
3. fix(compositor): make `render()` report unrecoverable GL/X failure so the
   documented fallback in mod.rs:596-607 can actually fire (RC-2).
4. fix(gl): verify GLX pixmap creation whenever a bind has already failed for a
   window, not only for the first pixmap of a visual (RC-1).
5. fix(focus): ignore inferior/grab focus transitions, mirroring on_enter (RC-3).
6. fix(wm): normalize client map-time `_NET_WM_STATE` for every client by
   default (the old per-class `ignore_initial_state` becomes the default policy,
   with an opt-out), and delete the `firefox` rule (RC-4, INV-B).
7. fix(wm): honour client fullscreen requests by default (`deny_fullscreen`
   leaves the defaults and stays available as an explicit opt-in) (RC-4, INV-B).
8. chore(config): drop `picom` from the example autostart (RC-7).
9. refactor(wm): remove the inert `ignore_unmaps` guard, or wire it if unmapping
   returns (RC-6).
10. tests(compositor): cover bind-failure / resize / unmap / destroy through a
    minimal `Surface` seam so the resource lifecycle becomes testable in-process.

Decisions taken by the maintainer: fixes 6 and 7 are approved in the
"normalize + honour by default" form. Both REMOVE per-application hacks rather
than adding them.

## Regression risks
- Wine (focus, fullscreen, stacking, geometry) from fixes 5, 6 and 7.
- Fix 5 must preserve focus repair after an external `XSetInputFocus`
  (`mode=Normal`, `detail=Nonlinear`) — this is what Wine depends on.
- Wine invariants to protect: `WM_TAKE_FOCUS` on every focus grant
  (render.rs:796-798, 1098-1100); faithful synthetic ConfigureNotify when a
  request is denied (events.rs:100-120); OR/unmanaged windows honoured directly
  (events.rs:165-186); external-focus repair; transient chains and
  `pending_transients`; `FullscreenPolicy::True` exclusive path.
- Fix 7 changes F11 behaviour in browsers (it starts working) — intended.

## Validation matrix
| Case | Focus | Tile | Animation | Fullscreen | Composite |
|---|---|---|---|---|---|
| Firefox normal | | | | N/A | |
| Firefox video fullscreen | | | | | |
| Firefox video FS + Mod+Shift+F | | | | | |
| Firefox browser fullscreen | | | | | |
| Zen normal | | | | N/A | |
| Zen fullscreen | | | | | |
| mpv normal | | | | N/A | |
| mpv fullscreen | | | | | |
| Wine normal | | | | N/A | |
| Wine fullscreen | | | | | |

Pass criteria: `maverick-msg query tree` fields (focus/x11_focus/overlay/
fullscreen/real/geom) for focus/tile/fullscreen; `xwininfo` rect == `mon.screen`
for fullscreen; `pxsample` under tests/xephyr-compositor.sh for composite;
`--features input-trace` REPAIR counts for focus churn.

## Experiments still to run
1. `MAVERICK_LOG=debug` + mpv -> grep "cannot texture windows of" (closes H1).
2. `xprop -spy _NET_WM_STATE` on Firefox during video fullscreen (closes H3).
3. Poll `xprop` on Zen at startup + `xtrace` (closes H4/H10).
4. `--features input-trace`: REPAIR count Zen vs Firefox (closes H5).
5. `tests/xephyr-compositor.sh` + `pxsample`, 20x fullscreen toggles (closes H2).
6. Wine: creation, focus switching, fullscreen, tiled/floating/workspace moves.

## Test coverage gaps (Agent 6)
CI runs only `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace` (.github/workflows/ci.yml); the Xephyr scripts are
manual. Uncovered: client-requested (ClientMessage) vs WM fullscreen parity;
fullscreen + unmap/destroy/workspace-switch/monitor-change; fullscreen while
zoomed; unmap->remap state retention; late re-classification; FocusIn mode/detail
variants; compositor pixmap re-acquire and bind-failure recovery (needs a seam).
