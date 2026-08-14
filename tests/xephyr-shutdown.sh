#!/usr/bin/env bash
#
# F3 — graceful shutdown regardless of client cooperation (CRITICAL).
#
#   A) cooperative client       (mgdwin default, honours WM_DELETE_WINDOW)
#   B) non-cooperative client   (mgdwin nowm, no WM_DELETE_WINDOW -> force-kill)
#   C) lying client             (mgdwin lie, advertises but IGNORES delete ->
#                               must exhaust the 3s SHUTDOWN_BUDGET then force-kill)
#   D) kill during normal op     (SIGKILL a client while WM is live; WM must
#                               survive and keep answering `maverickctl state`)
#
# For each: `maverickctl quit` must return within SHUTDOWN_BUDGET(3s)+2s slack,
# and NO maverick process may remain afterwards.
#
# Run: bash tests/xephyr-shutdown.sh

set -u
source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
trap mav_cleanup EXIT ERR

SHUTDOWN_BUDGET=3
SLACK=2
MAXWAIT=$((SHUTDOWN_BUDGET + SLACK))   # 5s hard cap on the whole quit

PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

# run one shutdown scenario; args: mode label
run_shutdown() {
    local mode="$1" label="$2"
    local disp=":98"
    local xep
    xep="$(start_xephyr "$disp" 1280 720)"
    export DISPLAY="$disp"
    local mav
    mav="$(mav_launch "$disp")"

    # one managed client in the requested cooperation mode
    "$BIN_DIR/mgdwin" $mode >/dev/null 2>&1 &
    local cpid=$!
    HELPER_PIDS+=("$cpid")
    # wait until maverick actually MANAGES the client (otherwise clients.is_empty()
    # makes quit instant and the cooperation path is not exercised)
    local i=0
    while [ $i -lt 50 ] && [ "$(win_count "$disp")" -lt 1 ]; do sleep 0.1; i=$((i+1)); done
    local wc
    wc="$(win_count "$disp")"
    if [ "$wc" -ge 1 ]; then
        ok "[$label] client is managed ($wc window(s)) before quit"
    else
        bad "[$label] client NOT managed before quit (win_count=$wc) — cooperation path untested"
    fi

    local start_ts
    start_ts="$(date +%s%N)"
    DISPLAY="$disp" "$MAVERICK_CTL" quit >/dev/null 2>&1

    local now elapsed
    now="$(date +%s%N)"
    while alive "$mav" && [ $(((now - start_ts) / 1000000000)) -lt $MAXWAIT ]; do
        sleep 0.1
        now="$(date +%s%N)"
    done

    if alive "$mav"; then
        bad "[$label] maverick HUNG past ${MAXWAIT}s (process still alive)"
        kill -9 "$mav" 2>/dev/null
    else
        elapsed=$(((now - start_ts) / 1000000000))
        if [ "$elapsed" -le "$MAXWAIT" ]; then
            ok "[$label] exited in ${elapsed}s (<=${MAXWAIT}s), no hang"
        else
            bad "[$label] exited but took ${elapsed}s (>${MAXWAIT}s)"
        fi
    fi

    # ensure nothing lingers
    alive "$mav" && { bad "[$label] maverick still running post-check"; kill -9 "$mav" 2>/dev/null; }
    return 0
}

echo "=== A) cooperative client ==="
run_shutdown "" "cooperative"

echo "=== B) non-cooperative (no WM_DELETE_WINDOW) ==="
run_shutdown "nowm" "non-coop-nowm"

echo "=== C) lying client (ignores WM_DELETE_WINDOW) ==="
run_shutdown "lie" "non-coop-lie"

# ── D) kill a client with SIGKILL during normal operation ─────────────────────
echo "=== D) SIGKILL a client mid-operation ==="
disp=":99"
xep="$(start_xephyr "$disp" 1280 720)"
export DISPLAY="$disp"
mav="$(mav_launch "$disp")"
"$BIN_DIR/mgdwin" >/dev/null 2>&1 & CPID1=$!; HELPER_PIDS+=("$CPID1")
"$BIN_DIR/mgdwin" >/dev/null 2>&1 & CPID2=$!; HELPER_PIDS+=("$CPID2")
sleep 1.5
kill -9 "$CPID1" 2>/dev/null
sleep 1.5
if alive "$mav"; then
    ok "maverick alive after SIGKILL of a client"
else
    bad "maverick DIED when a client was SIGKILLed"
fi
if DISPLAY="$disp" "$MAVERICK_CTL" state >/dev/null 2>&1; then
    ok "maverickctl state still responds after client death"
else
    bad "maverickctl state FAILED after client death"
fi
alive "$mav" && kill "$mav" 2>/dev/null

echo "────────────────────────────────────"
echo "F3 graceful-shutdown: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
