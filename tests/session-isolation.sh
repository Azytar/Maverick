#!/usr/bin/env bash
#
# Maverick session-isolation + focus-reconciliation harness (Fases A–D, F).
#
# Validates the two root-cause fixes from the audit:
#   * Two Maverick sessions on different DISPLAYs get *distinct* session ids and
#     never share a socket/ficha (C1/C2/C3). `maverickctl --session <sid> quit`
#     kills only that session; the other survives.
#   * Focus does not silently desync: a managed client mapped on session A is
#     focused by the WM, and the X server's published active window
#     (`xprop -root _NET_ACTIVE_WINDOW`) names that same window (H1/H2).
#
# This is a *manual / CI* harness: it needs two nested X servers (Xephyr) with
# GLX and cannot run under `cargo test`. No results are fabricated: every
# assertion reads live state (process table, `maverickctl list`,
# `maverickctl query tree`, xprop).
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
# `mgdwin` is a normal managed client (used for focus/layout/kill coverage);
# `staticwin` is intentionally NOT used here — it is override-redirect, so the
# WM never manages or focuses it and it can never become the active window.
gcc -O2 tests/mgdwin.c -o "$BIN/mgdwin" -lX11 2>/dev/null

LOG="$(mktemp -t maverick-session.XXXXXX.log)"
PASS=0; FAIL=0
log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); }

cleanup() {
    [ -n "${MGDWIN_PID:-}" ] && kill "$MGDWIN_PID" 2>/dev/null
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

# ── focus of a managed window on session A ───────────────────────────────────
# `mgdwin` is a normal (non-override-redirect) client: Maverick manages it on
# map and focuses it (`manage` → `focus_best` → `focus`), publishing
# `_NET_ACTIVE_WINDOW` for the focused window. That manage→focus contract is
# what this block checks — no `msg` focus verb is needed, and the removed
# `focus-best` verb never existed in the action grammar (it dispatched to a
# "unknown dispatch action" warning and was a silent no-op).
MGDWIN_LOG="$BIN/mgdwin-winid.log"
DISPLAY="$X1" "$BIN/mgdwin" >/dev/null 2>"$MGDWIN_LOG" &
MGDWIN_PID=$!
# Wait until the WM actually manages the client (bounded poll, not a blind
# sleep, so a slow map cannot masquerade as a focus failure).
i=0
while [ "$i" -lt 50 ] && ! DISPLAY="$X1" ./target/debug/maverickctl query tree 2>/dev/null | grep -q '"instance":"mgdwin"'; do sleep 0.1; i=$((i+1)); done
if DISPLAY="$X1" ./target/debug/maverickctl query tree 2>/dev/null | grep -q '"instance":"mgdwin"'; then
    ok "session A manages the mgdwin client"
else
    bad "session A never managed the mgdwin client — focus path untested"
fi
WINID="$(grep -oE 'WINID=0x[0-9a-fA-F]+' "$MGDWIN_LOG" 2>/dev/null | head -1 | cut -d= -f2 | tr 'A-Z' 'a-z' || true)"
ACTIVE="$(DISPLAY="$X1" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null | awk '{print $5}' | tr -d ',' | tr 'A-Z' 'a-z')"
if [ -n "$WINID" ] && [ "$ACTIVE" = "$WINID" ]; then
    ok "managed window focused: _NET_ACTIVE_WINDOW=$ACTIVE == $WINID"
else
    bad "focus mismatch: WINID='$WINID' _NET_ACTIVE_WINDOW='$ACTIVE'"
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
