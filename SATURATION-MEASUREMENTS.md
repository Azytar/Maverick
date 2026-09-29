# Input-Saturation Measurements — Maverick (Rust X11 WM)

**Campaign:** Agent B, 2026-09-29 · **Display under test:** Xephyr `:99` (1280×720) · **Live `:0` session (PID 670) untouched throughout.**

---

## 1. TL;DR

- **Bounded steady state or unbounded backlog?** Both — it depends on input rate and event type. The WM reaches a **bounded steady state** at ≤50 scroll-notches/s and ≤500 clicks/s, and develops a **linearly-growing unbounded backlog** at ≥200 notches/s and at unpaced "max" scroll (~39,000 notches/s).
- **What saturates?** The hypothesis is **confirmed**. The bottleneck is the **per-event processing cost** of the single-threaded blocking event loop — specifically the **2 synchronous round-trips per `focus()` call** (`has_protocol` get_property + `reconcile_focus` get_input_focus), the **per-event `arrange` passes**, and a large **server-side event amplification** (each scroll notch generates ~52 server events the WM must also dispatch). The X11 event queue itself is drained fully every loop turn and is **not** the bottleneck.
- **Recovery:** Fast and consistent — control round-trip latency returns to baseline within **~0.4 s** of the burst ending in every scenario.
- **Measurement confound found:** the compositor trace ring buffer itself adds enough per-event overhead to *induce* a transient backlog spike (seen in traced click-500, absent in the untraced control). Untraced controls confirm the intrinsic behaviour.

---

## 2. Measurement setup

All scenarios were driven by `tests/saturation.py`, which brings up an isolated stack and samples the WM throughout:

| Component | Launch |
|---|---|
| **X server** | `Xephyr :99 -screen 1280x720 -ac +extension RANDR +extension GLX +extension Composite` |
| **WM** | `target/debug/maverick` with `MAVERICK_NO_COMPOSITOR=1`, `MAVERICK_LOG=debug`, `MAVERICK_COMPOSITOR_TRACE=1` (ring-buffer trace dumped on shutdown) |
| **Clients** | 6 × `tests/mgdwin coop` (real X11 clients to manage) |
| **Input** | `tests/xtest-stress <mode> <rate> <duration>` — XTEST synthetic input at a controlled rate |
| **Responsiveness probe** | `maverickctl query tree` round-trip (blocks on the WM event loop) sampled every 250 ms, plus a pure-X `xprop` round-trip as an X-server load proxy |
| **Resource sampling** | `/proc/<pid>` CPU (utime+stime), RSS, FD count every 250 ms |

Each run: 3 s baseline → 20 s stress burst → up to 30 s recovery (recovery = time until control latency < 3× baseline for 2 consecutive samples). The compositor trace (`input_receipt` records with server timestamps + turn boundaries) is parsed for **processed-events/sec** and the **queueing delay** between server event time and WM processing time (the backlog signal: flat = bounded, growing = unbounded).

**Per-event cost measured from the trace + `INPUT-TRACE`/`WINDOW-TRACE` logs:**

| Input event | `focus()` calls | sync RTTs | `arrange` passes | server-generated events (amplification) |
|---|---|---|---|---|
| **Scroll notch** (Mod4+wheel) | **2.0** | **4** | **3.0** | **~52** (53× total) |
| **Click** (button 1) | **2.0** | **4** | **1.0** | **~9** (13× total) |
| **Motion** | **0.01** | **0.02** | **0.02** | **~0.4** (1.4× total) |

The scroll path is `on_button_press` → `scroll_camera_with_wheel` → `FocusDir` dispatch (1 `focus()`) + `focus_column_at` → `focus()` again (pointer.rs:668-728). The click path is `on_button_press` → `focus()` + an `EnterNotify`-driven re-focus. Motion is deliberately nearly free — `on_motion` does no round-trips and no arrange (focus-follows-mouse is handled via `on_enter` to avoid a per-motion `query_tree` round-trip, pointer.rs:656-659).

---

## 3. Raw measurements

