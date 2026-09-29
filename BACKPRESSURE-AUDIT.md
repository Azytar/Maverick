# Backpressure Audit — State Management & Backpressure Architecture

**Agent C scope.** Question answered: *does input cause `event → immediate layout → immediate reconcile → immediate X11 work` for every event, and where does backpressure belong?*

**Answer: yes, on every input event that changes focus, and the architecture forces it.** `WindowManager::focus()` (`src/backend/x11/render.rs:1140`) unconditionally runs the full `arrange → layout::arrange → present_into → DesiredState → reconcile → emit_geometry → stack_overlay` pipeline (`render.rs:604-669`) *inside the event dispatch*, and it is reached from every focus-changing input event. There is exactly one batching boundary available — the loop turn — and the uncommitted attempt uses it, but places the flush in the wrong position and adds an unbounded, semantically redundant queue.

Line numbers refer to the **working tree with the uncommitted diff applied**. Pre-diff sites are called out explicitly.

---

## architecture: event to x11

### The loop turn

`run_once()` (`src/backend/x11/mod.rs:609-695`) is the entire event loop body. It has **two** drain loops, not one:

```
mod.rs:612   trace::begin_turn()
mod.rs:616-624  SIGCONT regrab / SIGTERM quit        ── quit path RETURNS EARLY, line 623
mod.rs:630      flush_client_list()                   ← deferred EWMH property write
mod.rs:631      conn.flush()                          ← THE ONLY EXPLICIT SOCKET FLUSH IN THE TURN
mod.rs:638-640  DRAIN #1  while poll_for_event() { dispatch(ev) }
mod.rs:644      flush_deferred_arrange()              ← [uncommitted] coalesced reconcile
mod.rs:652-654  snap_animations(); anim_per_mon.clear(); animating = false
mod.rs:660-661  fd; timeout = wait_timeout(None, kbd_refresh_due, shutdown_deadline)
mod.rs:663-675  if timeout != ZERO { wait_readable_fds(...); DRAIN #2 while poll_for_event() { dispatch(ev) } }
mod.rs:679-685  kbd_refresh_due expired → refresh_keyboard()
mod.rs:689-690  drain_control(); publish_state()
mod.rs:694     return
```

Two facts follow immediately and both matter for the design:

1. **`wait_timeout(None, kbd_refresh_due, shutdown_deadline)` returns `None` whenever neither deadline is armed** (`mod.rs:140-151`). With `frame = None`, `keyboard = None`, `shutdown = None` the function returns `None`, and `mod.rs:663` (`None != Some(ZERO)`) is true, so the WM enters `wait_readable_fds(&fds, timeout)` with **no timeout at all**. The loop is deliberately event-driven with no frame clock — `mod.rs:656-659` states this as an invariant ("no heartbeat, no timer").
2. **`conn.flush()` at `mod.rs:631` is the *only* explicit flush, and it sits at the top of the turn — before both drains.** Anything a drain (or `flush_deferred_arrange`) issues stays in x11rb's `write_buffer` until the *next* turn's line 631 (`maverick-x11/src/lib.rs:109` `pub type XConn = XCBConnection`; x11rb buffers requests and writes them on `flush()` or when the buffer fills — `x11rb-0.13.2/src/rust_connection/mod.rs:303-315`, `:421-435`, `:781-785`). See *stale-read hazards* H8 for why this matters.

### The dispatch → state → effect → X11 chain

```
poll_for_event                                   mod.rs:638 / mod.rs:672
  └─ WindowManager::dispatch                     mod.rs:342
       ├─ Event::ButtonPress → on_button_press    mod.rs:348 → pointer.rs:139
       │    ├─ detail ≥ 4 + Mod4 → scroll_camera_with_wheel   pointer.rs:156-170
       │    │    └─ engine.dispatch(Action::FocusDir)          pointer.rs:680
       │    │         └─ Engine::dispatch → Command::execute   actions.rs:52
       │    │              (mutates State: ws.focus.column_idx, camera.retarget)  commands.rs:729-730
       │    │              returns Vec<Effect>  (Unfocus, ArrangeMonitor, FocusWindow, PublishIpcState)
       │    │         └─ run_effects(effects) → execute() per effect   actions.rs:59-67
       │    │              ├─ Effect::Unfocus(w)  → unfocus()           actions.rs:84  render.rs:1390
       │    │              ├─ Effect::ArrangeMonitor(mi) → arrange_dirty.insert(mi)   actions.rs:74-76   [uncommitted]
       │    │              ├─ Effect::FocusWindow(w) → focus(w)         actions.rs:83  render.rs:1140
       │    │              └─ Effect::PublishIpcState → publish_state() actions.rs:114  actions.rs:414
       │    │    └─ focus_column_at(px, py)                            pointer.rs:684 → pointer.rs:691
       │    │         └─ (pre-diff: self.focus(Some(w)) — REMOVED by the diff)
       │    └─ detail < 4  → find_client + self.focus(Some(cw))        pointer.rs:261-268
       ├─ Event::ButtonRelease → on_button_release pointer.rs:487  (drag end: ungrab_pointer + arrange, :508/:524)
       ├─ Event::MotionNotify  → on_motion        pointer.rs:536  (drag: MoveResize command)
       ├─ Event::EnterNotify   → on_enter         events.rs:833  (focus-follows-mouse → focus(), :871)
       ├─ Event::FocusIn/Out   → reconcile_focus  events.rs:891
       ├─ RandR / Map / Unmap / ConfigureRequest / Destroy → manage.rs, events.rs, struts.rs
       └─ Event::KeyPress      → on_key           events.rs:772 → do_action → engine.dispatch → run_effects
```

### `focus()` is the funnel, and it is where the architecture forces the pass

`focus()` (`render.rs:1140`) is reached from **every** focus-changing input: scroll (`pointer.rs:680` → `Effect::FocusWindow`), click (`pointer.rs:265`, `pointer.rs:328`), focus-follows-mouse (`events.rs:871`), `_NET_ACTIVE_WINDOW` (`manage.rs`), manage/unmanage re-focus, and every keyboard navigation. Per call it performs, in order:

