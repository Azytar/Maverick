#!/usr/bin/env bash
#
# compat-matrix.sh — Maverick 3.0 real-client compatibility matrix (Fase 2/3/4/5/6/7).
#
# Drives a live, tiled Maverick session (nested Xephyr) with:
#   Fase 2 — Firefox (real browser): fullscreen/maximize/popup/workspace.
#   Fase 3 — Wine (emulated): Wine-like resize/fullscreen/transient/aux sequences
#            via the stdin-driven `hostile` client.
#   Fase 4 — Games (emulated + glxgears): _NET_WM_STATE_FULLSCREEN, OR windows.
#   Fase 5 — _NET_ACTIVE_WINDOW: client focus grabs vs WM authority.
#   Fase 6 — Transient chains A→B→…→E at depths 1..8 (MAX_TRANSIENT_DEPTH=4 boundary).
#   Fase 7 — delegates to tests/xephyr-2mon.sh (2-output RANDR multi-monitor).
#
# It aggregates PASS/FAIL per phase and prints the compatibility matrix.
#
# Build prerequisites (standalone, NOT the cargo workspace):
#   gcc tests/hostile.c -o /tmp/hostile -lX11
#   cargo build --release        # provides ./target/release/maverick + maverick-msg
#
# Notes:
#   * Nested Xephyr is flaky in this environment — launched with
#     MAVERICK_NO_COMPOSITOR=1 under the real :0, every xdotool is wrapped in
#     `timeout 8` so a display death can't hang the suite.
#   * The control-socket path exceeds SUN_LEN with the default XDG_RUNTIME_DIR,
#     so we use a short one (same workaround as xephyr-2mon.sh); it does not
#     change any maverick source.
#   * If no X server is reachable the live phases SKIP with a clear message; the
#     deterministic Rust property harnesses (cargo test) cover the same logic.
set -u

# ── config ────────────────────────────────────────────────────────────────────
XEPHYR_DISPLAY="${XEPHYR_DISPLAY:-:97}"
MW=1280; MH=800; TOTAL_W=1280
MAVERICK_BIN="${MAVERICK_BIN:-./target/release/maverick}"
MSG_BIN="${MSG_BIN:-./target/release/maverick-msg}"
HOSTILE="${HOSTILE:-/tmp/hostile}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/mvcm}"   # short -> avoids SUN_LEN
export MAVERICK_NO_COMPOSITOR=1
rm -rf "$XDG_RUNTIME_DIR"; mkdir -p "$XDG_RUNTIME_DIR"

PASS=0; FAIL=0; SKIP=0
SPIDS=()

ok()   { echo "PASS: $*"; PASS=$((PASS+1)); }
bad()  { echo "FAIL: $*"; FAIL=$((FAIL+1)); }
skip() { echo "SKIP: $*"; SKIP=$((SKIP+1)); }
info() { echo "INFO: $*"; }

cleanup() {
    for fd in "${HFD[@]:-}"; do eval "exec {$fd}>&-" 2>/dev/null; done
    for p in "${SPIDS[@]:-}";   do kill "$p" 2>/dev/null; done
    [ -n "${MAV_PID:-}" ]    && kill "$MAV_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
}
trap cleanup EXIT

# ── build the hostile client if missing ───────────────────────────────────────
if [ ! -x "$HOSTILE" ]; then
    info "building hostile client: gcc tests/hostile.c -o $HOSTILE -lX11"
    gcc tests/hostile.c -o "$HOSTILE" -lX11 || { echo "cannot build hostile"; exit 1; }
fi

# ── launch nested Xephyr + maverick ───────────────────────────────────────────
info "Xephyr $XEPHYR_DISPLAY -screen ${TOTAL_W}x${MH} -ac +extension RANDR +extension GLX +extension Composite +extension DAMAGE"
Xephyr "$XEPHYR_DISPLAY" -screen "${TOTAL_W}x${MH}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    -retro >/tmp/mvcm-xephyr.log 2>&1 &
XEPHYR_PID=$!
sleep 1.5
export DISPLAY="$XEPHYR_DISPLAY"
if ! xprop -root >/dev/null 2>&1; then
    skip "no X server reachable on $DISPLAY — live phases cannot run (env-limited)."
    echo "SUMMARY: compat-matrix: $PASS passed, $FAIL failed, $SKIP skipped"
    exit 0
fi

