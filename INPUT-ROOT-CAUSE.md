# Input-Saturation / Liveness Failure — Root Cause Analysis

Agent A · static analysis of the X11 backend at working-tree state (dirty: the
uncommitted deferral diff is present and is analysed critically in
**root resource** / **causal path**). Line numbers cite the working tree unless
prefixed `HEAD:`. All defaults quoted are `Cfg::default()` (`src/config.rs:100-120`):
`focus_mouse = false` (`:110`), `warp_cursor = false` (`:111`), `corner_radius = 0`
(`:107`).

---

## reproduction

* `Mod4 + wheel` (Button4/5/6/7) at any sustained rate, and rapid Button1 clicks.
* Entry point: `on_button_press` scroll branch — `src/backend/x11/pointer.rs:156-174`,
  which calls `scroll_camera_with_wheel(detail, e.root_x, e.root_y)`
  (`pointer.rs:160`, body at `pointer.rs:668-686`).
* Plain wheel (no `Mod4`) is **not** a reproducer: it is
  `allow_events(REPLAY_POINTER)` with no `.check()` and no work
  (`pointer.rs:171-173`).
* `MotionNotify` is **not** a reproducer: `on_motion` does no X request and no
  arrange unless a drag is active (`pointer.rs:536-661`; the `focus_mouse` arm at
  `pointer.rs:656-659` is deliberately empty).
* Faster input reproduces sooner because the failure is a *rate* failure, not a
  capacity failure: the deficit `arrival − service` integrates without bound, so
  the time to freeze is `frozen / (arrival − service)`.
* A click on a **child** window (browser tab strip, terminal) is worse than a
  click on a bare toplevel, because `find_client` walks the tree with one
  `query_tree().reply()` **per level** (`manage.rs:1221-1240`, RTT at
  `manage.rs:1230`).

Measured corroboration already in the tree (`SATURATION-MEASUREMENTS.md`,
Agent B, Xephyr `:99`, 6 clients): queueing delay flat at 50 events/s, growing
linearly 0 → 594 → 1252 → 1957 → 2608 ms across a 20 s 200-notch/s burst.

---

## observed growth

Three things grow, and they are different from each other.

**1. Per-input-event cost, which is O(N) and repeated.**

One `Mod4`+wheel notch is *three* `arrange` passes and *two* `focus()` passes.
`FocusDir` emits exactly `Unfocus(from)`, `ArrangeMonitor(mi)`, `FocusWindow(Some(w))`
(`src/core/commands.rs:852-857`), executed in order by `run_effects`
(`src/backend/x11/actions.rs:63-66`); then `focus_column_at` runs a **second,
full** `focus()` (`pointer.rs:684`, `pointer.rs:691-723`).

`focus()` has **no early return when the window is already focused**. The only
`prev_focused` guard is around `unfocus(prev)` (`render.rs:1188-1194`) and the
`FocusChanged` notify (`render.rs:1380-1385`). Everything else — the two
round-trips, `grab_buttons`, `arrange`, `stack_overlay`, `_NET_ACTIVE_WINDOW`,
`warp_pointer` — runs unconditionally. So the second `focus()` is a full-cost
duplicate whenever it lands on the same window, which is the common case.

Each `arrange` = `arrange_full` → `arrange_full_phase` (`render.rs:592-669`):
`hide_offscreen` O(N) (`render.rs:614-616`, `render.rs:671-757`), `layout::arrange`
O(N) (`render.rs:619-627`), `present_into` O(N) (`render.rs:631-636`),
`DesiredState::from_placements` O(N) **allocation** (`render.rs:639`),
`reconcile` O(N) **allocation** (`render.rs:640`, `reconciler.rs:199-246`),
`emit_geometry` per changed window (`render.rs:650-653`), `stack_overlay` O(N)
(`render.rs:666`). So one notch = 3 × O(N) CPU with ~6 O(N) allocations, plus
5 × O(N) `stack_overlay` (arrange's own three at `render.rs:666` + `focus()`'s two
at `HEAD:render.rs:1336`).

**2. Blocking round-trips per input event, which is what serialises the loop.**

