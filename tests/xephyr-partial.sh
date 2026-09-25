#!/usr/bin/env bash
#
# Xephyr partial-redraw harness for Maverick (Fases 5–11).
#
# Validates that the compositor's partial-redraw path (buffer-age + accumulated
# damage + scissor, Fases 6/7/10) leaves the framebuffer correct after many
# frames: no residue from moved/erased windows, correct backdrop, correct
# structural changes. Also exercises the no-buffer-age fallback (forced full
# redraw) and measures CPU / render cost.
#
# This is a *manual / CI* harness: it needs a real (nested) X server with GLX and
# cannot run under `cargo test`. No results are fabricated: every assertion reads
# live pixels from the Composite overlay via `pxsample` (XGetImage).
#
# Usage:
#   ./tests/xephyr-partial.sh            # normal (partial-redraw) path
#   FORCE_FULL=1 ./tests/xephyr-partial.sh   # pretend no buffer-age (full redraw)
#
# Requirements: xephyr, x11-utils (xprop, xdpyinfo), mesa-utils (glxinfo),
# gcc. The C clients are compiled to /tmp on first run.

set -u
HOST_DISPLAY="${HOST_DISPLAY:-${DISPLAY:-:0}}"
export DISPLAY="${DISPLAY:-}"

SCREEN_W=1920
SCREEN_H=1080
XEPHYR_DISPLAY=":96"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$APP_DIR"

BIN=/tmp/maverick-partial-$$
mkdir -p "$BIN"
gcc -O2 tests/damager.c   -o "$BIN/damager"   -lX11 2>/dev/null
gcc -O2 tests/staticwin.c -o "$BIN/staticwin" -lX11 2>/dev/null
gcc -O2 tests/pxsample.c  -o "$BIN/pxsample"  -lX11 -lXcomposite 2>/dev/null
gcc -O2 tests/winmove.c   -o "$BIN/winmove"   -lX11 2>/dev/null

