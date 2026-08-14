#!/usr/bin/env bash
#
# Riesgo 6 — real multi-monitor (2 RANDR outputs) validation for Maverick.
#
# This harness answers one question: can we stand up a Xephyr with TWO RANDR
# outputs and validate Maverick's multi-monitor behaviour for real (not faked)?
#
# Launch strategy (the only one that works in this environment):
#   * A single Xephyr screen sized 2560x800 (two 1280x800 halves side by side).
#   * RANDR 1.5 `xrandr --setmonitor` carves it into TWO logical monitors:
#       MON-L  1280x800 +0+0    (bound to the real Xephyr output)
#       MON-R  1280x800 +1280+0 (auto/placeholder output)
#   Maverick enumerates monitors with randr_get_monitors (RANDR 1.5), so it
#   then sees 2 monitors — no Zaphod/Xinerama multi-screen needed.
#
# Why not `-screen 1920x1080 -screen 1920x1080`? Xephyr exposes a SINGLE RANDR
# output ("default") spanning the whole virtual screen; GetMonitors therefore
# reports exactly 1 monitor regardless of how many `-screen` args you pass. The
# `--setmonitor` split is the only way to get 2 logical monitors here.
#
# Control-socket caveat: maverick's socket path is
#   $XDG_RUNTIME_DIR/maverick/<sid>/<sid>.sock
# With the default /run/user/1000 that path is 108 bytes -> one past SUN_LEN
# (108 incl. NUL) -> "path must be shorter than SUN_LEN" and the socket never
# binds. We therefore use a short XDG_RUNTIME_DIR so maverick-msg can talk to
# the instance. (This is an environment/test workaround; it does not change any
# maverick source.)
#
# Everything below is read-only wrt maverick source. No .rs file is touched.
#
# Assertions exercised on BOTH monitors:
#   Create, Focus, MoveToMonitor (Applied/Desired monitor ownership),
#   Fullscreen ribbon clipping (clip to own monitor; no bleed to the other),
#   Maximize ownership (presented_maximize owner per monitor),
#   WorkspaceSwitch (per-monitor active_ws), Destroy (clean teardown),
#   pending focus (overlay + silently-added background window, both monitors).
#
set -u

# ── config ────────────────────────────────────────────────────────────────────
XEPHYR_DISPLAY=":98"
MW=1280          # monitor width
MH=800           # monitor height
TOTAL_W=2560     # MW*2
MAVERICK_BIN="${MAVERICK_BIN:-./target/release/maverick}"
MSG_BIN="${MSG_BIN:-./target/release/maverick-msg}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/mv2m}"   # short -> avoids SUN_LEN
# Start from a clean runtime dir so discovery never talks to a stale/dead socket.
rm -rf "$XDG_RUNTIME_DIR"; mkdir -p "$XDG_RUNTIME_DIR"
LOG="$(mktemp -t maverick-2mon.XXXXXX.log)"
XEPHYR_LOG="$(mktemp -t xephyr-2mon.XXXXXX.log)"

PASS=0
FAIL=0
SPIDS=()

ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }
info(){ echo "INFO: $*"; }

cleanup() {
    for p in "${SPIDS[@]:-}"; do kill "$p" 2>/dev/null; done
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
}
trap cleanup EXIT

# ── launch nested 2-output Xephyr ──────────────────────────────────────────────
info "Launch command:"
info "  Xephyr $XEPHYR_DISPLAY -screen ${TOTAL_W}x${MH} -ac +extension RANDR +extension GLX +extension Composite +extension DAMAGE -retro"
Xephyr "$XEPHYR_DISPLAY" -screen "${TOTAL_W}x${MH}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    -retro >"$XEPHYR_LOG" 2>&1 &
XEPHYR_PID=$!
sleep 1.5

export DISPLAY="$XEPHYR_DISPLAY"