The loop is single-threaded and blocking (`mod.rs:609-695`, `mod.rs:737-760`).
`focus()` blocks twice: `has_protocol` → `get_property().reply()`
(`render.rs:1218`, `ewmh.rs:293-306`) and `reconcile_focus` →
`get_input_focus().reply()` (`render.rs:1253` → `render.rs:1412-1428`). The scroll
branch adds a third at the end: `allow_events().check()` (`pointer.rs:166-168`).
The click path adds `allow_events().check()` (`pointer.rs:474-483`) and, for
`Mod4`-drag on a float, `grab_pointer().reply()` (`pointer.rs:414`).

**3. Server event amplification, which is the multiplier.**

This is the part that decides *which* event type saturates first.

| input event | sync RTTs | O(N) arranges | O(N) restack | fire-and-forget X requests | X events returned **to the WM** | amplification (events ÷ input) |
|---|---|---|---|---|---|---|
| `Button1/3` on a tiled window, focus changes | **3** (+1–3 `find_client` `query_tree`) | 1 | 2 (`arrange`'s + `focus`'s) | 1 `set_input_focus` + 1 `send_proto` + 1 `change_window_attributes` + **18** grab/ungrab + 1 `change_property32` + 2·Δ (configure+`send_event` per moved win) | 3·Δ + 2 focus + 1 `PropertyNotify` | **≈3** when the camera does not move; **≈3N+3** when it does |
| `Button1/3` on a float with `Mod4` | **4** | 1 | 2 | +1 `grab_pointer` | +1 `FocusIn` | ≈4–4.3N |
| `ButtonRelease`, no drag | **0** | 0 | 0 | 0 | 0 | **1** |
| `ButtonRelease`, drag end | **1** (`ungrab_pointer().check()`, `pointer.rs:508`) | 1 | 1 | 2·Δ′ + prefs sync | 3·Δ′ | ≈1–3N |
| `MotionNotify`, dragging | **0** | 0 | 0 | 2·Δ′ (`configure_window` + `send_event`) | 3·Δ′ | ≈3 |
| `MotionNotify`, not dragging | **0** | 0 | 0 | 0 | 0 | **1** |
| **`Button4/5/6/7` + `Mod4`** | **5** (2 `get_property` + 2 `get_input_focus` + 1 `allow_events().check()`) **+ 0–4** from the `FocusIn`/`FocusOut` echoes | **3** | **5** | **54** grab/ungrab (3 × `grab_buttons` × 18) + 2 `set_input_focus` + 2 `send_proto` + 2 `change_window_attributes` + 2 `change_property32` + 2·Δ·A (A = arranges that actually moved geometry) | 3·Δ·A + ≤4 focus + 2 `PropertyNotify` | **≈20–40** (N=6); Agent B measured ~52 |
| `Button4/5/6/7`, no `Mod4` | **0** | 0 | 0 | 1 `allow_events(REPLAY)` (no `.check()`) | 0 (replayed to the client) | **1** |

`grab_buttons` is **18** requests, not "a handful": `ungrab_button` (1,
`input.rs:370`) + the `SYNC` catch-all `grab_button` (1, `input.rs:385-395`) +
`8` modifier variants (`mod_variants`, `mod.rs:1833-1845`) × 2 buttons (`M1`,
`M3`) = 16 (`input.rs:415-430`). `unfocus` calls it too
(`render.rs:1390-1397`), so a focus change costs 36 grab requests and a wheel
notch costs 54.

Amplification arithmetic, per geometry write (`emit_geometry`, `render.rs:955-1056`):
1 `configure_window` (`render.rs:983-991`) + 1 `send_event` ConfigureNotify
(`render.rs:1014-1016`); `change_property32(_NET_FRAME_EXTENTS)` only when the
border changed (`render.rs:1026-1035`); `shape::rectangles` ×2 only when
`corner_radius > 0` (`render.rs:1058-1087`, default 0). The **server** answers
each `configure_window` with a `ConfigureNotify` to *both* the
`STRUCTURE_NOTIFY` client on the window (mask set at `manage.rs:477-485`) and the
`SUBSTRUCTURE_NOTIFY` client on the root (`input.rs:72`) — **2 real events** — plus
the **1 synthetic** `SendEvent` copy coming back to the WM. So:

> **3 X events returned to the WM per `emit_geometry` call, of which 2 are
> dispatched and 1 is discarded** at `events.rs:302-304` (the `response_type &
> 0x80` `SendEvent` bit).

For a `Mod4`+wheel notch that shifts the whole ribbon, arrange #1 moves all N
windows → **3N** events, and (in a scrollable ribbon) arrange #2 moves them all
back again → another **3N**. Hence ≈`6N + 6` ≈ 42 for N=6, matching Agent B's ~52
(the remainder is client-driven `ConfigureRequest`/extras).

