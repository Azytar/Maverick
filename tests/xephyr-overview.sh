#!/usr/bin/env bash
# Run the pixel/client-event regression on an owned nested display. Readiness
# must come from this Xephyr child; a foreign server is never ours to clean up.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
MAVERICK_BIN="${MAVERICK_BIN:-./target/release/maverick}"
MAVERICK_CTL="${MAVERICK_CTL:-./target/release/maverickctl}"
RT="$(mktemp -d /tmp/mvov.XXXXXX)"
HOST_DISPLAY="${DISPLAY:-}"
cleanup() {
    if [ -n "${XEPHYR_PID:-}" ]; then
        kill "$XEPHYR_PID" 2>/dev/null
        wait "$XEPHYR_PID" 2>/dev/null
    fi
    rm -rf "$RT"
}
trap cleanup EXIT
DISPLAY="$HOST_DISPLAY" Xephyr -displayfd 3 -screen 1920x1080 -ac \
    +extension RANDR +extension Composite +extension DAMAGE \
    3>"$RT/display" >"$RT/xephyr.log" 2>&1 &
XEPHYR_PID=$!
up=0
for _ in $(seq 1 60); do
    kill -0 "$XEPHYR_PID" 2>/dev/null || break
    if [ -s "$RT/display" ]; then
        read -r display_number <"$RT/display"
        if [[ "$display_number" =~ ^[0-9]+$ ]]; then
            DISP=":$display_number"
            if DISPLAY="$DISP" xprop -root >/dev/null 2>&1; then up=1; break; fi
        fi
    fi
    sleep 0.1
done
if [ "$up" != 1 ]; then
    echo "FAIL: Xephyr did not claim a ready display"
    tail -2 "$RT/xephyr.log"
    exit 1
fi
python3 "$HERE/xvfb-overview.py" --display "$DISP" --wm "$MAVERICK_BIN" --ctl "$MAVERICK_CTL"