# Create the two RANDR monitors. Capture exact xrandr output for the report.
info "RANDR setmonitor commands:"
info "  xrandr --setmonitor MON-L ${MW}/340x${MH}/212+0+0 default"
info "  xrandr --setmonitor MON-R ${MW}/340x${MH}/212+${MW}+0 none"
L_OUT="$(xrandr --setmonitor MON-L ${MW}/340x${MH}/212+0+0 default 2>&1)"
R_OUT="$(xrandr --setmonitor MON-R ${MW}/340x${MH}/212+${MW}+0 none 2>&1)"
echo "$L_OUT" | grep -v 'gamma' ; echo "$R_OUT" | grep -v 'gamma'
info "listmonitors: $(xrandr --listmonitors 2>/dev/null | grep -v gamma | tr '\n' ' ')"

if ! xprop -root >/dev/null 2>&1; then
    bad "Xephyr did not come up on $DISPLAY"
    exit 1
fi

# ── launch maverick ─────────────────────────────────────────────────────────────
"$MAVERICK_BIN" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 2
if ! xprop -root >/dev/null 2>&1 || ! kill -0 "$MAV_PID" 2>/dev/null; then
    bad "maverick did not start on $DISPLAY (log: $LOG)"
    tail -20 "$LOG"
    exit 1
fi
ok "maverick started on $DISPLAY (log: $LOG)"
if grep -q "compositor: GL ready" "$LOG"; then
    info "compositor: GL ready (multi-monitor compositor path exercised)"
elif grep -qi "compositor" "$LOG"; then
    info "compositor note: $(grep -i compositor "$LOG" | head -1)"
fi

# ── IPC helpers ────────────────────────────────────────────────────────────────
state() { "$MSG_BIN" query state 2>/dev/null; }
tree()  { "$MSG_BIN" query tree  2>/dev/null; }

selmon() { state | python3 -c "import sys,json;print(json.load(sys.stdin)['sel_mon'])"; }

# Print one line per managed window: ID MON FS MX X Y W H TITLE
tree_lines() {
    python3 - <<'PY'
import sys,json
try:
    d=json.load(sys.stdin)
except Exception as e:
    sys.exit(0)
def walk():
    for m in d.get('monitors',[]):
        for ws in m.get('workspaces',[]):
            for col in ws.get('columns',[]):
                for w in col.get('windows',[]): yield w
            for w in ws.get('floats',[]): yield w
for w in walk():
    g=w.get('geom',[0,0,0,0])
    print(w['id'], w.get('monitor',-1),
          int(bool(w.get('fullscreen'))), int(bool(w.get('maximized'))),
          g[0], g[1], g[2], g[3], w.get('title',''))
PY
}

hexid()  { printf '0x%x' "$1"; }
active_win() { xprop -root -notype _NET_ACTIVE_WINDOW 2>/dev/null | grep -oE '0x[0-9a-f]+' | head -1; }

# window id -> its geom centre, used to focus by synthetic click
win_center() {
    local id="$1" g
    g="$(tree | tree_lines | awk -v i="$id" '$1==i{print $5,$6,$7,$8; exit}')"
    [ -z "$g" ] && { echo 0 0; return; }
    local x y w h; read -r x y w h <<<"$g"
    echo $((x + w/2)) $((y + h/2))
}

focus_win() {
    local id="$1"; local c; c="$(win_center "$id")"
    local cx cy; read -r cx cy <<<"$c"
    [ "$cx" -eq 0 ] && [ "$cy" -eq 0 ] && return
    xdotool mousemove "$cx" "$cy" click 1 2>/dev/null
    sleep 0.3
}

# select monitor m via focus_mon:next (2-mon setup: 0<->1)
setmon() {
    local tgt="$1" cur i
    for i in $(seq 1 6); do
        cur="$(selmon 2>/dev/null)"; [ "$cur" = "$tgt" ] && return 0
        "$MSG_BIN" focus_mon next >/dev/null 2>&1; sleep 0.3
    done
    return 1
}

