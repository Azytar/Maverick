#!/usr/bin/env bash
#
# Fase-6 IPC edge cases (remaining 15-scenario items):
#   * Long IPC path (sid): start with a very long --name; state/quit still work.
#   * Socket inexistente:  `maverickctl --session does-not-exist quit` errors cleanly.
#   * Socket stale/muerto:  after `kill -9` maverick, `maverickctl quit` fails
#                          gracefully (no hang, no panic).
#   * Multiple displays:    two Xephyr servers, two maverick instances, both
#                          manageable and quittable.
#   * Restart repetido (N): loop `restart` and assert stability.
#   * Quit repetido:        launch/quit repeatedly and assert clean exit.
#
# Run: bash tests/xephyr-ipc-edge.sh

set -u
source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
trap mav_cleanup EXIT ERR

PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

# ── 1) long IPC path (sid) ─────────────────────────────────────────────────────
echo "=== long IPC path (sid) ==="
LONGNAME="$(printf 'a%.0s' {1..200})"
X1="$(start_xephyr ":98" 1280 720)"
M1="$(mav_launch ":98" --name "$LONGNAME")"
if DISPLAY=":98" "$MAVERICK_CTL" state >/dev/null 2>&1; then
    ok "maverick started with 200-char --name; state works"
else
    bad "maverick FAILED to serve state with long --name (possible SUN_LEN bug)"
fi
OUT="$(DISPLAY=":98" "$MAVERICK_CTL" quit 2>&1)"; RC=$?
sleep 1
alive "$M1" && { bad "maverick still alive after quit (long name)"; kill -9 "$M1" 2>/dev/null; } \
             || ok "maverick quit cleanly with long --name (rc=$RC)"

# ── 2) socket inexistente ──────────────────────────────────────────────────────
echo "=== socket inexistente ==="
OUT="$(timeout 5 "$MAVERICK_CTL" --session does-not-exist quit 2>&1)"; RC=$?
if [ "$RC" -ne 0 ]; then ok "quit on missing session exits non-zero (rc=$RC), no panic";
else bad "quit on missing session returned 0 (should error)"; fi
echo "$OUT" | grep -qi "panic" && bad "PANIC on missing session" || ok "no panic on missing session"

# ── 3) socket stale / proceso muerto ───────────────────────────────────────────
echo "=== socket stale (maverick SIGKILLed) ==="
X2="$(start_xephyr ":97" 1280 720)"
M2="$(mav_launch ":97")"
kill -9 "$M2" 2>/dev/null
sleep 0.5
OUT="$(timeout 5 env DISPLAY=":97" "$MAVERICK_CTL" quit 2>&1)"; RC=$?
alive "$M2" && bad "stale instance still alive (unexpected)" || ok "stale instance already dead"
echo "$OUT" | grep -qi "panic" && bad "PANIC on stale-socket quit" || ok "no panic on stale-socket quit"
echo "NOTE stale-socket quit rc=$RC (graceful failure expected, rc!=0 ideal)"

# ── 4) multiple displays ────────────────────────────────────────────────────────
echo "=== multiple displays (:96 and :95) ==="
XD1="$(start_xephyr ":96" 1280 720)"
XD2="$(start_xephyr ":95" 1280 720)"
MD1="$(mav_launch ":96")"
MD2="$(mav_launch ":95")"
if DISPLAY=":96" "$MAVERICK_CTL" state >/dev/null 2>&1 && DISPLAY=":95" "$MAVERICK_CTL" state >/dev/null 2>&1; then
    ok "both instances answer state independently"
else
    bad "one/both instances failed to answer state"
fi
LIST_N="$( "$MAVERICK_CTL" list 2>/dev/null | grep -cE 'maverick|session' )"
DISPLAY=":96" "$MAVERICK_CTL" quit >/dev/null 2>&1; sleep 0.5
DISPLAY=":95" "$MAVERICK_CTL" quit >/dev/null 2>&1; sleep 0.5
alive "$MD1" && { bad ":96 instance alive after quit"; kill -9 "$MD1" 2>/dev/null; } || ok ":96 instance quit"
alive "$MD2" && { bad ":95 instance alive after quit"; kill -9 "$MD2" 2>/dev/null; } || ok ":95 instance quit"

# ── 5) restart repetido (N veces) ──────────────────────────────────────────────
echo "=== restart repetido (5x) ==="
X3="$(start_xephyr ":94" 1280 720)"
M3="$(mav_launch ":94")"
launch_pid="$M3"
for i in $(seq 1 5); do
    DISPLAY=":94" "$MAVERICK_CTL" restart >/dev/null 2>&1
    sleep 2
    if alive "$launch_pid" && DISPLAY=":94" "$MAVERICK_CTL" state >/dev/null 2>&1; then
        ok "restart #$i: alive + state ok"
    else
        bad "restart #$i: broken (alive=$(alive "$launch_pid" && echo y||echo n))"
        break
    fi
done
alive "$launch_pid" && kill "$launch_pid" 2>/dev/null

# ── 6) quit repetido ───────────────────────────────────────────────────────────
echo "=== quit repetido (3x) ==="
for i in 1 2 3; do
    Xq="$(start_xephyr ":93" 1280 720)"
    Mq="$(mav_launch ":93")"
    DISPLAY=":93" "$MAVERICK_CTL" quit >/dev/null 2>&1
    sleep 0.5
    if alive "$Mq"; then bad "quit #$i left maverick running"; kill -9 "$Mq" 2>/dev/null;
    else ok "quit #$i: clean exit"; fi
done

echo "────────────────────────────────────"
echo "IPC-edge: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
