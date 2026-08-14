#!/usr/bin/env bash
#
# Maverick restart-survival harness (regression for the wallpaper/restart bug).
#
# Reproduces the reported failure: after `maverick-msg restart` (a re-exec), the
# previous compositor's GPU context is gone, so every pre-existing window must
# be re-adopted AND have its texture rebound — otherwise the renderer skips
# windows with no texture and the tiles vanish (only the wallpaper, if any,
# shows). This asserts a live window is still drawn after a restart.
#
# Scenario:
#   1. open a solid-colour window, sample it (baseline: drawn).
#   2. set a wallpaper (mirrors the user's "cambiar el fondo" step).
#   3. maverick-msg restart.
#   4. sample the SAME window again — must still show its colour (tiles survive).
#
# Requires: xephyr, x11-utils, ffmpeg, gcc. Helpers are built if missing.
# Run: ./tests/xephyr-restart.sh

set -u

SCREEN_W=1920
SCREEN_H=1080
XEPHYR_DISPLAY=":98"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
MSG_BIN="${MSG_BIN:-./target/debug/maverick-msg}"
BINDIR="$(cd "$(dirname "$0")" && pwd)"
# Short, explicit XDG_RUNTIME_DIR so the control-socket path stays under
# SUN_LEN (the default can be too long in some sandboxes). Shared by the daemon
# and the client tools (maverickctl/maverick-msg).
RTDIR="$(mktemp -d /tmp/mrt.XXXX)"
export XDG_RUNTIME_DIR="$RTDIR"
LOG="$(mktemp -t maverick-restart.XXXXXX.log)"
PASS=0
FAIL=0

log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); }

assert_px() {
    "$BINDIR/pxsample" "$1" "$2" "$3" "$4" "0x$5" >/dev/null 2>&1 \
        && ok "pixel $6 @($1,$2) is 0x$5" \
        || bad "pixel $6 @($1,$2) is NOT 0x$5"
}

# ── build helpers if absent ───────────────────────────────────────────────────
for h in staticwin pxsample winmove; do
    [ -x "$BINDIR/$h" ] || cc -O2 -o "$BINDIR/$h" "$BINDIR/$h.c" -lX11 -lXcomposite 2>>"$LOG" \
        || { bad "failed to build helper $h"; exit 1; }
done

WP_DIR="$(mktemp -d /tmp/maverick-restart.XXXXXX)"
BLUE="$WP_DIR/wp_blue.png"
ffmpeg -f lavfi -i "color=c=0x0000ff:s=${SCREEN_W}x${SCREEN_H}" -frames:v 1 "$BLUE" -y 2>>"$LOG" \
    || { bad "ffmpeg could not render blue fixture"; exit 1; }

cleanup() {
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    pkill -f staticwin 2>/dev/null
    rm -rf "$WP_DIR" "$RTDIR"
}
trap cleanup EXIT

Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    >"$LOG.xephyr" 2>&1 &
XEPHYR_PID=$!
sleep 1
export DISPLAY="$XEPHYR_DISPLAY"

"$MAVERICK_BIN" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1.5
xprop -root >/dev/null 2>&1 && ok "maverick started on $DISPLAY" \
    || { bad "maverick did not start on $DISPLAY"; exit 1; }

# ── open a solid-colour window (override-redirect, survives the WM restart) ────
# Centre is (400,350). Baseline drawing of ordinary windows is already covered by
# tests/xephyr-compositor.sh; this test guards the restart regression where
# pre-existing windows lost their GPU texture and the tiles vanished.
"$BINDIR/staticwin" 200 200 400 300 0x22cc44 >/dev/null 2>&1 &
sleep 1.5

# ── mirror the user's "cambiar el fondo" step ─────────────────────────────────
"$MSG_BIN" wallpaper set "$BLUE" >/dev/null 2>&1
sleep 1
assert_px 40 40 80 60 0000ff "wallpaper-before-restart"

# ── THE restart ───────────────────────────────────────────────────────────────
"$MSG_BIN" restart >/dev/null 2>&1
# Re-exec + re-adopt + rebind takes a couple seconds.
sleep 3.5

# THE assertion that failed before the fix: the window must still be drawn.
assert_px 400 350 80 60 22cc44 "window-after-restart"

log "────────────────────────────────────────"
log "restart suite: $PASS passed, $FAIL failed"
log "maverick log: $LOG"
[ "$FAIL" -eq 0 ]
