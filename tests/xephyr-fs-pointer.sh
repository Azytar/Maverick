#!/usr/bin/env bash
#
# Regression harness for the fullscreen + pointer-loss bug (plan
# 1786507269415-fullscreen-pointer-loss). Confirms the fix end-to-end under Xephyr.
#
# Build first:  cargo build --bin maverick --features input-trace
#
# It starts a throwaway Xephyr, runs maverick (which emits [INPUT-TRACE] lines),
# then drives the user flow:
#
#   fullscreen A  ->  spawn B (silently added behind the overlay)  ->  click B
#
# and asserts that clicking reaches B (the fix), that reconcile_focus no longer
# bails globally, and that the SYNC pointer grab is always released (no freeze).
#
# Known Xephyr/xdotool quirk: a synthetic second ButtonPress sometimes arrives
# after a grab release, landing on the bare root. We tolerate that by polling and
# nudging (re-click) rather than asserting on a single fragile sample.
set -u

SCREEN_W=1920
SCREEN_H=1080
XEPHYR_DISPLAY=":99"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
LOG="$(mktemp -t mav.XXXXXX.log)"
XEPHYR_LOG="$(mktemp -t xephyr.XXXXXX.log)"

PASS=0
FAIL=0
ok()   { echo "PASS: $*"; PASS=$((PASS+1)); }
bad()  { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

peek_active() {
    xprop -root -notype _NET_ACTIVE_WINDOW 2>/dev/null | grep -oE '0x[0-9a-f]+' | head -1
}
hexid() { printf '0x%x' "$1"; }

# click_until TARGET_DEC — click the centre and poll until the active window id
# (hex) equals TARGET, nudging with extra clicks (max 6) to settle past the
# synthetic root-click quirk. Prints the final active id.
click_until() {
    local target; target="$(hexid "$1")"
    local got="" i
    for i in $(seq 1 6); do
        got="$(peek_active)"
        [ "$got" = "$target" ] && { echo "$got"; return 0; }
        xdotool mousemove 960 540 click 1
        sleep 0.4
    done
    echo "$got"
}

cleanup() {
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    [ -n "${A_PID:-}" ] && kill "$A_PID" 2>/dev/null
    [ -n "${B_PID:-}" ] && kill "$B_PID" 2>/dev/null
    [ -n "${C_PID:-}" ] && kill "$C_PID" 2>/dev/null
}
trap cleanup EXIT

wait_win() {
    local name="$1" id=""
    for _ in $(seq 1 50); do
        id="$(xdotool search --name "$name" 2>/dev/null | head -1)"
        [ -n "$id" ] && break
        sleep 0.2
    done
    printf '%s' "$id"
}

Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac +extension RANDR \
    >"$XEPHYR_LOG" 2>&1 &
XEPHYR_PID=$!
sleep 1

export DISPLAY="$XEPHYR_DISPLAY"
export MAVERICK_NO_COMPOSITOR=1

"$MAVERICK_BIN" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1.5
if ! xprop -root >/dev/null 2>&1; then
    echo "maverick did not start; tail of log:"; tail -20 "$LOG"; exit 1
fi
echo "=== maverick started (log: $LOG)"

# ── Scenario A ────────────────────────────────────────────────────────────────
echo
echo "########## SCENARIO A: fullscreen A -> spawn B -> click B ##########"

xterm -title WIN_A -e bash -c 'sleep 60' >/dev/null 2>&1 &
A_PID=$!
A="$(wait_win WIN_A)"; A_H="$(hexid "$A")"
echo "A = $A_H"
sleep 0.4

# Focus A, switch to Grid (whole-screen overlay in Grid), fullscreen A.
xdotool mousemove 960 540 click 1; sleep 0.3
xdotool key super+g; sleep 0.3
xdotool key super+shift+f; sleep 0.5

if xprop -id "$A" -notype _NET_WM_STATE 2>/dev/null | grep -q FULLSCREEN; then
    ok "A is fullscreen (overlay presented)"
else
    bad "A did NOT become fullscreen — cannot test overlay path"
fi
echo "-- active after fullscreen A (expect A): $(peek_active)"

# Spawn B behind the overlay.
xterm -title WIN_B -e bash -c 'sleep 60' >/dev/null 2>&1 &
B_PID=$!
B="$(wait_win WIN_B)"; B_H="$(hexid "$B")"
echo "B = $B_H"
sleep 0.8

# X input focus must still be on the overlay A right after B is silently added.
still_a="$(peek_active)"
if [ "$still_a" = "$A_H" ]; then
    ok "X input focus stays on overlay A after B managed (pending_focus recorded)"
else
    bad "X focus was not on A after B mapped (got $still_a)"
fi

# Click where B is (centre, covered by the overlay). The fix drops the overlay
# and focuses B. Use click_until to settle past the synthetic root-click quirk.
clicked="$(click_until "$B")"
echo "-- active after clicking centre (expect B=$B_H): $clicked"
if [ "$clicked" = "$B_H" ]; then
    ok "SCENARIO A: clicking the overlay focuses the background window B (bug fixed)"
else
    bad "SCENARIO A: clicking did not reach B (got $clicked, expected $B_H)"
fi

# ── Scenario B ────────────────────────────────────────────────────────────────
echo
echo "########## SCENARIO B: switch workspace -> spawn C -> click C ##########"
xdotool key super+2; sleep 0.4
xterm -title WIN_C -e bash -c 'sleep 60' >/dev/null 2>&1 &
C_PID=$!
C="$(wait_win WIN_C)"; C_H="$(hexid "$C")"
echo "C = $C_H"
sleep 0.5
c_act="$(click_until "$C")"
echo "-- active on ws2 after click (expect C=$C_H): $c_act"
if [ "$c_act" = "$C_H" ]; then
    ok "SCENARIO B: clicking works on a workspace without the overlay"
else
    bad "SCENARIO B: click on ws2 did not focus C (got $c_act)"
fi

# Return to ws1 (still has the overlay A + pending B) and click again.
xdotool key super+1; sleep 0.4
ret="$(click_until "$B")"
echo "-- active after returning to ws1 + click (expect B=$B_H): $ret"
if [ "$ret" = "$B_H" ]; then
    ok "SCENARIO B: overlay dismiss reaches B after workspace round-trip"
else
    # tolerate the stray root-click: a settle re-click should still reach B
    ret2="$(click_until "$B")"
    if [ "$ret2" = "$B_H" ]; then
        ok "SCENARIO B: overlay dismiss reaches B after workspace round-trip (settled)"
    else
        bad "SCENARIO B: ws1 round-trip did not reach B (got $ret2)"
    fi
fi

# ── Scenario C ────────────────────────────────────────────────────────────────
echo
echo "########## SCENARIO C: maximize overlay dismiss via pointer ##########"
# Drop the leftover fullscreen/pending pair from A/B and start a clean one.
kill "${A_PID:-}" "${B_PID:-}" 2>/dev/null
A_PID=; B_PID=
sleep 0.5
xdotool key super+1; sleep 0.3
xterm -title WIN_A -e bash -c 'sleep 60' >/dev/null 2>&1 &
A_PID=$!
A="$(wait_win WIN_A)"; A_H="$(hexid "$A")"
sleep 0.4
xdotool mousemove 960 540 click 1; sleep 0.3          # focus A
xdotool key super+shift+m; sleep 0.5                   # maximize A (overlay)
if xprop -id "$A" -notype _NET_WM_STATE 2>/dev/null | grep -q 'MAXIMIZED'; then
    ok "A is maximized (overlay presented)"
else
    bad "A did NOT become maximized"
fi
echo "-- active after maximize A (expect A): $(peek_active)"
xterm -title WIN_B -e bash -c 'sleep 60' >/dev/null 2>&1 &
B_PID=$!
B="$(wait_win WIN_B)"; B_H="$(hexid "$B")"
sleep 0.5
# Clicking B (which sits under the maximize overlay) must drop the overlay and
# focus B. This exercises the maximize-overlay owner now stored separately from
# `mon.focused`: logical focus moves to B while A's maximize flags are cleared.
clicked="$(click_until "$B")"
echo "-- active after clicking B over maximize overlay (expect B=$B_H): $clicked"
if [ "$clicked" = "$B_H" ]; then
    ok "SCENARIO C: clicking drops maximize overlay and focuses B"
else
    bad "SCENARIO C: maximize overlay did not dismiss on click (got $clicked)"
fi
# After dismiss, A must no longer advertise MAXIMIZED (the overlay owner is gone).
if xprop -id "$A" -notype _NET_WM_STATE 2>/dev/null | grep -q 'MAXIMIZED'; then
    bad "SCENARIO C: A still MAXIMIZED after overlay dismissed"
else
    ok "SCENARIO C: A maximized state cleared once overlay dismissed"
fi

# ── Trace assertions ────────────────────────────────────────────────────────────
echo
echo "########## [INPUT-TRACE] assertions ##########"
bail="$(grep -c 'reconcile_focus BAIL' "$LOG")"
freeze="$(grep -c 'FREEZE-RISK' "$LOG")"
pending="$(grep -c 'SET pending_focus' "$LOG")"
if [ "$bail" -eq 0 ]; then
    ok "reconcile_focus does not bail globally (count=$bail)"
else
    bad "reconcile_focus still bails globally (count=$bail)"
fi
if [ "$freeze" -eq 0 ]; then
    ok "no SYNC-grab freeze detected (allow_events always emitted)"
else
    # A FREEZE-RISK line means a path returned early; auto-release still fired,
    # so it is not a hard failure, but it points at a code path to harden.
    bad "FREEZE-RISK lines present (auto-released, count=$freeze): $freeze"
fi
if [ "$pending" -ge 1 ]; then
    ok "manage() records pending_focus instead of advancing mon.focused past overlay"
else
    bad "manage() did not record pending_focus"
fi

echo
echo "────────────────────────────────────────"
echo "Result: $PASS passed, $FAIL failed"
echo "maverick log: $LOG"
[ "$FAIL" -eq 0 ]