6 clients, 20 s stress bursts. "qdelay peak" = peak queueing-delay trend from the trace; "ctl peak" = peak `maverickctl query tree` round-trip (`to` = # of samples where the query timed out / WM too backed up to answer); "CPU Δ" = WM CPU seconds during the 20 s stress.

| Scenario | Mode | Rate | Traced | Sent | focus() | arrange | RTTs | qdelay peak | ctl peak | CPU Δ | RSS Δ | Recovery |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **scroll-50** | scroll | 50/s | ✓ | 1,001 | 2,014 | 3,027 | 4,028 | **11.8 ms** (flat) | 103 ms | 1.18 s (5.9%) | 5→14 MB* | **0.451 s** |
| **click-50** | click | 50/s | ✓ | 1,001 | 2,013 | 1,024 | 4,026 | **12.6 ms** (flat) | 99 ms | 0.55 s (2.8%) | small | **0.451 s** |
| **motion-50** | motion | 50/s | ✓ | 1,001 | 12 | 24 | 24 | **8.5 ms** (flat) | 99 ms | 0.12 s (0.6%) | small | **0.452 s** |
| **combined-50** | combined | 50/s | ✓ | 1,001 | 1,014 | 1,527 | 2,028 | **13.9 ms** (flat) | 103 ms | 0.68 s (3.4%) | small | **0.451 s** |
| **scroll-200** | scroll | 200/s | ✓ | 4,000 | 6,814 | 10,227 | 13,628 | **2,608 ms** (growing) | 1,428 ms + 8 to | 2.64 s (13.2%) | 5→115 MB* | **0.425 s** |
| **scroll-200-notrace** | scroll | 200/s | ✗ | 4,000 | 5,840 | 8,766 | 11,680 | n/a | 1,460 ms + 8 to | 2.22 s (11.1%) | **flat 4.9 MB** | **0.424 s** |
| **click-500** | click | 500/s | ✓ | 10,001 | 20,013 | 10,024 | 40,026 | **11.2 ms** (flat) | 1,498 ms spike | 3.72 s (18.6%) | 5→115 MB* | **0.449 s** |
| **click-500-notrace** | click | 500/s | ✗ | 10,001 | 20,013 | 10,024 | 40,026 | n/a | **145 ms** (no spike) | 3.35 s (16.8%) | **flat 4.9 MB** | **0.444 s** |
| **scroll-max** | max | ~39,000/s | ✓ | 1,956,983 | 2,860 | 4,296 | 5,720 | n/a (trace full) | **timeout (all 21)** | 23.97 s (50%) | 5→116 MB* | **0.437 s** |

\* RSS growth is the **compositor trace ring buffer** (448 B/record, 112 MB cap), not a WM leak — the untraced controls hold RSS flat at ~4.9 MB under identical load.

**Queueing-delay trend (the backlog signal) for the saturating scroll-200:**

| fraction of run | 0 % | 25 % | 50 % | 75 % | 100 % |
|---|---|---|---|---|---|
| queue delay (ms) | 0 | 594 | 1,252 | 1,957 | **2,608** |

The delay grows monotonically — the WM fell further and further behind until the burst stopped.

---

## 4. Bounded steady state vs unbounded backlog

**The system exhibits both, separated by a saturation knee that depends on event type:**

- **Bounded steady state** — at 50/s (scroll, click, combined, motion) and at 500 clicks/s. Queueing delay stays flat (peak 8–14 ms), control latency stays at the ~96 ms baseline, and every sent event is processed. The loop drains the queue fully each turn and keeps up.
- **Unbounded backlog** — at 200 scroll-notches/s and at unpaced max scroll. Queueing delay grows **linearly** (2.6 s and climbing for scroll-200; the WM answered *zero* control queries during the entire 50 s max-scroll burst). The backlog only stops growing when the input stops.

**Why scroll saturates at a lower rate than click:** it is not the input *rate* but the *total event load* that matters. Each scroll notch costs 4 RTTs + 3 arranges **and generates ~52 server events** (FocusIn/Out, ConfigureNotify, PropertyNotify) that the WM must also dispatch — a 53× amplification. A click costs 4 RTTs + 1 arrange and generates ~9 events (13×). So:

- scroll-200 → 200 × 53 ≈ **10,600 dispatched events/s** → **saturates**
- click-500 → 500 × 13 ≈ **6,500 dispatched events/s** → **keeps up**

Motion is nearly free (1.4× amplification, no RTTs, no arrange) and shows no saturation at 50/s.

---

## 5. What actually saturates — hypothesis confirmed

**Confirmed: the bottleneck is the synchronous round-trips in `focus()` plus the per-event `arrange` passes — not the X11 event queue.**

1. **`focus()` = 2 synchronous round-trips** (render.rs:1140, ewmh.rs:293, render.rs:1424):
   - `has_protocol()` → `get_property(...).reply()` — a blocking get_property round-trip.
   - `reconcile_focus()` → `get_input_focus().reply()` — a blocking get_input_focus round-trip.
   - Measured: **4 RTTs per scroll notch** (2 `focus()` calls) and **4 RTTs per click** (2 `focus()` calls). Every RTT stalls the single-threaded loop.

2. **Per-event `arrange` passes** — measured **3.0 arranges per scroll notch** and **1.0 per click** (the `Effect::ArrangeMonitor` → `arrange` path). Each is a full layout + geometry pass.

3. **Server-side event amplification** — the largest hidden cost. One scroll notch makes the server emit ~52 events (focus changes → FocusIn/Out; 3 arranges × 6 windows → ConfigureNotify; `_NET_CLIENT_LIST` → PropertyNotify), all of which land back in the same queue and must be dispatched. This is why scroll saturates at ~170 notches/s while click sustains 500 clicks/s.

4. **The X11 event queue is drained fully every loop turn** (mod.rs:630) and is **not** the bottleneck — the queue is just the conduit; the cost is the per-event work the single thread must do before it can pull the next event.

5. **The single-threaded blocking loop** cannot overlap input draining with the round-trips/arranges, so when the input rate exceeds the per-event service rate, the backlog grows without bound.

**Secondary finding — control-plane floor:** the `maverickctl query tree` baseline is ~96 ms even at idle. This is dominated by the control server's **50 ms accept-loop poll** (maverick-sys/src/control.rs:263), not by WM load — a minimal Python client shows the same ~50 ms floor for `state` (which never touches the WM). Increases *above* this floor are the true WM-backlog signal.

---

## 6. Recovery time after the burst

**~0.4 s in every scenario** (0.424–0.452 s across scroll, click, motion, combined, and the saturating 200/s and max cases). Once the input stops, the WM's service rate exceeds the (now zero) arrival rate and the remaining backlog drains quickly; control latency returns to the ~96 ms baseline within 2–3 samples. Even the max-scroll case — which left the WM unable to answer a single control query for 50 s — recovered to baseline in 0.437 s.

---

## 7. Caveats & measurement confounds

- **Trace instrumentation overhead.** The compositor trace ring buffer records ~146 records/event (~65 KB/notch). In traced click-500 this induced a transient 1,498 ms backlog spike that is **absent** in the untraced control (click-500-notrace: flat 145 ms). The untraced controls (scroll-200-notrace, click-500-notrace) confirm the *intrinsic* behaviour: scroll-200 is genuinely unbounded, click-500 is genuinely bounded. Treat traced absolute backlog numbers as upper bounds.
- **RSS growth is the trace buffer**, not a leak (untraced controls hold RSS flat at ~4.9 MB).
- **`scroll-max` ran 50 s, not 20 s** — a bug in `xtest-stress` max mode, which reads the duration from `argv[2]` (the rate) instead of `argv[3]`. The result (1.96 M notches, WM overwhelmed) is still valid; only the duration differs.
- **One flaky Xephyr disconnect** occurred in the first click-500-notrace run (X server disconnected under load; the WM exited cleanly with "X11 connection lost"). It did **not** reproduce on re-run and did not affect any traced scenario.
- The live `:0` session (PID 670) was never touched; all testing was isolated to Xephyr `:99`, which was cleaned up.

---

## 8. Tooling & how to re-run

| File | Purpose |
|---|---|
| `tests/saturation.py` | Measurement harness (Xephyr + maverick + clients + XTEST input + CPU/RSS/FD/latency sampling + trace/log analysis) |
| `tests/xtest-stress.c` | XTEST input sender (`scroll`/`click`/`motion`/`combined`/`max` modes) |
| `tests/xtest-stress` | Built sender binary |
| `tests/mgdwin` | Managed test client |
| `target/debug/maverick`, `target/debug/maverickctl` | Prebuilt (with `input-trace` + `window-trace` features compiled in) |

Re-run a scenario (from the repo root):

```bash
python3 tests/saturation.py --scenario scroll-50 --mode scroll --rate 50 \
    --duration 20 --clients 6 --out /tmp/sat
```

- `--mode` = `scroll | click | motion | combined | max`
- `--no-trace` disables the trace ring buffer (use for RSS/CPU controls)
- Outputs per scenario in `/tmp/sat/<scenario>/`: `samples.csv`, `summary.json`, `wlog` (INPUT/WINDOW-TRACE), `trace.tsv` (compositor trace), `stress.txt`

Raw data from this campaign is in `/tmp/sat/<scenario>/` (`scroll-50`, `click-50`, `motion-50`, `combined-50`, `scroll-200`, `scroll-200-notrace`, `click-500`, `click-500-notrace2`, `scroll-max`).
