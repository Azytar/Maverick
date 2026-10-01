#!/usr/bin/env bash
# Forensic harness: real managed X11 fullscreen under Xephyr.
#
# It captures the trace and a screen dump for a client that asks for fullscreen
# on a server with no external compositor. It asserts nothing — the value is the
# artifact under `logs/`, for whoever is investigating a fullscreen report.
set -euo pipefail
APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$APP_DIR"
mkdir -p logs

XEPHYR_DISPLAY=":92"
MAVERICK_BIN="${MAVERICK_BIN:-target/debug/maverick}"
BIN="/tmp/maverick-real-fullscreen-$$"
mkdir -p "$BIN"
cc -O2 tests/realwin.c -o "$BIN/realwin" -lX11 -lXext 2>/dev/null || cc -O2 tests/realwin.c -o "$BIN/realwin" -lX11

BUILD_ID="$(date +%Y%m%d-%H%M%S)"
RUN_CFG="$BIN/run.toml"
RUN_TRACE="$BIN/trace-run.log"
RUN_SCREEN="$BIN/screen-run.xwd"
STATE_FILE="$BIN/realwin-state"
SUMMARY="$APP_DIR/logs/real-fullscreen-${BUILD_ID}.txt"

export XDG_RUNTIME_DIR="$(mktemp -d /tmp/mrt.XXXX)"
export XDG_CONFIG_HOME="$BIN/xdg"
mkdir -p "$XDG_CONFIG_HOME/maverick"

cat > "$RUN_CFG" <<'EOF'
[general]
n_tags = 4
auto_workspace_binds = false
focus_mouse = false

[keybindings]
key = "Mod4+F"
action = "toggle_fullscreen"
EOF

start_xephyr() {
  Xephyr "$XEPHYR_DISPLAY" -screen 1280x800 -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    >/tmp/xephyr-real.log 2>&1 &
  XEPHYR_PID=$!
  for _ in $(seq 1 80); do
    DISPLAY="$XEPHYR_DISPLAY" xprop -root >/dev/null 2>&1 && break
    sleep 0.1
  done
  export DISPLAY="$XEPHYR_DISPLAY"
}

start_maverick() {
  local cfg="$1" trace="$2"
  rm -f "$trace"
  MAV_FORENSIC_TRACE_XID=1 MAVERICK_TRACE=1 MAVERICK_COMPOSITION_TRACE=1 MAV_COMP_TRACE=1 \
    "$MAVERICK_BIN" --config "$cfg" >>"$trace" 2>&1 &
  MAV_PID=$!
  for _ in $(seq 1 120); do
    if xprop -root >/dev/null 2>&1; then break; fi
    sleep 0.1
  done
}

cleanup() {
  set +e
  [ -n "${CLIENT_PID:-}" ] && kill "$CLIENT_PID" 2>/dev/null || true
  [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null || true
  [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null || true
  pkill -f "realwin$$" 2>/dev/null || true
  pkill -f "target/debug/maverick" 2>/dev/null || true
  pkill -f "Xephyr :92" 2>/dev/null || true
  rm -rf "$XDG_RUNTIME_DIR"
  set -e
}
trap cleanup EXIT

pkill -f "target/debug/maverick" 2>/dev/null || true
pkill -f "Xephyr :92" 2>/dev/null || true
sleep 0.3

start_xephyr

start_maverick "$RUN_CFG" "$RUN_TRACE"
sleep 1
DISPLAY="$XEPHYR_DISPLAY" "$BIN/realwin" 80 60 640 400 0x2266cc >/dev/null 2>"$BIN/realwin.log" &
CLIENT_PID=$!
WID=""
for _ in $(seq 1 80); do
  WID="$(grep -oE 'WINID=0x[0-9a-f]+' "$BIN/realwin.log" | head -1 | cut -d= -f2 || true)"
  [ -n "${WID:-}" ] && break
  sleep 0.1
done
echo "REALWIN=$WID"
[ -z "${WID:-}" ] && { echo "FAIL: no realwin window"; exit 1; }
sleep 1
xprop -id "$WID" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
sleep 2
DISPLAY="$XEPHYR_DISPLAY" xwd -root -silent > "$RUN_SCREEN" 2>/dev/null || true
echo "RUN_SCREEN=$(file -b "$RUN_SCREEN")"

# Summarize active config + policy
{
  echo "=== CONFIG ==="
  cat "$RUN_CFG"
  echo
  echo "=== BUILD ==="
  echo "maverick_bin=$MAVERICK_BIN"
  echo "build_time=$(stat -c '%y' "$MAVERICK_BIN" 2>/dev/null || echo unknown)"
  echo
  echo "=== TRACE KEYLINES ==="
  grep -E "forensic_state|engage_bypass_entry|disengage_bypass_entry|bypass_resource_release|resume_window|NameWindowPixmap|texture_creation|compute_scene_end|present_after_swap|frame_plan" "$RUN_TRACE" | tail -n 120 || true
  echo
  echo "=== XWD ==="
  md5sum "$RUN_SCREEN" 2>/dev/null || true
} | tee "$SUMMARY"

echo
echo "SUMMARY=$SUMMARY"