| Step | Site | X11 cost |
|---|---|---|
| read `prev_focused` | `render.rs:1156-1161` | 0 |
| `unfocus(prev)` if focus changes | `render.rs:1188-1194` → `render.rs:1390-1397` | `change_window_attributes` + `ungrab_button` |
| `set_input_focus` | `render.rs:1207-1217` | 1 request |
| **`has_protocol(w, WM_TAKE_FOCUS)`** | `render.rs:1218` → `ewmh.rs:293-306` | **`get_property(...).reply()` — blocking RTT** |
| `send_proto(WM_TAKE_FOCUS)` | `render.rs:1219` → `ewmh.rs:308-329` | 1 request |
| write `mon.focused` + `focus_stack` | `render.rs:1232-1237` | 0 (authoritative state) |
| `assert_invariants()` | `render.rs:1248-1249` | 0, `#[cfg(debug_assertions)]` |
| **`reconcile_focus()`** | `render.rs:1253` → `render.rs:1412` | **`get_input_focus().reply()` — blocking RTT** (`render.rs:1415-1416`) |
| `change_window_attributes(border_pixel)` | `render.rs:1260-1262` | 1 request |
| `grab_buttons(w, true)` | `render.rs:1263` → `input.rs:365-432` | `ungrab_button` + `grab_button` + 2 grabs × `mod_variants` (`input.rs:370,385,414-430`) — **un-diffed, N regrabs per burst** |
| consume `URGENT`, `write_net_wm_state` | `render.rs:1265-1281` | 0-1 |
| `sync_presented_maximize(mi)` | `render.rs:1295` | 0 |
| `retarget_focus_to_window(...)` → camera retarget | `render.rs:1315` → `commands.rs:92-117` | 0 (authoritative state) |
| **full `arrange` + `stack_overlay`** | pre-diff `render.rs:1333,1336`; uncommitted `render.rs:1336-1337` | **O(windows) projection + diff + configures** |
| `change_property32(_NET_ACTIVE_WINDOW)` | `render.rs:1339-1345` | 1 request |
| `notify(FocusChanged)` | `render.rs:1380-1385` | 0 |

So per focus change: **2 blocking round-trips, ~10 fire-and-forget X11 requests, one full O(N) projection+reconcile pass.** That is the per-event cost the architecture mandates, and it is independent of whether the resulting geometry differs from what X11 already has.

### The arrange pass, in full

`arrange(mi)` (`render.rs:524-526`) → `arrange_full` (`render.rs:592-598`) → `arrange_full_phase(..., Phase::Settled)` (`render.rs:604-669`):

```
render.rs:614-616   if do_hide && self.drag.is_none() → hide_offscreen(mi)      O(clients on monitor)
render.rs:619-627   layout::arrange(state, mon_idx, cfg, registry, Phase::Settled, &mut self.desired, &mut self.ribbon_scratch)
render.rs:631-636   present_into(state, &monitors[mi], &mut self.desired, &mut self.present_scratch)
render.rs:639       DesiredState::from_placements(&self.desired, &self.present_scratch)   ← 2 fresh Vec allocs (desired.rs:56,67)
render.rs:640       reconcile(&desired, &state, &mut self.applied)  → Vec<GeometryEffect>  (reconciler.rs:199-246)
render.rs:645-649   mirror desired rects into client.last_desired (observability only)
render.rs:650-653   for each effect → emit_geometry(win, rect, border, true)
render.rs:663       self.desired.clear()
render.rs:666       stack_overlay(mon_idx)
render.rs:667       monitors[mi].layout_dirty = true
```

`emit_geometry` (`render.rs:955-1056`) is the only writer of X11 geometry. Per emitted window: `configure_window` (`:983-991`), a **synthetic `ConfigureNotify` `send_event`** (`:999-1016`), a conditional `_NET_FRAME_EXTENTS` rewrite (`:1026-1035`), `client.geom = wire` (`:1042`), `client.geometry_dirty = false` (`:1045`), `monitors[mon].layout_dirty = true` (`:1049-1051`), and `sync_rounded_frame` (`:1053`).

That synthetic `ConfigureNotify` is the mechanism behind the ~52 server events per scroll notch measured in `SATURATION-MEASUREMENTS.md` §4: each `emit_geometry` makes the server echo back to us, and those echoes land in the same queue we are draining.

### Where backpressure is *absent*

- **No gate on `focus()`.** There is no early return for `prev_focused == valid_win`. `render.rs:1188` uses that comparison only to decide whether to call `unfocus`; everything after runs unconditionally.
- **`Effect::ArrangeMonitor` had no dirty gate** (pre-diff `actions.rs:74`: `Effect::ArrangeMonitor(mi) => self.arrange(mi)?`).
- **Nine call sites call `self.arrange(mi)` directly**, bypassing the effect pipeline entirely, and the uncommitted diff leaves all nine immediate: `pointer.rs:524` (drag release), `actions.rs:395-398` (config reload — loops *all* monitors), `manage.rs:570` (new window mapped), `manage.rs:733`, `manage.rs:1166` (transient re-parenting), `struts.rs:269` and `:289` (dock strut add/remove), `events.rs:509` and `:528` (RandR topology / resolution change). **This partial deferral is a correctness problem in its own right** — see §*proposed dirty-state design*, P3.

---

## the per-event cost chain

For a burst of `scroll +1 ×4` (four Mod4+wheel notches), **pre-diff**, per notch:

```
on_button_press                             pointer.rs:139
├─ SyncGrabGuard constructed                pointer.rs:145-150      (no X cost)
├─ scroll_camera_with_wheel(detail,px,py)   pointer.rs:160
│  ├─ engine.dispatch(FocusDir(dir))        pointer.rs:680
│  │  ├─ FocusDirection::execute            commands.rs:678-…
│  │  │   mutates ws.focus.column_idx, camera.retarget(ideal_scroll)   commands.rs:729-730
│  │  │   emits Unfocus, ArrangeMonitor(mi), FocusWindow(Some(w)), PublishIpcState
│  │  └─ run_effects                       actions.rs:59-67
│  │     ├─ Unfocus(f)     → 2 requests     render.rs:1390
│  │     ├─ ArrangeMonitor → arrange(mi)    [full pass #1]
│  │     ├─ FocusWindow(w) → focus(w)       [full pass #2 + 2 RTTs]
│  │     └─ PublishIpcState → state_json + string compare   actions.rs:419-423
│  └─ focus_column_at(px,py)                pointer.rs:684 → 691
│     ├─ column_screen_extents(...)         layout.rs:716-726
│     ├─ ws.focus.column_idx = ci           pointer.rs:719-721
│     └─ self.focus(Some(w))                pointer.rs:725 (pre-diff)  [full pass #3 + 2 RTTs]
└─ allow_events(ASYNC_POINTER, t).check()   pointer.rs:166-168      (round-trip: `.check()`)
```

Measured, this is exactly **3 arrange passes, 2 `focus()` calls, 4 synchronous RTTs, ~52 amplified server events per notch** (`SATURATION-MEASUREMENTS.md` §2, §5). The third pass and the second pair of RTTs are pure redundancy: `focus_column_at` re-focuses the window `FocusDirection` already focused whenever the pointer sits over the column the camera just moved to.

For the whole burst of 4: **12 arrange passes where 1 suffices.**

Click (`pointer.rs:261-268`): `find_client` (hash hit, or `query_tree` RTT for child windows, `manage.rs:1230`) → `focus()` (2 RTTs + 1 arrange) → `allow_events(...).check()` (`pointer.rs:474`). 1 arrange per click.

Motion during a drag (`pointer.rs:536-661`): **no arrange, no RTT.** `on_motion` computes the rect from `start_geom` + the *absolute* pointer delta (`pointer.rs:555-640`) and routes it through `MoveResize` → `Effect::ConfigureWindow` → `apply_geom` (`render.rs:1089-1113`), which diffs against `AppliedState` and emits at most one `configure_window` + one `send_event`. Motion is already effectively coalesced — but **one event at a time**, not in batches.

---

## coalescible vs order-sensitive (explicit table)

