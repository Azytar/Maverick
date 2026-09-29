# X11 Input Audit — Maverick WM (Agent D, X11 protocol layer)

**Scope.** Every X11 request issued in response to `ButtonPress`, `ButtonRelease`,
`MotionNotify`, `Button4/5/6/7`, plus every feedback loop those requests close, plus every
blocking X call on the per-input-event hot path. Core layout state is Agent C's scope and is
referenced here only where it drives an X request.

**Method.** Static reading of `src/backend/x11/` in the current working tree (which contains the
unverified coalescing patch in `mod.rs`/`render.rs`/`actions.rs`/`pointer.rs`; `git diff` reviewed).
No X server was run. No source was modified.

**Terminology used throughout.**

| term | meaning |
|---|---|
| **blocking RTT** | a `.reply()` / `.reply_ok()` — `xcb_wait_for_reply`. Blocks the single WM thread until the server answers. |
| **`.check()`** | a `VoidCookie::check()` — `xcb_request_check` (`/usr/include/xcb/xcb.h:368-379`). Documented as *"will block until one of two conditions happens … will perform a sync if needed"*. See §2.3 for the caveat. |
| **F&F** | fire-and-forget: `let _ = …` with the cookie dropped. `RawCookie::drop` only does local sequence bookkeeping (`x11rb-0.13.2/src/cookie.rs:198-209`), so this is provably non-blocking. |
| **O(1)/O(N)** | constant in, or linear in, the number of managed windows. |

Every claim is tagged **[V]** = verified in this repo's source, or **[H]** = hypothesis /
server-side behaviour that I could not confirm statically.

---

## 0. The event loop this audit assumes

`run_once` (`src/backend/x11/mod.rs:609`) is strictly serial and has **no budget** — no event cap
per drain, no wall-clock cap:

```
mod.rs:630   flush_client_list()                    # .check()×2 on EWMH props, but only if dirty
mod.rs:631   conn.flush()                           # xcb_flush — writes the libxcb out-buffer
mod.rs:638-640  DRAIN A:  while let Some(ev) = poll_for_event() { dispatch(ev) }   # unbounded
mod.rs:644   flush_deferred_arrange()               # the coalesced pass (patch)
mod.rs:652-654 snap_animations(); anim_per_mon.clear(); animating = false
mod.rs:669   wait_readable_fds([x_fd, hub_fd], timeout)   # the only blocking point
mod.rs:672-674  DRAIN B:  while let Some(ev) = poll_for_event() { dispatch(ev) }   # unbounded
mod.rs:683-685 debounced refresh_keyboard()
mod.rs:689-690 drain_control(); publish_state()
```

`wait_timeout` (`mod.rs:140-151`) returns `None` when no keyboard refresh and no shutdown is
pending, so `wait_readable_fds` **blocks indefinitely** on an idle loop. The loop therefore
sleeps only when the queue is *completely* drained; any self-generated event (a synthetic
`ConfigureNotify`, a `PropertyNotify`, a warp-induced `MotionNotify`/`EnterNotify`) keeps it
awake. **[V]**

`poll_for_event` maps to `xcb_poll_for_event` (`x11rb-0.13.2/src/xcb_ffi/mod.rs:573-589`), which is
non-blocking (zero-timeout socket poll). It does **not** flush. **[V]**

---

## 1. Per-event X11 operation inventory

### 1.1 `ButtonPress`, detail 1/2/3 — `pointer.rs:139` `on_button_press`

Handler order: scroll early-exit (`pointer.rs:156-174`) → `last_event_time` (`:175`) →
`find_client` (`:261`) → optional `unfocus`/`focus`/overlay teardown (`:212-376`) → optional
Mod4 drag grab (`:401-415`) → `allow_events` (`:474-483`).

