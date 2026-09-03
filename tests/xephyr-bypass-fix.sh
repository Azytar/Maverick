#!/usr/bin/env bash
set -euo pipefail
APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$APP_DIR"

XEPHYR_DISPLAY=":94"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
BIN=/tmp/maverick-bypass-fix-$$
mkdir -p "$BIN"
gcc -O2 tests/staticwin.c -o "$BIN/staticwin" -lX11 2>/dev/null
gcc -O2 tests/fsclient.c  -o "$BIN/fsclient"  -lX11 2>/dev/null

TRACE="$BIN/trace.log"
rm -f "$TRACE"
touch "$TRACE"

start_xephyr() {
  Xephyr "$XEPHYR_DISPLAY" -screen 1280x1024 -ac \
    +extension GLX +extension RANDR +extension Composite >/dev/null 2>&1 &
  XEPHYR_PID=$!
  sleep 1.5
  export DISPLAY="$XEPHYR_DISPLAY"
}

start_maverick() {
  MAV_FORENSIC_TRACE_XID=1 MAVERICK_TRACE=1 MAVERICK_COMPOSITION_TRACE=1 MAV_COMP_TRACE=1 \
    "$MAVERICK_BIN" >>"$TRACE" 2>&1 &
  MAV_PID=$!
  for _ in $(seq 1 60); do
    if xprop -root >/dev/null 2>&1; then break; fi
    sleep 0.1
  done
}

cleanup() {
  [ -n "${CLIENT_PID:-}" ] && kill "$CLIENT_PID" 2>/dev/null || true
  [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null || true
  [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null || true
}
trap cleanup EXIT

start_xephyr
start_maverick

# Open a static tiled window
"$BIN/staticwin" 100 100 400 300 0xff3366 >/dev/null 2>"$BIN/staticwin.log" &
CLIENT_PID=$!
WINID=""
for _ in $(seq 1 80); do
  WINID="$(grep -oE 'WINID=0x[0-9a-f]+' "$BIN/staticwin.log" 2>/dev/null | head -1 | cut -d= -f2 || true)"
  [ -n "${WINID:-}" ] && break
  sleep 0.1
done
echo "STATIC_WIN=${WINID:-unknown}"
[ -z "${WINID:-}" ] && exit 1

sleep 1
# Enter fullscreen
xprop -id "$WINID" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
sleep 1

# Exit fullscreen
xprop -id "$WINID" -remove _NET_WM_STATE >/dev/null 2>&1 || true
sleep 1

# Capture final trace
echo "=== TRACE TAIL ==="
tail -n 60 "$TRACE"

echo ""
echo "=== KEY PATTERNS ==="
grep -E "engage_bypass_entry|disengage_bypass_entry|bypass_resource_release|resume_window|NameWindowPixmap|texture_creation|compute_scene_end|present_after_swap" "$TRACE" | tail -n 30 || true
