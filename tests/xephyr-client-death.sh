#!/usr/bin/env bash
#
# F4 — client death during fullscreen / focus / layout / animation (CRITICAL).
#
# For each variant: open managed windows, drive the WM into the target state,
# SIGKILL a client mid-state, then assert the WM did NOT crash:
#   * maverick process still alive
#   * `maverickctl state` still answers
#   * no "panic" / "thread panicked" in the log
#   * report (do not hide) any "BadWindow" occurrences (possible stale id)
#
# Run: bash tests/xephyr-client-death.sh

set -u
source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
trap mav_cleanup EXIT ERR

PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

DISP=":98"
MAV_LOG="/tmp/mav${DISP}.log"
XEP="$(start_xephyr "$DISP" 1280 720)"
MAV="$(mav_launch "$DISP")"

# launch a managed window with a given title; echo its pid
open_win() {
    local title="$1"
    export DISPLAY="$DISP"
    MGDTITLE="$title" "$BIN_DIR/mgdwin" >/dev/null 2>&1 &
    local pid=$!
    HELPER_PIDS+=("$pid")
    echo "$pid"
}

# return the pid of the window whose title matches $1 (from our own array)
pid_of() { eval "echo \${PID_$1:-}"; }

assert_survives() {
    local label="$1"
    if alive "$MAV"; then ok "[$label] maverick alive after client SIGKILL";
    else bad "[$label] maverick CRASHED after client SIGKILL"; fi
    if DISPLAY="$DISP" "$MAVERICK_CTL" state >/dev/null 2>&1; then
        ok "[$label] maverickctl state still responds"
    else
        bad "[$label] maverickctl state FAILED after client death"
    fi
    if grep -qi "panic\|thread '.*' panicked" "$MAV_LOG" 2>/dev/null; then
        bad "[$label] PANIC detected in maverick log"
    else
        ok "[$label] no panic in maverick log"
    fi
    local bw
    bw="$(grep -c "BadWindow" "$MAV_LOG" 2>/dev/null || true)"
    if [ "$bw" -gt 0 ]; then
        echo "NOTE [$label]: $bw 'BadWindow' line(s) in log (possible stale id — reported, not failed)"
    fi
}

# ── variant 1: death DURING fullscreen ────────────────────────────────────────
echo "=== F4.1 client death during FULLSCREEN ==="
P1="$(open_win WIN1)"; PID_WIN1="$P1"
P2="$(open_win WIN2)"; PID_WIN2="$P2"
P3="$(open_win WIN3)"; PID_WIN3="$P3"   # last mapped -> focused
sleep 1
# make the focused (WIN3) window fullscreen
DISPLAY="$DISP" "$MAVERICK_CTL" msg toggle_fullscreen >/dev/null 2>&1
sleep 1
FS="$(fs_count "$DISP")"
[ "$FS" -ge 1 ] && ok "a window is fullscreen (count=$FS)" || bad "no fullscreen window (count=$FS)"
kill -9 "$PID_WIN3" 2>/dev/null
sleep 1.5
assert_survives "fullscreen-death"

# ── variant 2: death DURING focus change ──────────────────────────────────────
echo "=== F4.2 client death during FOCUS change ==="
P4="$(open_win WIN4)"; PID_WIN4="$P4"
P5="$(open_win WIN5)"; PID_WIN5="$P5"
sleep 1
DISPLAY="$DISP" "$MAVERICK_CTL" msg focus-left >/dev/null 2>&1
sleep 0.4
kill -9 "$PID_WIN4" 2>/dev/null
sleep 0.4
DISPLAY="$DISP" "$MAVERICK_CTL" msg focus-right >/dev/null 2>&1
sleep 1
assert_survives "focus-death"

# ── variant 3: death DURING layout / workspace switch ─────────────────────────
echo "=== F4.3 client death during LAYOUT / workspace switch ==="
P6="$(open_win WIN6)"; PID_WIN6="$P6"
sleep 0.6
DISPLAY="$DISP" "$MAVERICK_CTL" msg view 2 >/dev/null 2>&1
sleep 0.4
kill -9 "$PID_WIN6" 2>/dev/null
sleep 0.4
DISPLAY="$DISP" "$MAVERICK_CTL" msg view 1 >/dev/null 2>&1
sleep 1
assert_survives "layout-death"

# ── variant 4: death DURING animation (rapid focus changes) ──────────────────
echo "=== F4.4 client death during ANIMATION ==="
P7="$(open_win WIN7)"; PID_WIN7="$P7"
P8="$(open_win WIN8)"; PID_WIN8="$P8"
P9="$(open_win WIN9)"; PID_WIN9="$P9"
sleep 1
# hammer focus changes to keep animations live, kill a client mid-stream
for i in 1 2 3 4 5; do
    DISPLAY="$DISP" "$MAVERICK_CTL" msg focus-left >/dev/null 2>&1
    DISPLAY="$DISP" "$MAVERICK_CTL" msg focus-right >/dev/null 2>&1
    sleep 0.15
    if [ "$i" = "3" ]; then kill -9 "$PID_WIN8" 2>/dev/null; fi
    sleep 0.15
done
sleep 1
assert_survives "animation-death"

echo "────────────────────────────────────"
echo "F4 client-death: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