The distinction is: *is the observable result a function of the final state only, or of the sequence?*

### Coalescible — only the final resulting state is observable

| Event / effect | Why last-wins is sound | Authoritative accumulator |
|---|---|---|
| **Mod4+wheel notch** (`pointer.rs:160`) | `camera.retarget` (`commands.rs:729`) and `ws.focus.column_idx` (`commands.rs:730`) are overwrite accumulators; no intermediate camera position is ever displayed (`Phase::Settled`, `mod.rs:652`, `render.rs:597`) | `ws.camera.target`, `ws.focus.column_idx`, `monitors[mi].focused` (`render.rs:1234`) |
| **Keyboard focus nav (h/l/j/k)** | same accumulators; `KeyPress` is not paired with anything | same |
| **MotionNotify during a drag** (`pointer.rs:554-640`) | the rect is a **pure function of the latest absolute pointer position** (`e.root_x/root_y` minus `drag.ptr_x/ptr_y`, `pointer.rs:555-556`); `start_geom` is frozen at press (`pointer.rs:424`). Intermediate positions are pure overdraw | `DragState.ptr_x/ptr_y` (`pointer.rs:425-426`) |
| **`Effect::ArrangeMonitor(mi)`** (`actions.rs:74`) | arrange is a projection of `(State, Phase)`; running it on the final state yields the same `Desired` | `State` |
| **`stack_overlay`** (`render.rs:790`) | already diff-guarded by `last_stack_order` (`render.rs:891-900`); only the final order is observable | `last_stack_order` (`mod.rs:325`) |
| **`warp_pointer`** (post-focus, `render.rs:1348-1357` pre-diff / `mod.rs:710-720` post-diff) | final pointer position wins; and coalescing **breaks** the current warp → `MotionNotify` → `EnterNotify` → `focus()` feedback loop, which is a bug amplifier | `monitors[mi].focused` |
| **`hide_offscreen`** (`render.rs:671`) | acts only on the `wm_hidden` **transition** (`render.rs:728-750`); N passes and 1 pass reach the same latch state | `client.wm_hidden` |
| **`publish_state` / `flush_client_list`** | already deferred + diffed: `actions.rs:414-423` (JSON compare), `ewmh.rs:267-274` (`client_list_dirty`) | `last_state_json`, `client_list_dirty` |
| **`_NET_FRAME_EXTENTS`, Shape mask** | diff-cached in `frame_extents` (`render.rs:1026`) and `shape_mask_cache` (`render.rs:1082`) | those caches |

### Order-sensitive — the sequence itself is the semantics