| # | operation | site | blocking | cost | notes |
|---|---|---|---|---|---|
| 1 | `QueryTree` + `.reply()` | `manage.rs:1230` via `find_client` (`pointer.rs:261`) | **blocking RTT** | O(depth) | 0 RTT when `e.event` is itself a managed window (passive grab `owner_events=false` makes the grab window the `event` field, `input.rs:386`). **1 RTT when `e.event == root`**, and 1..k when the click lands on an unmanaged/OR window — `find_client` walks parents with one `query_tree` round trip each. |
| 2 | `ChangeWindowAttributes` (border) | `render.rs:1392-1394` (`unfocus`) | F&F | O(1) | |
| 3 | `UngrabButton(ANY)` + `GrabButton`×17 | `input.rs:370`, `:385`, `:414-429` (`grab_buttons`) | F&F | O(1) | **17 requests per call** (1 ungrab + 1 SYNC catch-all + 8 `mod_variants` × 2 buttons; `mod.rs:1833`). No `.check()`. Called from **both** `focus()` (`render.rs:1263`) and `unfocus()` (`render.rs:1395`) ⇒ **34 requests per focus change**. |
| 4 | `SetInputFocus` | `render.rs:1208-1217` | F&F | O(1) | uses `last_event_time`, **not** `CURRENT_TIME` |
| 5 | `GetProperty(WM_PROTOCOLS)` + `.reply()` | `ewmh.rs:301` via `has_protocol` (`render.rs:1218`) | **blocking RTT** | O(1) | once per `focus()` |
| 6 | `SendEvent(WM_TAKE_FOCUS)` | `ewmh.rs:328` | F&F | O(1) | only if `has_protocol` said yes |
| 7 | `GetInputFocus` + `.reply()` | `render.rs:1415-1416` via `reconcile_focus` (`render.rs:1253`) | **blocking RTT** | O(1) | issued *after* the `SetInputFocus`, so the RTT must wait for the server to drain the whole preceding batch |
| 8 | `ChangeWindowAttributes` (focused border) | `render.rs:1260-1262` | F&F | O(1) | |
| 9 | `GrabButton`×17 (new window) | `input.rs:370/385/414` via `render.rs:1263` | F&F | O(1) | |
| 10 | `GetProperty(_NET_WM_STATE)` + `.reply()` | `manage.rs:1331` via `write_net_wm_state` (`render.rs:1280`) | **blocking RTT** | O(1) | only when the just-focused window was `URGENT` |
| 11 | `ChangeProperty(_NET_ACTIVE_WINDOW)` on root | `render.rs:1339-1345` | F&F | O(1) | |
| 12 | `ChangeProperty/DeleteProperty(_NET_WM_BYPASS_COMPOSITOR)` | `manage.rs:1261-1271` | F&F | O(1) | fullscreen teardown path only |
| 13 | `GetProperty(_NET_WM_STATE)` + `.reply()` | `manage.rs:1331` via `set_fullscreen`→`write_net_wm_state` | **blocking RTT** | O(1) | fullscreen teardown path only |
| 14 | `configure_window` ×N + `SendEvent(ConfigureNotify)` ×N | `render.rs:983-991`, `render.rs:1014-1016` | F&F | **O(N)** | the overlay-dismiss path calls `ToggleMaximize`→`set_maximized`→`arrange(mi)` **synchronously** (`manage.rs:1302`) — this one is *not* covered by the coalescing patch |
| 15 | `configure_window(stack_mode ABOVE)` ×N | `render.rs:1135-1137` (`raise`) | F&F | O(N) | same path, from `stack_overlay` (`render.rs:896-898`) |
| 16 | `configure_window(stack_mode BELOW)` | `render.rs:924-927` | F&F | O(1) | fullscreen covering transition only |
| 17 | `GrabPointer` + `.reply()` | `pointer.rs:401-415` | **blocking RTT** | O(1) | Mod4+Button1/3 on an already-floating window only |
| 18 | `AllowEvents` + `.check()` | `pointer.rs:474-483` | **`.check()`** | O(1) | *always* for detail < 4 |
| 19 | `ChangeProperty`/`DeleteProperty(maverick_*)` | `manage.rs:1014-1034` | F&F | O(1) | drag-release only |

**Steady-state blocking count for a normal click that changes focus: 2 blocking RTTs**
(`ewmh.rs:301` + `render.rs:1416`) **+ 1 `.check()`** (`pointer.rs:483`), plus 2 blocking RTTs more
from the `FocusOut`/`FocusIn` the focus change itself generates (see §1.6) — **4 RTTs total per
focus-changing click**. Worst case (root click + Mod4 float drag + urgent window): **6 RTTs**.

### 1.2 `ButtonPress`, detail 4/5/6/7 — `pointer.rs:156-174`

| branch | operations | blocking |
|---|---|---|
| no Mod4 | `AllowEvents(REPLAY_POINTER)` only (`pointer.rs:172`, **no `.check()`**) | **0** |
| Mod4 (wheel camera) | `FocusDirection` (`core/commands.rs:678-860`) → `Unfocus` + `ArrangeMonitor` + `FocusWindow` → `focus()` → items 2-11 above; then `AllowEvents(ASYNC_POINTER)` + `.check()` (`pointer.rs:166-168`) | **2 blocking RTTs + 1 `.check()`** |

`focus_column_at` (`pointer.rs:691-723`) is pure core arithmetic. `ArrangeMonitor` is now *deferred*
(`actions.rs:74-76`), so no O(N) `arrange` happens on this path any more. **[V]**

Note the asymmetry at `pointer.rs:172`: the no-Mod4 scroll path issues `allow_events` **without**
`.check()`, while every other `allow_events` site checks. Both are F&F-equivalent in cost. **[V]**

### 1.3 `ButtonRelease` — `pointer.rs:487` `on_button_release`

| # | operation | site | blocking | cost |
|---|---|---|---|---|
| 1 | `UngrabPointer` + `.check()` | `pointer.rs:508` | **`.check()`** | O(1) — only when a drag was active |
| 2 | `arrange(mi)` | `pointer.rs:524` | F&F, **synchronous O(N)** | O(N) — **not** deferred by the patch; runs inline in the handler |
| 3 | `configure_window` + `SendEvent(ConfigureNotify)` ×changed | `render.rs:983`, `render.rs:1014` | F&F | O(N) |
| 4 | `configure_window(ABOVE)` ×N | `render.rs:1135` | F&F | O(N) |
| 5 | `sync_window_prefs` ×2 | `manage.rs:1014/1025/1033` | F&F | O(1) |

**Blocking RTTs: 0. `.check()`: 0 or 1.** The O(N) `arrange` at `pointer.rs:524` is the real cost —
it runs inside `dispatch`, i.e. inside DRAIN A/B, so the drain is stalled for the whole pass. **[V]**

### 1.4 `MotionNotify` — `pointer.rs:536` `on_motion`

`on_motion` itself issues **zero X requests** (`pointer.rs:542` clears the guard, then either a
drag or nothing).

| # | operation | site | blocking | cost |
|---|---|---|---|---|
| — | (guard clear, pure) | `pointer.rs:542` | — | O(1) |
| 1 | `configure_window` | `render.rs:983-991` via `apply_geom` | F&F | O(1) — drag only |
| 2 | `SendEvent(ConfigureNotify)` | `render.rs:1014-1016` | F&F | O(1) |
| 3 | `ChangeProperty(_NET_FRAME_EXTENTS)` | `render.rs:1027-1033` | F&F | O(1) — only when the border changed |
| 4 | `shape::rectangles` CLIP+BOUNDING | `render.rs:487/496/512` | F&F | O(1) — **only when `corner_radius > 0`** (`render.rs:1059`); default is `0` (`config.rs:107`) |

