#!/usr/bin/env bash
#
# Maverick pointer/floating interaction audit — real-client Xephyr scenarios.
#
# Covers the acceptance criteria of the FLOATING / POINTER forensic audit:
#
#   SCENARIO A — click-to-focus must NOT warp the pointer. The cursor stays
#                exactly where the user clicked; the clicked window becomes
#                active (`_NET_ACTIVE_WINDOW`).
#   SCENARIO B — a TORN-OFF tile (ToggleFloat on a tiled window) keeps its
#                tiled ORIGIN: dropping it back over a tiled window re-inserts
#                it into the tree (the intended drop-to-tile feature).
#   SCENARIO C — a NATIVE float (WM_TRANSIENT_FOR dialog): must be born into
#                `ws.floats` with its own requested geometry (300x200 centred
#                on its parent), never a tile rectangle.
#   SCENARIO D — dragging that native float (Mod4+drag) and releasing it over
#                a tiled window must NOT mutate the tiled tree: the float keeps
#                floating with a moved geometry and its tiled neighbour keeps
#                the exact rect it had before the drag.
#   SCENARIO E — workspace switch away and back: the native float survives,
#                still floating, still its own geometry.
#
# REQUIREMENTS: Xephyr, xdotool, xprop, python3, gcc + libX11 (hostile client).
# Usage: tests/xephyr-pointer-float.sh
#
set -u
cd "$(dirname "$0")/.."

source tests/common.sh
trap mav_cleanup EXIT ERR

DISP="${DISP:-:99}"
MSG_BIN="${MAVERICK_MSG:-./target/debug/maverickctl}"
HOSTILE="${HOSTILE:-/tmp/mv-hostile}"

pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

# ── helpers ──────────────────────────────────────────────────────────────────
tree()  { DISPLAY="$DISP" "$MSG_BIN" query tree 2>/dev/null; }
dispatch() { DISPLAY="$DISP" "$MSG_BIN" dispatch "$@" >/dev/null 2>&1; sleep 0.3; }

active_win() { DISPLAY="$DISP" xprop -root -notype _NET_ACTIVE_WINDOW 2>/dev/null | grep -oE '0x[0-9a-f]+' | head -1; }

# tree JSON helpers (python3; tree output is one JSON document)
ws0() { tree | python3 -c "import sys,json;d=json.load(sys.stdin);print(json.dumps(d['monitors'][0]['workspaces'][d['monitors'][0]['active_ws']]))"; }
float_ids()   { ws0 | python3 -c "import sys,json;print(' '.join(str(w['id']) for w in json.load(sys.stdin)['floats']))"; }
tiled_ids()   { ws0 | python3 -c "import sys,json;print(' '.join(str(w['id']) for c in json.load(sys.stdin)['columns'] for w in c['windows']))"; }
win_geom() { # $1 = decimal window id
    ws0 | python3 -c "
import sys,json
w=int('$1')
d=json.load(sys.stdin)
for lst in ([x for c in d['columns'] for x in c['windows']] + d['floats']):
    if lst['id']==w:
        print(' '.join(map(str,lst['geom'])))
        break"
}
geom_center() { # $1 = id → "X Y" (root coords of the window's centre)
    set -- $(win_geom "$1")
    echo "$(( ${1:-0} + ${3:-0} / 2 )) $(( ${2:-0} + ${4:-0} / 2 ))"
}

# Poll until the WM has MANAGED $1 (present in `query tree`), up to ~10s.
# `hostile_winid` only proves the client mapped; manage() runs on the later
# MapNotify, so geometry/float queries right after create race and yield
# empty strings (which once produced negative click coordinates that killed
# xdotool and tripped the ERR trap).
wait_managed() { # $1 = decimal window id
    local id="$1"
    for _ in $(seq 1 50); do
        tree | python3 -c "
import sys,json
d=json.load(sys.stdin)
ids=set()
for m in d.get('monitors',[]):
    for ws in m.get('workspaces',[]):
        for c in ws.get('columns',[]):
            ids.update(w['id'] for w in c['windows'])
        ids.update(w['id'] for w in ws.get('floats',[]))
sys.exit(0 if $id in ids else 1)" 2>/dev/null && return 0
        sleep 0.2
    done
    return 1
}

