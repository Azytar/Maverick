#!/usr/bin/env bash
#
# Shared harness for Maverick Xephyr end-to-end tests.
#
# Provides:
#   build_helpers        compile tests/mgdwin (+ pxsample/staticwin/winmove if absent)
#   start_xephyr DISP W H   start a nested X server, return its pid
#   mav_launch DISP ARGS..  start `maverick ARGS` on DISP, return its pid
#   mav_track PID         register a child, including inside command substitution
#   mav_cleanup           quit owned WMs, stop registered children, remove runtime
#
# Every script MUST `source` this file and `trap mav_cleanup EXIT ERR` so that
# Xephyr / maverick / helper processes never leak between scenarios.

set -u

APP_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$APP_DIR/tests"
cd "$APP_DIR"

MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
MAVERICK_CTL="${MAVERICK_CTL:-./target/debug/maverickctl}"
MAVERICK_MSG="${MAVERICK_MSG:-./target/debug/maverickctl}"
MAV_TEST_HOST_DISPLAY="${DISPLAY:-}"

# Short, explicit XDG_RUNTIME_DIR so the control-socket path stays under
# SUN_LEN (the default can be too long in some sandboxes).
source "$(dirname "${BASH_SOURCE[0]}")/private-env.sh"
mkdir -p "$MAV_TEST_RUNTIME/pids"

# Read the kernel start time and parent without splitting a comm containing
# spaces or parentheses. Zombies have already exited and need no more signals.
mav_process() {
    local pid="$1" stat
    local -a fields
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] && [ -O "/proc/$pid" ] || return 1
    { IFS= read -r stat <"/proc/$pid/stat"; } 2>/dev/null || return 1
    read -r -a fields <<<"${stat##*) }"
    [ "${#fields[@]}" -ge 20 ] && [ "${fields[0]}" != Z ] || return 1
    printf '%s %s\n' "${fields[19]}" "${fields[1]}"
}

mav_track() {
    local pid="$1" identity start parent
    identity="$(mav_process "$pid")" || return 1
    read -r start parent <<<"$identity"
    [ "$parent" = "$BASHPID" ] || return 1
    # Files survive $(...) subshells; Bash arrays alone lose those children.
    printf '%s\n' "$start" "${2:-child}" "${3:-}" "${4:-${DISPLAY:-}}" "$XDG_RUNTIME_DIR" \
        >"$MAV_TEST_RUNTIME/pids/$pid"
}

mav_owned() {
    local pid="$1" identity start
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] && [ -f "$MAV_TEST_RUNTIME/pids/$pid" ] || return 1
    IFS= read -r start <"$MAV_TEST_RUNTIME/pids/$pid" || return 1
    identity="$(mav_process "$pid")" || return 1
    [ "${identity%% *}" = "$start" ]
}

mav_stop() {
    local pid="$1" i
    mav_owned "$pid" || return 0
    kill -TERM "$pid" 2>/dev/null || true
    for i in {1..10}; do
        mav_owned "$pid" || break
        sleep 0.05
    done
    if mav_owned "$pid"; then kill -KILL "$pid" 2>/dev/null || true; fi
    wait "$pid" 2>/dev/null || true
}

mav_cleanup() {
    local record pid lock lock_pid
    local -a info
    [ -d "$MAV_TEST_RUNTIME/pids" ] || return 0
    # Ask each owned WM first; its clients may close cooperatively. A hung WM
    # still has a bounded, identity-checked PID fallback.
    for record in "$MAV_TEST_RUNTIME"/pids/*; do
        [ -f "$record" ] || continue
        pid="${record##*/}"
        mav_owned "$pid" || continue
        mapfile -t info <"$record"
        if [ "${info[1]}" = wm ] && [ -n "${info[2]}" ]; then
            env -u MAVERICK_INSTANCE -u MAVERICK_SESSION DISPLAY="${info[3]}" \
                XDG_RUNTIME_DIR="${info[4]}" timeout 6 "$MAVERICK_CTL" \
                quit --session "${info[2]}" --yes >/dev/null 2>&1 || true
            mav_wait_exit "$pid" 4 || mav_stop "$pid"
        fi
    done
    for record in "$MAV_TEST_RUNTIME"/pids/*; do
        [ -f "$record" ] || continue
        mav_stop "${record##*/}"
        mapfile -t info <"$record"
        if [ "${info[1]}" = server ] && ! alive "${record##*/}"; then
            lock="/tmp/.X${info[3]#:}-lock"
            # A failed Xephyr can leave its own lock before opening the host
            # display. Never remove a lock naming any other server.
            if { read -r lock_pid <"$lock"; } 2>/dev/null \
                && [ "$lock_pid" = "${record##*/}" ]; then
                rm -f -- "$lock"
            fi
        fi
    done
    rm -rf -- "$MAV_TEST_RUNTIME"
    return 0
}