**What does *not* grow, and this is the part of your hypothesis I refute.** The
X11 *event queue* is not a resource that fills up: it is drained to empty every
turn (`mod.rs:638-640`, and again at `mod.rs:672-674`). There is no accounting
structure inside Maverick that grows per input event — `last_key_times` is
`retain`-bounded (`events.rs:820`), `last_stack_order` is per monitor
(`mod.rs:325`), `frame_extents`/`shape_mask_cache` are per window, `apply_geom`
scratch buffers are `take`/`restore` (`render.rs:721`, `render.rs:755`), and
Agent B's untraced controls hold RSS flat at ~4.9 MB under identical load. So the
growth is **rate**, not **capacity**.

The accumulating backlog lives in two places neither of which Maverick can skip
past:

* the **X server's per-client event buffer** for the WM connection, and
* the **X server's per-device input queue**, which Maverick has explicitly made
  *unskippable*: `grab_buttons` installs `GrabMode::SYNC` on every managed window
  (`input.rs:385-395`), so every `ButtonPress` **freezes the pointer on the server**
  from the moment it is queued until `allow_events` runs. In the scroll branch
  that is the *last* statement in the handler, after the entire
  `scroll_camera_with_wheel` cost (`pointer.rs:160` … `pointer.rs:166-168`). The
  server therefore cannot drop or coalesce a queued detent: the WM owes full
  service for every one, forever.

---

## root resource

**The X11 connection's request/reply channel plus the WM thread's single
execution context — i.e. a service-rate deficit, not a queue-capacity defect.**

Precise decomposition of the four candidates you listed:

| candidate | verdict | evidence |
|---|---|---|
| (a) CPU in O(N) `arrange` | **contributing, not primary** | 3 arranges + 5 restacks + 2 `focus()` per notch (`commands.rs:852-857` + `pointer.rs:684`), each O(N) with O(N) allocations (`render.rs:619-666`). Agent B measured 13–18 % of one core at 200–500 events/s — enough to matter, not enough to explain a total freeze. |
| (b) Blocking synchronous round-trips serialising the single-threaded loop | **primary serialiser** | 5 (scroll) / 3 (click) blocking RTTs per input event, each of which cannot overlap anything because the loop is one thread (`mod.rs:737-760`). The RTT also cannot be served until the server has drained the O(N) `configure_window` batch the *previous* arrange queued, so per-event latency is a function of the backlog — the classic convoy. |
| (c) Amplified event stream | **primary multiplier — your hypothesis, confirmed** | `3·Δ·A + O(1)` server events per input event (`render.rs:983-1016` + the two-mask `ConfigureNotify` rule). This is what makes wheel saturate at a *lower* input rate than clicks: ~20–40× vs ~3×. |
| (d) Genuinely unbounded / self-perpetuating loop | **refuted for the default config** | see below. |
| (e) Combination | **yes — (b) + (c) + (a), with a turn-starvation amplifier** | see below. |

### The feedback chains, settled one by one

**Chain 1 — `focus()` → `set_input_focus` → `FocusIn`/`FocusOut` → `reconcile_focus()` → …**
`set_input_focus` (`render.rs:1208-1217`) makes the server emit `FocusOut(old)` +
`FocusIn(new)`. `on_focus_in` (`events.rs:891-912`) and `on_focus_out`
(`events.rs:925-940`) each call `reconcile_focus()` (`events.rs:910`,
`events.rs:938`), which is another **blocking `get_input_focus().reply()`**
(`render.rs:1415-1428`). It terminates: `focus()` writes `mon.focused = Some(w)`
*before* reconciling (`render.rs:1232-1237` precisely so that `logical == real`
and the repair arm at `render.rs:1526-1571` is a no-op), and the `input == False`
(`render.rs:1468-1480`) and presented-overlay (`render.rs:1497-1515`) guards
suppress the rest. **But it is not free:** it *doubles the number of blocking
round-trips* for a focus change — 2 from `focus()` plus up to 2 more from the
echo. This is an amplifying, terminating loop. It is the reason the scroll RTT
count is 5–9, not 5.

