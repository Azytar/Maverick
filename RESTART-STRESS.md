# Repeated-Restart Stress Test — Results

Deterministic stress test for `maverickctl restart`: start Maverick on a nested
Xephyr, open real managed clients, then restart the WM **N times in the same
shell** (no terminal close/reopen) and verify full health after every restart.

**Headline result: repeated restart does NOT fail.** 15/15 restarts pass in both
idle and input-stress modes, and 30/30 pass in an extended idle run. The
per-restart session-id churn is real but benign.

---

## 1. Test procedure (exact commands)

```bash
cd /path/to/Maverick-reconstructed

# idle WM between restarts (RESTARTS defaults to 15)
./tests/xephyr-restart-stress.sh

# XTEST input storm during each restart + verification
./tests/xephyr-restart-stress.sh stress

# extended run (more iterations)
RESTARTS=30 ./tests/xephyr-restart-stress.sh
```

What the script does, deterministically, every run:

1. **Preflight** — kill any stray `target/debug/maverick`, `Xephyr`, `mgdwin`,
   `xtest_input` (pattern `target/debug/maveri[c]k` cannot match the live
   `:0` session, whose cmdline is bare `maverick`).
2. **Build helpers** — `tests/mgdwin` and `tests/xtest_input` (cc + `-lX11
   [-lXtst]`).
3. **Pick a free display** — first `:n` in `90..199` with no `/tmp/.X$n-lock`.
4. **Hermetic runtime dir** — `XDG_RUNTIME_DIR=$(mktemp -d /tmp/mrt.XXXX)`;
   `MAVERICK_INSTANCE` is **unset** (this shell is the "user's terminal", not
   a WM child, so resolution must go through the DISPLAY/tty context path).
5. **Start Xephyr** on the nested display, nesting into the host `DISPLAY`
   (`:0`). Wait for `xprop -root` to answer.
6. **Launch maverick** with **no `--session-id`** (the default user flow, so
   every restart re-execs into a NEW random session id → NEW socket path).
7. **Open 3 managed clients** (`mgdwin`, titles `RS_A/RS_B/RS_C`) and record
   their XIDs.
8. **Baseline verification** (V1–V5).
9. **Restart loop** — for each of N iterations: record sid+socket, (stress mode:
   launch `xtest_input 12`), `maverickctl restart`, wait for readiness, record
   new sid+socket, run V1–V5.
10. **Fresh-terminal phase** — F1 clean shell, F2 `MAVERICK_INSTANCE=<current
    sid>`, F3 `MAVERICK_INSTANCE=<stale sid>`, F4 restart under the stale env.
11. **Summary** — per-iteration pass/fail, runtime-dir entry count, totals.

The script exits non-zero if any check fails. It cleans up its Xephyr and
runtime dir via an EXIT trap.

### Readiness gate (important, non-obvious)

`wait_ready` polls **`maverickctl query tree`**, not `state`. The WM event loop
blocks in `wait_readable_fds` when idle and only publishes its state snapshot
after a *wake* (an X11 event, or a control command that pushes to the queue —
`query`/`dispatch`/`restart`/`reload`/`quit`). `state`/`ping`/`identify` are
answered server-side and do **not** wake the loop, so an idle WM answers
`maverickctl state` with `{}` forever (verified: 15 s of `{}` on an idle WM).
`query tree` is processed by the WM thread, so it both wakes the loop
(publishing state) and returns valid JSON — a deterministic readiness+wake
signal. After the first `query tree`, `state` returns full JSON.

---

## 2. Verification checklist after each restart (V1–V5)

From the script header:

| # | Check | How |
|---|-------|-----|
| **V1** | maverickctl responsive | `state` answers JSON with `"monitors"` **and** `query tree` answers JSON with `"instance"` |
| **V2** | windows still managed | every pre-restart client XID is in `_NET_CLIENT_LIST` (xprop) **and** in `maverickctl query tree` |
| **V3** | X11 ownership valid | `_NET_SUPPORTING_WM_CHECK` names a window whose `_NET_WM_NAME` is `maverick` |
| **V4** | runtime state valid | exactly one ALIVE instance in the runtime dir, its socket answers, its pid is alive, `state` JSON well-formed |
| **V5** | WM process alive | the pid recorded in the ficha is running (`kill -0`) |

Stress mode additionally requires the XTEST storm to complete cleanly
(`XTEST_INPUT_DONE` in the log) during the restart window.

---

## 3. Results

### Idle WM — 15 restarts

```
restart stress (idle): 100 passed, 0 failed
iteration results:  1 PASS … 15 PASS   (15/15)
runtime dir entries after run: 17 (alive: 1)
```

Every iteration passed V1–V5. No failure mode observed.

### Idle WM — extended 30 restarts

```
restart stress (idle): 190 passed, 0 failed
iteration results:  1 PASS … 30 PASS  (30/30)
runtime dir entries after run: 32 (alive: 1)
```

### Under input stress — 15 restarts (XTEST storm)

`xtest_input 12 :90` fires ~15 000 synthetic events (keys/motion/clicks) over
12 s during each restart + verification window. Observed volume:
`XTEST_INPUT_DONE keys=5798 motion=7759 click=1429`.

```
restart stress (stress): 115 passed, 0 failed
iteration results:  1 PASS … 15 PASS   (15/15)
runtime dir entries after run: 17 (alive: 1)
```

Every iteration passed V1–V5 **and** the input storm completed cleanly. No
failure mode observed.

### At which iteration does repeated restart break?

**It does not break** — not at iteration 15 (both modes), not at iteration 30
(idle). There is no failure mode to report. The restart path is robust:
the WM keeps the same PID (in-place `exec`), re-adopts the surviving clients,
re-establishes X11 ownership and the control socket every time.

