# Floating Debug Investigation — Live Reproduction (2026-08-16)

## Environment
- Commit baseline: `fbafc67` (clean tree).
- `cargo check --workspace`: OK. `cargo test --workspace`: 342 WM unit tests PASS, 0 failures.
- Repro harness: isolated `Xephyr :2` (1400x900), separate Maverick instance `--name debug2`
  (production `:0` WM PID 4751 left untouched). Targeted via
  `maverick-msg query tree --session <sid>` (stale fichas avoided with `--session`).

## What was REPRODUCED (confirmed)

### BUG 1 — Late `WM_CLASS` / `WM_TRANSIENT_FOR` classification (root cause class)
- `mpv` and GTK set `WM_CLASS` AFTER `MapRequest`. At `manage()` time `client.class == ""`,
  so `apply_rules` (manage.rs:334) sees empty class and the `float=true` rule MISSES.
- Observed: `mpv` launched with `[[rule]] class="mpv" float=true` is **tiled** (`float=False`,
  geom [6,6,1384,884]), NOT floated. Floated only via post-manage `toggle_float`.
- Same gap hits `WM_TRANSIENT_FOR` read once (manage.rs:166-171, 302-321); a window that sets
  these late is never re-classified (`on_property` refreshes only title/hints, events.rs:456-503).
- This matches the "Steam disappears" hypothesis (transient-for-root filtered at manage.rs:952;
  late `WM_CLASS` → rule miss → tiled/mis-managed). CONFIRMED mechanism, not just a theory.

### BUG 2 — Oversized float clamps to top-left (Problem B, "moves up-left")
- manage.rs:380-385: `cx = target.x.clamp(wa.x, (wa.x+wa.w-target.w).max(wa.x))`.
  If `target.w > wa.w` the upper bound collapses to `wa.x` → `cx = wa.x` (left edge);
  same vertically → `cy = wa.y` (top edge).
- Observed: `mpv` 1920x1080 video, floated → pinned to `[6,6,1384,884]` (top-left, fills workarea).
  This is the concrete "moves up-left" mechanism. Stable (no loop), but wrong placement.

### BUG 3 — map-before-arrange flash (Problem A)
- manage.rs:491 `map_window` runs BEFORE manage.rs:498 `arrange()`. Window is briefly visible at
  the raw X geometry, then centered. Code-confirmed; explains the first "up-left → center".

## What was NOT reproduced here (and why)

### The center↔up-left infinite oscillation (the reported ping-pong)
- When `mpv` IS floated (via `toggle_float`) with a fitting video, geometry is **stable**
  (`[561,6,828,884]`, no oscillation over 8s polling). No WM-internal loop observed.
- `zenity --file-selection` (and `--info`) route through `xdg-desktop-portal`: zenity only creates
  a 1x1 `override_redirect` placeholder (xwininfo: Override Redirect State: yes, 1x1+0+0,
  IsUnMapped). The real GTK dialog is portal-side and not managed by Maverick in this env.
  So the GTK-dialog loop could not be exercised here.
- Code analysis (events.rs:242-264, reconciler.rs:187): floats use `follow=true`; on a client
  `ConfigureNotify` the WM adopts the rect and syncs `AppliedState`, so a single client request
  CONVERGES (next echo is `Compliant`). A true loop requires the client to request *alternating*
  geometry (e.g. recreate its window each cycle, or re-assert every frame) — not triggered by idle
  mpv/zenity in this environment.

## Decision: who moves first? (still needs the user's real `:0`)
The isolated Xephyr env cannot trigger the real loop (mpv not floated by rule due to late class;
zenity intercepted by portal). The user's `:0` WM (where mpv IS floated and the loop happens) is
the place to capture. Run there:

```bash
# On the REAL session (:0), with mpv floated by rule:
xtrace -D :9 -d :0 -o /tmp/xtrace.log &
DISPLAY=:9 mpv <video> &            # or the exact repro case
# In parallel, from another shell:
maverick-msg query tree > /tmp/mavtree.log      # poll desired/applied/real
xev -id <win> -event structure > /tmp/xev.log   # watch synthetic flag on ConfigureNotify
```
Decision rule:
- `ConfigureRequest(client) → configure_window(WM) → ConfigureNotify → ConfigureRequest(client) …`
  ⇒ **client-driven** (H-G2). Fix authority ONLY if capture justifies it.
- `Maverick places X → ConfigureNotify → Maverick changes X → ConfigureNotify …` with NO
  `ConfigureRequest` between ⇒ **internal WM geometry bug** (re-investigate arrange→reconcile→apply_geom).

## Proposed fixes (Phase 8, hold until capture for the loop; safe to do now for BUG 1/2/3)
1. `fix(floating)`: preserve centered intent for oversized floats (clamp should keep the window
   on-screen centered, not collapse to `wa.x/wa.y`) — BUG 2.
2. `fix(floating)`: apply initial geometry before `map_window` — BUG 3.
3. `fix(transient)`: re-read `WM_TRANSIENT_FOR`/`WM_CLASS` on `PropertyNotify` and handle
   transient-for-root as float-on-active-monitor — BUG 1 / Steam.
4. `fix(floating)`: prevent client geometry feedback oscillation — ONLY if the `:0` capture proves
   a client-driven loop that Maverick must not honor.

## Regression checklist (after fixes)
Firefox, Firefox fullscreen, Wine (normal/floating/fullscreen/popups), mpv float=false (must stay OK),
mpv float=true, Steam, file chooser, tiled window, workspace/monitor switching, focus changes.