**Blocking RTTs: 0. `.check()`: 0.** `MotionNotify` is by far the cheapest event per unit of user
input — a 1000 Hz motion stream costs the WM one `configure_window` + one `send_event` while
dragging and literally nothing when not. **[V]**

Two caveats:
* During a **resize** drag the shape mask key `(outer_w, outer_h, r, bw)` changes every event
  (`render.rs:1081-1085`), so with `corner_radius > 0` each motion issues 2 extra `Shape` requests.
  `manage.rs:488` unconditionally does `shape_select_input(win, true)`, so every one of those comes
  straight back as a `ShapeNotify` the `dispatch` match drops via `_ => {}` (`mod.rs:415`) — still
  a wakeup per event. **[V]**
* `QueryPointer` is **never** called anywhere in `src/backend/x11/`. **[V]**

### 1.5 `EnterNotify` — `events.rs:833` `on_enter`

Only does anything when `cfg.focus_mouse` (`events.rs:841`); default is `false` (`config.rs:110`). **[V]**

| # | operation | site | blocking | cost |
|---|---|---|---|---|
| 1 | `QueryTree` + `.reply()` | `manage.rs:1230` via `find_client` (`events.rs:851`) | **blocking RTT** | O(depth) — 0 for a managed window, **1 for the root**, 1..k for unmanaged |
| 2 | `focus()` (items 2-11 of §1.1) | `events.rs:871` | **2 blocking RTTs** | O(1) + 34 grab requests |

### 1.6 `FocusIn` / `FocusOut` — `events.rs:891` / `events.rs:925`

| # | operation | site | blocking | cost |
|---|---|---|---|---|
| 1 | `GetInputFocus` + `.reply()` | `render.rs:1415-1416` via `reconcile_focus` (`events.rs:910`, `events.rs:938`) | **blocking RTT, unconditional** | O(1) |
| 2 | `SetInputFocus` (repair only) | `render.rs:1536-1570` | F&F | O(1) |

These are the two round trips nobody accounts for: every `SetInputFocus` the WM issues produces
exactly one `FocusOut` + one `FocusIn`, each of which costs a full blocking `GetInputFocus` RTT.
**A click-induced focus change therefore costs 4 blocking RTTs: 2 from `focus()` and 2 from the
focus events it causes.** **[V]**

### 1.7 `ConfigureNotify` — `events.rs:287`

Zero X requests are issued from `dispatch`'s perspective. Two sub-cases:
* **Synthetic echo we sent ourselves** — `emit_geometry` fabricates a `ConfigureNotify` and
  `SendEvent`s it to the window (`render.rs:1014-1016`). Because `STRUCTURE_NOTIFY` is selected on
  every managed window (`manage.rs:477-487`) the server hands it straight back, and it is dropped
  at `events.rs:302` (`e.response_type & 0x80 != 0`). **[V]**
* **Real echo of our own `configure_window`** — two copies (StructureNotify on the window,
  SubstructureNotify on root). Only the root copy is examined (`events.rs:321`); it is classified
  and, on `Stale`, re-asserted once via `apply_geom` (`events.rs:353-356`), which produces a new
  echo that classifies `Compliant`. **Bounded at depth 1.** **[V]**
* `e.window == root` diverts to `handle_monitor_change()` (`events.rs:291-293`) → `detect_monitors`
  → `randr_get_monitors` + `.reply()` (`mod.rs:1078`) — **blocking, but only on a real monitor
  change**, and `handle_monitor_change` no-ops when the topology is unchanged (`events.rs:374-380`).

### 1.8 `PropertyNotify` — `events.rs:559`

The WM selects `PROPERTY_CHANGE` on the root (`input.rs:79`) and on every client (`manage.rs:484`).

| trigger | operation | site | blocking |
|---|---|---|---|
| root `WM_NAME` | `GetProperty` + `.reply()` | `ewmh.rs:259` (`update_status`) | **blocking RTT** |
| `_NET_WM_STRUT[_PARTIAL]` | `GetProperty` + `.reply()` ×1-3, `GetGeometry` + `.reply()` | `struts.rs:61`, `:86`, `:143`, `:115-119` | **blocking RTT ×2-4** |
| `_NET_WM_BYPASS_COMPOSITOR` | `GetProperty` + `.reply()` | `events.rs:963` | **blocking RTT** |
| `WM_NORMAL_HINTS` | `GetProperty` + `.reply()` | `manage.rs:1215` | **blocking RTT** |
| client `_NET_WM_NAME` / `WM_NAME` | `GetProperty` + `.reply()` ×1-2 | `mod.rs:1791`, `:1798` | **blocking RTT ×1-2** |
| client `WM_HINTS` | `GetProperty` + `.reply()` | `mod.rs:1813` | **blocking RTT** |
| everything else, incl. **every property the WM writes to root itself** | none | `events.rs:627-641` | — |

The last row is the important one: `on_property` matches nothing for `_NET_CLIENT_LIST`,
`_NET_CLIENT_LIST_STACKING` or `_NET_ACTIVE_WINDOW`, and the final guard is
`clients.contains_key(&e.window)` which is false for the root. **The WM ignores its own writes
outright.** See §3.4. **[V]**

### 1.9 Summary of blocking-call counts by event

