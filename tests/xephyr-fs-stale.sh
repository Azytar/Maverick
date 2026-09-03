#!/usr/bin/env bash
set -euo pipefail
APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$APP_DIR"

XEPHYR_DISPLAY=":92"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
BIN=/tmp/maverick-forensic-$$
mkdir -p "$BIN"
gcc -O2 tests/staticwin.c -o "$BIN/staticwin" -lX11 2>/dev/null
gcc -O2 tests/damager.c   -o "$BIN/damager"   -lX11 2>/dev/null
gcc -O2 tests/pxsample.c  -o "$BIN/pxsample"  -lX11 -lXcomposite 2>/dev/null
gcc -O2 tests/fsclient.c  -o "$BIN/fsclient"  -lX11 2>/dev/null

TRACE="$BIN/trace.log"
CLIENT_LOG="$BIN/client.log"
rm -f "$TRACE" "$CLIENT_LOG"
touch "$TRACE" "$CLIENT_LOG"

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

launch_client() {
  "$BIN/staticwin" 100 100 400 300 0xff3366 >/dev/null 2>"$CLIENT_LOG" &
  CLIENT_PID=$!
  for _ in $(seq 1 80); do
    WINID="$(grep -oE 'WINID=0x[0-9a-f]+' "$CLIENT_LOG" 2>/dev/null | head -1 | cut -d= -f2 || true)"
    if [ -n "${WINID:-}" ]; then echo "$WINID"; return 0; fi
    sleep 0.1
  done
  echo ""
  return 1
}

sample_px() {
  "$BIN/pxsample" "$1" "$2" "$3" "$4" "$5" "${6:-28}" >/dev/null 2>&1 || true
}

cleanup() {
  [ -n "${CLIENT_PID:-}" ] && kill "$CLIENT_PID" 2>/dev/null || true
  [ -n "${FS_PID:-}" ] && kill "$FS_PID" 2>/dev/null || true
  [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null || true
  [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null || true
}
trap cleanup EXIT

start_xephyr
start_maverick

CLIENT_WIN="$(launch_client)"
if [ -z "$CLIENT_WIN" ]; then
  echo "FAIL: client did not start" >&2
  exit 1
fi
echo "CLIENT=$CLIENT_WIN"

sample_px 120 120 40 40 0xff3366
sleep 1

xprop -id "$CLIENT_WIN" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
sleep 2

xprop -id "$CLIENT_WIN" -remove _NET_WM_STATE >/dev/null 2>&1 || true

"$BIN/fsclient" 600 200 360 260 0x44aa88 >/dev/null 2>"$BIN/fsclient.log" &
FS_PID=$!
FS_WIN=""
for _ in $(seq 1 80); do
  FS_WIN="$(grep -oE 'WINID=0x[0-9a-f]+' "$BIN/fsclient.log" 2>/dev/null | head -1 | cut -d= -f2 || true)"
  [ -n "${FS_WIN:-}" ] && break
  sleep 0.1
done
echo "FS_WIN=${FS_WIN:-unknown}"

for i in $(seq 1 180); do
  sample_px 120 120 40 40 0xff3366 || true
  sample_px 620 220 40 40 0x44aa88 || true
  sleep 0.25
done

echo "TRACE=$TRACE"
if [ -f "$TRACE" ]; then
  grep -E "FS-TRACE|FRAME-DUMP|PRESENT|LIFECYCLE|frame_plan|present_after_swap|render_end_dirty_clear|event_damage|sched_decision|draw |scene_hash|observed_age|cam_pos|cam_target|cam_vel|cam_anim|frame_id=" "$TRACE" | tail -260 || true
fi