# ── build the managed-window helper + any missing static helpers ──────────────
build_helpers() {
    [ -x "$BIN_DIR/mgdwin" ] || cc -O2 -o "$BIN_DIR/mgdwin" "$BIN_DIR/mgdwin.c" -lX11 2>/dev/null \
        || { echo "FAIL: could not build tests/mgdwin"; exit 1; }
    for h in staticwin pxsample winmove; do
        [ -x "$BIN_DIR/$h" ] || cc -O2 -o "$BIN_DIR/$h" "$BIN_DIR/$h.c" -lX11 -lXcomposite 2>/dev/null
    done
}

# ── start a nested X server; echo its pid ─────────────────────────────────────
start_xephyr() {
    local disp="$1" w="${2:-1280}" h="${3:-720}"
    local number="${disp#:}"
    [[ "$number" =~ ^[0-9]+$ ]] || return 1
    mav_require_free_display "$disp" || return 1
    DISPLAY="$MAV_TEST_HOST_DISPLAY" Xephyr "$disp" -screen "${w}x${h}" -ac \
        +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
        >"/tmp/xephyr${disp}.log" 2>&1 &
    local realpid=$!
    mav_track "$realpid" server "" "$disp" || return 1
    local i=0
    while [ $i -lt 60 ]; do
        mav_owned "$realpid" || break
        if mav_display_ready "$realpid" "$disp"; then
            printf '%s\n' "$realpid" >"$MAV_TEST_RUNTIME/server-$number"
            echo "$realpid"
            return 0
        fi
        sleep 0.1
        i=$((i+1))
    done
    echo "FAIL: our Xephyr did not become ready on $disp" >&2
    return 1
}

# ── start maverick on DISP with ARGS; echo its pid ───────────────────────────
mav_launch() {
    local disp="$1"; shift
    local server sid
    { IFS= read -r server <"$MAV_TEST_RUNTIME/server-${disp#:}"; } 2>/dev/null || return 1
    mav_owned "$server" || return 1
    sid="$(mktemp -d "$MAV_TEST_RUNTIME/test-XXXXXX")" || return 1
    sid="${sid##*/}"
    DISPLAY="$disp" "$MAVERICK_BIN" --session-id "$sid" "$@" >"/tmp/mav${disp}.log" 2>&1 &
    local pid=$!
    mav_track "$pid" wm "$sid" "$disp" || return 1
    local i=0
    while [ $i -lt 80 ]; do
        # Query the WM thread, rather than waiting on the hub's startup cache.
        mav_owned "$pid" || break
        if DISPLAY="$disp" "$MAVERICK_CTL" query tree --session "$sid" 2>/dev/null | grep -q '"monitors"'; then
            echo "$pid"
            return 0
        fi
        sleep 0.1
        i=$((i+1))
    done
    echo "FAIL: our Maverick did not become ready on $disp" >&2
    return 1
}

# is a process (by pid) still alive?
alive() { mav_process "$1" >/dev/null; }

# count workspaces reported by `maverickctl state` on DISP
ws_count() {
    DISPLAY="$1" "$MAVERICK_CTL" state 2>/dev/null \
        | python3 -c "import sys,json;d=json.load(sys.stdin);print(len(d['monitors'][0]['workspaces']))" 2>/dev/null
}

# count MANAGED clients reported by `maverickctl query tree` on DISP
# (every managed window emits an "instance":"…" field in the per-window JSON).
win_count() {
    DISPLAY="$1" "$MAVERICK_CTL" query tree 2>/dev/null | grep -c '"instance":"'
}

# count fullscreen windows reported by `maverickctl query tree` on DISP
fs_count() {
    DISPLAY="$1" "$MAVERICK_CTL" query tree 2>/dev/null | grep -c '"fullscreen":true'
}

# wait up to $2 seconds for pid $1 to exit (0 = exited, 1 = still alive)
mav_wait_exit() {
    local pid="$1" max=$(( ${2:-10} * 5 )) i=0
    while alive "$pid"; do
        [ $i -ge "$max" ] && return 1
        sleep 0.2; i=$((i+1))
    done
    return 0
}

# SIGKILL pid $1 and wait up to $2 seconds for it to be gone
mav_kill_wait() {
    mav_owned "$1" || return 1
    kill -9 "$1" 2>/dev/null
    mav_wait_exit "$1" "${2:-10}"
}
