#!/usr/bin/env bash
set -euo pipefail
APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$APP_DIR"

XEPHYR_DISPLAY=":93"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
BIN=/tmp/maverick-determ-$$
export BIN
mkdir -p "$BIN"
gcc -O2 tests/staticwin.c -o "$BIN/staticwin" -lX11 2>/dev/null
gcc -O2 tests/damager.c   -o "$BIN/damager"   -lX11 2>/dev/null
gcc -O2 tests/pxsample.c  -o "$BIN/pxsample"  -lX11 -lXcomposite 2>/dev/null
gcc -O2 tests/fsclient.c  -o "$BIN/fsclient"  -lX11 2>/dev/null

TRACE="$BIN/trace.log"
CLIENT_LOG="$BIN/fsclient.log"
SUMMARY="$BIN/summary.tsv"
rm -f "$TRACE" "$CLIENT_LOG" "$SUMMARY"
touch "$TRACE"
echo -e "wall_time_s\tclient_frame\tdamage\tcomp_frame\tneeds_frame\tpresent\tscene_hash\tobserved_age\tmode" > "$SUMMARY"

start_xephyr() {
  Xephyr "$XEPHYR_DISPLAY" -screen 1280x1024 -ac \
    +extension GLX +extension RANDR +extension Composite >/dev/null 2>&1 &
  XEPHYR_PID=$!
  sleep 1.5
  export DISPLAY="$XEPHYR_DISPLAY"
}

start_maverick() {
  MAVERICK_TRACE=1 \
    "$MAVERICK_BIN" >>"$TRACE" 2>&1 &
  MAV_PID=$!
  for _ in $(seq 1 60); do
    if xprop -root >/dev/null 2>&1; then break; fi
    sleep 0.1
  done
}

launch_client() {
  local state="$BIN/fsclient.state"
  rm -f "$state"
  MAVERICK_TEST_STATE="$state" "$BIN/staticwin" 100 100 400 300 0xff3366 >/dev/null 2>"$CLIENT_LOG" &
  CLIENT_PID=$!
  for _ in $(seq 1 80); do
    WINID="$(grep -oE 'WINID=0x[0-9a-f]+' "$CLIENT_LOG" 2>/dev/null | head -1 | cut -d= -f2 || true)"
    if [ -n "${WINID:-}" ]; then echo "$WINID"; return 0; fi
    sleep 0.1
  done
  echo ""
  return 1
}

sample_client_frame() {
  local state="$BIN/fsclient.state"
  if [ -f "$state" ]; then
    awk 'NF{print $1}' "$state" 2>/dev/null | tail -1
  fi
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

sleep 1
xprop -id "$CLIENT_WIN" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
sleep 1

xprop -id "$CLIENT_WIN" -remove _NET_WM_STATE >/dev/null 2>&1 || true
START=$(date +%s)
export START

MAVERICK_TEST_STATE="$BIN/fsclient.state" "$BIN/fsclient" 600 200 360 260 0x44aa88 >/dev/null 2>"$BIN/fsclient.log" &
FS_PID=$!
FS_WIN=""
for _ in $(seq 1 80); do
  FS_WIN="$(grep -oE 'WINID=0x[0-9a-f]+' "$BIN/fsclient.log" 2>/dev/null | head -1 | cut -d= -f2 || true)"
  [ -n "${FS_WIN:-}" ] && break
  sleep 0.1
done
echo "FS_WIN=${FS_WIN:-unknown}"

sleep 1
if [ -n "${FS_WIN:-}" ]; then
  xprop -id "$FS_WIN" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_FULLSCREEN >/dev/null 2>&1 || true
  sleep 1
  xprop -id "$FS_WIN" -remove _NET_WM_STATE >/dev/null 2>&1 || true
fi

START=$(date +%s)
export START

# deterministic data collection: 120 samples, every 250ms
python3 - <<'PY'
import os, re, time
BIN = os.environ['BIN']
START = float(os.environ['START'])
trace_path = f"{BIN}/trace.log"
state_path = f"{BIN}/fsclient.state"
summary_path = f"{BIN}/summary.tsv"
pat_damage = re.compile(r'event=DamageNotify')
pat_comp = re.compile(r'comp_frame=(\d+)')
pat_dump = re.compile(r'frame_gen=\d+ .*scene_hash=(0x[0-9a-fA-F]+)')
pat_present = re.compile(r'present_after_swap .* mode=(\w+)')
pat_age = re.compile(r'observed_age=(\d+)')
pat_needs_frame = re.compile(r'\[FRAME-DUMP\][^\n]*needs_frame=(\w+)')
last_client = ''
with open(summary_path, 'w') as out:
    out.write('wall_time_s\tclient_frame\tdamage\tcomp_frame\tneeds_frame\tpresent\tscene_hash\tobserved_age\n')
    for i in range(120):
        time.sleep(0.25)
        wall = time.time() - START
        try:
            if os.path.exists(state_path):
                txt = open(state_path, errors='ignore').read()
                m = re.findall(r'^(\d+) ', txt, re.M)
                last_client = m[-1] if m else last_client
        except Exception:
            pass
        damage_count = 0
        comp_frame = ''
        needs_frame = ''
        scene_hash = ''
        observed_age = ''
        present = ''
        if os.path.exists(trace_path):
            lines = open(trace_path, errors='ignore').read().splitlines()
            for line in lines:
                if pat_damage.search(line): damage_count += 1
                if not comp_frame:
                    m = pat_comp.search(line)
                    if m: comp_frame = m.group(1)
                if not needs_frame:
                    m = pat_needs_frame.search(line)
                    if m: needs_frame = m.group(1)
                if not scene_hash:
                    m = pat_dump.search(line)
                    if m: scene_hash = m.group(1)
                if not present:
                    m = pat_present.search(line)
                    if m: present = m.group(1)
                if not observed_age:
                    m = pat_age.search(line)
                    if m: observed_age = m.group(1)
        out.write(f"{wall:.3f}\t{last_client}\t{damage_count}\t{comp_frame}\t{needs_frame}\t{present}\t{scene_hash}\t{observed_age}\n")
PY

echo "SUMMARY=$SUMMARY"
if [ -f "$SUMMARY" ]; then
  cat "$SUMMARY"
fi