| event | blocking RTTs (`.reply()`) | `.check()` | O(N) synchronous work inside the handler |
|---|---|---|---|
| `ButtonPress` 1-3, managed tile, focus changes | **2** (+2 from the resulting FocusIn/Out = **4**) | 1 | only on the overlay-teardown path (`manage.rs:1302`) |
| `ButtonPress` 1-3, on root / unmanaged | 3..3+k | 1 | — |
| `ButtonPress` 1-3, Mod4 drag on a float | 3 (+2 = 5) | 1 | — |
| `ButtonPress` 4-7, no Mod4 | **0** | **0** | **0** |
| `ButtonPress` 4-7, Mod4 | 2 (+2 = 4) | 1 | 0 (deferred) |
| `ButtonRelease`, no drag | **0** | 0 | 0 |
| `ButtonRelease`, drag end | **0** | 1 | **O(N)** (`pointer.rs:524`) |
| `MotionNotify`, no drag | **0** | 0 | 0 |
| `MotionNotify`, drag | **0** | 0 | 0 (O(1) F&F) |
| `EnterNotify` (needs `focus_mouse=true`) | 0 or 1, +2 if focus changes | 0 | 0 (deferred) |
| `FocusIn` / `FocusOut` | **1 each, unconditional** | 0 | 0 |

---

## 2. Every blocking call site, classified

Complete inventory of `.reply()` / `.check()` in `src/backend/x11/` (excluding `tests.rs`).

### 2.1 Hot path — reachable from per-input-event dispatch

| file:line | call | reachable from | per event? |
|---|---|---|---|
| `manage.rs:1230` | `QueryTree.reply()` (`find_client`) | `ButtonPress` (`pointer.rs:261`), `EnterNotify` (`events.rs:851`) | yes, conditional |
| `ewmh.rs:301` | `GetProperty.reply()` (`has_protocol`) | `focus()` (`render.rs:1218`, `render.rs:1546`) ⇒ every click / EnterNotify / `_NET_ACTIVE_WINDOW` / key action | **yes** |
| `render.rs:1416` | `GetInputFocus.reply()` (`reconcile_focus`) | `focus()` (`render.rs:1253`, `render.rs:1365`), `on_focus_in` (`events.rs:910`), `on_focus_out` (`events.rs:938`) | **yes, unconditional for focus events** |
| `manage.rs:1331` | `GetProperty.reply()` (`write_net_wm_state`) | `focus()` urgent branch (`render.rs:1280`), `set_fullscreen`, `set_maximized` | yes, conditional |
| `pointer.rs:414` | `GrabPointer.reply()` | `ButtonPress` Mod4+float | yes, conditional |
| `pointer.rs:168` | `AllowEvents.check()` | `ButtonPress` 4-7 Mod4 | yes, unconditional in branch |
| `pointer.rs:483` | `AllowEvents.check()` | `ButtonPress` 1-3 | yes, unconditional |
| `pointer.rs:508` | `UngrabPointer.check()` | `ButtonRelease` | yes, drag only |
| `manage.rs:1331` | `GetProperty.reply()` | `ClientMessage` `_NET_WM_STATE` | client-driven, not pointer |
| `events.rs:963` | `GetProperty.reply()` (`read_bypass_hint`) | `PropertyNotify` | client-driven |
| `manage.rs:1215` | `GetProperty.reply()` (`read_size_hints`) | `PropertyNotify` | client-driven |
| `mod.rs:1791`, `:1798`, `:1813` | `GetProperty.reply()` | `PropertyNotify` (title/hints) | client-driven |
| `struts.rs:61`, `:86`, `:143`, `struts.rs:117` | `GetProperty`/`GetGeometry.reply()` | `PropertyNotify` (dock strut) | client-driven |
| `ewmh.rs:259` | `GetProperty.reply()` (`update_status`) | root `WM_NAME` `PropertyNotify` | bar-driven |

### 2.2 Lifecycle only — not reachable from the pointer path

`manage.rs:106,134,228,234,239,251,258,292,315,337,350,887,982,999,1230,1331` (window manage),
`events.rs:71` (`get_window_attributes` on `MapRequest`), `manage.rs:1073-1081` (`transient_for`),
`input.rs:82-143` + `input.rs:236,316` (`setup_root`, `grab_keys`), `mod.rs:848`
(`create_window`), `mod.rs:996/1007` (WM presence check), `mod.rs:1041` (`--replace` handover),
`mod.rs:1078` (`detect_monitors`), `mod.rs:1173/1177/1189/1219/1224` (`fetch_keyboard_state`),
`rootwall.rs:134/139/140`, `ewmh.rs:61/90/110/125/140/157/240/289` (EWMH publishers),
`events.rs:131-147`-adjacent `manage.rs:134`. All of these are **fine** — they sit on manage,
hotplug, keymap or startup paths, not on the per-event path. **[V]**

One note on the two called out in the brief:
* `ewmh.rs:301` (`has_protocol`, from `focus()`) — **hot path.** One blocking RTT per focus change.
* `render.rs:1416` (`reconcile_focus`) — **hot path, and triple-counted.** It is called from
  `focus()` *and* from both focus-event handlers, so a single click-induced focus change pays it
  three times: `render.rs:1253`, `events.rs:938`, `events.rs:910`.

### 2.3 The `.check()` caveat — flagged, not resolved

`xcb_request_check` is documented in `/usr/include/xcb/xcb.h:368-379` as blocking until an error
arrives *or a reply to a later request arrives*, and as performing "a sync if needed". I could not
read the libxcb sources in this environment, so I cannot state statically whether it reads the
socket or only flushes the out-buffer. In practice libxcb's `XCB_REQUEST_CHECK` path flushes and
scans the receive buffer without reading, which would make `.check()` ≈ a `flush()`, not an RTT.
**[H]**

If that reading is wrong, the hot-path `.check()` cost rises: `pointer.rs:483` (1 per click),
`pointer.rs:508` (1 per drag end) and `pointer.rs:168` (1 per Mod4 wheel notch) would each become a
full round trip. That is worth one runtime experiment (compare click latency with `.check()`
removed) but it is **not** the dominant term either way.

