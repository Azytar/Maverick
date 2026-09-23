#!/usr/bin/env bash
#
# Xephyr rounded-corner clip regression (Fase 6).
#
# Asserts the Fase 6 geometry contract end-to-end with real pixels:
#
#   Client geometry = rectangular X11 geometry (never shrunk/moved for corners).
#   Frame geometry  = rectangular WM geometry.
#   Visual shape    = compositor/Shape clipping of that rectangular surface.
#
#   1. A tiled managed window with `corner_radius > 0` shows NO client content
#      outside the rounded visual region (outer corner pixel is NOT the client
#      colour, an inner pixel IS).
#   2. Fullscreen keeps its contract: border 0, square corners (corner pixel IS
#      the client colour), `_NET_FRAME_EXTENTS = 0,0,0,0`.
#   3. `_NET_FRAME_EXTENTS` still means "border", never the corner radius.
#   4. `_NET_DESKTOP_VIEWPORT` is not announced.
#
# Uses the existing harness helpers (`mgdwin`, `pxsample`) and live
# `maverickctl`/X properties — no fabricated results. The only colour assumed
# is mgdwin's own fill (0x2266cc, baked into tests/mgdwin.c).
#
# Run:  ./tests/xephyr-rounded-clip.sh
# Requirements: Xephyr, xprop, xdotool, xwininfo, import (ImageMagick), gcc.

set -u

SCREEN_W=800
SCREEN_H=600
XEPHYR_DISPLAY=":97"
BINDIR="$(cd "$(dirname "$0")" && pwd)"
MAVERICK_BIN="${MAVERICK_BIN:-./target/release/maverick}"
[ -x "$MAVERICK_BIN" ] || MAVERICK_BIN="./target/debug/maverick"
MCTL="${MAVERICKCTL_BIN:-./target/release/maverickctl}"
[ -x "$MCTL" ] || MCTL="./target/debug/maverickctl"
LOG="$(mktemp -t maverick-rounded.XXXXXX.log)"
CONTENT_HEX="0x2266cc"
PASS=0
FAIL=0

log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); }

cleanup() {
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    pkill -f '/tmp/maverick-rounded-mgdwin' 2>/dev/null
    pkill -x mgdwin 2>/dev/null
}
trap cleanup EXIT

for h in mgdwin pxsample; do
    [ -x "$BINDIR/$h" ] || cc -O2 -o "$BINDIR/$h" "$BINDIR/$h.c" -lX11 -lXcomposite 2>>"$LOG" \
        || { bad "failed to build helper $h"; exit 1; }
done

Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    >"$LOG.xephyr" 2>&1 &
XEPHYR_PID=$!
sleep 1
export DISPLAY="$XEPHYR_DISPLAY"

CONFIG="$(mktemp -t maverick-rounded-cfg.XXXXXX.toml)"
# Bypass disabled on purpose: with bypass the overlay keeps a transparent hole
# over the fullscreen window (by design) and `pxsample` — which reads the
# overlay — would see transparency instead of content. Compositing the
# fullscreen window exercises the square-corner policy under test.
printf '[general]\ncorner_radius = 12\n[compositor]\nenabled = true\nfullscreen_bypass = false\n' >"$CONFIG"

"$MAVERICK_BIN" --config "$CONFIG" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1.5
xprop -root >/dev/null 2>&1 || { bad "maverick did not start"; exit 1; }
ok "maverick started with corner_radius=12"

"$BINDIR/mgdwin" >/tmp/maverick-rounded-mgdwin.log 2>&1 &
sleep 1.5
WIN="$(xdotool search --class mgdwin 2>/dev/null | head -1)"
[ -n "${WIN:-}" ] || { bad "managed window not found"; exit 1; }
ok "managed window $WIN mapped"

# Rectangular client geometry from the WM (never shrunk for the radius).
GEOM="$("$MCTL" query tree 2>/dev/null | python3 -c "
import json,sys
d = json.load(sys.stdin)
w = d['monitors'][0]['workspaces'][0]['columns'][0]['windows'][0]
print('%d %d %d %d' % tuple(w['geom']))
")"
set -- $GEOM
GX="$1"; GY="$2"; GW="$3"; GH="$4"
log "tiled geom: $GX $GY $GW $GH"
[ "$GW" -gt 24 ] && [ "$GH" -gt 24 ] || { bad "geometry implausibly small: $GEOM"; exit 1; }
ok "client geometry is rectangular ($GX,$GY ${GW}x${GH})"