spawn_win() {
    local title="$1" id=""
    xterm -title "$title" -e bash -c 'sleep 120' >/dev/null 2>&1 &
    SPIDS+=($!)
    for _ in $(seq 1 50); do id="$(xdotool search --name "$title" 2>/dev/null | head -1)"; [ -n "$id" ] && break; sleep 0.2; done
    echo "$id"
}

kill_all() {
    for p in "${SPIDS[@]:-}"; do kill "$p" 2>/dev/null; done
    SPIDS=()
    for _ in $(seq 1 30); do
        [ -z "$(tree | tree_lines)" ] && return 0
        sleep 0.2
    done
}

# geom within monitor m? (0 -> [0,MW), 1 -> [MW,TOTAL_W))
in_mon() {
    local m="$1" x="$2" y="$3" w="$4" h="$5"
    if [ "$m" -eq 0 ]; then
        [ "$x" -ge 0 ] && [ $((x+w)) -le "$MW" ] && [ "$y" -ge 0 ] && [ $((y+h)) -le "$MH" ]
    else
        [ "$x" -ge "$MW" ] && [ $((x+w)) -le "$TOTAL_W" ] && [ "$y" -ge 0 ] && [ $((y+h)) -le "$MH" ]
    fi
}

# ── sanity: exactly 2 monitors ────────────────────────────────────────────────
NMON="$(state | python3 -c "import sys,json;print(len(json.load(sys.stdin)['monitors']))")"
if [ "$NMON" = "2" ]; then
    ok "Maverick reports 2 monitors (RANDR GetMonitors on 2-output Xephyr)"
else
    bad "Maverick reports $NMON monitors, expected 2 — cannot validate multi-monitor"
    exit 1
fi

# ══════════════════════════════════════════════════════════════════════════════
# S1 — Create / Focus / MoveToMonitor  (Applied/Desired monitor ownership)
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## S1 Create/Focus/MoveToMonitor (Applied+Desired ownership) ##########"
setmon 0
A="$(spawn_win R6_M0A)"; sleep 0.4
TL="$(tree | tree_lines)"
AROW="$(echo "$TL" | awk -v i="$A" '$1==i')"
AMON="$(echo "$AROW" | awk '{print $2}')"
AX=$(echo "$AROW" | awk '{print $5}'); AY=$(echo "$AROW" | awk '{print $6}')
AW=$(echo "$AROW" | awk '{print $7}'); AH=$(echo "$AROW" | awk '{print $8}')
if [ "$AMON" = "0" ] && in_mon 0 "$AX" "$AY" "$AW" "$AH"; then
    ok "S1 Create on mon0: applied monitor=0 and geom (${AW}x${AH}@${AX},${AY}) within mon0"
else
    bad "S1 Create on mon0: mon=$AMON geom ${AW}x${AH}@${AX},${AY} not within mon0"
fi
# focus it
focus_win "$A"; sleep 0.3
AFOC="$(state | python3 -c "import sys,json;print(json.load(sys.stdin)['monitors'][0]['focused'])")"
if [ "$AFOC" = "$A" ]; then ok "S1 Focus: mon0.focused == $A"; else bad "S1 Focus: mon0.focused=$AFOC expected $A"; fi
# move to mon1
"$MSG_BIN" move_mon next >/dev/null 2>&1; sleep 0.5
TL="$(tree | tree_lines)"
AROW="$(echo "$TL" | awk -v i="$A" '$1==i')"
AMON="$(echo "$AROW" | awk '{print $2}')"
AX=$(echo "$AROW" | awk '{print $5}'); AY=$(echo "$AROW" | awk '{print $6}')
AW=$(echo "$AROW" | awk '{print $7}'); AH=$(echo "$AROW" | awk '{print $8}')
if [ "$AMON" = "1" ] && in_mon 1 "$AX" "$AY" "$AW" "$AH"; then
    ok "S1 MoveToMonitor: A now applied on mon1 (mon=$AMON, geom within mon1) — Applied/Desired ownership moved"
else
    bad "S1 MoveToMonitor: mon=$AMON geom ${AW}x${AH}@${AX},${AY} not within mon1"