---

## 3. Feedback loops

### 3.1 `focus()` → `SetInputFocus`/`ChangeWindowAttributes`/`GrabButton`/`ChangeProperty(_NET_ACTIVE_WINDOW)` → `FocusIn`/`FocusOut` → `reconcile_focus`

Mechanism (all verified):
1. `focus()` issues `SetInputFocus` (`render.rs:1208`).
2. The server delivers `FocusOut(old)` + `FocusIn(new)` to the WM (`FOCUS_CHANGE` is selected on
   every client, `manage.rs:483`).
3. `on_focus_out` (`events.rs:925`) and `on_focus_in` (`events.rs:891`) each call `reconcile_focus`.
4. `reconcile_focus` issues `GetInputFocus` + `.reply()` (`render.rs:1416`). If `logical == real`
   it returns (`render.rs:1458-1462`); otherwise it re-issues `SetInputFocus` (`render.rs:1536`)
   and repaints two borders (`render.rs:1560`, `:1576`).

**Bound: yes, depth ≤ 2.** After the repair `set_input_focus`, the new `FocusIn` sees
`logical == real` and no-ops. Two extra focus-event pairs at most.

The residual cost is not the loop, it is the *amplification*: one logical focus change becomes
**four blocking RTTs** (2 in `focus()`, 2 from the focus events), plus 34 `GrabButton`/`UngrabButton`
requests (`render.rs:1263` + `render.rs:1395`), plus 2 `ChangeWindowAttributes`. That is the
per-click budget. **[V]**

One failure mode worth naming: `SetInputFocus` with a stale `last_event_time` is silently ignored by
the server (`render.rs:1198-1201` documents the `CURRENT_TIME` variant of this). If it is ignored,
`reconcile_focus` re-issues the same stale-timestamp request and the divergence persists silently —
two wasted RTTs per click and a focus state that never converges, until some later event bumps
`last_event_time`. `last_event_time` is written in `on_key` (`events.rs:773`), `on_button_press`
(`pointer.rs:175`, only for detail < 4) and `on_enter` (`events.rs:840`) — **never in `on_motion`**
(`pointer.rs:536-661`). **[V]** for the omission; **[H]** for the server-side ignore semantics.

### 3.2 `WarpPointer` → `MotionNotify` + `EnterNotify`/`LeaveNotify` → `on_enter` → `focus()` → `WarpPointer`

**This is the worst loop, and the working-tree patch made it worse. It is also config-gated.**

`warp_pointer` appears exactly **once** in the whole backend: `mod.rs:719`, inside
`flush_deferred_arrange`. **[V]**

The current code:
```rust
// mod.rs:707-721
let deferred = std::mem::take(&mut self.deferred_focus);
for (mon, w) in deferred {
    self.stack_overlay(mon);
    if self.engine.cfg.warp_cursor { ... self.conn.warp_pointer(NONE, w, 0,0,0,0, dx, dy); }
}
```
`focus()` pushes `(mon_i, w)` into `deferred_focus` **unconditionally, with no dedup**
(`render.rs:1336-1337`). **[V]**

Runaway mechanism (config `focus_mouse = true` **and** `warp_cursor = true`):

* Turn *N*, DRAIN A: two clicks on tiles `w0` then `w1` ⇒ `deferred_focus = [(m,w0), (m,w1)]`,
  `mon.focused == w1`.
* `mod.rs:644` → flush: `arrange(m)`, then **warp to `C(w0)`** (pointer is elsewhere ⇒ server
  generates `MotionNotify` + `EnterNotify(w0)`), then **warp to `C(w1)`** (`LeaveNotify(w0)` +
  `EnterNotify(w1)`). `deferred_focus` is now empty.
* Turn *N+1*, `mod.rs:631` flushes the warps; DRAIN A dispatches
  `EnterNotify(w0)` → `on_enter` (`events.rs:851`) → `focused (w1) != w0` → **`focus(w0)`**
  (`events.rs:871`) → `deferred_focus = [(m,w0)]`. Then `EnterNotify(w1)` → **`focus(w1)`** →
  `deferred_focus = [(m,w0),(m,w1)]`.
* Turn *N+1* flush: 2 warps ⇒ 2 `EnterNotify` ⇒ next turn 2 more `focus()` ⇒ …
* Entry count grows roughly as `n → 2n`. **[H]** on the exact server-side rate (does `WarpPointer`
  to the *same* window emit `EnterNotify`? — no: `PointerWarping` + `DeliverMotionEvents` only
  emits `EnterNotify` when the entered window differs), **[V]** on every step of the Maverick side.

The loop is **self-perpetuating** because the pointer never settles on the window that the queued
`EnterNotify` names: the flush leaves it on the *last* entry, while older entries keep naming
earlier windows. Each turn costs `arrange` + `|deferred_focus| × stack_overlay` +
`|deferred_focus| × warp_pointer` + 2 blocking RTTs per `focus()`, and it never reaches
`wait_readable_fds` with an empty queue, so `poll()` never sleeps. **This matches the reported
symptom exactly (wedged under sustained input, cleared by restart — restart rebuilds the struct
with `deferred_focus: Vec::new()`, `mod.rs:927`).**

Pre-patch behaviour (`git diff`: `render.rs:1325-1356`) warped *inside* `focus()` and left the
pointer on the just-focused window, so the follow-up `EnterNotify` named that same window and the
`on_enter` guard at `events.rs:870` (`if focused != Some(cw)`) short-circuited. **The pre-patch code
converged in one extra iteration; deferring the warp into a batch broke that invariant.**