# The rounded visual region must not show client content outside it: the
# outer corner pixel is NOT the client colour, a pixel well inside IS.
if "$BINDIR/pxsample" "$GX" "$GY" 4 4 "$CONTENT_HEX" >/dev/null 2>&1; then
    bad "tiled outer corner @($GX,$GY) shows rectangular client content (must be clipped)"
else
    ok "tiled outer corner @($GX,$GY) is not client content (clipped)"
fi
IX=$((GX + 24)); IY=$((GY + 24))
if "$BINDIR/pxsample" "$IX" "$IY" 8 8 "$CONTENT_HEX" >/dev/null 2>&1; then
    ok "tiled inner pixel @($IX,$IY) is client content"
else
    bad "tiled inner pixel @($IX,$IY) is NOT client content"
fi

# Frame extents still mean "border", never the corner radius (12 would show
# up as 12,12,12,12 if the radius leaked into the EWMH contract).
EXTENTS="$(xprop -id "$WIN" _NET_FRAME_EXTENTS 2>/dev/null || true)"
log "tiled _NET_FRAME_EXTENTS: $EXTENTS"
case "$EXTENTS" in
    *"= 1, 1, 1, 1"*) ok "tiled _NET_FRAME_EXTENTS = 1,1,1,1 (border, not radius)" ;;
    *) bad "tiled _NET_FRAME_EXTENTS wrong (must be border, not radius): $EXTENTS" ;;
esac

# Viewport must stay unannounced (scrolling is an internal transform).
# NOTE: `xprop` exits 0 even for a missing property ("no such atom"), so the
# output — not the exit status — decides.
if xprop -root _NET_DESKTOP_VIEWPORT 2>&1 | grep -q "="; then
    bad "_NET_DESKTOP_VIEWPORT is announced (must stay unannounced)"
else
    ok "_NET_DESKTOP_VIEWPORT not announced"
fi

# Fullscreen keeps its contract: border 0, square full-surface corners.
"$MCTL" msg toggle_fullscreen >/dev/null 2>&1
sleep 1.2
FGEOM="$("$MCTL" query tree 2>/dev/null | python3 -c "
import json,sys
d = json.load(sys.stdin)
w = d['monitors'][0]['workspaces'][0]['columns'][0]['windows'][0]
print('%d %d %d %d %s' % (w['geom'][0], w['geom'][1], w['geom'][2], w['geom'][3], w['fullscreen']))
")"
log "fullscreen: $FGEOM"
set -- $FGEOM
[ "$1" = "0" ] && [ "$2" = "0" ] && [ "$3" = "$SCREEN_W" ] && [ "$4" = "$SCREEN_H" ] \
    && ok "fullscreen covers the screen (rectangular)" \
    || bad "fullscreen geometry wrong: $FGEOM"
if "$BINDIR/pxsample" 0 0 4 4 "$CONTENT_HEX" >/dev/null 2>&1; then
    ok "fullscreen corner (0,0) is content (square, no rounding)"
else
    bad "fullscreen corner (0,0) is NOT content (must stay square)"
fi
FEXT="$(xprop -id "$WIN" _NET_FRAME_EXTENTS 2>/dev/null || true)"
case "$FEXT" in
    *"= 0, 0, 0, 0"*) ok "fullscreen _NET_FRAME_EXTENTS = 0,0,0,0" ;;
    *) bad "fullscreen _NET_FRAME_EXTENTS wrong: $FEXT" ;;
esac

# Back to tiled: the mask must follow the resize exactly (no stale corners).
"$MCTL" msg toggle_fullscreen >/dev/null 2>&1
sleep 1.2
GEOM2="$("$MCTL" query tree 2>/dev/null | python3 -c "
import json,sys
d = json.load(sys.stdin)
w = d['monitors'][0]['workspaces'][0]['columns'][0]['windows'][0]
print('%d %d %d %d' % tuple(w['geom']))
")"
set -- $GEOM2
[ "$1" = "$GX" ] && [ "$2" = "$GY" ] && [ "$3" = "$GW" ] && [ "$4" = "$GH" ] \
    && ok "fullscreen-exit restores the rectangular tiled geometry" \
    || bad "fullscreen-exit geometry changed: $GEOM -> $GEOM2"
if "$BINDIR/pxsample" "$1" "$2" 4 4 "$CONTENT_HEX" >/dev/null 2>&1; then
    bad "tiled corner after fullscreen-exit shows content (stale mask)"
else
    ok "tiled corner after fullscreen-exit still clipped"
fi

rm -f "$CONFIG"
log "────────────────────────────────────────"
log "rounded-clip suite: $PASS passed, $FAIL failed"
log "maverick log: $LOG"
[ "$FAIL" -eq 0 ]
