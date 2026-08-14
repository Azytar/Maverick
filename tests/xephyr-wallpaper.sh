#!/usr/bin/env bash
#
# Maverick wallpaper end-to-end harness (Fase 13).
#
# Drives a REAL compositor under Xephyr (GLX + Composite) and proves the whole
# Fase 7–12 chain works at runtime — not just in unit tests:
#
#   maverick-msg wallpaper set /img.png
#        -> Action::Wallpaper -> SetWallpaper command
#        -> Effect::SetWallpaper -> comp.set_wallpaper
#        -> maverick-img decode -> GPU upload -> drawn behind everything
#
# The config-seed part launches the daemon ONCE with a [wallpaper] TOML so we
# exercise the startup path too; the rest drives runtime IPC on that same
# session (no display restart, which would leave the previous compositor's
# GLX/Composite state behind and make init fail).
#
# Assertions sample live composited pixels (pxsample reads the Composite
# overlay) — every check observes the wallpaper exactly as drawn:
#   1. CONFIG seed     — [wallpaper] path in TOML is applied at startup.
#   2. IMAGE (Fill)    — full-screen solid image covers centre AND corners.
#   3. MODE center     — a smaller image is drawn at native size, leaving
#                         background bands at the corners (proves mode wiring).
#   4. SHADER          — a .glsl source is compiled by the GPU and fills screen
#                         with its Fragment-shader colour.
#   5. CLEAR           — wallpaper is removed; the screen is no longer the image.
#   6. state JSON      — the live snapshot reports the active wallpaper path.
#
# REQUIREMENTS: xephyr, x11-utils, ffmpeg, gcc. C helpers are built if missing.
# No results are fabricated: every assertion reads live pixels / state JSON.
#
# Run:  ./tests/xephyr-wallpaper.sh

set -u

SCREEN_W=1920
SCREEN_H=1080
XEPHYR_DISPLAY=":99"
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
MSG_BIN="${MSG_BIN:-./target/debug/maverick-msg}"
CTL_BIN="${CTL_BIN:-./target/debug/maverickctl}"
BINDIR="$(cd "$(dirname "$0")" && pwd)"
# Short, explicit XDG_RUNTIME_DIR so the control-socket path stays under
# SUN_LEN (the default can be too long in some sandboxes). Shared by the daemon
# and the client tools (maverickctl/maverick-msg).
RTDIR="$(mktemp -d /tmp/mrt.XXXX)"
export XDG_RUNTIME_DIR="$RTDIR"
LOG="$(mktemp -t maverick-wp.XXXXXX.log)"
PASS=0
FAIL=0

log() { printf '%s\n' "$*" | tee -a "$LOG"; }
ok()  { log "PASS: $*"; PASS=$((PASS+1)); }
bad() { log "FAIL: $*"; FAIL=$((FAIL+1)); }

# pxsample wrapper: assert PROP at (X,Y,W,H) is HEX within tolerance.
assert_px() {
    local x="$1" y="$2" w="$3" h="$4" hex="$5" label="$6"
    if "$BINDIR/pxsample" "$x" "$y" "$w" "$h" "0x$hex" >/dev/null 2>&1; then
        ok "pixel $label @($x,$y) is 0x$hex"
    else
        bad "pixel $label @($x,$y) is NOT 0x$hex"
    fi
}
# inverted: the region must NOT be the given colour (proves a change happened).
assert_not_px() {
    local x="$1" y="$2" w="$3" h="$4" hex="$5" label="$6"
    if "$BINDIR/pxsample" "$x" "$y" "$w" "$h" "0x$hex" >/dev/null 2>&1; then
        bad "pixel $label @($x,$y) is STILL 0x$hex (no change)"
    else
        ok "changed: $label @($x,$y) is NOT 0x$hex"
    fi
}

# ── build helpers if absent ───────────────────────────────────────────────────
[ -x "$BINDIR/pxsample" ] || cc -O2 -o "$BINDIR/pxsample" "$BINDIR/pxsample.c" \
    -lX11 -lXcomposite 2>>"$LOG" || { bad "failed to build pxsample"; exit 1; }

# ── image + shader fixtures in /tmp ────────────────────────────────────────────
WP_DIR="$(mktemp -d /tmp/maverick-wp.XXXXXX)"
BLUE="$WP_DIR/wp_blue.png"      # full screen, Fill
RED_SMALL="$WP_DIR/wp_red.png"  # 960x540, center
MAGENTA="$WP_DIR/wp.glsl"       # shader
CFG="$WP_DIR/maverick-wp.toml"