**`pointer_guard_until` does NOT bound this loop.** It is written in exactly one place —
`on_key`, `events.rs:828-829` — and cleared unconditionally by the first `MotionNotify`
(`pointer.rs:542`). It is never armed by `focus()`, `on_enter`, or any pointer path. It covers the
**keyboard-navigation case only**, exactly as `mod.rs:309-313` describes. **[V]**

**Default-config reachability: `focus_mouse = false` and `warp_cursor = false`**
(`config.rs:110-111`, `config/config.toml:79,81`). **With the shipped defaults this loop is
unreachable.** It only arms for a user who enables both. If the failing repro uses the shipped
config, this loop is *not* the cause — check `INPUT-ROOT-CAUSE.md` and the measurement logs for the
effective config.

### 3.3 `arrange()` → `emit_geometry` → `ConfigureWindow` + synthetic `ConfigureNotify` → `ConfigureNotify` handler → ?

**Bounded, terminates immediately.** The two echoes are both dropped:

* The synthetic `SendEvent` echo is dropped at `events.rs:302` (`response_type & 0x80 != 0`).
* The real echo of our `ConfigureWindow` reaches `events.rs:321` (root-targeted copy only) and is
  classified against `AppliedState`. `Compliant` → no-op (`events.rs:344`). `Stale` → `reassert_stale`
  + one `apply_geom` (`events.rs:353-356`) — and *that* re-assertion's own echo is `Compliant` by
  construction, and the echo that triggered it was already in flight when the re-assert was issued.

So the maximum chain depth is 1 and it does not self-perpetuate. **[V]** The comment at
`events.rs:345-352` states the same argument explicitly.

The residual cost is volume, not loop: one arrange emits **2 requests + 2 events per changed
window**. A monitor with N windows therefore generates ~2N self-events that the WM must read and
discard, and each one keeps the loop from sleeping. With the patch this is once per turn
(`mod.rs:704-706`); without it, once per `focus()` call. **[V]**

### 3.4 `_NET_CLIENT_LIST` rewrite → root `PropertyNotify` → does the WM react?

**No. The loop does not exist.** `flush_client_list` (`ewmh.rs:267-274`) writes
`_NET_CLIENT_LIST` and `_NET_CLIENT_LIST_STACKING` on the root. The root has `PROPERTY_CHANGE`
selected (`input.rs:79`), so the WM gets its own `PropertyNotify` back. Tracing `on_property`
(`events.rs:559-643`):

* `e.window == root && e.atom == WM_NAME`? No.
* `_NET_WM_STRUT[_PARTIAL]`? No. `_NET_WM_BYPASS_COMPOSITOR`? No. `WM_NORMAL_HINTS`? No.
* `e.state == Property::DELETE`? No (it is `NewValue`).
* Final guard `clients.contains_key(&e.window)` — **the root is never a client** (`manage.rs` only
  inserts mapped non-override-redirect children). → falls through to `Ok(())` at `events.rs:642`.

**Verified: the WM does not react to any property it writes to the root**, including
`_NET_ACTIVE_WINDOW` (`render.rs:1339-1345`) and the two client lists. There is no self-trigger
feedback. The cost is one cheap event per flush, plus the `.check()` at `ewmh.rs:157`/`:240`, and it
only runs when `client_list_dirty` (`mod.rs:630`). **[V]**

### 3.5 `GrabButton` churn

`grab_buttons` (`input.rs:365-432`) issues, per call: 1 `UngrabButton(ANY)`, 1 `GrabButton` with
`BUTTON_PRESS / ModMask::ANY / owner_events=false / SYNC|ASYNC` (`input.rs:385-395`), and
8 `mod_variants` × 2 buttons = 16 `GrabButton` (`input.rs:414-430`) — **17 requests**. It is called
from `focus()` (`render.rs:1263`) and `unfocus()` (`render.rs:1395`), so **34 requests per focus
change**, and from `manage()` (`manage.rs:490`) and `refresh_keyboard()` (`mod.rs:473`). **[V]**

* All are F&F (no `.check()`), so none of them blocks. **[V]**
* The server does real work per `GrabButton` (insert/remove an entry in `passiveGrabs`), but it is
  O(1) per request and the WM never waits for the answer. **[H]**
* Grab re-installation does **not** itself generate `GrabNotify` — that is a core-protocol event
  (`input.rs` never selects it, and `dispatch` has no arm for it). **[V]** for "not selected/not
  handled".
* The dangerous part is *ordering*, see §4: these 34 requests are issued **while the pointer is
  frozen by the very SYNC grab they are replacing**.

### 3.6 Other paths checked and cleared

* `on_enter` → `focus()` is guarded by `if focused != Some(cw)` (`events.rs:870`) — one-hop.
* `on_button_press` → `focus()` is guarded by `focused != Some(cw)` (`pointer.rs:264`) and by
  `focused_present` (`pointer.rs:262`).
* `on_button_press` overlay-dismiss (`pointer.rs:291-333`) consumes `pending_focus` and sets
  `replay_event = false` — one-shot, no loop.
* `set_maximized` → `arrange` (`manage.rs:1302`) → echoes → dropped per §3.3.
* `hide_offscreen`/`apply_parked` → `apply_geom` → `emit_geometry` — same, bounded.

---

## 4. The SYNC grab freeze and its effect on the input queue

### 4.1 What is installed

`input.rs:385-395` installs, on **every managed window** (`owner_events=false`, `ModMask::ANY`,
`ButtonIndex::ANY`, `EventMask::BUTTON_PRESS`), a passive grab with
`pointer_mode = GrabMode::SYNC, keyboard_mode = GrabMode::ASYNC`. The comment at `input.rs:374-384`
explains the intent: the WM wants to be able to call `AllowEvents(REPLAY_POINTER)`, which the server
rejects with `BadValue` unless the device is genuinely frozen. **[V]**