# long-lived hostile sessions (same pattern as compat-matrix.sh)
declare -A HFD=(); declare -A HOUT=()
hostile_start() {
    local tag="$1"
    local out="/tmp/mvpf-h.$tag.out"
    exec {fd}> >(exec "$HOSTILE" "$DISP" >"$out" 2>/dev/null)
    HFD[$tag]=$fd; HOUT[$tag]="$out"
}
hostile_cmd() {
    local tag="$1"; shift
    echo "$*" >&"${HFD[$tag]}"
    sleep 0.3
}
hostile_winid() {
    local out="${HOUT[$1]}"
    for _ in $(seq 1 50); do
        local w; w="$(grep -m1 '^WINID=' "$out" 2>/dev/null | sed 's/^WINID=//')"
        [ -n "$w" ] && { echo "$w"; return; }
        sleep 0.2
    done
    echo ""
}
hostile_stop() {
    local fd="${HFD[$1]:-}"
    [ -n "$fd" ] && { echo "destroy" >&"$fd"; exec {fd}>&-; }
    sleep 0.3
}

# Mod4+drag: from (x1,y1) to (x2,y2), left button
mod4_drag() {
    xdotool keydown super
    xdotool mousemove "$1" "$2"; sleep 0.15
    xdotool mousedown 1; sleep 0.15
    xdotool mousemove "$3" "$4"; sleep 0.25
    xdotool mouseup 1; sleep 0.15
    xdotool keyup super
    sleep 0.4
}

# ══════════════════════════════════════════════════════════════════════════════
echo "── building hostile client (if missing)"
[ -x "$HOSTILE" ] || gcc -O2 -o "$HOSTILE" tests/hostile.c -lX11 || { echo "cannot build hostile"; exit 1; }

echo "── starting Xephyr + maverick on $DISP"
# Headless Xephyr is not a texture-from-pixmap target (GLX texture-from-pixmap fails
# with a fatal XIO that kills the WM mid-suite) — same policy as
# xephyr-suite.sh: validate focus/float logic on the plain X11 path.
mav_preflight
start_xephyr "$DISP" 1280 720 >/dev/null
mav_launch "$DISP" >/dev/null

# Two tiled windows; switch to Grid so both are fully visible for clicking.
# Two tiled windows side by side (Column-only since Grid was removed; both
# tiles are visible for clicking without any layout switch).
hostile_start A
hostile_cmd A "create"
WIN_A_DEC=$(( $(hostile_winid A) ))
wait_managed "$WIN_A_DEC" || fail "window A never managed"
hostile_start B
hostile_cmd B "create"
WIN_B_DEC=$(( $(hostile_winid B) ))
wait_managed "$WIN_B_DEC" || fail "window B never managed"
[ "$WIN_A_DEC" -gt 0 ] && [ "$WIN_B_DEC" -gt 0 ] || fail "hostile windows did not map"
sleep 0.5

echo "── SCENARIO A: click-to-focus must not warp the pointer"
# NOTE: B (mapped last) already owns focus. Click the OTHER window (A): the
# click must change focus to A without moving the pointer. (Clicking the
# already-focused window is a different path: hostile does not select
# ButtonPress, so the replayed press lands on the root and unfocuses —
# pre-existing, out of scope here.)
read AX AY <<< "$(geom_center "$WIN_A_DEC")"
DISPLAY="$DISP" xdotool mousemove "$AX" "$AY"; sleep 0.3
eval "$(DISPLAY="$DISP" xdotool getmouselocation --shell | grep -E '^(X|Y)=' | tr '\n' ' ')"
PX_BEFORE=$X; PY_BEFORE=$Y
DISPLAY="$DISP" xdotool click 1; sleep 0.5
eval "$(DISPLAY="$DISP" xdotool getmouselocation --shell | grep -E '^(X|Y)=' | tr '\n' ' ')"
if [ "$PX_BEFORE" = "$X" ] && [ "$PY_BEFORE" = "$Y" ]; then
    pass "pointer stayed at ($PX_BEFORE,$PY_BEFORE)"
else
    fail "pointer moved on click: ($PX_BEFORE,$PY_BEFORE) → ($X,$Y)"
fi
ACTIVE="$(active_win)"
AW_DEC=$(( ACTIVE ))
if [ "$AW_DEC" -eq "$WIN_A_DEC" ]; then
    pass "_NET_ACTIVE_WINDOW is the clicked window"
else
    fail "active window after click = $ACTIVE, expected clicked window id $WIN_A_DEC"
fi

