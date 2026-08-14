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
# Requirements: xephyr, x11-utils (xprop), gcc. The C clients are compiled to
# /tmp on first run.

set -u
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
log()  { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()   { log "PASS: $*"; PASS=$((PASS+1)); }
bad()  { log "FAIL: $*"; FAIL=$((FAIL+1)); }

# Launch a client; echoes its WINID (parsed from the client's stderr "WINID=...").
launch() { # $1=cmd...  -> prints WINID
    local lf; lf="$(mktemp -t client.XXXXXX.log)"
    "$@" >"$lf" 2>&1 &
    local pid=$!
    for _ in $(seq 1 50); do
        local w; w="$(grep -oE 'WINID=0x[0-9a-f]+' "$lf" 2>/dev/null | head -1 | cut -d= -f2)"
        if [ -n "$w" ]; then echo "$w"; return 0; fi
        sleep 0.1
    done
    bad "client did not report WINID: $*"; echo ""; return 1
}

sample() { # X Y W H HEX [TOL]
    local out; out="$("$BIN/pxsample" "$1" "$2" "$3" "$4" "$5" "${6:-28}" 2>&1)"
    log "$out"
    if printf '%s' "$out" | grep -q '^OK'; then ok "pxsample@$1,$2"; else bad "pxsample@$1,$2"; fi
}

# ── bring up the nested server (only if we're not already on one) ─────────────
cleanup() {
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    rm -rf "$BIN"
}
trap cleanup EXIT

if [ -z "$DISPLAY" ]; then
    Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac \
        +extension GLX +extension RANDR +extension Composite >"$LOG.xephyr" 2>&1 &
    XEPHYR_PID=$!
    sleep 1.5
    export DISPLAY="$XEPHYR_DISPLAY"
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
BACKDROP="$(launch "$BIN/staticwin" 50 50 800 600 0x223355)"
DAMAGER="$(launch "$BIN/damager" 0x33aa55 0xff3366)"
sleep 2

# Backdrop far from any window must show its colour.
sample 100 100 60 60 0x223355
# A corner of the damager that the moving dot never reaches must show the base.
sample 215 215 30 30 0x33aa55
# After the dot settles (frame > 30) an EARLIER dot position must have been
# redrawn to the base colour (partial-redraw must cover the erased area).
sleep 2
sample 230 230 30 30 0x33aa55

# ── Scenario B: move the damager away — old rect must not ghost ───────────────
"$BIN/winmove" "$DAMAGER" 1500 800
sleep 1.5
# The area the damager vacated (on the backdrop) must be clean backdrop.
sample 300 300 80 80 0x223355
# The damager at its new location shows its base.
sample 1520 820 30 30 0x33aa55

# ── Scenario C: resize ─────────────────────────────────────────────────────────
"$BIN/winmove" "$DAMAGER" 200 200 600 400
sleep 1.5
sample 230 230 30 30 0x33aa55   # still base after resize
sample 100 100 60 60 0x223355   # backdrop untouched

# ── Scenario D: overflow of DamageRegion (>32 damaging windows) ────────────────
OVERFLOW=0
for i in $(seq 1 40); do
    launch "$BIN/damager" 0x44aa88 0xffaa00 >/dev/null 2>&1 &
    OVERFLOW=$!
done
sleep 2
if kill -0 "$MAV_PID" 2>/dev/null; then ok "maverick survived 40 damaging windows (overflow -> full redraw)"; else bad "maverick crashed under overflow"; fi
sample 100 100 60 60 0x223355   # backdrop still correct under overflow
# tidy the overflow windows
pkill -f "$BIN/damager" 2>/dev/null || true
sleep 1

# ── Scenario E: structural — destroy a window, area must revert to backdrop ────
STRUCT="$(launch "$BIN/staticwin" 1200 200 300 300 0x8833aa)"
sleep 1.5
sample 1220 220 40 40 0x8833aa
# Kill the client process → window destroyed → compositor full-repaints.
kill "$STRUCT" 2>/dev/null || true
sleep 1.5
sample 1220 220 40 40 0x223355   # backdrop back, no ghost of the dead window

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
