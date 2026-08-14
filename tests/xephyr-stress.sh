#!/usr/bin/env bash
#
# Riesgo 7 — hostile stress-client scenario for Maverick.
#
# Spins up a private Xephyr (does NOT touch the host DISPLAY), launches maverick
# on it, and drives the self-contained hostile client (tests/stress.c) which,
# per iteration, hammers a window with: Map, ConfigureRequest (incl. degenerate
# 0x0 and huge geometry), synthetic ConfigureNotify, _NET_ACTIVE_WINDOW
# ClientMessage, Resize, Unmap, Remap, Destroy.
#
# The client requests the appropriate EWMH/ICCCM hints so maverick treats the
# window as: tiled, fullscreen, float, maximize, and transient/modal.
#
# This is DETECTION ONLY. It does NOT modify maverick source, and maverick is
# run as-is (no backoff). We observe:
#   - client counters: config-request / config-notify / focus-in / focus-out /
#     state-prop-change (reassertion proxy)
#   - maverick liveness (kill -0), responsiveness (xprop -root under `timeout`),
#     and crash/hang (timeout on the client == X calls blocked == WM loop).
#   - OPTIONALLY, with MAVERICK_TRACE=1, maverick is rebuilt with --features
#     input-trace and the log is grepped for `reconcile_focus REPAIR` (focus
#     oscillation) and geometry `reconcile` calls (reconciliation count).
#
# Run:  ./tests/xephyr-stress.sh
# Env:  STRESS_ITERS (default 500)  MAVERICK_BIN  MAVERICK_TRACE=1  HANG_TIMEOUT (default 90s)

set -u

SCREEN_W=1920
SCREEN_H=1080
XEPHYR_DISPLAY="${XEPHYR_DISPLAY:-:99}"
STRESS_ITERS="${STRESS_ITERS:-500}"
HANG_TIMEOUT="${HANG_TIMEOUT:-90}"
MAVERICK_BIN="${MAVERICK_BIN:-./target/release/maverick}"
MAVERICK_TRACE="${MAVERICK_TRACE:-0}"

BINDIR="$(cd "$(dirname "$0")" && pwd)"
STRESS_SRC="$BINDIR/stress.c"
STRESS_BIN="${STRESS_BIN:-/tmp/maverick-stress}"
LOG="$(mktemp -t maverick-stress.XXXXXX.log)"
XLOG="$(mktemp -t maverick-stress-xephyr.XXXXXX.log)"

PASS=0
FAIL=0
FAILURES=()

log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); FAILURES+=("$*"); }

cleanup() {
    [ -n "${STRESS_PID:-}" ] && kill "$STRESS_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
}
trap cleanup EXIT

# ── build the hostile client ──────────────────────────────────────────────
if ! cc -O2 -o "$STRESS_BIN" "$STRESS_SRC" -lX11 2>>"$LOG"; then
    log "FATAL: failed to compile $STRESS_SRC (see $LOG)"
    exit 2
fi
[ -x "$STRESS_BIN" ] || { log "FATAL: stress binary not executable"; exit 2; }

# ── optional input-trace build (no source modification) ──────────────────
if [ "$MAVERICK_TRACE" = "1" ]; then
    log "MAVERICK_TRACE=1: rebuilding maverick with --features input-trace (detection only)"
    if ! cargo build --release --features input-trace >>"$LOG" 2>&1; then
        log "WARN: input-trace build failed; falling back to $MAVERICK_BIN for external observation"
    else
        MAVERICK_BIN="./target/release/maverick"
        log "input-trace build OK: $MAVERICK_BIN"
    fi
fi

# ── bring up the nested server ────────────────────────────────────────────
Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac +extension RANDR \
    >"$XLOG" 2>&1 &
XEPHYR_PID=$!
sleep 1
export DISPLAY="$XEPHYR_DISPLAY"

# ── launch maverick ───────────────────────────────────────────────────────
"$MAVERICK_BIN" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1

mav_alive() { kill -0 "$MAV_PID" 2>/dev/null; }
mav_responsive() {
    # maverick is responsive iff the X server answers a root query within 5s.
    timeout 5 xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1
}

if ! mav_responsive; then
    bad "maverick did not become responsive on $DISPLAY (see $LOG)"
    exit 1
fi
ok "maverick started (pid $MAV_PID) on $DISPLAY"

KINDS="tiled fullscreen float maximize transient"