**Chain 2 — `warp_pointer` → `MotionNotify`/`EnterNotify` → `on_enter` → `focus()` → `warp_pointer`**
**Your suspicion about the guard is correct, and the guard is worse than useless
here.** `pointer_guard_until` is armed in exactly one place — `on_key`
(`events.rs:828-829`) — and cleared in exactly one place — `on_motion`
(`pointer.rs:542`). It is therefore a *keyboard-path* guard only. It does not
bound a warp-induced `EnterNotify` at all. `on_enter` (`events.rs:833-876`) calls
`focus(Some(cw))` whenever `focus_mouse` is on and the entered client differs from
the monitor's focus (`events.rs:870-872`).

**However, with the shipped default (`focus_mouse = false`, `warp_cursor = false`)
this chain cannot run at all** — `on_enter` returns at `events.rs:841` and no
warp is ever issued. It only becomes live for a user who enables both. In that
configuration it is *not* self-perpetuating but it is a **ratchet of up to N
iterations per turn**: `focus(w)` → `retarget_focus_to_window` (`render.rs:1315` →
`commands.rs:92-116`) re-centres the camera on `w`'s column → `arrange` moves `w`
under the stationary pointer → `EnterNotify` on a *different* window → `focus()`
again. It terminates only because `ideal_scroll` clamps at `cam_min`/`cam_max`
(`layout.rs:771-797`) or because the ribbon fits the workarea, i.e. after at most
one pass over the ribbon. Cost: up to N full `focus()` calls, each with 2
blocking RTTs and 36 grab requests, **in a single turn**. Worth fixing (arm
`pointer_guard_until` from `focus()`, not only from `on_key`) but it is not the
reported bug under default config.

**Chain 3 — `arrange()` → `emit_geometry` → `configure_window` + synthetic `send_event` → `ConfigureNotify` → `on_configure_notify`**
**Terminating, but with a non-trivial constant factor.** The synthetic copy is
provably discarded: `send_event` sets bit 7 of `response_type`
(`render.rs:1014-1016`) and the handler drops it at `events.rs:302-304`. The
window-targeted real copy (`event == win`) falls through to `Ok(())` at
`events.rs:362`. Only the **root-targeted** copy (`event == root`, from
`SUBSTRUCTURE_NOTIFY`, `input.rs:72`) is classified
(`events.rs:321-360`). If it is `Stale` — i.e. the echo of an *older* in-flight
configure arrives after a newer record was written — it calls
`reassert_stale` (forget the record, `reconciler.rs:265-268`) and re-asserts
`client.geom` (`events.rs:353-357`). Because `forget` makes `diff` see
`!prev.seen` (`reconciler.rs:145-152`), that always emits exactly **one** extra
`configure_window`, whose own echo is `Compliant`. The document comment at
`reconciler.rs:250-268` is correct: bounded at one extra configure per stale
echo. It is an amplifier, not a loop.

**Chain 4 — `_NET_CLIENT_LIST` rewrite → `PropertyNotify` → WM reacts to its own write**
**Dead end — no amplification.** `flush_client_list` (`ewmh.rs:267-274`) is called
once at the *top* of the turn (`mod.rs:630`) and is only armed by
manage/unmanage or `Effect::MarkRestack` (`actions.rs:81`). The resulting
`PropertyNotify` lands on `on_property` (`events.rs:559-643`) and matches none of
the handled atoms (`WM_NAME` root `events.rs:563`, strut `events.rs:574`,
bypass `events.rs:584`, `WM_NORMAL_HINTS` `events.rs:611`) and `root` is not in
`state.clients` (`events.rs:631`), so it returns at `events.rs:642`. Same for
`_NET_ACTIVE_WINDOW` written at `render.rs:1339-1345`. The WM is correctly
indifferent to its own root property writes.

**Chain 5 (unprompted, real, and the one that makes a *tiled* client a second
saturation source) — client `ConfigureRequest` ← synthetic `ConfigureNotify`.**
`emit_geometry` sends the client a synthetic `ConfigureNotify` every time
(`render.rs:1014-1016`). A tiled client that dislikes the resulting rect answers
with a `ConfigureRequest`; `on_configure_request` for a non-float replies with a
synthetic `ConfigureNotify` and issues **no** request of its own
(`events.rs:174-194`). That is a correct, closed answer — but a toolkit that
re-asks on every notification produces a client↔WM conversation at *the client's*
rate, each iteration costing the WM a full `dispatch()`. It is client-driven and
survives nothing except a restart that re-negotiates the geometry, so it cannot
be ruled out as a co-contributor for a specific app.