ffmpeg -f lavfi -i "color=c=0x0000ff:s=${SCREEN_W}x${SCREEN_H}" -frames:v 1 "$BLUE" -y 2>>"$LOG" \
    || { bad "ffmpeg could not render blue fixture"; exit 1; }
ffmpeg -f lavfi -i "color=c=0xff0000:s=960x540" -frames:v 1 "$RED_SMALL" -y 2>>"$LOG" \
    || { bad "ffmpeg could not render red fixture"; exit 1; }
cat > "$MAGENTA" <<'GLSL'
#version 330 core
out vec4 frag;
void main() {
    // Solid (0, 255, 128) — distinct from blue/image fixtures.
    frag = vec4(0.0, 1.0, 0.5, 1.0);
}
GLSL
# Config seed: start the daemon with this wallpaper already applied.
cat > "$CFG" <<TOML
[wallpaper]
path = "$BLUE"
mode = "fill"
TOML

# ── nested X server (Composite + RANDR + GLX) ──────────────────────────────────
cleanup() {
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    rm -rf "$WP_DIR" "$RTDIR"
}
trap cleanup EXIT

# Xephyr nests into the parent X (the DISPLAY this script inherited); only the
# daemon is pointed at the nested :99 display.
Xephyr "$XEPHYR_DISPLAY" -screen "${SCREEN_W}x${SCREEN_H}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    >"$LOG.xephyr" 2>&1 &
XEPHYR_PID=$!
sleep 1
export DISPLAY="$XEPHYR_DISPLAY"

# ── launch maverick (compositor on, config-seeded) ─────────────────────────────
"$MAVERICK_BIN" --config "$CFG" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1.5
if xprop -root >/dev/null 2>&1; then
    ok "maverick started (config-seeded) on $DISPLAY"
else
    bad "maverick did not start on $DISPLAY"
    exit 1
fi

send() { "$MSG_BIN" "$@" >/dev/null 2>&1; }
wait_frame() { sleep 1.0; }

# ══════════════════════════════════════════════════════════════════════════════
# Scenario 1 — CONFIG seed: [wallpaper] path applied at startup.
# ══════════════════════════════════════════════════════════════════════════════
wait_frame
assert_px 960 540 200 200 0000ff "config-seed-center"
assert_px 40 40 120 120 0000ff "config-seed-corner"

# state JSON must reflect the configured wallpaper path.
if "$CTL_BIN" state 2>/dev/null | grep -q "wp_blue.png"; then
    ok "state JSON reports wallpaper path"
else
    bad "state JSON does not report wallpaper path"
fi

# ══════════════════════════════════════════════════════════════════════════════
# Scenario 2 — IMAGE wallpaper via IPC, Fill: full coverage (re-sets blue).
# ══════════════════════════════════════════════════════════════════════════════
send wallpaper mode fill
send wallpaper set "$BLUE"
wait_frame
assert_px 960 540 200 200 0000ff "image-fill-center"
assert_px 40 40 120 120 0000ff "image-fill-corner"

# ══════════════════════════════════════════════════════════════════════════════
# Scenario 3 — MODE center: smaller image drawn at native size, corners bare.
# ══════════════════════════════════════════════════════════════════════════════
send wallpaper mode center
send wallpaper set "$RED_SMALL"
wait_frame
assert_px 960 540 120 120 ff0000 "mode-center-image-center"
assert_not_px 40 40 120 120 ff0000 "mode-center-corner-background"

# ══════════════════════════════════════════════════════════════════════════════
# Scenario 4 — SHADER wallpaper: GPU-compiled fragment fills the screen.
# ══════════════════════════════════════════════════════════════════════════════
send wallpaper set "$MAGENTA"
wait_frame
assert_px 960 540 200 200 00ff80 "shader-fill-center"
assert_not_px 960 540 120 120 ff0000 "shader-replaced-image"

# ══════════════════════════════════════════════════════════════════════════════
# Scenario 5 — CLEAR: wallpaper removed, screen is no longer the shader.
# ══════════════════════════════════════════════════════════════════════════════
send wallpaper clear
wait_frame
assert_not_px 960 540 200 200 00ff80 "clear-removed-shader"

log "────────────────────────────────────────"
log "wallpaper suite: $PASS passed, $FAIL failed"
log "maverick log: $LOG"
[ "$FAIL" -eq 0 ]
