#!/usr/bin/env bash
#
# X connection loss — the record of the session must outlive the X server.
#
# SIGKILL the X server under a running window manager and assert that the
# process still leaves behind a *truthful* record: no identity ficha, no control
# socket, and a trace whose header says the X half of the shutdown was skipped.
#
# The trap this guards: every X call in the X half of the teardown is a void
# request that fails silently, so a shutdown that runs it anyway and ignores the
# error looks perfect from the outside. The only evidence is the header.
#
# Run: bash tests/xephyr-disconnect.sh

set -u

# `common.sh` defaults its binaries to ./target/debug; honour an explicit
# CARGO_TARGET_DIR (the same convention tests/session-suite.sh uses) and let the
# scratch area be redirected so a run leaves nothing in the source tree.
: "${CARGO_TARGET_DIR:=$PWD/target}"
export MAVERICK_BIN="${MAVERICK_BIN:-$CARGO_TARGET_DIR/debug/maverick}"
export MAVERICK_CTL="${MAVERICK_CTL:-$CARGO_TARGET_DIR/debug/maverickctl}"
SCRATCH="${MAV_TEST_SCRATCH:-/tmp/mav-xdis}"

source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
trap mav_cleanup EXIT ERR

PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

# One Xvfb per server, so `kill -9` names exactly one process that this script
# started. The display number is *probed* rather than assumed: another test
# running at the same time holds its own server, and pointing this one at a live
# display would make the kill land on someone else's server instead of the one
# under test.
XVFB_PID=""

pick_display() { # -> echoes a display nothing is serving, or nothing
    local n
    for n in $(seq 150 220); do
        [ -e "/tmp/.X$n-lock" ] && continue
        [ -e "/tmp/.X11-unix/X$n" ] && continue
        if DISPLAY=":$n" xdpyinfo >/dev/null 2>&1; then continue; fi
        echo ":$n"; return 0
    done
    return 1
}

start_server() { # -> sets XVFB_PID and exports DISPLAY; fails if none is free
    local disp
    if ! disp="$(pick_display)"; then
        bad "no free display for Xvfb (150-220 all in use)"
        return 1
    fi
    Xvfb "$disp" -screen 0 800x600x24 >"$SCRATCH/xvfb${disp#:}.log" 2>&1 &
    XVFB_PID=$!
    local i=0
    while [ $i -lt 60 ]; do
        DISPLAY="$disp" xdpyinfo >/dev/null 2>&1 && break
        alive "$XVFB_PID" || break
        sleep 0.1; i=$((i+1))
    done
    if ! DISPLAY="$disp" xdpyinfo >/dev/null 2>&1; then
        bad "Xvfb did not come up on $disp"
        stop_server
        return 1
    fi
    export DISPLAY="$disp"
    return 0
}

stop_server() {
    # Only ever the pid this script started, and only while it is still ours.
    if [ -n "$XVFB_PID" ] && alive "$XVFB_PID"; then kill -9 "$XVFB_PID" 2>/dev/null; fi
    XVFB_PID=""
}

# wait up to $2 tenths for the "X connection lost" line to appear in the log
saw_disconnect() {
    local log="$1" tenths="$2" i=0
    while [ $i -lt "$tenths" ]; do
        grep -q 'X11 connection lost' "$log" 2>/dev/null && return 0
        sleep 0.1; i=$((i+1))
    done
    return 1
}

