#!/usr/bin/env bash
#
# Config-wallpaper regression harness:
#   1. `[wallpaper] path = "~/rel.png"` (tilde) is expanded and applied at
#      startup (proves tilde expansion + startup seeding).
#   2. Editing the config and `maverickctl reload` re-applies `[wallpaper]`
#      (proves reload re-seeds the wallpaper, not just keybinds).
#
# Requires: xephyr, x11-utils, ffmpeg, gcc. Helpers built if missing.
# Run: ./tests/xephyr-config-wallpaper.sh

set -u

SCREEN_W=1920
SCREEN_H=1080
XEPHYR_DISPLAY=":97"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
CTL_BIN="${CTL_BIN:-./target/debug/maverickctl}"
MSG_BIN="${MSG_BIN:-./target/debug/maverick-msg}"
BINDIR="$(cd "$(dirname "$0")" && pwd)"
# Use a short, explicit XDG_RUNTIME_DIR so the control-socket path stays under
# SUN_LEN (the default can be too long in some sandboxes), shared by the daemon
# and the maverickctl client.
RTDIR="$(mktemp -d /tmp/mrt.XXXX)"
export XDG_RUNTIME_DIR="$RTDIR"
LOG="$(mktemp -t maverick-cfgwp.XXXXXX.log)"
PASS=0
FAIL=0

log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); }

assert_px() {
    "$BINDIR/pxsample" "$1" "$2" "$3" "$4" "0x$5" >/dev/null 2>&1 \
        && ok "pixel $6 @($1,$2) is 0x$5" || bad "pixel $6 @($1,$2) is NOT 0x$5"
}

for h in pxsample; do
    [ -x "$BINDIR/$h" ] || cc -O2 -o "$BINDIR/$h" "$BINDIR/$h.c" -lX11 -lXcomposite 2>>"$LOG" \
        || { bad "failed to build $h"; exit 1; }
done

TMPHOME="$(mktemp -d /tmp/mw.XXXX)"
BLUE="$TMPHOME/tilde_blue.png"
RED="/tmp/maverick-cfgwp_red.png"   # absolute
CFG="$TMPHOME/config.toml"

ffmpeg -f lavfi -i "color=c=0x0000ff:s=${SCREEN_W}x${SCREEN_H}" -frames:v 1 "$BLUE" -y 2>>"$LOG" \
    || { bad "ffmpeg blue failed"; exit 1; }
ffmpeg -f lavfi -i "color=c=0xff0000:s=${SCREEN_W}x${SCREEN_H}" -frames:v 1 "$RED" -y 2>>"$LOG" \
    || { bad "ffmpeg red failed"; exit 1; }

# Startup config uses a TILDE path (must be expanded to $HOME).
cat > "$CFG" <<TOML
[wallpaper]
path = "~/tilde_blue.png"
mode = "fill"
TOML

cleanup() {
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    rm -rf "$TMPHOME" "$RTDIR"
}
trap cleanup EXIT

Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    >"$LOG.xephyr" 2>&1 &
XEPHYR_PID=$!
sleep 1
export DISPLAY="$XEPHYR_DISPLAY"

# HOME points at TMPHOME so "~" expands to it (where tilde_blue.png lives).
HOME="$TMPHOME" "$MAVERICK_BIN" --config "$CFG" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1.5
xprop -root >/dev/null 2>&1 && ok "maverick started on $DISPLAY" \
    || { bad "maverick did not start"; exit 1; }

# 1) Tilde path expanded + applied at startup.
sleep 1
assert_px 960 540 200 200 0000ff "startup-tilde-wallpaper"

# 2) Edit config to an absolute red wallpaper, then reload -> must re-apply.
cat > "$CFG" <<TOML
[wallpaper]
path = "$RED"
mode = "fill"
TOML
"$CTL_BIN" reload >/dev/null 2>&1
sleep 1.5
assert_px 960 540 200 200 ff0000 "reload-applies-new-wallpaper"

log "────────────────────────────────────────"
log "config-wallpaper suite: $PASS passed, $FAIL failed"
log "maverick log: $LOG"
[ "$FAIL" -eq 0 ]