### What is bounded, and what is not

**Bounded:**
* the X11 event queue as seen by the WM — drained to empty every turn
  (`mod.rs:638-640`, `mod.rs:672-674`);
* every feedback chain above — each terminates; none has steady-state gain ≥ 1;
* the per-turn `arrange` cost — `hide_offscreen`, the scratch buffers, the
  `stack_overlay` raise storm (cached by `last_stack_order`, `mod.rs:325`) and
  the shape-mask re-upload (cached by `shape_mask_cache`, `render.rs:1082-1085`)
  are all memoised;
* WM memory — Agent B's untraced controls hold RSS flat at 4.9 MB under the same
  load that traced builds push to 116 MB (the trace ring buffer, not a leak).

**Not bounded:**
* **the deficit `arrival − service`.** Once it is positive the backlog is
  `∫(arrival − service) dt` and grows without limit. Nothing in the loop ever
  discards work.
* **the length of one `run_once` turn.** The drain at `mod.rs:638-640` has no
  iteration cap and no yield. Under sustained saturation it does not return, so
  everything after it is **starved**: `flush_client_list` (`:630`),
  `snap_animations` (`:652`), `drain_control` (`:689`), `publish_state` (`:690`).
  Two consequences worth stating: `Effect::MarkRestack` sets
  `client_list_dirty` (`actions.rs:81`) which is therefore never flushed, and
  `maverickctl` latency — not mouse latency — is the honest liveness probe
  because `drain_control` is reached once per *completed* turn.
* **the device queue held by the `SYNC` grab.** This is the reason the failure is
  *sticky* rather than merely laggy: the server will not skip a queued detent,
  so the WM must pay full price for every event it is behind by, forever.

### Verdict on the uncommitted diff

`mod.rs:331-338` (new `arrange_dirty: BTreeSet<usize>`, `deferred_focus:
Vec<(usize, Window)>`), `actions.rs:74-75`, `render.rs:1336-1337`,
`mod.rs:699-724` (`flush_deferred_arrange`).

**It does not bound pending work, and it introduces a new unbounded structure.**

1. `deferred_focus` is a `Vec` that takes **one entry per `focus()` call within a
   turn** (`render.rs:1337`) and is only drained by `std::mem::take` at
   `mod.rs:707`. Nothing caps it. Under exactly the condition that produces the
   bug (a turn that never completes), it is unbounded. Yes — **it should be a
   map keyed by monitor with a single "last focused window" value**, or better a
   plain `Option<Window>`: the loop at `mod.rs:708-721` currently issues *N*
   `warp_pointer` requests and *N* `stack_overlay` passes for what is, at most,
   one final cursor position. It should be coalesced to one warp to the last
   focused window.
2. The flush is placed at `mod.rs:644`, **after** the unbounded drain. So when the
   turn does not complete, `arrange_dirty`/`deferred_focus` never flush: geometry
   goes stale *and* memory grows. That is a liveness regression relative to HEAD,
   where `focus()` arranged inline.
3. The second drain (`mod.rs:672-674`) has **no** flush after it at all, so
   anything it dispatches is owed until the next turn's `mod.rs:644`.
4. **Coalescing `arrange` alone bounds nothing on the dominant path.** `focus()`
   still performs, per call, unconditionally: `set_input_focus`
   (`render.rs:1208`), `has_protocol` — a blocking `get_property().reply()`
   (`render.rs:1218`, `ewmh.rs:298-301`) — `send_proto` (`render.rs:1219`),
   `reconcile_focus` — a blocking `get_input_focus().reply()`
   (`render.rs:1253` → `render.rs:1415-1428`) — `change_window_attributes`
   (`render.rs:1260-1262`) and **`grab_buttons` = 18 requests**
   (`render.rs:1263` → `input.rs:365-432`). The diff removes the *CPU/arrange*
   term and leaves the *serialising round-trip* and *request-volume* terms
   untouched. Since the amplification that decides which input type saturates
   first is `3·Δ·A` (the `configure_window` fan-out), and `A` collapses, that
   helps — but the `FocusIn`/`FocusOut` echo RTTs and the 54 grab requests per
   notch are untouched.
5. One thing the diff gets **right**: the deferred `warp_pointer` reads
   `client.geom` *after* `arrange` has run (`mod.rs:703-706` before
   `mod.rs:711-716`), so it warps onto the post-arrange rect, matching what
   `HEAD:render.rs:1333-1358` did. No stale-geometry regression there.