`SyncGrabGuard` (`pointer.rs:88-116`) guarantees `allow_events(REPLAY_POINTER)` on drop for
`on_button_press` and `ungrab_pointer` for `on_button_release`. Every normal exit sets
`emitted = true` first (`pointer.rs:165`, `:171`, `:459`/`:472`, `:505`, `:531`). **[V]**

### 4.2 While the pointer is frozen, does `MotionNotify` still arrive?

**No — while a device is frozen by a synchronous grab the server holds that device's events and does
not deliver them until the grab is released.** This is the documented behaviour of a SYNC passive
grab: the activating event and everything the device produces until `AllowEvents`/`AsyncPointer`/
`ReplayPointer` are accumulated in the device's grab-event queue. **[H]** — I could not read the X
protocol spec or Xorg/DIX sources in this environment to cite it line-by-line. It is however the
explicit premise of Maverick's own code (`pointer.rs:14-16`, `input.rs:24-26`).

**Mitigating factor that matters a lot:** the X server *coalesces* motion. `mieqProcessMotionEvents`
replaces the pending motion event rather than appending when the device queue head has not been
consumed, so a freeze of duration *T* produces roughly *O(1)* buffered `MotionNotify` events, not
*O(T × poll rate)*. The freeze therefore creates a bounded burst, not an unbounded backlog.
**[H]** — same unverifiable-server-side caveat.

### 4.3 So can the freeze itself cause the backlog?

**Not as an unbounded queue. Yes, as a latency amplifier, and it has a hard floor.**

* Bounded burst: the frozen device's events are replayed on `AllowEvents` and then drained by DRAIN A.
  They cost ~nothing each (`on_motion` issues 0 X requests when not dragging, §1.4). **[V]**
* Hard floor: while the freeze is held, the pointer is *logically frozen*. **No `MotionNotify` can
  be observed by the WM, and no further button events are dispatched, until the handler returns and
  the server processes `AllowEvents`.** All pointer throughput for that window is serialised behind
  the handler body. **[V]** (given §4.2's premise).
* The handler body is where the cost is. Between the freeze and `allow_events` (`pointer.rs:483`),
  `on_button_press` performs:
  * `find_client` — up to k blocking RTTs (`manage.rs:1230`),
  * `focus()` — **2 blocking RTTs** (`ewmh.rs:301`, `render.rs:1416`) + `has_protocol` again if the
    repair path runs (`render.rs:1546`),
  * `grab_buttons` **twice** — 34 `GrabButton`/`UngrabButton` requests (`render.rs:1395`, `:1263`),
  * `ChangeProperty(_NET_ACTIVE_WINDOW)` (`render.rs:1339`),
  * on the overlay-teardown path, a **full synchronous O(N) `arrange`** (`manage.rs:1302`) —
    `O(N) configure_window` + `O(N) send_event` + `O(N) stack_overlay` `raise`s,
  * optionally `grab_pointer` + `.reply()` (`pointer.rs:414`).
* Two blocking RTTs ≈ 0.1-0.5 ms on a local socket, more under load. 34 grab requests and an
  O(N) arrange are pure server-side + syscall work. **Under saturation, with the server already
  backed up, this window is where a click becomes expensive — and every one of those pointer events
  that arrives during it is queued, not lost.** **[H]** — I cannot measure this statically.

### 4.4 `AllowEvents(REPLAY_POINTER)` re-entering the passive grab — self-sustaining?

**Highest-severity hypothesis in this audit, and genuinely undecidable from this repo.**

The hazard is real in the code and is created by the *ordering*, verified:

```
ButtonPress on w1  ──► server activates passive SYNC grab on w1, pointer FROZEN
pointer.rs:265     ──► focus(w2)
  render.rs:1391   ──► unfocus(w1) ──► input.rs:370 UngrabButton(ANY, w1, ANY)
                                         input.rs:385 GrabButton(SYNC, ANY, ANY, w1)   ← grab on the
                                                                                        FROZEN window
                                                                                        is destroyed
                                                                                        and rebuilt
  render.rs:1263   ──► grab_buttons(w2, true) ──► same for w2
pointer.rs:474-483 ──► AllowEvents(REPLAY_POINTER, e.time)
```

The `ungrab_button` + `grab_button` pair runs **between the freeze and the release, on the very grab
that is holding the freeze**, with an identical catch condition (`BUTTON_PRESS`, `ANY`, `ANY`).

Two server behaviours are possible and I cannot choose between them from here:
1. The server treats the `UngrabButton` as terminating the sync grab (releasing the device and
   dropping the frozen press), and the subsequent `GrabButton` re-arms the passive grab. Then the
   replayed press re-activates it and the device freezes again — but **no `ButtonPress` is
   delivered to the WM this time** (the event was already consumed by the first grab). Nothing will
   ever call `AllowEvents` again. **Permanent global input freeze until restart — exactly the
   reported symptom, and exactly what "restart clears it" looks like.** **[H]**
2. The server's replay path does not re-freeze on a replayed passive-grab event, and the net effect
   is the intended "press goes to the client". **[H]**

Maverick's own comment at `input.rs:374-384` asserts intent (1) is not what happens — the SYNC grab
is installed precisely so replay is legal. If (1) were true the WM would freeze on the very first
click, which would have been noticed. So the *design* assumes (2). But nothing in this repo pins the
server behaviour, and the code deliberately mutates the grab inside the frozen window, which is the
one thing that can flip it.

**Concrete, cheap experiment (not run here, static-analysis-only constraint):** remove the
`grab_buttons` call from `focus()`/`unfocus()` and re-run the saturation test. If the wedge
disappears or becomes non-reproducible, this is the mechanism. Independently: log
`Event::Error` — `dispatch` already routes X errors to `log::debug!` (`mod.rs:414`), so raising that
to `warn!` would immediately reveal the `BadAccess`/`BadValue` that `GrabMatchesSecond`-style
server-side rejections produce. **That is the single highest-value next step, because it converts
this from hypothesis to fact in one run.**

---

## 5. Head-of-line blocking: the answer

**Yes. Any `.reply()` inside `dispatch` prevents the main loop from consuming further input for the
duration of a full server round trip** — there is one thread (`run_once`, `mod.rs:609`), and while it
is inside `.reply()` → `xcb_wait_for_reply` it cannot call `poll_for_event`. Worse, neither DRAIN A
(`mod.rs:638-640`) nor DRAIN B (`mod.rs:672-674`) has any cap, and `wait_readable_fds` (`mod.rs:669`)
is only reached once the queue is **empty**. So if the arrival rate exceeds the drain rate, the loop
never sleeps and latency to any given event grows without bound while the socket queue grows — the
classic unbounded-head-of-line shape. **[V]**

### The dominant blocking operation

**`GetInputFocus` + `.reply()` in `reconcile_focus` — `src/backend/x11/render.rs:1415-1416`.**

It dominates on every axis:

* **Reachability.** Three hot-path entry points: `focus()` (`render.rs:1253` and `render.rs:1365`),
  `on_focus_in` (`events.rs:910`), `on_focus_out` (`events.rs:938`). **[V]**
* **Frequency.** A single click-induced focus change executes it **three times** — once
  synchronously from `focus()`, then once from each of the `FocusOut`/`FocusIn` that its own
  `SetInputFocus` (`render.rs:1208`) caused. That is **3 of the 4 blocking RTTs a click costs**
  (the fourth being `has_protocol`, `ewmh.rs:301`). **[V]**
* **Position in the chain.** It is issued *after* `SetInputFocus` and after the whole `focus()`
  preamble, so its round trip must wait for the server to drain every request queued before it —
  including the 34 `GrabButton`/`UngrabButton` requests from `render.rs:1263` and
  `render.rs:1395`. It is the longest-latency call in the per-click sequence. **[V]**
* **Placement.** On the pointer path it sits **inside the SYNC-grab-frozen window**
  (`render.rs:1253` runs before `pointer.rs:483`'s `AllowEvents`), so it directly extends the time
  the frozen device holds up the input queue. **[V]**

`ewmh.rs:301` (`has_protocol`) is the clear second: one RTT per `focus()`, always, for a value the
WM could cache per window and refresh only on `PropertyNotify(WM_PROTOCOLS)`.

Neither is the *only* problem. In priority order for the saturation failure:

1. **§3.2's warp → `EnterNotify` → `focus()` self-perpetuating loop** (a genuine unbounded loop,
   but gated on `focus_mouse && warp_cursor`, both `false` by default). If the repro config enables
   either, this is *the* bug and nothing else matters.
2. **`reconcile_focus`'s `GetInputFocus` RTT, ×3 per focus change** (the dominant blocking call,
   reachable with the shipped config).