| Event / effect | What breaks if reordered or merged |
|---|---|
| **ButtonPress ↔ ButtonRelease pairing** | press installs `self.drag` + `grab_pointer` (`pointer.rs:401-431`, itself a `.reply()` RTT at `:414`); release must `ungrab_pointer(...).check()?` (`pointer.rs:508`) and arrange (`pointer.rs:524`). A dropped/reordered release leaks an **active pointer grab** = permanently frozen input. `SyncGrabGuard`'s `Drop` (`pointer.rs:88-108`) exists precisely because this contract is fragile. **Never coalesce.** |
| **`allow_events(ASYNC/REPLAY_POINTER, time)`** (`pointer.rs:167`, `:172`, `:483`) | must be issued exactly once per press. `on_button_press` runs under a `SYNC` button grab installed by `grab_buttons` (`input.rs:385-395`); if `allow_events` is skipped or duplicated, the pointer freezes (see the `FREEZE-RISK` guard at `pointer.rs:84-108`). **Never drop or merge.** |
| **`unfocus(prev)` before `focus(new)`** (`render.rs:1188-1194`) | border repaint + button regrab must both land; `focus_stack` is an MRU list whose **order is client-visible** (`render.rs:1235-1236`, invariant clause #8/#8b asserted at `render.rs:1248-1249`) |
| **`set_input_focus` → `WM_TAKE_FOCUS`** (`render.rs:1207-1220`) | ICCCM 4.1.7 ordering; the protocol message must follow the focus request and carry a real timestamp (`ewmh.rs:314-319`) |
| **`_NET_ACTIVE_WINDOW` announcements** (`render.rs:1339-1345`) | written **per focus change, immediately**. Taskbars and pagers observe the *sequence*; merging them changes what clients see. The uncommitted diff correctly leaves these immediate. |
| **`Effect::ConfigureWindow` during a drag** (`pointer.rs:652-655` → `render.rs:1089`) | `apply_geom` writes `client.geom` (`render.rs:1042`), which `arrange()` **reads back** for floats (`layout.rs:699-702`). Drag geometry must be committed before any projection that consumes it. |
| **`MarkRestack` → `ArrangeMonitor`** (`actions.rs:77-82`, `:74`) | the restack request must be recorded before the pass that would consume it — the codebase already asserts this ordering in comments at `actions.rs:79-81` |
| **`SetFullscreen`/`SetMaximized` → `ArrangeMonitor`** (`actions.rs:91-92`, `:74`) | the presentation flags must be committed before `present_into` (`present.rs:57-68`) reads them |
| **`retarget_cameras(mi)` → `arrange(mi)`** (`struts.rs:268-269`, `events.rs:508-509`, `:527-528`) | explicitly documented as load-bearing three times: `struts.rs:266-268`, `events.rs:496-508`, `events.rs:523-527`. A stale `camera.target` writes geometry thousands of pixels off the end of the ribbon. |

---

## critical path (must stay ordered)

Three things must be **completely untouched** by any coalescing scheme:

1. **Event dispatch order itself.** `dispatch()` (`mod.rs:342`) runs events in arrival order and each `Command::execute` (`engine.rs`) mutates `State` synchronously. Backpressure must be inserted strictly *below* the state layer, never above it. Any design that batches *events* (rather than *derived work*) is wrong for this codebase, because the state transitions are the observable semantics — `ws.focus.column_idx`, `camera.target`, `focus_stack`, `pending_focus`, `wm_hidden`, `URGENT` are all state, not work.

2. **Press/release and grab accounting.** `SyncGrabGuard` (`pointer.rs:88-108`), the SYNC `grab_button` (`input.rs:385-395`), `allow_events` (`pointer.rs:167`, `:172`), `grab_pointer` (`pointer.rs:401-415`), `ungrab_pointer` (`pointer.rs:508`). This is the one subsystem where "drop a redundant event" is *input corruption*, not a performance trade.

3. **The `retarget → project` ordering invariant** (`struts.rs:266-268`, `events.rs:496-508`, `events.rs:523-527`): every `camera.target` mutation must precede the settled projection that writes `client.geom` from it. A deferred pass must never observe a `camera.target` that was mutated *after* it was scheduled — or, rather, it must always project from the *latest* target, which is exactly what "flush at end of turn" gives you and "flush at schedule point" does not.

### What a coalesced pass must reproduce exactly

For the coalesced pass to be equivalent to N immediate passes, **every field the pass reads must be either (a) authoritative `State` mutated synchronously by the events, or (b) a pure function of (a) plus the pass's own previous output.** The only field that violates (b) is `client.geom` — see *stale-read hazards*.

---

## idempotence: does `AppliedState` already suppress no-op x11 work

**Yes — fully, at the geometry level. This is the single strongest fact in the audit and it is what makes deferral safe in principle.**

`AppliedState` (`reconciler.rs:92-94`) is a `HashMap<WindowId, AppliedWindow>`; `AppliedWindow` (`reconciler.rs:77-88`) is `{ rect, border_w, seen, sequence }` — "what X11 is holding right now".

`reconcile(desired, state, applied)` (`reconciler.rs:199-246`) is documented as *"Pure with respect to logical state… mutates ONLY `applied`"* (`reconciler.rs:196-198`) and it does exactly that:

- dedupes duplicate placements by last index, deterministically (`reconciler.rs:215-225`, with the rationale at `:204-214`),
- reads `client.geometry_dirty` purely as an input flag (`reconciler.rs:226-229`),
- calls `applied.diff(win, rect, border, dirty)` (`reconciler.rs:230`).

`AppliedState::diff` (`reconciler.rs:130-156`) emits **only** when
`geometry_dirty || !prev.seen || prev.rect != want_rect || prev.border_w != want_bw` (`reconciler.rs:146-147`),
and it compares the **wire** geometry (`reconciler.rs:144`, `wire_geometry` at `:110-120`) rather than the request — so the record can never permanently disagree with the server and re-emit forever. Proven by the suite: `reconciler.rs:538` `desired_equals_applied_produces_no_effect`, `reconciler.rs:1497` `a_repeated_reconcile_emits_nothing`, `reconciler.rs:336` `changed_rect_emits_only_the_delta`.

The same discipline is applied at every other emit site, and it is worth noting because it shows the codebase already believes in this model:

| Cached sink | Cache | Guard |
|---|---|---|
| stacking raises | `last_stack_order: HashMap<usize, Vec<WindowId>>` (`mod.rs:325`) | `render.rs:891-900` |
| `_NET_FRAME_EXTENTS` | `frame_extents: HashMap<Window, u32>` (`mod.rs:245`) | `render.rs:1026-1035` |
| Shape `BOUNDING` mask | `shape_mask_cache` (`mod.rs:240`) | `render.rs:1082-1085` |
| fullscreen-covering raise | `fs_covering: HashMap<usize, Option<WindowId>>` (`mod.rs:330`) | `render.rs:919-940` |
| `_NET_CLIENT_LIST` | `client_list_dirty: bool` (`mod.rs:226`) | `ewmh.rs:267-274` |
| IPC state JSON | `last_state_json: String` (`mod.rs:304`) | `actions.rs:419-423` |

### What is **not** idempotent

Idempotence at the *request* level does not make the pass cheap or idempotent at the *CPU/allocation* level:

| Per-call cost | Site | Amortized? |
|---|---|---|
| `layout::arrange` — O(windows × columns) projection | `render.rs:619-627` | buffer reused (`self.desired`, `mod.rs:263`) |
| `present_into` — O(windows) | `present.rs:49-89` | buffer reused (`self.present_scratch`, `mod.rs:271`) |
| `hide_offscreen` — O(clients on monitor) | `render.rs:671-757` | buffers reused (`mod.rs:256-257`, take-and-restore at `:721`/`:755`) |
| **`DesiredState::from_placements` — 2 fresh Vec allocs** | `render.rs:639`, `desired.rs:56` and `desired.rs:67` | **no — allocates every arrange** |
| **`reconcile` — 1 HashMap + 1 Vec alloc** | `reconciler.rs:215-219`, `reconciler.rs:221` | **no** |
| **`stack_overlay` — 3 Vec allocs + O(windows × transient depth)** | `render.rs:798`, `:833-839`, `:847-858`, `:885-889` | **no** |
| `focus()` — 2 blocking RTTs + ~10 requests + un-diffed `grab_buttons` | `render.rs:1218`, `:1253`, `:1263` | **no** |
| `publish_state` — O(windows) JSON serialization | `actions.rs:419` | no (only the *publish* is deduped) |

### The honest ceiling on coalescing

Coalescing N arranges into 1 removes (N−1)× of: projection, `present_into`, diff, allocations, `stack_overlay` compute, `hide_offscreen`, and — critically — **(N−1)× the synthetic `ConfigureNotify` `send_event`s**, because those are issued only from `emit_geometry` (`render.rs:999-1016`), i.e. only when the diff fires.

But it cannot remove O(changed windows) configures **when the state genuinely changed each time**. For `scroll +1 ×4`, the camera target genuinely differs at each notch, so four distinct geometry sets exist; only the fourth is ever displayed. That is the real win — and it is available *only* because the projection is deferred, which is what the diff does.

---

## proposed dirty-state design + critique of the uncommitted attempt

The uncommitted attempt (in `mod.rs`, `actions.rs`, `render.rs`, `pointer.rs`) adds:

```rust
// mod.rs:335
arrange_dirty: std::collections::BTreeSet<usize>,
// mod.rs:338
deferred_focus: Vec<(usize, Window)>,
// actions.rs:74-76
Effect::ArrangeMonitor(mi) => { self.arrange_dirty.insert(mi); }
// render.rs:1336-1337   (was: self.arrange(retargeted…)?; self.stack_overlay(mon_i); …warp…)
self.arrange_dirty.insert(retargeted.unwrap_or(mon_i));
self.deferred_focus.push((mon_i, w));
// mod.rs:644 + mod.rs:696-724
self.flush_deferred_arrange()?;
```

**Verdict: the direction is right and the batching site is right; the queue shape is wrong, the flush position is wrong, and one of the four edits is not a deferral at all — it is a state-corrupting regression.** `cargo check --bins` passes cleanly, so this is a design critique, not a build critique.

### P1 — `deferred_focus: Vec<(usize, Window)>` is the wrong shape and unbounded

- **Unbounded within a turn.** One push per `focus()` call (`render.rs:1337`), and `focus()` is called once per focus change. `SATURATION-MEASUREMENTS.md` §3 records 1.96 M notches over 50 s (~39 k/s) and a 2.6 s growing queueing delay at 200 notches/s; a single drain (`mod.rs:638-640`) that processes a few thousand events leaves a few thousand `(usize, u32)` pairs live. The vector is O(events-in-a-turn), which is precisely the quantity the fix is supposed to stop scaling with.
- **It duplicates authoritative state.** The flush loop uses each pair for exactly two things: `stack_overlay(mon)` (`mod.rs:709`) and a warp onto `w` (`mod.rs:710-720`). Both are *current-state* queries. `monitors[mon].focused` (`render.rs:1234`) is already the last-wins answer, written synchronously. The vector carries no information that is not already in `State`.
- **`stack_overlay` is called redundantly.** `arrange_full_phase` **already** calls `stack_overlay(mon_idx)` at `render.rs:666`, and `flush_deferred_arrange` calls it again per entry at `mod.rs:709`. The second call is a pure O(windows) recompute of an order that cannot have changed since the first (all state mutations already happened during dispatch). With N entries you get N redundant `stack_overlay` passes.
- **What last-wins preserves:** final geometry, final stack order, final warp target. **What it breaks:** nothing observable — *provided* the warp fallback is fixed (see H4). So the vector buys nothing and costs memory + redundant compute.

**Correct shape:** `deferred_focus` should not exist. After the arrange loop, warp onto `monitors[mi].focused` for each dirty monitor, gated on "focus changed this turn". That is O(monitors), bounded, and reads only authoritative state.

### P2 — The flush is in the wrong position in `run_once()`

`flush_deferred_arrange()` sits at `mod.rs:644`, i.e. **between drain #1 (`mod.rs:638-640`) and the wait + drain #2 (`mod.rs:663-675`)**. Consequences:

- **Drain #2's events are never reconciled in the turn they are dispatched.** They set `arrange_dirty` / `deferred_focus` and wait for the next turn's `mod.rs:644`. One full turn of latency on every event that arrives while the WM is blocked — which, under saturation, is most of them.
- **Nothing flushes the socket after the reconcile.** `conn.flush()` is at `mod.rs:631`, *before* both drains. `wait_timeout(None, kbd_refresh_due, shutdown_deadline)` returns `None` when neither deadline is armed (`mod.rs:140-151`), so `wait_readable_fds` (`mod.rs:669`) blocks with **no timeout**. x11rb buffers requests until `flush()` or buffer-full (`x11rb-0.13.2/src/rust_connection/mod.rs:303-315`, `:781`). **When input stops, the final coalesced pass can sit in the write buffer indefinitely and the last geometry change never reaches the screen.** This is a *latent* pre-existing property (pre-diff, the last event's arrange had the same exposure), but the diff makes it structural: it now applies to *every* reconcile, not just the tail of a burst.

**Correct placement:** move the flush to the **end of the turn**, after `drain_control()` / `publish_state()` (`mod.rs:689-690`), and terminate it with `self.conn.flush()?`. One edit, and the batching boundary becomes the true end-of-turn covering **both** drains. Note the existing comment at `mod.rs:693` ("Loop back → `flush_client_list()` rewrites `_NET_CLIENT_LIST` at most once per batch") is written on the assumption that the flush is next-turn — moving the flush changes that comment's meaning and it should be updated with it.

### P3 — The deferral is partial: nine `self.arrange(mi)` sites bypass `arrange_dirty` entirely

`pointer.rs:524`, `actions.rs:395-398`, `manage.rs:570`, `manage.rs:733`, `manage.rs:1166`, `struts.rs:269`, `struts.rs:289`, `events.rs:509`, `events.rs:528` all call `arrange` **directly**, not through `Effect::ArrangeMonitor`. Consequences:

- A burst that maps windows (`manage.rs:570`) *and* focuses them still runs one immediate full arrange per map — the coalescing does not apply to the manage path at all, which is the highest-amplification path (manage is what generates `PropertyNotify`/`ConfigureNotify` amplification).
- `arrange_full_phase` reads `self.drag` to decide whether to run `hide_offscreen` (`render.rs:614`). With both an immediate arrange (drain #2, drag active) and a deferred one (drain #1's dirt, flushed at `mod.rs:644` before drain #2 — so drag state is the same) the ordering happens to work *this turn*. But `manage.rs:570` inside drain #1 runs `arrange` with the drag state *at that instant*, and `flush_deferred_arrange` at `mod.rs:644` runs it again with the drag state *at the end of the drain*. Two passes, two different `hide_offscreen` decisions, same turn. See H6.
- `actions.rs:395-398` loops **all** monitors on config reload, so a reload produces N immediate arranges and *also* leaves `arrange_dirty` populated from earlier in the turn — a redundant second pass per dirty monitor at `mod.rs:644`.

Either route every call site through the dirty set, or accept and document that the deferral is scoped to the `Effect::ArrangeMonitor` + `focus()` paths only.

### P4 — `BTreeSet<usize>` of dirty monitors: acceptable, bounded by construction

- **Is it genuinely bounded?** Yes. A `BTreeSet` dedupes, so `|arrange_dirty| ≤ (number of distinct monitor indices inserted in one turn)`. Monitor indices are renumbered from `0..n-1` on every RandR topology change (`events.rs:514-518` rewrites `state.monitors` wholesale; `events.rs:449-490` re-homes orphans), so the set cannot accumulate stale high indices across a hotplug. A stale index is also harmless: `arrange_full_phase` bounds-checks and returns `Ok(())` (`render.rs:610-612`).
- **Is it the right *shape*?** For this codebase, marginally worse than what already exists. `Monitor::layout_dirty` is already a per-monitor flag (`render.rs:667`, `render.rs:1050`) but has the **inverse** meaning ("projection invalidated" — written *by* arrange, read by nobody in this tree). `anim_per_mon: Vec<bool>` (`mod.rs:267`, cleared per turn at `mod.rs:653`) is the exact structural analogue: a per-monitor "still owes work" vector. A `Vec<bool>` of length `monitors.len()`, or three small per-monitor bits (`arrange`, `restack`, `warp`), is cheaper and clearer than a `BTreeSet`. The `BTreeSet` is not wrong — it is one more mechanism when the codebase already has two.
- **Verdict:** shape is acceptable, cost is irrelevant at n ≤ 8. Not the problem.

### P5 — The existing mechanisms the diff should have reused instead of adding new fields

| Existing field | Site | Status | Relevance |
|---|---|---|---|
| `client_list_dirty: bool` | `mod.rs:226`; set `manage.rs:543`, `:684`, `actions.rs:81`; flushed `mod.rs:630` → `ewmh.rs:267-274` | **working** | **This is the exact precedent.** Set-on-effect, cleared-before-work, drained once per turn. `arrange_dirty` should be shaped identically. |
| `stack_dirty: bool` | `mod.rs:228`; set `actions.rs:78`, `manage.rs:509` | **DEAD — written, never read anywhere in the tree** | This is *precisely* the "deferred restack" flag the diff reinvents as `deferred_focus`. The correct move is to wire `stack_dirty` into the per-turn flush and delete `deferred_focus`. |
| `kbd_refresh_due: Option<Instant>` | `mod.rs:221`; armed `mod.rs:426`; consumed `mod.rs:679-685` | **working** | The authors already solved "collapse a burst into one unit of work" — for keyboard regrabs, with the same set-flag/consume-once shape. The doc comment at `mod.rs:216-220` names the exact hazard ("losing a grab mid-burst is exactly when it hurts"). Generalizing it is better than adding a second idiom. |
| `anim_per_mon: Vec<bool>` | `mod.rs:267`; cleared `mod.rs:653` | working | Closest structural analogue to a per-monitor dirty vector. |
| `Monitor::layout_dirty` | `render.rs:667`, `:1050` | written, never read here | Inverse polarity; do not reuse as-is, but note it: the GL compositor build does read it. |
| `AppliedState` / `last_stack_order` / `shape_mask_cache` / `frame_extents` / `fs_covering` | see idempotence table | working | Model for emit-side diffing; proof the codebase already relies on idempotence. |

**The single most important structural observation about the diff: it adds two new collections to solve a problem for which the codebase already contains (a) a working per-turn deferral pattern (`client_list_dirty`), and (b) an unwired, half-finished `stack_dirty` flag that is the exact same idea.**

### P6 — `pointer.rs:719-722`: the removal of `focus()` from `focus_column_at` is not a deferral — it is a correctness regression

```diff
             self.engine.state.monitors[mi].workspaces[ws_i]
                 .focus
                 .column_idx = ci;
-            if let Some(w) =
-                self.engine.state.monitors[mi].workspaces[ws_i].columns[ci].focused_win()
-            {
-                let _ = self.focus(Some(w));
-            }
```

This is the **biggest measured win in the whole diff** (2 `focus()` → 1, 3 arranges → 1, 4 RTTs → 2 per notch; cf. `SATURATION-MEASUREMENTS.md` §2) and it is worth keeping — but not in this form. As written it leaves the workspace in a self-contradictory state:

- `ws.focus.column_idx` is set to the **pointer's** column (`pointer.rs:719-721`).
- `monitors[mi].focused` and `focus_stack` were set to `FocusDirection`'s window by `focus()` **earlier in the same call** (`render.rs:1232-1237`), and the camera was retargeted onto *that* column (`render.rs:1315` → `commands.rs:108-115`).
- Result: `column_idx` names column *A*, `mon.focused` names a window in column *B*, and `camera.target` was computed for *B*.

Every consumer that pairs them is now inconsistent:

| Consumer | Site | Effect of the split |
|---|---|---|
| accordion boost | `col.boost = (i == focus_i)` — `invariants.rs:117-119` | column *A* is boosted while focus (ring, border, stacking) is on *B* |
| `present_into` "focused last" | `present.rs:84-89` reads `mon.focused` | raise list ordered for *B*, camera on *A* |
| `stack_overlay` presented sort + peek | `render.rs:859`, `:869` read `mon.focused` | stacking for *B*, camera on *A* |
| `hide_offscreen` visibility | `render.rs:681-712` walks the *workspace* tree | column *A*'s windows re-shown while *B*'s stay hidden |
| next `h`/`l` press | `commands.rs:734` reads `ws.focus.column_idx` | jumps relative to *A* while the user is looking at *B* |

Note the runtime checker would **not** catch this: `assert_invariants` clauses #8/#8b (`render.rs:1238-1249`, `invariants.rs:154-160`) only constrain `focus_stack` well-formedness, never `column_idx ↔ mon.focused` agreement. And "Invariant A" (`invariants.rs:254-269`, `assert_all_tiled_match_settled`) — *"`client.geom` equals the settled projection it was written from"* — is only exercised in the pure suite, not at runtime.

**Correct form:** either (a) keep the `focus()` call and instead make `focus()` cheap via a `prev_focused == valid_win` early return (`render.rs:1188`'s comparison already exists — it just isn't used as a gate), or (b) route the pointer-column selection through the same funnel as `FocusDirection` so `focused`, `focus_stack` and `column_idx` are committed together. (a) is smaller and also fixes the click-on-already-focused-window case.

### P7 — What the diff gets right (keep these)

- Batching **below** the state layer: `Command::execute` still runs per event; only derived work is deferred. Correct.
- Batching at the **loop-turn** boundary, which is the only boundary the architecture offers (there is no frame clock — `mod.rs:656-659`, `wait_timeout` at `mod.rs:140-151`).
- Leaving `_NET_ACTIVE_WINDOW` (`render.rs:1339-1345`) and the whole ICCCM focus sequence (`render.rs:1207-1220`) **immediate and ordered**. Correct — these are client-visible sequences.
- Preserving the warp-after-arrange ordering: `mod.rs:705` runs `arrange(mi)` before `mod.rs:716` reads `client.geom`. The pre-diff comment at `render.rs:1317-1326` explicitly required this ("warp onto *that*, not the stale pre-scroll `geom` captured at the top"), and the deferred version honors it.
- The tree type-checks clean (`cargo check --bins`).

---

## stale-read hazards (concrete, with file:line)

Deferral is safe **iff** nothing between a state mutation and the deferred pass reads a derived field. `AppliedState`, `last_stack_order`, `frame_extents`, `shape_mask_cache`, `last_desired` are all fine (they are diff caches, and reading them stale only costs a redundant emit, which is self-correcting). **`client.geom` is the one derived field that `arrange()` itself reads back**, and it is written only by `emit_geometry` (`render.rs:1042`) and by commands (`commands.rs:228`, `:1230`, `:479`).

| # | Site | Hazard | Reachability under the diff |
|---|---|---|---|
| **H1** | `layout.rs:699-702` — `arrange()`'s float branch reads `client.geom` (either `adopt_client_float_geometry` or `normalize_float_geom(c.geom, c.hints, full_wa, bw)`) | A deferred pass projects floats from a rect that a *prior* arrange would have rewritten. `layout::arrange` is therefore **not a pure function of `(State, Phase)`** for floats — it is a fixed-point iteration on `client.geom`. The idempotence argument in `layout.rs:676-684` depends on `normalize_float_geom` being a fixed point, which it is for *unmoved* floats (`render.rs:354-355`) but not for a float whose rect changed in the same turn. | Latent: floats are not moved by a focus-induced arrange, so today `client.geom` for floats is stable across a deferred pass. Becomes live the moment a float-moving command is also coalesced (`Effect::ConfigureWindow` → `apply_geom`, `actions.rs:89`, writes `client.geom` at `render.rs:1042`) |
| **H2** | `render.rs:734` — `hide_offscreen`: `let off_rect = parked_rect(client.geom);` and `render.rs:741-747` — the re-show path restores `client.geom` **verbatim** (`Rect::new(gx, gy, client.geom.w, client.geom.h)`) | The parking spot *and* the restore rect are derived from the model rect. A one-burst-stale `client.geom` parks the window at a stale offset and, on the next workspace switch, **restores it to the stale position** — a silent, persistent misplacement that no later arrange corrects, because the restore used `write_client_geom = false` (`render.rs:736`, `:747`) and therefore did not update the model. | **Live.** `hide_offscreen` runs at `render.rs:615` inside the deferred pass; between a `camera.target` mutation (which is what changes tiled `client.geom`) and the flush, a workspace switch in the same drain would park/restore against the pre-burst rect. |
| **H3** | `render.rs:1128` — `apply_parked`: `let (rect, bw) = (parked_rect(c.geom), c.border_w);` | Same staleness class as H2, on the client-driven geometry sink. | Latent — `apply_parked` is reached from `ConfigureRequest`/`MapNotify`, which the diff does not defer. |
| **H4** | `mod.rs:711-716` (post-diff) vs `render.rs:1348-1357` (pre-diff) | The warp fallback changed. Pre-diff: `.map_or(geom, \|c\| c.geom)` — the *captured pre-arrange* rect. Post-diff: `.map_or(Rect::new(0, 0, 1, 1), \|c\| c.geom)`. If the window was destroyed between the push (`render.rs:1337`) and the flush, the post-diff version warps by (1,1) on a dead XID instead of using the last known rect. `warp_pointer` is fire-and-forget so this is a swallowed `BadWindow` via `maverick_x11`'s silent error handler, not a crash — but it is a behavior regression, and for a still-valid window it loses the warp entirely. | **Live.** Restore the pre-arrise fallback (keep the captured `geom` in the deferred entry) or drop the entry when `clients.get(&w)` is `None`. |
| **H5** | `render.rs:614` — `if do_hide && self.drag.is_none() { self.hide_offscreen(mon_idx)?; }` | The drag check is evaluated at **flush** time in the deferred version but at **schedule** time in the original. A burst that *starts* a drag after scheduling (`pointer.rs:401-431`) but before the flush now skips `hide_offscreen` for a monitor where the original would have run it. | **Live.** One-turn divergence on `Mod4+Button1`-over-a-float bursts. |
| **H6** | `pointer.rs:399` — `let geom = c.geom;` feeding `DragState.start_geom` (`pointer.rs:424`) | `start_geom` is frozen for the drag's lifetime and every motion rect is computed from it (`pointer.rs:555-556`, `:566`, `:635-639`). If the pending arrange would have re-normalized this float's rect (`layout.rs:702`) or a `reposition_floats` ran (`render.rs:538-588`, reachable from `events.rs:495`/`:521`), the drag anchor is stale and **the entire drag is offset for its whole duration** — the worst failure mode in this table, because it is not self-correcting. | Latent-but-sharp: `reposition_floats` arranges immediately and is not deferred, so the two cannot currently interleave. It becomes live if P3 routes `manage`/RandR/strut arranges through the dirty set. **This is the concrete reason the direct `self.arrange(...)` sites must not be deferred without also making `hide_offscreen`'s inputs pure.** |
| **H7** | `pointer.rs:719-721` writing `ws.focus.column_idx` **after** `focus()` already committed `mon.focused`/`focus_stack` (`render.rs:1232-1237`) and retargeted the camera (`render.rs:1315`) | See P6. A one-line state divergence that four independent consumers read (`invariants.rs:117-119`, `present.rs:84-89`, `render.rs:859`/`:869`, `commands.rs:734`) and that no runtime invariant checks. | **Live, immediately, on every scroll notch where the pointer is over a different column than the one `FocusDirection` moved to.** |
| **H8** | `mod.rs:631` (`conn.flush()`) before both drains; `mod.rs:140-151` (`wait_timeout` → `None`); `mod.rs:669` (`wait_readable_fds` with `None`) | Not a stale *read*, but the paired hazard: the deferred pass's output is buffered and may never be written. x11rb flushes only on `flush()` or buffer-full (`x11rb-0.13.2/src/rust_connection/mod.rs:303-315`, `:781-785`). When input stops with no `kbd_refresh_due` and no `shutdown_deadline`, the WM blocks with the last reconcile unwritten. | **Live.** Fix: `flush_deferred_arrange` ends with `self.conn.flush()?`, or move the whole flush to the end of the turn. |
| **H9** | `mod.rs:620-624` — the quit path `return Ok(())` at `mod.rs:623` bypasses `flush_deferred_arrange` | `arrange_dirty` and `deferred_focus` are dropped un-drained. | Benign — `teardown_x` (`mod.rs:561-596`) ungrabs and deletes properties without arranging. But it means the fields are not "always drained", which matters if anyone later adds a second flush site. |
| **H10** | `mod.rs:738-745` — a non-connection-loss `Err` from `dispatch` (`mod.rs:639` / `mod.rs:673`) propagates out of `run()` with `arrange_dirty` un-drained | Same as H9, plus the WM exits. | Benign for correctness; noted for completeness. |
| **H11** | `mod.rs:1336` — `arrange_dirty.insert(retargeted.unwrap_or(mon_i))` uses the **retarget** monitor, while `mod.rs:1337` records `mon_i` (the window's `client.monitor`) | If the two disagree — invariant clause #5 (`invariants.rs`, "a client's `monitor`/`workspace` agreeing with the tree that holds it") is checked only after commands and after RandR (`events.rs:548-549`), never after a mouse-path `focus()` — then the arrange is owed on monitor A while the warp lands on monitor B's window. | Latent. Note `retarget_focus_to_window` returns `None` for a float (`commands.rs:108`), falling back to `mon_i`, so floats are safe; tiled windows in a mis-homed column are not. |
| **H12** | `render.rs:667` and `render.rs:1049-1051` — `monitors[mi].layout_dirty = true` | Set by arrange and by every `emit_geometry`. A pass that is skipped leaves the flag describing the pre-burst projection. Nothing reads it in this tree, but the GL compositor build (`presentation=gl`) does. | Cross-mode latent. If the flag's meaning ever changes from "projection invalidated" to "arrange owed", the two would collide — which is why P4 says do **not** reuse it as-is. |

---

## rejected approaches and why

### Rejected: `sleep` (anywhere)

The loop has no clock by construction. `run_once` computes `timeout = wait_timeout(None, self.kbd_refresh_due, self.shutdown_deadline)` (`mod.rs:661`) and the comment at `mod.rs:656-659` states the invariant explicitly: *"With nothing animating the loop is idle, so the poll has no frame deadline: no heartbeat, no timer."* Adding a sleep would (a) add latency to the responsive path, where `SATURATION-MEASUREMENTS.md` §6 measures recovery at ~0.44 s already, (b) not reduce per-event work at all — the work is paid during dispatch, not during the wait, and (c) re-introduce the animation-style frame loop that `presentation=x11_settled` (`mod.rs:729-736`) deliberately removed.

### Rejected: fixed-rate limiting (e.g. "arrange at most 60 Hz")

This is rate-limiting by another name and it is the same defect as dropping events: it decides *not to run* a pass, on a timer, which means the pass that finally runs may be arbitrarily stale relative to the state it projects. It also requires a timer in a loop that has none (`mod.rs:140-151`, `mod.rs:656-659`), and it makes the final geometry a function of wall-clock jitter rather than of state. `SATURATION-MEASUREMENTS.md` §3 shows recovery in ~0.44 s at *every* rate including 39 k/s, i.e. the system needs no artificial brake — it needs less work per event.

### Rejected: dropping input events

Structurally illegal, not merely undesirable:

- `on_button_press` runs under a `SYNC` button grab installed by `grab_buttons` (`input.rs:385-395`). Every press must be answered with exactly one `allow_events` (`pointer.rs:167`, `:172`); dropping a press without answering it **freezes the pointer for the rest of the session**. `SyncGrabGuard` (`pointer.rs:88-108`) and its `FREEZE-RISK` logging exist because this failure is silent and permanent.
- Press/release pairing is load-bearing (`pointer.rs:401-431` vs `pointer.rs:502-525`); dropping a release leaks an active `grab_pointer` = frozen input.
- `reconcile_focus` (`render.rs:1412`) is a *repair* path for lost input focus (`render.rs:1517-1525`). Dropping focus events defeats the mechanism that keeps `logical == real`.

### Rejected: enlarging queues

The X11 event queue is **already drained in full every turn** (`mod.rs:638-640`), and `SATURATION-MEASUREMENTS.md` §5 measures it as *not* the bottleneck — the queueing delay it reports is the delay *between* the server generating an event and the WM dispatching it, i.e. time spent in per-event work, not time spent queued. Enlarging the connection's write buffer (or an intermediate event queue) converts visible per-event latency into invisible server-side backlog; it does not reduce the O(N) projection or the 2 round-trips. The one queue growth the data *does* show is the compositor trace ring buffer (`SATURATION-MEASUREMENTS.md` §7: 448 B/record, 112 MB cap) — instrumentation, not the WM.

### Rejected: making the reconcile pass cheaper by micro-optimizing

The dominant cost is not the arithmetic. `layout::arrange` and `present_into` already run on preallocated buffers (`mod.rs:263`, `mod.rs:271`), and `hide_offscreen` uses take-and-restore scratch (`render.rs:721`, `:755`). The per-arrange allocations that remain (`desired.rs:56`, `desired.rs:67`, `reconciler.rs:215`, `reconciler.rs:221`, `render.rs:798`) are real but are `O(windows)` *Vec* allocations, and `SATURATION-MEASUREMENTS.md` §2 already separates them from the round-trips. Amortizing them is worth doing as a secondary change (three `Vec`/`HashMap` fields on `WindowManager`, exactly like `desired` and `present_scratch`) — but it does not change the *asymptotics*, and the deferral does.

---

## what a principled fix looks like

Ordered by (payoff ÷ risk). Only the first four are load-bearing.

### 1. Make `focus()` cheap instead of deferring it *(highest payoff, lowest risk)*

The two blocking round-trips are the hard serialization point. Neither is needed per event:

- **Cache `WM_TAKE_FOCUS` support per window.** `has_protocol` (`render.rs:1218` → `ewmh.rs:293-306`) issues `get_property` + `.reply()` on *every* focus change. `WM_PROTOCOLS` changes only via `PropertyNotify`, which the WM already receives. A `HashMap<Window, bool>` populated at `manage()` and invalidated on `WM_PROTOCOLS` `PropertyNotify` removes a full RTT from the per-focus path with **zero** semantic change — the property cannot change between two events in the same drain.
- **Make `reconcile_focus`'s verification asynchronous.** `render.rs:1250-1253` says it outright: *"Verify the server accepted the focus… No polling: this runs only on a focus action we just issued."* But issuing the request and immediately blocking to confirm is the definition of a round-trip. `FocusIn`/`FocusOut` already call `reconcile_focus` (`events.rs:891`); that is where the verification belongs. Delete `render.rs:1253` (and `:1365`) and the repair path is unchanged, minus the stall.
- **Diff-guard `grab_buttons`.** `render.rs:1263` → `input.rs:365-432` issues `ungrab_button` + 1 + 2×`mod_variants` grabs on **every** focus change with no cache — unlike `frame_extents`, `shape_mask_cache` and `last_stack_order`, which all do cache. A `HashMap<Window, bool>` last-grabbed-state makes a focus burst cost zero regrabs.

### 2. Gate `focus()` on an actual focus change

`render.rs:1188` already computes `prev_focused != valid_win`. Everything after that line currently runs unconditionally. An early return when the focus is unchanged (with the `wants_input`/URGENT/overlay-owner cases carved out) collapses the redundant `focus()` in the scroll path and the click-on-already-focused-window path. **This is the correct replacement for the `pointer.rs` deletion** (P6): it removes the same measured work while keeping `ws.focus.column_idx`, `mon.focused` and `camera.target` consistent.

### 3. One pending reconciliation, drained at the true end of the turn

Not two collections — one per-monitor dirty bit vector (or three bits per monitor: `arrange`, `restack`, `warp`), sitting next to `anim_per_mon` (`mod.rs:267`), consuming the already-existing `stack_dirty` (`mod.rs:228`, currently dead) instead of adding `deferred_focus`. Placed at **`mod.rs` after `drain_control()`/`publish_state()` (`mod.rs:690`)**, so it covers drain #1 *and* drain #2, and terminated with `self.conn.flush()?`.

This is justified because: `reconcile` is already a pure function of `(Desired, State, AppliedState)` (`reconciler.rs:199-203`); the emit side is already idempotent (`reconciler.rs:146-147`); and `State` is already the authoritative accumulator, written synchronously by `Command::execute`. "Project once from final state, diff once" is *definitionally* "project N times, diff N times, keep the last" whenever `layout::arrange` is a pure function of `(State, Phase)` — which it is for tiled windows, and is a fixed point for floats (`render.rs:354-355`, `layout.rs:676-684`).

**Not** justified and **not** to be added: the `Vec<(usize, Window)>`, the per-entry `stack_overlay` (arrange already does it at `render.rs:666`), and deferring past `conn.flush()` without a flush.

### 4. Route the nine direct `self.arrange(mi)` sites through the same dirty set — *or* explicitly scope the deferral

`pointer.rs:524`, `actions.rs:395-398`, `manage.rs:570`, `manage.rs:733`, `manage.rs:1166`, `struts.rs:269`, `struts.rs:289`, `events.rs:509`, `events.rs:528`. If they are *not* deferred, that is a defensible scoping decision (manage/RandR/strut paths are rare and have ordering contracts at `struts.rs:266-268` and `events.rs:496-508` that deferral would violate), and it protects H2 and H6. If they *are*, then `hide_offscreen`'s `client.geom` inputs (H2) and `DragState.start_geom` (H6) must first be made independent of the projection.

### 5. Make `focus_column_at` write state through one funnel

`pointer.rs:691-723` currently mutates `ws.focus.column_idx` behind the core's back, with no `focus()`, no `FocusChanged` event, and no `camera.retarget`. Either it should emit a proper command/focus (as it did pre-diff), or the `column_idx` write should be removed entirely and the pointer-tracking done through the same `FocusDirection`-equivalent path. Writing one of the three focus representations without the other two is the P6 regression.

### 6. Amortize the four per-arrange allocations *(secondary)*

`DesiredState::from_placements` (`desired.rs:56`, `:67`), `reconcile`'s `last_index` HashMap and `out` Vec (`reconciler.rs:215`, `:221`), and `stack_overlay`'s `order`/`dock_wins`/`presented` Vecs (`render.rs:798`, `:833`, `:847`). All belong on `WindowManager` as reusable buffers exactly like `desired` (`mod.rs:263`) and `present_scratch` (`mod.rs:271`). Worth doing; will not move the asymptotics.

### Explicitly not justified by this architecture

- Merging the synthetic `ConfigureNotify` (`render.rs:999-1016`) out of `emit_geometry` — it is what keeps `client.geom` and X11 in agreement and what clients rely on for `ConfigureNotify`.
- Deferring `_NET_ACTIVE_WINDOW` (`render.rs:1339-1345`) — client-visible sequence.
- Deferring the ICCCM focus sequence (`render.rs:1207-1220`) — ordering contract.
- Deferring `grab_pointer`/`ungrab_pointer`/`allow_events` (`pointer.rs:401`, `:508`, `:167`) — input-integrity contract.
- Batching *events* rather than *derived work* — the state transitions are the semantics.
