#!/usr/bin/env bash
#
# Shared harness for Maverick Xephyr end-to-end tests (Fase 6).
#
# Provides:
#   build_helpers        compile tests/mgdwin (+ pxsample/staticwin/winmove if absent)
#   start_xephyr DISP W H   start a nested X server, return its pid
#   mav_launch DISP ARGS..  start `maverick ARGS` on DISP, return its pid
#   mav_cleanup           kill tracked pids + helper clients, remove runtime dir
#
# Every script MUST `source` this file and `trap mav_cleanup EXIT ERR` so that
# Xephyr / maverick / helper processes never leak between scenarios.

set -u

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$APP_DIR/tests"
cd "$APP_DIR"

MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
MAVERICK_CTL="${MAVERICK_CTL:-./target/debug/maverickctl}"
MAVERICK_MSG="${MAVERICK_MSG:-./target/debug/maverick-msg}"

# Short, explicit XDG_RUNTIME_DIR so the control-socket path stays under
# SUN_LEN (the default can be too long in some sandboxes).
export XDG_RUNTIME_DIR="$(mktemp -d /tmp/mrt.XXXX)"

declare -a MAV_PIDS=()
declare -a XEPHYR_PIDS=()
declare -a HELPER_PIDS=()

mav_cleanup() {
    local p
    for p in "${HELPER_PIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null; done
    for p in "${MAV_PIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null; done
    for p in "${XEPHYR_PIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null; done
    # bracket-regex avoids matching this very shell's own command line.
    pkill -f "tests/mgdwi[n]" 2>/dev/null
    pkill -f "target/debug/maveri[c]ck" 2>/dev/null
    pkill -x Xephyr 2>/dev/null
    rm -rf "$XDG_RUNTIME_DIR"
}

# Kill any leftovers from a previous (possibly timed-out) run so a competing WM
# on the same DISPLAY can't make `maverickctl` return `{}` or hang. Uses the
# bracket trick so it never matches the running script's own shell.
mav_preflight() {
    pkill -9 -f "target/debug/maveri[c]ck" 2>/dev/null
    pkill -9 -x Xephyr 2>/dev/null
    pkill -9 -f "tests/mgdwi[n]" 2>/dev/null
    sleep 0.3
}

# ── build the managed-window helper + any missing static helpers ──────────────
build_helpers() {
    [ -x "$BIN_DIR/mgdwin" ] || cc -O2 -o "$BIN_DIR/mgdwin" "$BIN_DIR/mgdwin.c" -lX11 2>/dev/null \
        || { echo "FAIL: could not build tests/mgdwin"; exit 1; }
    for h in staticwin pxsample winmove; do
        [ -x "$BIN_DIR/$h" ] || cc -O2 -o "$BIN_DIR/$h" "$BIN_DIR/$h.c" -lX11 -lXcomposite 2>/dev/null
    done
}

# ── start a nested X server; echo its pid ─────────────────────────────────────
start_xephyr() {
    local disp="$1" w="${2:-1280}" h="${3:-720}"
    Xephyr "$disp" -screen "${w}x${h}" -ac \
        +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
        >"/tmp/xephyr${disp}.log" 2>&1 &
    local realpid=$!
    XEPHYR_PIDS+=("$realpid")
    local i=0
    while [ $i -lt 60 ]; do
        DISPLAY="$disp" xprop -root >/dev/null 2>&1 && break
        sleep 0.1
        i=$((i+1))
    done
    echo "$realpid"
}

# ── start maverick on DISP with ARGS; echo its pid ───────────────────────────
mav_launch() {
    local disp="$1"; shift
    DISPLAY="$disp" "$MAVERICK_BIN" "$@" >"/tmp/mav${disp}.log" 2>&1 &
    local pid=$!
    MAV_PIDS+=("$pid")
    local i=0
    while [ $i -lt 80 ]; do
        # Wait for a *real* state snapshot (not the transient `{}` some
        # startup phases emit), so callers don't proceed on empty state.
        if DISPLAY="$disp" "$MAVERICK_CTL" state 2>/dev/null | grep -q '"monitors"'; then
            break
        fi
        sleep 0.1
        i=$((i+1))
    done
    echo "$pid"
}

# is a process (by pid) still alive?
alive() { kill -0 "$1" 2>/dev/null; }

# count workspaces reported by `maverickctl state` on DISP
ws_count() {
    DISPLAY="$1" "$MAVERICK_CTL" state 2>/dev/null \
        | python3 -c "import sys,json;d=json.load(sys.stdin);print(len(d['monitors'][0]['workspaces']))" 2>/dev/null
}

# count MANAGED clients reported by `maverickctl query tree` on DISP
# (every managed window emits an "instance":"…" field in the per-window JSON).
win_count() {
    DISPLAY="$1" "$MAVERICK_CTL" query tree 2>/dev/null | grep -c '"instance":"'
}

# count fullscreen windows reported by `maverickctl query tree` on DISP
fs_count() {
    DISPLAY="$1" "$MAVERICK_CTL" query tree 2>/dev/null | grep -c '"fullscreen":true'
}

# wait up to $2 seconds for pid $1 to exit (0 = exited, 1 = still alive)
mav_wait_exit() {
    local pid="$1" max="${2:-10}" i=0
    while alive "$pid"; do
        [ $i -ge "$max" ] && return 1
        sleep 0.2; i=$((i+1))
    done
    return 0
}

# SIGKILL pid $1 and wait up to $2 seconds for it to be gone
mav_kill_wait() {
    kill -9 "$1" 2>/dev/null
    mav_wait_exit "$1" "${2:-10}"
}
