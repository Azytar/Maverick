#!/usr/bin/env bash
# Forensic A/B harness: real managed X11 fullscreen under Xephyr.
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
RUN_A_CFG="$BIN/runA.toml"
RUN_B_CFG="$BIN/runB.toml"
RUN_A_TRACE="$BIN/trace-runA.log"
RUN_B_TRACE="$BIN/trace-runB.log"
RUN_A_SCREEN="$BIN/screen-runA.xwd"
RUN_B_SCREEN="$BIN/screen-runB.xwd"
STATE_FILE="$BIN/realwin-state"
SUMMARY="$APP_DIR/logs/real-fullscreen-${BUILD_ID}.txt"

export XDG_RUNTIME_DIR="$(mktemp -d /tmp/mrt.XXXX)"
export XDG_CONFIG_HOME="$BIN/xdg"
mkdir -p "$XDG_CONFIG_HOME/maverick"

cat > "$RUN_A_CFG" <<'EOF'
[general]
n_tags = 4
auto_workspace_binds = false
compositor_enabled = true
focus_mouse = false

[compositor]
enabled = true
fullscreen_bypass = false

[keybindings]
key = "Mod4+F"
action = "toggle_fullscreen"
EOF

cat > "$RUN_B_CFG" <<'EOF'
[general]
n_tags = 4
auto_workspace_binds = false
compositor_enabled = true
focus_mouse = false

[compositor]
enabled = true
fullscreen_bypass = true

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

echo "[RUN A] fullscreen_bypass=false"
start_maverick "$RUN_A_CFG" "$RUN_A_TRACE"
sleep 1
DISPLAY="$XEPHYR_DISPLAY" "$BIN/realwin" 80 60 640 400 0x2266cc >/dev/null 2>"$BIN/realwin-runA.log" &
CLIENT_PID=$!
for _ in $(seq 1 80); do
  WID="$(grep -oE 'WINID=0x[0-9a-f]+' "$BIN/realwin-runA.log" | head -1 | cut -d= -f2 || true)"
  [ -n "${WID:-}" ] && break
  sleep 0.1
done
echo "REALWIN_A=$WID"
[ -z "${WID:-}" ] && { echo "FAIL: no realwin window"; exit 1; }
sleep 1
xprop -id "$WID" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
sleep 2
DISPLAY="$XEPHYR_DISPLAY" xwd -root -silent > "$RUN_A_SCREEN" 2>/dev/null || true
echo "RUN_A_SCREEN=$(file -b "$RUN_A_SCREEN")"

echo "[RUN B] fullscreen_bypass=true"
kill "$MAV_PID" 2>/dev/null || true
sleep 0.6
start_maverick "$RUN_B_CFG" "$RUN_B_TRACE"
sleep 1
DISPLAY="$XEPHYR_DISPLAY" "$BIN/realwin" 80 60 640 400 0x2266cc >/dev/null 2>"$BIN/realwin-runB.log" &
CLIENT_PID=$!
for _ in $(seq 1 80); do
  WID="$(grep -oE 'WINID=0x[0-9a-f]+' "$BIN/realwin-runB.log" | head -1 | cut -d= -f2 || true)"
  [ -n "${WID:-}" ] && break
  sleep 0.1
done
echo "REALWIN_B=$WID"
[ -z "${WID:-}" ] && { echo "FAIL: no realwin window"; exit 1; }
sleep 1
xprop -id "$WID" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
sleep 2
DISPLAY="$XEPHYR_DISPLAY" xwd -root -silent > "$RUN_B_SCREEN" 2>/dev/null || true
echo "RUN_B_SCREEN=$(file -b "$RUN_B_SCREEN")"

# Summarize active config + policy
{
  echo "=== CONFIG RUN A ==="
  cat "$RUN_A_CFG"
  echo
  echo "=== CONFIG RUN B ==="
  cat "$RUN_B_CFG"
  echo
  echo "=== BUILD ==="
  echo "maverick_bin=$MAVERICK_BIN"
  echo "build_time=$(stat -c '%y' "$MAVERICK_BIN" 2>/dev/null || echo unknown)"
  echo
  echo "=== RUN A TRACE KEYLINES ==="
  grep -E "forensic_state|engage_bypass_entry|disengage_bypass_entry|bypass_resource_release|resume_window|NameWindowPixmap|texture_creation|compute_scene_end|present_after_swap|frame_plan" "$RUN_A_TRACE" | tail -n 120 || true
  echo
  echo "=== RUN B TRACE KEYLINES ==="
  grep -E "forensic_state|engage_bypass_entry|disengage_bypass_entry|bypass_resource_release|resume_window|NameWindowPixmap|texture_creation|compute_scene_end|present_after_swap|frame_plan" "$RUN_B_TRACE" | tail -n 120 || true
  echo
  echo "=== XWD HASHES ==="
  md5sum "$RUN_A_SCREEN" "$RUN_B_SCREEN" 2>/dev/null || true
} | tee "$SUMMARY"

echo
echo "SUMMARY=$SUMMARY"
