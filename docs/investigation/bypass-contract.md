# BYPASS FORENSIC TRACE — fullscreen ↔ compositor contract

Status: audit complete (no production code changed).
Method: static audit of `engage_bypass()` / `disengage_bypass()` / `bypass_window()`
/ `resume_window()` / `compute_scene()` / `rename_and_bind()` + live reproduction in
Xephyr (`MAV_FORENSIC_TRACE_XID`, `MAV_GLX_TRACE`, `MAVERICK_COMPOSITION_TRACE`).

Trace evidence (live, `target/debug/maverick`, Xephyr :80 640x480, Mesa llvmpipe):

```
frame 1    compute_scene_end_state  tex_present=true tex=2 bound=true included=true scene_len=1  bypass=false
frame 1    bypass_resource_release_before  needs_rebind=true damaged=true dirty=true needs_full=true
frame 1    bypass_resource_release_after   tex_present=false pixmap_present=false needs_rebind=false damaged=false dirty=true needs_full=true
frame 2    compute_scene_end_state  tex_present=false needs_rebind=false damaged=false dirty=true  bypass=true scene_len=0
frame 3+   compute_scene_end_state  tex_present=false needs_rebind=false damaged=false dirty=false bypass=true scene_len=0 frame_plan=Idle
...
disengage  disengage_bypass_before  bypass=true   tex_present=false needs_rebind=false
           resume_window_before     needs_rebind=false damaged=false dirty=true needs_full=true
           resume_window_after      needs_rebind=true damaged=true dirty=true needs_full=true bypass=true
frame N+1  rename_and_bind_begin -> texture_creation success tex=2 -> draw -> present (Full)  bypass=false
```

## 1. Modelo real de bypass

**Modelo A** (compositor steps aside; X presents the window directly) **with Model B
state-retention requirements**. Maverick un-redirects the window via
`composite_unredirect_window` (`bypass_window`, compositor.rs:1856-1873) and then
**destroys** the GL texture + named pixmap. The `CompWin` entry is *kept* (it still
holds `outer`, `mapped`, `format`), but `tex`/`pixmap`/`needs_rebind`/`damaged` are
reset. `compute_scene` skips any window in `bypassed_set` (compositor.rs:2055-2063).
So `tex is None` is **not** what excludes it — the `bypassed_set` membership check does.