6. Deferring `Effect::ArrangeMonitor` (`actions.rs:74-75`) is safe for the
   invariant question: no consumer of `client.geom` runs between the mutation and
   the flush. `find_client` hit-tests on the *window id*
   (`manage.rs:1221-1224`), not on `geom`; `on_configure_notify` echoes are only
   processed in the *next* turn's drain, by which time the previous turn's flush
   has already written `client.geom` (`render.rs:1037-1046`); and
   `retarget_focus_to_window`'s `#[must_use]` monitor is still consumed
   (`render.rs:1336`). The one residual is the `Stale` arm at `events.rs:353-357`,
   which can re-assert a `client.geom` that a same-turn pending arrange is about
   to replace — bounded churn, not divergence.

---

## causal path

### The hot path, end to end

```
run_once()                                         mod.rs:609
 └─ flush_client_list(); conn.flush()              mod.rs:630-631
 └─ while let Some(ev) = conn.poll_for_event()     mod.rs:638-640   <-- UNBOUNDED, NO YIELD
     └─ dispatch(ev)                                mod.rs:342-418
        └─ on_button_press(detail >= 4, Mod4)       pointer.rs:139,156-174
           │   [device is FROZEN here: GrabMode::SYNC, input.rs:385-395]
           ├─ scroll_camera_with_wheel              pointer.rs:668-686
           │  ├─ engine.dispatch(FocusDir)         pointer.rs:680
           │  │  └─ run_effects                     actions.rs:63-66
           │  │     ├─ Effect::Unfocus(from)        commands.rs:853 -> render.rs:1390
           │  │     │    = change_window_attributes(1) + grab_buttons(18)
           │  │     ├─ Effect::ArrangeMonitor(mi)   commands.rs:856 -> render.rs:592-669
           │  │     │    = O(N) CPU, N x [configure_window + send_event]
           │  │     │      -> 3N ConfigureNotify back to the WM
           │  │     └─ Effect::FocusWindow(w)       commands.rs:857 -> render.rs:1140
           │  │          = set_input_focus(1) + get_property.RTT
           │  │            + send_event(1) + get_input_focus.RTT
           │  │            + change_window_attributes(1) + grab_buttons(18)
           │  │            + retarget_focus_to_window (O(cols)) commands.rs:92-116
           │  │            + arrange  (2nd O(N) pass, N x 2 requests)
           │  │            + stack_overlay (O(N))  + change_property32(1)
           │  └─ focus_column_at(px, py)            pointer.rs:684,691-723
           │     └─ focus(Some(w2))  [HEAD only, :722-727]  <-- the duplicate
           │          = the entire 30-request + 2-RTT block, AGAIN
           └─ allow_events(ASYNC_POINTER).check()   pointer.rs:166-168   [1 RTT, thaws device]
 └─ flush_deferred_arrange()                       mod.rs:644       [DIFF ONLY — starved]
 └─ snap_animations()                              mod.rs:652       [starved]
 └─ wait_readable_fds / second drain               mod.rs:660-674   [starved]
 └─ drain_control(); publish_state()               mod.rs:689-690   [starved -> control latency]
```

Plus, asynchronously, the amplified return traffic:
`FocusOut`/`FocusIn` → `on_focus_out`/`on_focus_in` → `reconcile_focus()` →
**one more blocking `get_input_focus` RTT each** (`events.rs:910`, `events.rs:938`).

### The `focus_column_at` deletion: a semantic regression, settled

**Your suspicion is right, and it is worse than "leaves X focus on the wrong
column".**

The deleted block was (`HEAD:pointer.rs:722-727`):

```rust
if let Some(ci) = col {
    self.engine.state.monitors[mi].workspaces[ws_i].focus.column_idx = ci;
    if let Some(w) = ...columns[ci].focused_win() { let _ = self.focus(Some(w)); }
}
```

**The hit test itself is stale by exactly one notch.** `column_screen_extents`
computes the column rects from `ws.camera.position` (`layout.rs:750`), with no
`Phase` parameter. But the arrange that ran one line earlier in the same handler
projected with `Phase::Settled`, i.e. `ws.camera.target` (`layout.rs:554-557`,
reached via `render.rs:597`). `Camera::retarget` moves only `target`
(`maverick-core/src/types.rs:514-530`); `position` is not updated until
`snap_animations()` at the end of the turn (`maverick-core/src/types.rs:2635-2654`,
called at `mod.rs:652`). So `focus_column_at` hit-tests the pointer against the
layout that was on screen **before this notch's scroll**, while the screen has
already been reconfigured to the layout after it.