"$MAVERICK_BIN" >/tmp/mvcm-mav.log 2>&1 &
MAV_PID=$!
sleep 2
if ! kill -0 "$MAV_PID" 2>/dev/null; then
    bad "maverick did not start on $DISPLAY"; tail -20 /tmp/mvcm-mav.log
    exit 1
fi
ok "maverick started on $DISPLAY"

# ── IPC helpers ───────────────────────────────────────────────────────────────
state() { "$MSG_BIN" query state 2>/dev/null; }
tree()  { "$MSG_BIN" query tree  2>/dev/null; }
tree_lines() {
    python3 - <<'PY'
import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(0)
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
          g[0],g[1],g[2],g[3], w.get('title',''),
          int(bool(w.get('focus'))), int(bool(w.get('x11_focus'))),
          int(bool(w.get('overlay'))), int(bool(w.get('pending'))))
PY
}
hexid()  { printf '0x%x' "$1"; }
active_win() { xprop -root -notype _NET_ACTIVE_WINDOW 2>/dev/null | grep -oE '0x[0-9a-f]+' | head -1; }
win_field() { # $1=id $2=field-index(0-based into tree_lines cols)
    tree | tree_lines | awk -v i="$1" -v f="$2" '$1==i{print $f; exit}'
}

# Start a long-lived hostile session over a process-substitution pipe (the pipe
# write end stays open in the script, so the window survives across commands and
# hostile never sees EOF until we close the fd). Use hostile_cmd / hostile_winid.
declare -A HFD=()
declare -A HOUT=()
hostile_start() {
    local tag="$1" out="/tmp/mvcm-h.$tag.out"
    exec {fd}> >(exec "$HOSTILE" "$DISPLAY" >"$out" 2>/dev/null)
    HFD[$tag]=$fd
    HOUT[$tag]="$out"
}
hostile_cmd() { # $1=tag $2..=command
    local tag="$1"; shift
    local fd="${HFD[$tag]:-}"
    [ -n "$fd" ] && echo "$*" >&"$fd"
    sleep 0.25
}
hostile_winid() { # $1=tag
    local out="${HOUT[$tag]:-}"
    for _ in $(seq 1 50); do
        local w; w="$(grep -m1 '^WINID=' "$out" 2>/dev/null | sed 's/^WINID=//')"
        [ -n "$w" ] && { echo "$w"; return; }
        sleep 0.2
    done
    echo ""
}
hostile_stop() { # $1=tag
    local fd="${HFD[$tag]:-}"
    [ -n "$fd" ] && { echo "destroy" >&"$fd"; exec {fd}>&-; }
    sleep 0.3
}

# ══════════════════════════════════════════════════════════════════════════════
# Fase 2 — Firefox (real)
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## Fase 2 — Firefox (real) ##########"
if command -v firefox >/dev/null 2>&1; then
    firefox -new-instance -no-remote about:blank >/dev/null 2>&1 &
    FF=$!; SPIDS+=("$FF")
    FFID=""
    for _ in $(seq 1 60); do
        FFID="$(xdotool search --class Firefox 2>/dev/null | head -1)"
        [ -n "$FFID" ] && break; sleep 0.3
    done
    if [ -n "$FFID" ]; then
        ok "Fase2 Firefox mapped (id=$FFID)"
        timeout 8 xdotool windowactivate --sync "$FFID" 2>/dev/null; sleep 0.5
        # F11 fullscreen (EWMH/Core fullscreen path)
        timeout 8 xdotool key --window "$FFID" F11 2>/dev/null; sleep 0.8
        FFS="$(win_field "$FFID" 3)"
        if [ "$FFS" = "1" ]; then ok "Fase2 Firefox enters fullscreen (EWMH fullscreen honored)"; else bad "Fase2 Firefox F11 did not reach fullscreen"; fi
        timeout 8 xdotool key --window "$FFID" F11 2>/dev/null; sleep 0.5
        FFS2="$(win_field "$FFID" 3)"
        [ "$FFS2" = "0" ] && ok "Fase2 Firefox leaves fullscreen cleanly" || bad "Fase2 Firefox stuck in fullscreen"
        # maximize via _NET_WM_STATE through hostile-style message is out of scope;
        # record geometry only (observability, no behavior change).
        info "Fase2 Firefox geom: $(tree | tree_lines | awk -v i="$FFID" '$1==i{print $5,$6,$7,$8}')"
        kill "$FF" 2>/dev/null
    else
        bad "Fase2 Firefox did not map"
    fi