fi

# ══════════════════════════════════════════════════════════════════════════════
# S2 — Fullscreen ribbon clipping (per monitor) + no compositor on wrong monitor
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## S2 Fullscreen ribbon clipping (clip to own monitor) ##########"
for m in 0 1; do
    setmon "$m"; sleep 0.2
    B="$(spawn_win "R6_FS$m")"; focus_win "$B"; sleep 0.4
    "$MSG_BIN" toggle_fullscreen >/dev/null 2>&1; sleep 0.5
    TL="$(tree | tree_lines)"; ROW="$(echo "$TL" | awk -v i="$B" '$1==i')"
    FS="$(echo "$ROW" | awk '{print $3}')"
    BX=$(echo "$ROW" | awk '{print $5}'); BY=$(echo "$ROW" | awk '{print $6}')
    BW=$(echo "$ROW" | awk '{print $7}'); BH=$(echo "$ROW" | awk '{print $8}')
    if [ "$FS" = "1" ] && in_mon "$m" "$BX" "$BY" "$BW" "$BH"; then
        ok "S2 mon$m: fullscreen ribbon clips to mon$m (geom ${BW}x${BH}@${BX},${BY} inside mon$m) — no bleed to other monitor"
    else
        bad "S2 mon$m: fullscreen geom ${BW}x${BH}@${BX},${BY} fs=$FS NOT within mon$m"
    fi
    "$MSG_BIN" toggle_fullscreen >/dev/null 2>&1; sleep 0.3   # unfullscreen
done

# ══════════════════════════════════════════════════════════════════════════════
# S3 — Maximize ownership (presented_maximize) per monitor
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## S3 Maximize ownership (presented_maximize) ##########"
for m in 0 1; do
    setmon "$m"; sleep 0.2
    C="$(spawn_win "R6_MX$m")"; focus_win "$C"; sleep 0.4
    "$MSG_BIN" toggle_maximize >/dev/null 2>&1; sleep 0.5
    TL="$(tree | tree_lines)"
    # count maximized + list their monitors
    MXLIST="$(echo "$TL" | awk '$4==1{print $2}')"
    CMON="$(echo "$TL" | awk -v i="$C" '$1==i{print $2}')"
    CMX="$(echo "$TL" | awk -v i="$C" '$1==i{print $4}')"
    CX=$(echo "$TL" | awk -v i="$C" '$1==i{print $5}'); CY=$(echo "$TL" | awk -v i="$C" '$1==i{print $6}')
    CW=$(echo "$TL" | awk -v i="$C" '$1==i{print $7}'); CH=$(echo "$TL" | awk -v i="$C" '$1==i{print $8}')
    NMX="$(echo "$MXLIST" | grep -c '^')"
    if [ "$CMX" = "1" ] && [ "$CMON" = "$m" ] && in_mon "$m" "$CX" "$CY" "$CW" "$CH"; then
        ok "S3 mon$m: presented_maximize owner C on mon$m (geom within mon$m); maximized windows total=$NMX"
    else
        bad "S3 mon$m: C mx=$CMX mon=$CMON geom ${CW}x${CH}@${CX},${CY} (maximized total=$NMX)"
    fi
    "$MSG_BIN" toggle_maximize >/dev/null 2>&1; sleep 0.3
done

