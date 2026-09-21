#!/usr/bin/env bash
#
# EWMH workarea publication (Fase 4B, commit 1).
#
#   A) startup without docks: `_NET_WORKAREA` and `_NET_DESKTOP_GEOMETRY` must
#      exist immediately (they used to appear only after a strut/RandR event).
#   B) dock with a 30px top strut: workarea shrinks to (0,30,W,H-30), the root
#      property follows, and killing the dock restores it.
#
# Geometry values below assume the 1280x720 nested server this script starts.
# Run: bash tests/xephyr-ewmh-workarea.sh

set -u
source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
[ -x "$BIN_DIR/dockstrut" ] || cc -O2 -o "$BIN_DIR/dockstrut" "$BIN_DIR/dockstrut.c" -lX11 \
    || { echo "FAIL: could not build tests/dockstrut"; exit 1; }
trap mav_cleanup EXIT ERR

export MAVERICK_NO_COMPOSITOR=1

PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

DISP=":97"
start_xephyr "$DISP" 1280 720 >/dev/null
export DISPLAY="$DISP"
mav_launch "$DISP" >/dev/null

# ── A) workarea published at startup, no docks ──────────────────────────────
WA="$(DISPLAY="$DISP" xprop -root _NET_WORKAREA 2>/dev/null)"
if printf '%s' "$WA" | grep -q "0, 0, 1280, 720"; then
    ok "startup _NET_WORKAREA present without docks"
else
    bad "startup _NET_WORKAREA missing/wrong: $WA"
fi
DG="$(DISPLAY="$DISP" xprop -root _NET_DESKTOP_GEOMETRY 2>/dev/null)"
if printf '%s' "$DG" | grep -q "1280, 720"; then
    ok "startup _NET_DESKTOP_GEOMETRY present without docks"
else
    bad "startup _NET_DESKTOP_GEOMETRY missing/wrong: $DG"
fi

# ── B) dock strut shrinks the workarea and the property follows ─────────────
"$BIN_DIR/dockstrut" 30 >/dev/null 2>&1 &
DOCKPID=$!
HELPER_PIDS+=("$DOCKPID")
i=0
while [ $i -lt 50 ]; do
    DISPLAY="$DISP" xprop -root _NET_WORKAREA 2>/dev/null | grep -q "0, 30, 1280, 690" && break
    sleep 0.1; i=$((i+1))
done
WA2="$(DISPLAY="$DISP" xprop -root _NET_WORKAREA 2>/dev/null)"
if printf '%s' "$WA2" | grep -q "0, 30, 1280, 690"; then
    ok "dock strut shrinks _NET_WORKAREA to (0,30,1280,690)"
else
    bad "dock strut _NET_WORKAREA wrong: $WA2"
fi

# a tiled client must sit below the 30px reservation (y=30+outer gap 8=38)
MGDTITLE=wa-tile "$BIN_DIR/mgdwin" >/dev/null 2>&1 &
HELPER_PIDS+=("$!")
i=0
while [ $i -lt 50 ] && [ "$(win_count "$DISP")" -lt 1 ]; do sleep 0.1; i=$((i+1)); done
GY="$(DISPLAY="$DISP" "$MAVERICK_CTL" query tree 2>/dev/null \
    | python3 -c "import sys,json;d=json.load(sys.stdin);print(d['monitors'][0]['workspaces'][0]['columns'][0]['windows'][0]['geom'][1])" 2>/dev/null)"
if [ "${GY:-}" = "38" ]; then
    ok "tiled window respects the strut (y=38)"
else
    bad "tiled window y wrong under strut (y=${GY:-?}, want 38)"
fi

kill -9 "$DOCKPID" 2>/dev/null
i=0
while [ $i -lt 50 ]; do
    DISPLAY="$DISP" xprop -root _NET_WORKAREA 2>/dev/null | grep -q "0, 0, 1280, 720" && break
    sleep 0.1; i=$((i+1))
done
WA3="$(DISPLAY="$DISP" xprop -root _NET_WORKAREA 2>/dev/null)"
if printf '%s' "$WA3" | grep -q "0, 0, 1280, 720"; then
    ok "dock removal restores _NET_WORKAREA"
else
    bad "dock removal _NET_WORKAREA wrong: $WA3"
fi

echo "workarea: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