Consequences, in the two regimes:

* **Short ribbon (whole ribbon fits the workarea — `ideal_scroll` returns a
  constant, `layout.rs:792-794`).** `position == target`, the hit test is
  accurate, and the deleted `focus()` was focusing the window `FocusDir` had just
  focused. The deletion is a **pure win**: it removes a full duplicate `focus()`
  (2 RTTs + 36 grab requests + 1 O(N) arrange) with **zero** semantic change.
  This is the common case and is most of the scroll bug.
* **Scrollable multi-column ribbon.** The stale hit test names the *previous*
  column. Under `HEAD`, the deleted `focus(Some(w2))` then wrote
  `monitors[mi].focused = w2` **and** called `retarget_focus_to_window`
  (`render.rs:1315`), which retargets `camera.target` back to *that* column and
  arranges again — i.e. `HEAD` scrolled one column right and immediately scrolled
  back. **Under the diff, `ws.focus.column_idx = ci` (pointer.rs:719-721) is still
  written but nothing re-focuses**, so the tree ends up split-brained:
  `monitors[mi].focused` and the camera point at `FocusDir`'s column `ci_new`,
  while `ws.focus.column_idx` names `ci`. That breaks three things:
  - the **accordion boost**, which is derived from `ws.focus.column_idx`
    (`maverick-core/src/types.rs:2639-2648`);
  - the **next `FocusDir` step**, whose base *is* `ws.focus.column_idx`
    (`commands.rs:704`) — so subsequent notches walk from the wrong column;
  - the codebase's own stated contract: "Keep `ws.focus.column_idx` in sync (the
    backend's focus handler does this too)" (`core/invariants.rs:194-203`).
    `check_invariants` does not catch it (`maverick-core/src/types.rs:2304-2343`
    only range-checks the index), so it fails silently.

**Verdict on the deletion: keep it, but also delete the now-orphaned
`ws.focus.column_idx = ci` write at `pointer.rs:719-721`**, or — better — fix the
hit test to use the settled camera so the original two-line contract holds. As the
diff stands, the deletion trades a duplicate `focus()` for a silent
focus/column split-brain. That is a real regression in the uncommitted tree,
independent of the saturation bug.

### Net cost of one `Mod4`+wheel notch, and the saturation condition

Per notch: **~102 X requests** (54 of them grab/ungrab), **5–9 blocking
round-trips**, **3 O(N) arrange passes + 5 O(N) restack passes** (~6 O(N)
allocations), and **≈6N + 6 X events** returned for the WM to dispatch
(≈20–40× amplification; Agent B measured ~52). Per click: ~40 requests,
**3–4 blocking round-trips**, **1 O(N) arrange**, **≈3–3N events** (≈3–21×).

Saturation is therefore the ordinary queueing condition
`arrival_rate > service_rate`, and the two are separated by *total event load*,
not by *input rate* — which is exactly why the same harness sustains 500 clicks/s
(≈6 500 dispatched events/s, bounded) while falling behind at 200 notches/s
(≈10 600 dispatched events/s, growing).

---

## why restart clears it

`restart()` (`actions.rs:131-162`) is an in-place `exec()` of the same binary
with the same argv (`actions.rs:153-159`, with the `--session-id` preservation
added by this working tree at `actions.rs:436-443`). Three things are reset, and
the first is the decisive one:

1. **A brand-new X connection with an empty per-client event buffer.** The old
   connection's fd is explicitly set `FD_CLOEXEC` before `exec`
   (`actions.rs:140-151`), so the socket is closed. The X server discards
   *everything* queued for the dead client and releases any grab it held —
   including the `GrabMode::SYNC` button grab (`input.rs:385-395`) that was
   holding the device frozen. The accumulated `arrival − service` deficit is
   destroyed server-side, atomically, and no amount of draining could have
   discarded it that fast.
2. **All WM state rebuilt from scratch** by `WindowManager::new`
   (`mod.rs:765+`): fresh `AppliedState` records, fresh `focus_stack`s, cameras
   reset via `Camera::new` (`maverick-core/src/types.rs:498-508`), and — for the
   uncommitted diff — empty `arrange_dirty`/`deferred_focus`
   (`mod.rs:926-927`).