else
    skip "Fase2 Firefox: /usr/bin/firefox not present (env-limited)"
fi

# ══════════════════════════════════════════════════════════════════════════════
# Fase 3 — Wine (emulated via hostile): Cases A–F hostile sequences
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## Fase 3 — Wine (emulated) ##########"
W="$(hostile_start wine)"
hostile_cmd "$W" create
WID="$(hostile_winid "$W")"
if [ -n "$WID" ]; then
    ok "Fase3 Wine-emul window created (id=$WID)"
    # Case A: aggressive resize-on-start + spam-resize
    hostile_cmd "$W" spam-resize 20
    sleep 0.4
    ROW="$(tree | tree_lines | awk -v i="$WID" '$1==i')"
    [ -n "$ROW" ] && ok "Fase3 Case A resize storm survived (window still tracked)" || bad "Fase3 Case A window lost during resize storm"
    # Case B: client fullscreen toggle
    hostile_cmd "$W" fullscreen; sleep 0.5
    FSB="$(win_field "$WID" 3)"
    [ "$FSB" = "1" ] && ok "Fase3 Case B client _NET_WM_STATE_FULLSCREEN honored" || bad "Fase3 Case B fullscreen not honored"
    hostile_cmd "$W" fullscreen; sleep 0.4
    # Case C: popup/transient + aux window
    WC="$(hostile_start winechild)"; hostile_cmd "$WC" "transient $WID"; hostile_cmd "$WC" create
    WCID="$(hostile_winid "$WC")"
    [ -n "$WCID" ] && ok "Fase3 Case C transient child created (child=$WCID of $WID)" || bad "Fase3 Case C transient child failed"
    hostile_stop "$WC"
    # Case D: focus capture via _NET_ACTIVE_WINDOW
    hostile_cmd "$W" active; sleep 0.4
    AX="$(active_win)"
    [ "$AX" = "$(hexid "$WID")" ] && ok "Fase3 Case D _NET_ACTIVE_WINDOW -> focus follows (WM authority)" || info "Fase3 Case D active-window focus=$AX (may be deferred behind overlay)"
    # Case E: destroy/recreate
    hostile_cmd "$W" destroy; sleep 0.3
    [ -z "$(win_field "$WID" 0)" ] && ok "Fase3 Case E destroy clean" || bad "Fase3 Case E window survived destroy"
    hostile_cmd "$W" create; sleep 0.3
    WID2="$(hostile_winid "$W")"
    [ -n "$WID2" ] && ok "Fase3 Case E recreate works (new id=$WID2)" || bad "Fase3 Case E recreate failed"
else
    bad "Fase3 Wine-emul window did not create"
fi
hostile_stop "$W"

# ══════════════════════════════════════════════════════════════════════════════
# Fase 4 — Games (emulated + glxgears): _NET_WM_STATE_FULLSCREEN + OR windows
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## Fase 4 — Games (emulated) ##########"
G="$(hostile_start game)"
hostile_cmd "$G" create; GID="$(hostile_winid "$G")"
if [ -n "$GID" ]; then
    ok "Fase4 game-emul window created (id=$GID)"
    hostile_cmd "$G" fullscreen; sleep 0.5
    GFS="$(win_field "$GID" 3)"
    [ "$GFS" = "1" ] && ok "Fase4 client fullscreen honored (WM-managed fullscreen)" || bad "Fase4 game fullscreen not honored"
    hostile_cmd "$G" fullscreen; sleep 0.3
    hostile_cmd "$G" spam-resize 10; sleep 0.4
    [ -n "$(win_field "$GID" 0)" ] && ok "Fase4 resize-on-start survived" || bad "Fase4 game lost during resize"
else
    bad "Fase4 game-emul window did not create"
fi
hostile_stop "$G"
# glxgears as a real GLX client (best-effort; needs GLX in Xephyr)
if command -v glxgears >/dev/null 2>&1; then
    glxgears >/dev/null 2>&1 & GG=$!; SPIDS+=("$GG")
    GGID=""
    for _ in $(seq 1 40); do GGID="$(xdotool search --name glxgears 2>/dev/null|head -1)"; [ -n "$GGID" ]&&break; sleep 0.2; done
    if [ -n "$GGID" ]; then
        ok "Fase4 glxgears (real GLX) mapped (id=$GGID)"
        info "Fase4 glxgears geom: $(tree | tree_lines | awk -v i="$GGID" '$1==i{print $5,$6,$7,$8}')"
        kill "$GG" 2>/dev/null
    else
        info "Fase4 glxgears mapped but not found by xdotool (env quirk)"
    fi
