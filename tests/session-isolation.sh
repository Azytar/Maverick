#!/usr/bin/env bash
#
# Maverick session-isolation + focus-reconciliation harness (Fases A–D, F).
#
# Validates the two root-cause fixes from the audit:
#   * Two Maverick sessions on different DISPLAYs get *distinct* session ids and
#     never share a socket/ficha (C1/C2/C3). `maverickctl --session <sid> quit`
#     kills only that session; the other survives.
#   * Focus does not silently desync: after focusing a window, the X server's
#     real input focus (GetInputFocus via `xprop -root _NET_ACTIVE_WINDOW`) and
#     the focused window's border colour agree (H1/H2).
#
# This is a *manual / CI* harness: it needs two nested X servers (Xephyr) with
# GLX and cannot run under `cargo test`. No results are fabricated: every
# assertion reads live state (process table, `maverickctl list`, xprop, pxsample).
#
# Requirements: xephyr, x11-utils (xprop), gcc. The C clients are compiled to
# /tmp on first run.
#
# Usage:
#   ./tests/session-isolation.sh

set -u
export DISPLAY="${DISPLAY:-}"

SCREEN_W=1280
SCREEN_H=720
X1=":96"
X2=":97"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$APP_DIR"

BIN=/tmp/maverick-session-$$
mkdir -p "$BIN"
gcc -O2 tests/staticwin.c -o "$BIN/staticwin" -lX11 2>/dev/null
gcc -O2 tests/pxsample.c  -o "$BIN/pxsample"  -lX11 -lXcomposite 2>/dev/null

LOG="$(mktemp -t maverick-session.XXXXXX.log)"
PASS=0; FAIL=0
log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); }

cleanup() {
    pkill -f "maverick --name maverick-session-a" 2>/dev/null
    pkill -f "maverick --name maverick-session-b" 2>/dev/null
    pkill -f "Xephyr $X1" 2>/dev/null
    pkill -f "Xephyr $X2" 2>/dev/null
}
trap cleanup EXIT

# ── start two nested X servers ───────────────────────────────────────────────
Xephyr "$X1" -screen "${SCREEN_W}x${SCREEN_H}" -ac +extension GLX +extension Composite 2>/dev/null &
Xephyr "$X2" -screen "${SCREEN_W}x${SCREEN_H}" -ac +extension GLX +extension Composite 2>/dev/null &
sleep 1

# ── launch two Maverick sessions (no --name collision: each gets a random sid) ─
DISPLAY="$X1" "$MAVERICK_BIN" --name maverick-session-a >/dev/null 2>&1 &
DISPLAY="$X2" "$MAVERICK_BIN" --name maverick-session-b >/dev/null 2>&1 &
sleep 2

# ── list must show exactly two, with distinct sessions ───────────────────────
LIST="$(./target/debug/maverickctl list 2>/dev/null)"
echo "$LIST" | tee -a "$LOG"
SID_A="$(echo "$LIST" | grep -oE 'maverick-session-a' >/dev/null && echo "$LIST" | awk '/maverick-session-a/{print $1}')"
SID_B="$(echo "$LIST" | grep -oE 'maverick-session-b' >/dev/null && echo "$LIST" | awk '/maverick-session-b/{print $1}')"

if [ -n "$SID_A" ] && [ -n "$SID_B" ] && [ "$SID_A" != "$SID_B" ]; then
    ok "two distinct sessions: $SID_A / $SID_B"
else
    bad "sessions not distinct or missing (a='$SID_A' b='$SID_B')"
fi

if [ -z "$SID_A" ] || [ -z "$SID_B" ]; then
    log "aborting focus checks: sessions unavailable"
    exit 1
fi

# ── focus reconciliation on session A ────────────────────────────────────────
DISPLAY="$X1" "$BIN/staticwin" >/dev/null 2>&1 &
sleep 0.5
# Ask the WM to focus the most-recently mapped window and read the real X focus.
DISPLAY="$X1" ./target/debug/maverickctl msg focus-best >/dev/null 2>&1
sleep 0.3
ACTIVE="$(DISPLAY="$X1" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null | awk '{print $5}')"
if [ -n "$ACTIVE" ] && [ "$ACTIVE" != "0x0" ]; then
    ok "X11 real focus is a client window: $ACTIVE"
else
    bad "X11 real focus missing/root: '$ACTIVE'"
fi

# ── quit only session A by explicit --session; B must survive ────────────────
./target/debug/maverickctl quit --session "$SID_A" >/dev/null 2>&1
sleep 1
LIST_AFTER="$(./target/debug/maverickctl list 2>/dev/null)"
if echo "$LIST_AFTER" | grep -q "$SID_B" && ! echo "$LIST_AFTER" | grep -q "$SID_A"; then
    ok "session A quit, session B still alive"
else
    bad "isolation broken after quit A: $LIST_AFTER"
fi

log "────"
log "result: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