for kind in $KINDS; do
    log "──────── kind=$kind iters=$STRESS_ITERS ────────"
    # Snapshot maverick CPU time before, to spot a run-away reconcile loop.
    CPU_BEFORE="$(ps -o times= -p "$MAV_PID" 2>/dev/null | tr -d ' ')"

    # Run the hostile client under `timeout`: if maverick enters an infinite
    # loop the X calls block and the client never prints STRESS_DONE → timeout.
    out="$(timeout "$HANG_TIMEOUT" "$STRESS_BIN" "$kind" "$STRESS_ITERS" 2>/dev/null)"
    rc=$?

    if [ "$rc" -eq 124 ]; then
        bad "HANG detected for kind=$kind (client blocked > ${HANG_TIMEOUT}s — maverick likely in an infinite loop / ConfigureNotify storm)"
        # Capture whatever maverick is doing, then stop.
        if [ "$MAVERICK_TRACE" = "1" ]; then
            log "reconcile_focus REPAIR lines so far: $(grep -c 'reconcile_focus REPAIR' "$LOG" 2>/dev/null)"
        fi
        break
    fi

    if ! mav_alive; then
        bad "CRASH: maverick exited during kind=$kind (rc=$rc)"
        break
    fi
    if ! mav_responsive; then
        bad "CRASH/HANG: maverick unresponsive after kind=$kind"
        break
    fi

    CPU_AFTER="$(ps -o times= -p "$MAV_PID" 2>/dev/null | tr -d ' ')"
    CPU_DELTA=$(( ${CPU_AFTER:-0} - ${CPU_BEFORE:-0} ))

    # Emit the client's counters.
    printf '%s\n' "$out" | tee -a "$LOG"

    # Pull counters out for the pass/fail note.
    cfg_req="$(printf '%s\n' "$out"   | grep -oE 'configure_request=[0-9]+'   | cut -d= -f2)"
    cfg_nfy="$(printf '%s\n' "$out"   | grep -oE 'configure_notify_recv=[0-9]+' | cut -d= -f2)"
    fin="$(printf '%s\n' "$out"       | grep -oE 'focus_in=[0-9]+' | cut -d= -f2)"
    fout="$(printf '%s\n' "$out"      | grep -oE 'focus_out=[0-9]+' | cut -d= -f2)"
    pstate="$(printf '%s\n' "$out"    | grep -oE 'state_prop_change=[0-9]+' | cut -d= -f2)"

    note="kind=$kind: cfg_req=${cfg_req:-?} cfg_notify=${cfg_nfy:-?} focus_in=${fin:-?} focus_out=${fout:-?} state_changes=${pstate:-?} cpu_delta=${CPU_DELTA}s"
    if printf '%s\n' "$out" | grep -q 'STRESS_DONE'; then
        ok "survived $kind ($note)"
    else
        bad "client did not finish $kind (no STRESS_DONE marker) — $note"
    fi

    # Divergence probe: after the churn, the window is destroyed. The client
    # list should be empty (no zombie managed windows). We just check maverick
    # still answers; a leftover zombie would surface as a stuck/leaked window
    # and is best confirmed manually via xprop -root _NET_CLIENT_LIST.
    nclients="$(xprop -root _NET_CLIENT_LIST 2>/dev/null | grep -oE '0x[0-9a-f]+' | wc -l)"
    log "  post-run managed-client count on root: ${nclients}"
done

# ── crash / panic scan of the maverick log ────────────────────────────────
if grep -qiE 'panic|thread .* panicked|core dumped|Segmentation fault|double free' "$LOG" 2>/dev/null; then
    bad "maverick log contains a panic/crash signature"
    log "---- excerpt ----"
    grep -iE 'panic|thread .* panicked|core dumped|Segmentation fault|double free' "$LOG" | head -20 | tee -a "$LOG"
fi

# ── focus oscillation / reconciliation scan (only with input-trace) ───────
if [ "$MAVERICK_TRACE" = "1" ]; then
    repair="$(grep -c 'reconcile_focus REPAIR' "$LOG" 2>/dev/null)"
    bail="$(grep -c 'reconcile_focus BAIL' "$LOG" 2>/dev/null)"
    recon="$(grep -c 'reconcile' "$LOG" 2>/dev/null)"
    log "INPUT-TRACE: reconcile_focus REPAIR=$repair BAIL=$bail (total reconcile lines=$recon)"
    if [ "${repair:-0}" -gt 200 ]; then
        log "NOTE: high reconcile_focus REPAIR count ($repair) — possible focus oscillation; inspect $LOG"
    fi
fi

log "────────────────────────────────────────"
log "Riesgo7 stress: $PASS passed, $FAIL failed"
log "maverick log: $LOG"
log "xephyr log:  $XLOG"
[ "$FAIL" -eq 0 ]