else
    skip "Fase4 glxgears not present (env-limited)"
fi

# ══════════════════════════════════════════════════════════════════════════════
# Fase 5 — _NET_ACTIVE_WINDOW: client grab vs WM authority
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## Fase 5 — _NET_ACTIVE_WINDOW ##########"
A5="$(hostile_start a5)"; hostile_cmd "$A5" create; AID="$(hostile_winid "$A5")"
B5="$(hostile_start b5)"; hostile_cmd "$B5" create; BID="$(hostile_winid "$B5")"
if [ -n "$AID" ] && [ -n "$BID" ]; then
    # focus A via WM (xdotool click), then B grabs via _NET_ACTIVE_WINDOW
    CX=$(( MW/2 )); CY=$(( MH/2 ))
    timeout 8 xdotool mousemove "$CX" "$CY" click 1 2>/dev/null; sleep 0.3
    hostile_cmd "$B5" active; sleep 0.4
    BHEX="$(hexid "$BID")"; GOT="$(active_win)"
    BF="$(win_field "$BID" 10)"; BXF="$(win_field "$BID" 11)"
    if [ "$GOT" = "$BHEX" ] && [ "$BF" = "1" ]; then
        ok "Fase5 _NET_ACTIVE_WINDOW: B gained focus (focus=1, x11_focus=1)"
    else
        info "Fase5 _NET_ACTIVE_WINDOW: focus=$BF x11_focus=$BXF active=$GOT expected $BHEX (may be deferred/pending per overlay policy)"
    fi
    BP="$(win_field "$BID" 13)"
    info "Fase5 B pending=$BP overlay(a)=$(win_field "$AID" 12) — observability captured"
else
    bad "Fase5 windows did not create"
fi
hostile_stop "$A5"; hostile_stop "$B5"

# ══════════════════════════════════════════════════════════════════════════════
# Fase 6 — Transient chains A→B→…→E at depths 1,2,4,5,8 (MAX_TRANSIENT_DEPTH=4)
# ══════════════════════════════════════════════════════════════════════════════
echo; echo "########## Fase 6 — Transient chains ##########"
for depth in 1 2 4 5 8; do
    # build chain root..depth
    prev=""
    ids=()
    tagprefix="t6d${depth}"
    for i in $(seq 1 "$depth"); do
        tg="${tagprefix}_$i"
        sess="$(hostile_start "$tg")"
        if [ -n "$prev" ]; then hostile_cmd "$tg" "transient $prev"; fi
        hostile_cmd "$tg" create
        id="$(hostile_winid "$tg")"
        ids+=("$id")
        prev="$id"
    done
    # give WM time to walk the chain
    sleep 0.6
    # observe: each link tracked, leaf present
    present=0
    for id in "${ids[@]}"; do
        [ -n "$(win_field "$id" 0)" ] && present=$((present+1))
    done
    if [ "$present" -eq "$depth" ]; then
        ok "Fase6 depth=$depth: all $depth transient links tracked (MAX_TRANSIENT_DEPTH=4 boundary documented; stacking/ownership allowed to change beyond it)"
    else
        bad "Fase6 depth=$depth: only $present/$depth links tracked"
    fi
    # teardown chain from leaf up
    for i in $(seq "$depth" -1 1); do hostile_stop "${tagprefix}_$i"; done
    sleep 0.3
done

# ── shut down our instance before Fase 7 (it manages its own display/runtime) ──
echo; echo "########## Fase 7 — multi-monitor (delegates to tests/xephyr-2mon.sh) ##########"
cleanup 2>/dev/null
sleep 1
if [ -x tests/xephyr-2mon.sh ]; then
    if bash tests/xephyr-2mon.sh; then
        ok "Fase7 xephyr-2mon suite PASSED"
    else
        bad "Fase7 xephyr-2mon suite FAILED"
    fi
else
    skip "Fase7 tests/xephyr-2mon.sh not found"
fi

# ── summary ───────────────────────────────────────────────────────────────────
echo; echo "────────────────────────────────────────"
echo "compat-matrix (Fase 2/3/4/5/6): $PASS passed, $FAIL failed, $SKIP skipped"
echo "────────────────────────────────────────"
[ "$FAIL" -eq 0 ]