echo "── SCENARIO B: torn-off tile (tiled origin) may drop back into the tree"
# Focus is on A (scenario A clicked it). Move keyboard focus to B (the column
# to the right), toggle it floating (same funnel as the keybind), then drag it
# back over tiled window A and release: drop-to-tile must re-insert it because
# its ORIGIN is tiled.
dispatch focus-right
sleep 0.3
FOCUSED_NOW="$(DISPLAY="$DISP" "$MSG_BIN" query focused 2>/dev/null | grep -oE '"window":[0-9]+' | grep -oE '[0-9]+')"
if [ "$FOCUSED_NOW" = "$WIN_B_DEC" ]; then
    pass "keyboard focus moved to window B"
else
    fail "focus-right did not reach B (focused=$FOCUSED_NOW, expected $WIN_B_DEC)"
fi
dispatch toggle_float
sleep 0.3
case " $(float_ids) " in *" $WIN_B_DEC "*) : ;;
    *) fail "toggle_float did not float window B" ;; esac
read TBX TBY <<< "$(geom_center "$WIN_B_DEC")"
read AAX AAY <<< "$(geom_center "$WIN_A_DEC")"
mod4_drag "$TBX" "$TBY" "$AAX" "$AAY"
case " $(tiled_ids) " in *" $WIN_B_DEC "*)
    pass "torn-off tile re-joined the tiling tree on release (drop-to-tile intact)" ;;
    *) fail "torn-off tile could not re-join the tree (gate too strict?)" ;; esac

echo "── SCENARIO C: native float (transient dialog) born with its own geometry"
hostile_start C
hostile_cmd C "transient $(printf '0x%x' "$WIN_A_DEC")"
hostile_cmd C "create"
WIN_C_DEC=$(( $(hostile_winid C) ))
[ "$WIN_C_DEC" -gt 0 ] || fail "transient window did not map"
wait_managed "$WIN_C_DEC" || fail "transient window never managed"
sleep 0.4
FLOATS="$(float_ids)"; TILED="$(tiled_ids)"
case " $FLOATS " in *" $WIN_C_DEC "*) pass "transient is floating (ws.floats)" ;;
    *) fail "transient not in floats: floats=[$FLOATS] tiled=[$TILED]" ;; esac
read CGX CGY CGW CGH <<< "$(win_geom "$WIN_C_DEC")"
if [ "$CGW" = "300" ] && [ "$CGH" = "200" ]; then
    pass "native float keeps its own 300x200 geometry"
else
    fail "native float geometry = ${CGW}x${CGH}, expected 300x200 (tile-sized?)"
fi

echo "── SCENARIO D: dragging the native float must not mutate the tiled tree"
# C is born centred on A, so drag it all the way to B (a *tiled* window) and
# release there: the drop lands over a tile on purpose.
read TAGX TAGY <<< "$(geom_center "$WIN_C_DEC")"
read DRPX DRPY <<< "$(geom_center "$WIN_B_DEC")"
GEOM_B_BEFORE="$(win_geom "$WIN_B_DEC")"
mod4_drag "$TAGX" "$TAGY" "$DRPX" "$DRPY"
FLOATS="$(float_ids)"; TILED="$(tiled_ids)"
GEOM_B_AFTER="$(win_geom "$WIN_B_DEC")"
GEOM_C_AFTER="$(win_geom "$WIN_C_DEC")"
case " $FLOATS " in *" $WIN_C_DEC "*)
    pass "native float still floating after drag release over a tile" ;;
    *) fail "native float was swallowed into the tree: tiled=[$TILED]" ;; esac
if [ "$GEOM_B_BEFORE" = "$GEOM_B_AFTER" ]; then
    pass "tiled neighbour geometry untouched by the float drag"
else
    fail "tiled neighbour changed: [$GEOM_B_BEFORE] → [$GEOM_B_AFTER]"
fi
if [ "$GEOM_C_AFTER" != "$CGX $CGY $CGW $CGH" ]; then
    pass "native float moved freely (drag took effect on its own geometry)"
else
    fail "native float did not move at all"
fi

echo "── SCENARIO E: workspace switch preserves the native float"
dispatch view 2
dispatch view 1
sleep 0.4
case " $(float_ids) " in *" $WIN_C_DEC "*) pass "native float survived the workspace switch" ;;
    *) fail "native float lost after workspace switch" ;; esac

hostile_stop A; hostile_stop B; hostile_stop C
echo "ALL SCENARIOS PASSED"