* Introduced in `a662dcd` ("Add configurable compositor policy and safe fullscreen
  bypass").
* Policy (pure, `compositor_policy.rs`) decides per-output Bypass/Compose.
* `mod.rs:820-855` engages/disengages each turn before the frame decision.

## 2. Máquina de estados real

| State | `bypass` | `tex` | `needs_rebind` | `damaged` | `dirty` | scene | draw allowed |
|---|---|---|---|---|---|---|---|
| NORMAL | false | Some | false | false/true | — | included | yes |
| ENTER_BYPASS | true | None (destroyed) | **false** | **false** | true (1 frame) | skipped | no |
| BYPASS_ACTIVE | true | None | false | false | false | skipped | no |
| EXIT_BYPASS | false | None | (re)set true | true | true | included | next frame |

## 3. Enter bypass

`engage_bypass()` (compositor.rs:1712-1731) → `bypass_window(win)`:

1. `composite_unredirect_window(win)` — X presents directly.
2. `cw.tex.take()` → `renderer.destroy_texture(t)`.
3. `cw.pixmap.take()` → `free_pixmap(pm)`.
4. `cw.damaged = false; cw.needs_rebind = false;`
5. `mark_full(GEOMETRY)` (one dirty frame to clear the overlay).

The question posed in the task ("why does releasing visual resources also erase the
intention to rebuild them?") is answered directly: **it shouldn't**. After
`bypass_window`, `tex = None` with `needs_rebind = false` violates the compositor's
own invariant from `needs_texture_fixup` (line 108-110):
`debug_assert!(!tex_none || needs_rebind)`. The only reason the over-assert does not
fire is that `compute_scene` skips `bypassed_set` before reaching the fixup gate —
the invariant is *silently parked* for the duration of bypass.

## 4. Bypass is NOT resource destruction

| Resource | Enter | Active | Exit |
|---|---|---|---|
| Texture | **destroyed** (should keep or mark for rebuild) | absent | re-created by `rename_and_bind` |
| Pixmap | destroyed | absent | re-created |
| Damage | **kept** (`damages` untouched) | live | live |
| geometry (`outer`/transform) | kept | kept | kept |
| damage state (`damaged`) | **cleared** | n/a | re-set true by resume |
| dirty state | one `mark_full` then idle | none | re-set true |

The *Damage* XID is intentionally preserved (line 1373: a bypassed window's damage
is consumed and ignored, not destroyed). So the design already treats Damage as
"conservable"; texture/pixmap are treated as ephemeral — but their destruction is
not paired with the *intention* flag.

## 5. Invariantes

- `tex.is_none() && needs_rebind == false` for a *mapped, non-bypassed* window is
  an invariant violation. It is currently prevented only by the `bypassed_set` skip
  in `compute_scene`.
- `rename_and_bind` (composition, 2884): `tex = Some => needs_rebind == false`.
- `on_map`/`on_configure(resize)`/`resume_window` all set `needs_rebind=true`
  when they drop a texture.

**The minimum invariant violated by `engage_bypass`:** when `bypass_window` destroys
  the texture it must leave `needs_rebind=true` (and `damaged=true`) — the same
  contract every other texture-destroying path honours — so that `resume_window`
  or an aborted bypass can rebuild in the first Compose frame without depending on
  a client damage event.

## 6. Enter / Exit fast

- `engage->engage` same win: no-op (idempotent, `bypassed.get(&mon)==Some(&win)`).
- `engage->engage different win`: `resume_window(old)` then `bypass_window(new)`.
  `resume_window` requires a `GetWindowAttributes` RTT for VIEWABLE; if the old
  window unmapped in between, `needs_rebind` stays false and `tracked` may drop —
  acceptable (unmap implies rebuild-on-map).
- `disengage` after `engage` in the same turn: draws one Compose frame that
  rebuilds via `rename_and_bind`.

**No invalid transient state was observed** in exit. The problematic state
(`bypass=false, tex=None, needs_rebind=true, damaged=true`) is *exactly* what
`resume_window` produces, and the trace proves it recovers in one frame.

## 7. compute_scene contract

- A window is excluded **iff** `bypassed_set.contains(win)` (line 2055). `tex.is_none()`
  is a *second, independent* gate further down (line 2130).
- `bypass=true, tex=Some`: excluded because of the set check (single, deterministic).
- `bypass=false, tex=None, needs_rebind=true, damaged=true`: passes the fixup gate
  (`needs_rebind && damaged`) → `rename_and_bind` runs → texture created → drawn.

## 8. Damage / client content contract

- During bypass: `on_damage` still subtracts the pending `Damage` region (fixed
  — a prior version of this code returned before the subtract, which starved
  `ReportLevel::NON_EMPTY`'s empty→non-empty edge and permanently silenced
  `DamageNotify` for the window after the *first* bypass; that was the actual
  cause of the "video freezes ~2s after leaving fullscreen" bug). It still
  takes no other action (no `dirty`, no `cw.damaged`) since the window isn't
  drawn while bypassed.
- On exit: `resume_window` sets `damaged=true` explicitly, so the first Compose
  frame forces a bind + draw regardless of whether the client repainted. The
  content the client produced *while bypassed* is in its real window; the first
  `NameWindowPixmap` after `composite_redirect_window` snapshots the current
  contents, and `damaged=true` repaints it. The client does not need to repaint.
  `resume_window` also flushes any Damage region left unsubtracted at the exact
  engage/disengage boundary, as a second guard on top of the continuous
  subtract in `on_damage`.

## 9. Scheduler contract

- `engage_bypass`: `mark_full` sets `dirty=true` (one frame). During bypass,
  `dirty=false` and the loop goes idle (no presentation) — correct (nothing to draw).
- `disengage_bypass`: `resume_window` `mark_full` → next turn renders (Full) and
  rebuilds. Confirmed by trace.

## 10. Root cause (confirmed)

`bypass_window` destroys the texture/pixmap and clears `needs_rebind`+`damaged`.
During bypass this is invisible (the set-check hides the window). The real defect is
the **missing rebuild arm**: if `disengage` ever runs while `GetWindowAttributes`
does not report VIEWABLE (e.g. the window is unmapped during the transition, or an
async race), `resume_window` skips re-arming, leaving `bypass=false, tex=None,
needs_rebind=false, damaged=false` — the window is permanently excluded until the
next damage/map event. The design already depends on `resume_window`'s re-arm; the
asymmetric entry path (`bypass_window` clearing the flags) breaks the invariant for
every non-VIEWABLE exit.

## 11. Fix mínimo recomendado (NOT implemented, per instructions)

Make `bypass_window` symmetric with `resume_window`:

```text
bypass_window:
    release tex/pixmap
    damaged = true  (instead of false)
    needs_rebind = true (instead of false)
    bypass = true
```

i.e., treat the release as an "invalidation" (as `on_map`/`on_configure(resize)`
do!) rather than a "reset to pristine". Then even a silent exit (non-VIEWABLE
`resume_window`, or a `disengage_all_bypass` path) leaves the window with
`needs_rebind=true, damaged=true` and the next Compose frame rebuilds without any
client involvement. `compute_scene`'s existing `needs_rebind && damaged` gate does
the rest. No change to the advisory policy, to VSync, or to the damage counter.

Note: this is a *minimal* correction. The deeper consideration — whether bypass
should destroy the texture at all vs. keep it for zero-recreate exit — is a
performance question (TFP rebind cost) and is deliberately out of scope here.