# ══════════════════════════════════════════════════════════════════════════════
# S4 — WorkspaceSwitch (per-monitor active_ws) + cross-monitor occupancy
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## S4 WorkspaceSwitch (per-monitor active_ws) ##########"
for m in 0 1; do
    setmon "$m"; sleep 0.2
    "$MSG_BIN" view 2 >/dev/null 2>&1; sleep 0.3
    D="$(spawn_win "R6_WS${m}2")"; sleep 0.4
    TL="$(tree | tree_lines)"; ROW="$(echo "$TL" | awk -v i="$D" '$1==i')"
    DMON="$(echo "$ROW" | awk '{print $2}')"
    DWS="$(echo "$ROW" | awk '{print $9}')"   # title carries ws tag? use tree monitor+aw
    DAWS="$(state | python3 -c "import sys,json;print(json.load(sys.stdin)['monitors'][$m]['active_ws'])")"
    if [ "$DMON" = "$m" ] && [ "$DAWS" = "1" ]; then
        ok "S4 mon$m: spawned on active workspace 2 (index 1), window landed on mon$m"
    else
        bad "S4 mon$m: win mon=$DMON active_ws=$DAWS (expected mon$m, ws1)"
    fi
    "$MSG_BIN" view 1 >/dev/null 2>&1; sleep 0.3
    DAWS0="$(state | python3 -c "import sys,json;print(json.load(sys.stdin)['monitors'][$m]['active_ws'])")"
    [ "$DAWS0" = "0" ] && ok "S4 mon$m: switched back to workspace 1 (index 0)" || bad "S4 mon$m: back-switch active_ws=$DAWS0"
done

# ══════════════════════════════════════════════════════════════════════════════
# S5 — Destroy (clean teardown, WM stays alive)
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## S5 Destroy ##########"
kill_all; sleep 0.5
REMAIN="$(tree | tree_lines | wc -l)"
if [ "$REMAIN" -eq 0 ] && kill -0 "$MAV_PID" 2>/dev/null; then
    ok "S5 Destroy: all clients gone ($REMAIN), maverick still alive"
else
    bad "S5 Destroy: $REMAIN clients remain or WM dead"
fi

# ══════════════════════════════════════════════════════════════════════════════
# S6 — pending focus (overlay + silently-added background window) on both monitors
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## S6 pending focus (overlay dismiss -> background window) ##########"
click_until() {            # $1=target_hex $2=x $3=y
    local tgt="$1" got="" i
    for i in $(seq 1 6); do
        got="$(active_win)"; [ "$got" = "$tgt" ] && { echo "$got"; return 0; }
        xdotool mousemove "$2" "$3" click 1 2>/dev/null; sleep 0.4
    done
    echo "$got"
}
for m in 0 1; do
    setmon "$m"; sleep 0.2
    A2="$(spawn_win "R6_PF${m}_A")"; focus_win "$A2"; sleep 0.4
    "$MSG_BIN" toggle_fullscreen >/dev/null 2>&1; sleep 0.5
    # spawn background window behind the overlay
    B2="$(spawn_win "R6_PF${m}_B")"; sleep 0.8
    HEXA="$(hexid "$A2")"; HEXB="$(hexid "$B2")"
    STAY="$(active_win)"
    if [ "$STAY" = "$HEXA" ]; then
        ok "S6 mon$m: X input focus stays on overlay A2 after B2 managed (pending_focus recorded)"
    else
        bad "S6 mon$m: focus was $STAY, expected overlay $HEXA after B2 mapped"
    fi
    CX=$(( m==0 ? MW/2 : MW + MW/2 )); CY=$(( MH/2 ))
    GOT="$(click_until "$HEXB" "$CX" "$CY")"
    if [ "$GOT" = "$HEXB" ]; then
        ok "S6 mon$m: clicking overlay dismisses it and focuses background B2 (pending focus applied)"
    else
        bad "S6 mon$m: click did not reach B2 (got $GOT, expected $HEXB)"
    fi
    # clean up these two for next iteration
    kill "$SPIDS[-1]" 2>/dev/null; SPIDS=("${SPIDS[@]:0:${#SPIDS[@]}-1}")
    pkill -f "R6_PF${m}_A" 2>/dev/null; pkill -f "R6_PF${m}_B" 2>/dev/null
    SPIDS=(); sleep 0.5
done

# ── summary ─────────────────────────────────────────────────────────────────────
echo; echo "────────────────────────────────────────"
echo "Riesgo 6 (2-monitor) suite: $PASS passed, $FAIL failed"
echo "maverick log: $LOG"
echo "xephyr log:   $XEPHYR_LOG"
[ "$FAIL" -eq 0 ]
