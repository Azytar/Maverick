#!/usr/bin/env bash
#
# EWMH frame-extents sync.
#
#   C) normal tiled window: border=1  -> `_NET_FRAME_EXTENTS = 1,1,1,1`
#   D) fullscreen:           border=0  -> `_NET_FRAME_EXTENTS = 0,0,0,0`, window = screen
#   E) leave fullscreen:     extents follow the real border back
#   F) maximize (+ restore): extents follow the real border
#
# Run: bash tests/xephyr-ewmh-extents.sh

set -u
source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
trap mav_cleanup EXIT ERR


PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

DISP=":96"
start_xephyr "$DISP" 1280 720 >/dev/null
export DISPLAY="$DISP"
mav_launch "$DISP" >/dev/null

MGDTITLE=ext-tile "$BIN_DIR/mgdwin" >/dev/null 2>&1 &
HELPER_PIDS+=("$!")
i=0
while [ $i -lt 50 ] && [ "$(win_count "$DISP")" -lt 1 ]; do sleep 0.1; i=$((i+1)); done
WIN="$(DISPLAY="$DISP" xdotool search --class mgdwin 2>/dev/null | head -1)"
[ -n "${WIN:-}" ] || { bad "no mgdwin mapped"; echo "extents: PASS=$PASS FAIL=$FAIL"; exit 1; }

extents() { DISPLAY="$DISP" xprop -id "$WIN" _NET_FRAME_EXTENTS 2>/dev/null; }
border()  { DISPLAY="$DISP" xwininfo -id "$WIN" 2>/dev/null | awk '/Border width:/{print $NF}'; }
geom()    { DISPLAY="$DISP" xwininfo -id "$WIN" 2>/dev/null | awk '/Absolute upper-left X:/{x=$NF} /Width:/{w=$2} /Height:/{h=$2} END{print x, w, h}'; }

# ── C) tiled, border 1 ──────────────────────────────────────────────────────
sleep 0.5
if extents | grep -q "= 1, 1, 1, 1"; then
    ok "tiled _NET_FRAME_EXTENTS = 1,1,1,1"
else
    bad "tiled extents wrong: $(extents)"
fi

# ── D) fullscreen: border 0, extents 0, window = screen ─────────────────────
DISPLAY="$DISP" "$MAVERICK_CTL" msg toggle_fullscreen >/dev/null 2>&1
sleep 1
if [ "$(border)" = "0" ] && extents | grep -q "= 0, 0, 0, 0"; then
    ok "fullscreen border=0 with extents 0,0,0,0"
else
    bad "fullscreen border/extents wrong: border=$(border) $(extents)"
fi
if [ "$(geom)" = "0 1280 720" ]; then
    ok "fullscreen covers the screen"
else
    bad "fullscreen geometry wrong: $(geom) (want '0 1280 720')"
fi

# ── E) leave fullscreen: extents follow the real border back ────────────────
DISPLAY="$DISP" "$MAVERICK_CTL" msg toggle_fullscreen >/dev/null 2>&1
sleep 1
EB="$(border)"
if [ "$EB" = "1" ] && extents | grep -q "= 1, 1, 1, 1"; then
    ok "post-fullscreen extents restored to 1,1,1,1"
else
    bad "post-fullscreen border/extents wrong: border=$EB $(extents)"
fi

# ── F) maximize (+ restore): extents follow the real border ─────────────────
DISPLAY="$DISP" "$MAVERICK_CTL" msg toggle_maximize >/dev/null 2>&1
sleep 1
MB="$(border)"
ME="$(extents)"
if [ "$MB" = "0" ] && printf '%s' "$ME" | grep -q "= 0, 0, 0, 0"; then
    ok "maximized border=0 with extents 0,0,0,0"
else
    bad "maximized border/extents wrong: border=$MB $ME"
fi
DISPLAY="$DISP" "$MAVERICK_CTL" msg toggle_maximize >/dev/null 2>&1
sleep 1
RB="$(border)"
if [ "$RB" = "1" ] && extents | grep -q "= 1, 1, 1, 1"; then
    ok "post-maximize extents restored to 1,1,1,1"
else
    bad "post-maximize border/extents wrong: border=$RB $(extents)"
fi

echo "extents: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