# One sub-case: $1 label.
run_case() {
    local label="$1"
    local dir="$SCRATCH/$label"
    local log="$dir/wm.log" log2="$dir/wm-restart.log" trace="$dir/trace.tsv"
    local sid="xdis-$label"
    rm -rf "$dir"; mkdir -p "$dir"
    # A runtime dir of its own, so a leaked record cannot be mistaken for a
    # clean one.
    export XDG_RUNTIME_DIR="$dir/runtime"
    mkdir -p "$XDG_RUNTIME_DIR"
    local ficha="$XDG_RUNTIME_DIR/maverick/$sid/$sid.json"
    local sock="$XDG_RUNTIME_DIR/maverick/$sid/control.sock"

    start_server || return 0
    echo "=== $label ($DISPLAY) ==="

    # A mapped client so the window manager owns real X resources: without a
    # window the teardown has nothing to reach and the case would quietly stop
    # testing the thing it exists for.
    "$BIN_DIR/mgdwin" >/dev/null 2>&1 &
    local client=$!
    HELPER_PIDS+=("$client")
    sleep 0.3

    env MAVERICK_TRACE=1 MAVERICK_TRACE_PATH="$trace" \
        "$MAVERICK_BIN" --session-id "$sid" --config /dev/null >"$log" 2>&1 &
    local mav=$!
    MAV_PIDS+=("$mav")
    local i=0
    while [ $i -lt 100 ] && ! grep -q 'maverick ready' "$log" 2>/dev/null; do sleep 0.1; i=$((i+1)); done
    sleep 0.8

    # ── 1. preconditions: the record exists and the trace does not yet ─────────
    if [ -f "$ficha" ]; then ok "[$label] precondition: identity ficha exists"; else bad "[$label] precondition: NO identity ficha at $ficha — the case is not set up"; fi
    if [ -S "$sock" ]; then ok "[$label] precondition: control socket exists"; else bad "[$label] precondition: NO control socket at $sock — the case is not set up"; fi
    if [ -f "$trace" ]; then bad "[$label] precondition: the trace already exists before any shutdown"; else ok "[$label] precondition: no trace yet"; fi

    # ── 2. the X server dies ─────────────────────────────────────────────────
    kill -9 "$XVFB_PID"; XVFB_PID=""

    if mav_wait_exit "$mav" 5; then
        ok "[$label] maverick exited within 5s of losing the X server"
    else
        bad "[$label] maverick still alive 5s after the X server died"
        kill -9 "$mav" 2>/dev/null
    fi
    if saw_disconnect "$log" 10; then
        ok "[$label] the log reports the lost connection"
    else
        bad "[$label] the log never says the X connection was lost"
    fi

    # ── 3. the record of a dead session must be gone ─────────────────────────
    # This is the assertion the whole file exists for: without a local teardown
    # the ficha outlives the process and `maverickctl list` reports a phantom
    # STALE entry for a session that no longer exists.
    if [ -e "$ficha" ]; then bad "[$label] the identity ficha survived the disconnect ($ficha)"; else ok "[$label] the identity ficha is gone"; fi
    if [ -e "$sock" ]; then bad "[$label] the control socket survived the disconnect ($sock)"; else ok "[$label] the control socket is gone"; fi

    # ── 4. the trace must be written, and must say what happened ─────────────
    if [ -s "$trace" ]; then
        ok "[$label] the trace was written ($(wc -c <"$trace" | tr -d ' ') bytes)"
        local header
        header="$(head -1 "$trace")"
        case "$header" in
            *"end=x_connection_lost x_teardown=skipped"*)
                ok "[$label] the trace header reads 'end=x_connection_lost x_teardown=skipped'" ;;
            *)
                bad "[$label] trace header does not report the skipped X teardown: $header" ;;
        esac
        # Proof the case is not vacuous: the ring buffer really was recording
        # before the server died, so its header is evidence and not an artifact
        # of an empty file.
        if grep -q 'turn_begin' "$trace"; then
            ok "[$label] the trace recorded turns before the loss"
        else
            bad "[$label] the trace has no records — the case proved nothing"
        fi
    else
        bad "[$label] the trace was NOT written — the whole ring buffer is lost"
    fi

    # ── 5. the cause: no X/GLX request may be issued after the loss ──────────
    # libX11's I/O error handler prints this and calls exit(1): no unwinding, no
    # Drop, no local teardown. One occurrence is a note; one *after* the
    # disconnect is the shutdown having reached back into a dead server.
    local after
    after="$(awk '/X11 connection lost/{seen=1} seen' "$log" | grep -c 'broken (explicit kill or server shutdown)' || true)"
    if [ "${after:-0}" -eq 0 ]; then
        ok "[$label] no 'X connection broken' after the disconnect (no request reached the dead server)"
    else
        bad "[$label] $after 'X connection broken' line(s) after the disconnect — the shutdown touched a dead display"
    fi
    if grep -q 'X teardown skipped' "$log"; then
        ok "[$label] the log says the X teardown was skipped"
    else
        bad "[$label] the log never says the X teardown was skipped"
    fi
    kill -9 "$client" 2>/dev/null

    # ── 6. the same session id starts again on a healthy server ──────────────
    # A leftover ficha does not block this (the record is truncated and the
    # stale socket unlinked), so what is being asserted is that the restarted
    # instance is reported *alive* — i.e. nothing above was load-bearing for a
    # restart, and nothing above is recoverable by hand afterwards either.
    # A second log file: waiting for "maverick ready" in the first one would
    # match the line the *dead* instance already wrote.
    start_server || return 0
    env MAVERICK_TRACE=1 MAVERICK_TRACE_PATH="$trace" \
        "$MAVERICK_BIN" --session-id "$sid" --config /dev/null >"$log2" 2>&1 &
    local mav2=$!
    MAV_PIDS+=("$mav2")
    i=0
    while [ $i -lt 100 ] && ! grep -q 'maverick ready' "$log2" 2>/dev/null; do
        alive "$mav2" || break
        sleep 0.1; i=$((i+1))
    done
    i=0
    while [ $i -lt 60 ]; do
        "$MAVERICK_CTL" list 2>/dev/null | grep -q "^  $sid " && break
        sleep 0.1; i=$((i+1))
    done
    local line
    line="$("$MAVERICK_CTL" list 2>/dev/null | grep "^  $sid " || true)"
    case "$line" in
        *alive*) ok "[$label] a fresh instance with the same session id is listed alive" ;;
        *)      bad "[$label] the restarted instance is not listed alive: ${line:-<no line>}" ;;
    esac

    # ── 7. control: a clean quit is still a full teardown ────────────────────
    # The same code path with a live server. If this regresses, the fix is not
    # a disconnect fix but a teardown that stopped working.
    "$MAVERICK_CTL" quit >/dev/null 2>&1
    mav_wait_exit "$mav2" 8 || kill -9 "$mav2" 2>/dev/null
    if [ -e "$ficha" ]; then bad "[$label] control: 'quit' left the identity ficha behind"; else ok "[$label] control: 'quit' removes the identity ficha"; fi
    if [ -e "$sock" ]; then bad "[$label] control: 'quit' left the control socket behind"; else ok "[$label] control: 'quit' removes the control socket"; fi
    if [ -s "$trace" ]; then
        header="$(head -1 "$trace")"
        case "$header" in
            *"end=clean_exit x_teardown=full"*)
                ok "[$label] control: the trace header reads 'end=clean_exit x_teardown=full'" ;;
            *)
                bad "[$label] control: trace header is not the clean-exit one: $header" ;;
        esac
    else
        bad "[$label] control: 'quit' wrote no trace"
    fi
    stop_server
    echo
}

mkdir -p "$SCRATCH"
run_case "xdisconnect"

echo "────────────────────────────────────"
echo "X disconnect: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