---

## 4. Fresh-terminal resolution (F1–F4)

The script ends by re-running the resolution paths a new shell would take:

| Case | Env | Result |
|------|-----|--------|
| **F1** clean shell | no `MAVERICK_INSTANCE` | **resolves** (DISPLAY/tty context match) |
| **F2** fresh terminal | `MAVERICK_INSTANCE=<current sid>` | **resolves** (env hit) |
| **F3** stale env | `MAVERICK_INSTANCE=<pre-restart sid>` | **resolves** — `read_meta(stale)` returns `None`, so `resolve_target` falls through to context |
| **F4** restart under stale env | `MAVERICK_INSTANCE=<stale sid>` | **succeeds** — same context fallback, then restart |

**A fresh terminal does not need to "avoid" a failure, because there is no
failure.** The session-id instability does not break targeting: when
`MAVERICK_INSTANCE` points at a dead sid, `resolve_target`
(`maverick-sys/src/ctl/mod.rs:604`) simply falls through to the
DISPLAY+TTY context path and finds the new instance. F3/F4 confirm this
empirically.

---

## 5. Observed per-restart socket-path churn

The sid is random per process and the restart re-execs with only the original
`launch_args` (no `--session-id`), so **every restart yields a brand-new sid and
a brand-new control-socket path**. Representative excerpt (idle run, display
`:90`, runtime dir `/tmp/mrt.ixFB/maverick/`):

```
baseline  3f7e-18d9e546cdebb6c0-d3cde244380ef9e7
iter 1    3f7e-18d9e546cdebb6c0-d3cde244380ef9e7 -> 3f7e-18d9e5474e8021af-66f032a8a9d5fa35
iter 2    3f7e-18d9e5474e8021af-66f032a8a9d5fa35 -> 3f7e-18d9e5477283f9e0-2103fdc8c67b8f53
iter 3    3f7e-18d9e5477283f9e0-2103fdc8c67b8f53 -> 3f7e-18d9e5479686927a-b783a172459096ca
iter 4    3f7e-18d9e5479686927a-b783a172459096ca -> 3f7e-18d9e547bd8dbca1-97de49200eeca0cf
iter 5    3f7e-18d9e547bd8dbca1-97de49200eeca0cf -> 3f7e-18d9e547f08bf625-c222d00391689508
…
iter 15   3f7e-18d9e54946bbef2f-62eae5ae373ea55d -> 3f7e-18d9e5496dc2998e-40459a94cef1b760
```

The socket path is `$XDG_RUNTIME_DIR/maverick/<sid>/control.sock`, so it
churns every iteration. The **WM process PID stays constant** across all
restarts (e.g. `pid 16254` for all 15 idle iterations, `pid 20651` for all 15
stress iterations) — confirming the restart is an in-place `exec`
(`actions.rs:129`), not a fork+spawn.

---

## 6. Hypotheses — confirmed / refuted

| Hypothesis | Verdict | Evidence |
|------------|---------|----------|
| Restart re-execs maverick in place (`actions.rs:129`) | **CONFIRMED** | WM PID identical across all restarts |
| Session id is random per process (unless `--session-id`) | **CONFIRMED** | new sid every restart |
| Restart re-execs with only original `launch_args` (no `--session-id`) → new sid → new socket path | **CONFIRMED** | socket path churns every iteration |
| Old sid's ficha/socket are deleted on restart | **CONFIRMED (files)** | `cleanup_meta` removes ficha+socket; only 1 instance alive after the run |
| `MAVERICK_INSTANCE` in a terminal goes stale after restart | **CONFIRMED** | F3 uses a pre-restart sid |
| Stale `MAVERICK_INSTANCE` breaks targeting (`read_meta(old_sid)` → `None`) | **REFUTED** | `resolve_target` falls through to context; F3/F4 resolve and restart fine |
| Fresh terminal (no env) discovers via DISPLAY+TTY context | **CONFIRMED** | F1 passes |
| `teardown_x` releases grabs/redirect; new instance re-adopts clients via `scan_windows` | **CONFIRMED** | V2 passes every iteration — all 3 clients stay managed |

---

## 7. Additional findings

1. **Idle WM answers `maverickctl state` with `{}`.** The event loop blocks in
   `wait_readable_fds` and publishes state only after a wake. `state`/`ping`/
   `identify` don't wake it; `query tree` (WM-thread command) does. This is why
   the readiness gate uses `query tree`. (The live `:0` session returns full
   state because real client activity keeps waking it.)

2. **Restart leaves empty per-session directories behind.** `cleanup_meta`
   (`identity.rs:435`) removes the ficha and socket *files* but not the
   `<sid>/` *directory*. The runtime dir accumulates one empty dir per restart:
   17 entries after 15 restarts, 32 after 30 (only 1 alive). Cosmetic, but it
   grows without bound over many restarts.

3. **Window IDs in `query tree` are decimal** (`"id":4194305`), while
   `_NET_CLIENT_LIST`/mgdwin XIDs are hex (`0x400001`). The test converts
   before comparing.

---

## 8. Test tooling

| File | Role |
|------|------|
| `tests/xephyr-restart-stress.sh` | the deterministic repeated-restart harness (idle + stress modes, V1–V5, F1–F4) |
| `tests/xtest_input.c` → `tests/xtest_input` | XTEST synthetic input sender (stress mode); prints `XTEST_INPUT_DONE` on stdout |
| `tests/mgdwin.c` → `tests/mgdwin` | managed (tiled) client; logs `WINID=0x…` on stderr |
| `target/debug/maverick`, `target/debug/maverickctl` | the WM and its control client (pre-built) |

Requires: `Xephyr`, `x11-utils` (xprop/xwininfo), `gcc`, `python3`.