3. **The unbounded drain loops + no budget in `run_once`** (`mod.rs:638-640`, `:672-674`) — this is
   the amplifier that turns any per-event cost into a wedge, and it is the structural reason the
   failure has no natural recovery.
4. **§4.4's grab-mutation-inside-the-frozen-window hazard** — the one candidate that produces a
   *permanent* freeze rather than a slowdown, and the only one that matches "restart clears it" in
   its strictest reading.

---

## Appendix A — what is verified vs. hypothesised

**Verified in source (this working tree):**
* Every operation inventory row in §1, with file:line.
* `warp_pointer` exists in exactly one place (`mod.rs:719`).
* `pointer_guard_until` is armed only in `on_key` (`events.rs:828`) and cleared only in `on_motion`
  (`pointer.rs:542`) — it does not cover the pointer path.
* `deferred_focus` has no dedup and no cap (`render.rs:1337`, `mod.rs:707`).
* `grab_buttons` emits 17 requests and is called twice per focus change (`input.rs:370/385/414`,
  `render.rs:1263`, `render.rs:1395`).
* `find_client` costs one `query_tree` RTT per tree level and costs one level for the root
  (`manage.rs:1221-1240`).
* `reconcile_focus` is called from `focus()`, `on_focus_in` and `on_focus_out` (`render.rs:1253`,
  `events.rs:910`, `events.rs:938`).
* `QueryPointer` is never called.
* The WM ignores its own root property writes (§3.4).
* F&F cookies are provably non-blocking on drop (`x11rb-0.13.2/src/cookie.rs:198-209`).
* `poll_for_event` is non-blocking (`x11rb-0.13.2/src/xcb_ffi/mod.rs:573-589`).
* `set_maximized`'s `arrange` (`manage.rs:1302`) and `on_button_release`'s `arrange`
  (`pointer.rs:524`) run inline and are **not** covered by the coalescing patch.

**Hypotheses / not statically decidable here:**
* X server freeze semantics for a SYNC passive grab (§4.2) and motion coalescing.
* Whether `AllowEvents(REPLAY_POINTER)` re-activates a passive grab that was destroyed and rebuilt
  inside the frozen window (§4.4) — **and whether it leaves the device permanently frozen**.
* Whether `xcb_request_check` (`.check()`) reads the socket or only flushes (§2.3).
* Whether `WarpPointer` emits `EnterNotify` when the destination window is unchanged (§3.2).
* Whether a stale `last_event_time` causes the server to ignore `SetInputFocus` (§3.1).
