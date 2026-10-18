#!/usr/bin/env bash
# A test's runtime, configuration and display ownership are independent of the
# desktop from which it was launched. This file is sourced by shell harnesses.
MAV_TEST_RUNTIME="$(mktemp -d /tmp/mrt.XXXXXX)" || exit 1
export XDG_RUNTIME_DIR="$MAV_TEST_RUNTIME"
export XDG_CONFIG_HOME="$MAV_TEST_RUNTIME/config"
export XDG_STATE_HOME="$MAV_TEST_RUNTIME/state"
export XDG_CACHE_HOME="$MAV_TEST_RUNTIME/cache"
unset MAVERICK_INSTANCE MAVERICK_SESSION MAVERICK_RESTART_PID
mkdir -p "$XDG_CONFIG_HOME/maverick"
printf '[autostart]\ncommands = []\n' >"$XDG_CONFIG_HOME/maverick/config.toml"

mav_env_cleanup() { rm -rf -- "$MAV_TEST_RUNTIME"; }
trap mav_env_cleanup EXIT

mav_require_free_display() {
    local disp="$1" number="${1#:}"
    [[ "$number" =~ ^[0-9]+$ ]] || return 1
    if [ -e "/tmp/.X$number-lock" ] || [ -e "/tmp/.X11-unix/X$number" ] \
        || DISPLAY="$disp" xprop -root >/dev/null 2>&1; then
        echo "FAIL: display $disp is occupied; refusing to reuse or stop it" >&2
        return 1
    fi
}

mav_display_ready() {
    local pid="$1" disp="$2" lock_pid
    { read -r lock_pid <"/tmp/.X${disp#:}-lock"; } 2>/dev/null || return 1
    [ "$lock_pid" = "$pid" ] && kill -0 "$pid" 2>/dev/null \
        && DISPLAY="$disp" xprop -root >/dev/null 2>&1
}

mav_wait_display() {
    local pid="$1" disp="$2" i
    for i in {1..60}; do
        kill -0 "$pid" 2>/dev/null || break
        mav_display_ready "$pid" "$disp" && return 0
        sleep 0.1
    done
    echo "FAIL: our X server did not claim $disp" >&2
    return 1
}