3. **A re-negotiated geometry with every window's client re-managed**
   (`manage.rs:570`, `manage.rs:1302`), so the `AppliedState` record and the
   server agree again and the `Stale` arm at `events.rs:353-357` starts from
   `Compliant`.

It is worth being explicit about what restart does **not** clear, because it
rules out the alternative explanations: it does not change `N`, the number of
managed windows, and it does not change any per-event cost. A restart that
merely dropped the backlog without a structural change would therefore buy
exactly as much time as letting the input stop — which is what the measurements
show (~0.4 s recovery, `SATURATION-MEASUREMENTS.md` §6). Restart buys more than
that only because the *server-side* buffer is destroyed rather than drained, and
because in a real session the input does not actually stop.

---

## why it returns

Because nothing in the restart touched the structural cause. Every one of the
following is unchanged by `exec`, and each is on the hot path:

| structural cause | site | per-notch share |
|---|---|---|
| `focus()` has no already-focused early return | `render.rs:1140-1388` | doubles the whole `focus()` cost whenever the duplicate is a no-op |
| the duplicate `focus()` in `scroll_camera_with_wheel` | `pointer.rs:684` + `HEAD:pointer.rs:722-727` | ~50 % of the scroll notch's requests and RTTs |
| `grab_buttons` = 18 requests, called on every focus **and** unfocus | `input.rs:365-432`, `render.rs:1263`, `render.rs:1395` | 54 requests/notch |
| `has_protocol` blocking `get_property().reply()` | `render.rs:1218` → `ewmh.rs:298-301` | 1 RTT per `focus()` |
| `reconcile_focus` blocking `get_input_focus().reply()` | `render.rs:1253` → `render.rs:1415-1428` | 1 RTT per `focus()` **plus 1 more per `FocusIn`/`FocusOut` echo** (`events.rs:910`, `events.rs:938`) |
| `arrange` re-entered from `focus()` and from `Effect::ArrangeMonitor` | `render.rs:1333` (HEAD), `commands.rs:856` | 3 O(N) passes/notch |
| `3N` server events per geometry write | `render.rs:983-1016` + `manage.rs:477-485` + `input.rs:72` | the 20–40× multiplier |
| `GrabMode::SYNC` button grab with `allow_events` as the **last** statement of the handler | `input.rs:385-395`; `pointer.rs:160-168`, `pointer.rs:474-483` | makes the deficit unskippable → sticky, not merely laggy |
| `run_once`'s drain has no cap and no yield | `mod.rs:638-640` | turn starvation; every post-drain stage (`flush_client_list`, `snap_animations`, `drain_control`) is skipped while saturated |

The same stress therefore re-saturates the same connection at the same threshold,
and a faster stress reaches it sooner because the deficit integrates faster. The
uncommitted diff removes the `arrange` term from that table and adds two new
unbounded structures (`deferred_focus`, and a flush placed after an unbounded
drain), which is a net regression on the liveness axis even though it is a net
win on the CPU axis in the common case.

**Cheapest structural wins, in the order the evidence supports:**
1. `allow_events` **first**, before the handler body — the handler already holds
   everything it needs from the event struct (`pointer.rs:160-168`,
   `pointer.rs:474-483`). This alone stops the `SYNC` grab from making the
   backlog unskippable.
2. Delete the duplicate `focus()` in `focus_column_at` **and** the orphaned
   `ws.focus.column_idx = ci` write (`pointer.rs:719-721`), or make
   `column_screen_extents` take a `Phase` so the hit test matches the geometry
   that is actually on screen (`layout.rs:750`).
3. Give `focus()` an early return for `prev_focused == valid_win` on the
   mouse-repeat path (`render.rs:1188`), or cache `WM_PROTOCOLS` at map time so
   `has_protocol` (`ewmh.rs:293-306`) stops being a round-trip.
4. Replace `grab_buttons`' 18 requests with a single `ANY`/`ANY` grab plus the
   one `Mod4` grab actually needed (`input.rs:365-432`), or re-grab only on the
   transitions that matter rather than on every focus change.
5. Cap the drain loop (`mod.rs:638-640`) so a turn always completes and
   `drain_control` (`mod.rs:689`) is always serviced — and if the deferral diff
   is kept, make `deferred_focus` a `BTreeMap<usize, Window>` and move
   `flush_deferred_arrange` to *before* the drain, not after.