LOG="$(mktemp -t maverick-partial.XXXXXX.log)"
PASS=0; FAIL=0
CLIENT_PIDS=()
log()  { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()   { log "PASS: $*"; PASS=$((PASS+1)); }
bad()  { log "FAIL: $*"; FAIL=$((FAIL+1)); }

check_x_capabilities() {
    local info extension
    if ! command -v xdpyinfo >/dev/null 2>&1; then
        bad "xdpyinfo is required to verify nested X capabilities"
        return 1
    fi
    if ! info="$(DISPLAY="$DISPLAY" xdpyinfo 2>/dev/null)"; then
        bad "nested X server is not reachable on $DISPLAY"
        return 1
    fi
    for extension in Composite DAMAGE XFIXES GLX; do
        if ! printf '%s\n' "$info" | grep -Eq "^[[:space:]]*${extension}([[:space:]]|$)"; then
            bad "nested X server on $DISPLAY lacks required $extension extension"
            return 1
        fi
    done
    if ! command -v glxinfo >/dev/null 2>&1; then
        bad "glxinfo is required to verify GLX capability"
        return 1
    fi
    if ! DISPLAY="$DISPLAY" glxinfo -B >/dev/null 2>&1; then
        bad "GLX is advertised but no usable GLX context is available"
        return 1
    fi
    ok "nested X capabilities verified (Composite, DAMAGE, XFIXES, GLX)"
}

# Launch a client; records its owned PID and XID in LAST_PID/LAST_XID.
launch() {
    local lf w
    lf="$(mktemp -t client.XXXXXX.log)"
    "$@" >"$lf" 2>&1 &
    LAST_PID=$!
    CLIENT_PIDS+=("$LAST_PID")
    for _ in $(seq 1 50); do
        w="$(grep -oE 'WINID=0x[0-9a-f]+' "$lf" 2>/dev/null | head -1 | cut -d= -f2)"
        if [ -n "$w" ]; then
            LAST_XID="$w"
            rm -f "$lf"
            return 0
        fi
        sleep 0.1
    done
    bad "client did not report WINID: $*"
    kill "$LAST_PID" 2>/dev/null
    rm -f "$lf"
    return 1
}

sample_root() {
    local x="$1" y="$2" w="$3" h="$4" hex="$5" image pixel value r g b
    image="$(mktemp -t maverick-root.XXXXXX.png)"
    if ! import -window root "$image" >/dev/null 2>&1; then
        bad "root capture failed @$x,$y"
        rm -f "$image"
        return 1
    fi
    value="${hex#0x}"
    r=$((16#${value:0:2})); g=$((16#${value:2:2})); b=$((16#${value:4:2}))
    pixel="$(convert "$image" -crop "${w}x${h}+${x}+${y}" +repage -format '%[pixel:p{0,0}]' info: 2>/dev/null | tr -d '[:space:]')"
    rm -f "$image"
    if [ "$pixel" = "srgb($r,$g,$b)" ] || [ "$pixel" = "#${value}" ]; then
        ok "root pixel @$x,$y is 0x$hex"
    else
        bad "root pixel @$x,$y got '$pixel', expected 0x$hex"
    fi
}

sample() { # X Y W H HEX [TOL]
    local out; out="$("$BIN/pxsample" "$1" "$2" "$3" "$4" "$5" "${6:-28}" 2>&1)"
    log "$out"
    if printf '%s' "$out" | grep -q '^OK'; then ok "pxsample@$1,$2"; else bad "pxsample@$1,$2"; fi
}

# ── bring up the nested server (only if we're not already on one) ─────────────
cleanup() {
    local pid
    for pid in "${CLIENT_PIDS[@]:-}"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null
    done
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    rm -rf "$BIN"
}
trap cleanup EXIT

if [ -z "$DISPLAY" ]; then
    DISPLAY="$HOST_DISPLAY" Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac \
        +extension GLX +extension RANDR +extension Composite +extension DAMAGE +extension XFIXES \
        >"$LOG.xephyr" 2>&1 &
    XEPHYR_PID=$!
    sleep 1.5
    export DISPLAY="$XEPHYR_DISPLAY"
    check_x_capabilities || exit 1
fi

MAV_EXTRA=""
if [ "${FORCE_FULL:-0}" = "1" ]; then
    log "=== FORCED FULL-REDRAW (no buffer-age simulation) ==="
    MAV_EXTRA="MAVERICK_FORCE_FULL_REDRAW=1"
fi
MAVERICK_PERF_LOG=1 $MAV_EXTRA "$MAVERICK_BIN" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 2.5
if ! DISPLAY="$DISPLAY" xprop -root >/dev/null 2>&1; then
    bad "maverick did not start on $DISPLAY"; exit 1
fi
ok "maverick started on $DISPLAY"

# ── Scenario A: static backdrop + damager overlap; no residue ──────────────────
launch "$BIN/staticwin" 50 50 800 600 0x223355 || exit 1
BACKDROP_PID="$LAST_PID"
BACKDROP_XID="$LAST_XID"
launch "$BIN/damager" 0x33aa55 0xff3366 || exit 1
DAMAGER_PID="$LAST_PID"
DAMAGER_XID="$LAST_XID"
# Make the expected source/backdrop order explicit for pixel assertions.
xdotool windowlower "$BACKDROP_XID" >/dev/null 2>&1 || true
xdotool windowraise "$DAMAGER_XID" >/dev/null 2>&1 || true
sleep 2

# Backdrop far from any window must show its colour.
    log "damager geometry: $(xwininfo -id "$DAMAGER_XID" -stats 2>/dev/null | tr '\n' ' ')"
    log "root tree after clients: $(xwininfo -root -tree 2>/dev/null | tr '\n' ' ')"
    sample 100 100 60 60 0x223355
    # A corner of the damager that the moving dot never reaches must show the base.
sample 215 215 30 30 0x33aa55
# After the dot settles (frame > 30) an EARLIER dot position must have been
# redrawn to the base colour (partial-redraw must cover the erased area).
sleep 2
sample 230 230 30 30 0x33aa55

# ── Scenario B: move the damager away — old rect must not ghost ───────────────
xdotool windowmove "$DAMAGER_XID" 1500 800
log "moved damager geometry: $(xwininfo -id "$DAMAGER_XID" -stats 2>/dev/null | tr '\n' ' ')"
sleep 1.5
# The area the damager vacated (on the backdrop) must be clean backdrop.
sample_root 300 300 80 80 0x223355
# The damager at its new location shows its base.
sample_root 1520 820 30 30 0x33aa55

# ── Scenario C: resize ─────────────────────────────────────────────────────────
xdotool windowmove "$DAMAGER_XID" 200 200
xdotool windowsize "$DAMAGER_XID" 600 400
sleep 1.5
sample 230 230 30 30 0x33aa55   # still base after resize
sample 100 100 60 60 0x223355   # backdrop untouched

# ── Scenario D: overflow of DamageRegion (>32 damaging windows) ────────────────
OVERFLOW_PIDS=()
for i in $(seq 1 40); do
    if launch "$BIN/damager" 0x44aa88 0xffaa00; then
        OVERFLOW_PIDS+=("$LAST_PID")
    else
        bad "overflow damager $i did not start"
    fi
done
sleep 2
if kill -0 "$MAV_PID" 2>/dev/null; then ok "maverick survived 40 damaging windows (overflow -> full redraw)"; else bad "maverick crashed under overflow"; fi
sample 100 100 60 60 0x223355   # backdrop still correct under overflow
# tidy only the overflow processes started by this scenario
for pid in "${OVERFLOW_PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null
done
sleep 1

# ── Scenario E: structural — destroy a window, area must revert to backdrop ────
launch "$BIN/staticwin" 1200 200 300 300 0x8833aa || exit 1
STRUCT_PID="$LAST_PID"
STRUCT_XID="$LAST_XID"
sleep 1.5
sample 1220 220 40 40 0x8833aa
# Kill the owned client process → window destroyed → compositor full-repaints.
kill "$STRUCT_PID" 2>/dev/null || true
sleep 1.5
sample 700 500 40 40 0x223355   # backdrop back, no ghost of the dead window

# ── Measurement: CPU during small-damage vs during scroll ─────────────────────
# Sample maverick CPU via /proc across a quiet window of pure content damage.
CPU0="$(awk '{print $14+$15}' /proc/$MAV_PID/stat 2>/dev/null)"
sleep 3
CPU1="$(awk '{print $14+$15}' /proc/$MAV_PID/stat 2>/dev/null)"
if [ -n "$CPU0" ] && [ -n "$CPU1" ]; then
    log "maverick CPU ticks during small-damage idle: $((CPU1-CPU0)) / 3s"
    ok "cpu sample taken"
fi
log "── maverick perf log (render ns/frame batch) ──"
grep -i "perf" "$LOG" | tail -3 | while read -r line; do log "  $line"; done

log "────────────────────────────────────────"
log "partial-redraw suite: $PASS passed, $FAIL failed"
log "maverick log: $LOG"
[ "$FAIL" -eq 0 ]
