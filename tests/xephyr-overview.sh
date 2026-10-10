#!/usr/bin/env bash
#
# End-to-end validation of the Overview navigation viewport (`Mod+O`).
#
# Overview is a fixed-scale spatial viewport over the current View. The scale
# is fixed once, on entry (`layout::overview_entry_scale_for`, default
# `0.76`), and navigation pans the camera at that stored scale. What this suite
# pins down, reading live X geometry through xwininfo/xprop plus the control
# socket, is:
#
#   - entering Overview *rescales* the tiles: the real X11 rectangles shrink by
#     the entry scale, visibly and measurably, for 1, 2, 3, 6 and 9 clients
#     (a mode that entered at 1.0 would leave the desktop pixel-identical,
#     which is the one thing `Mod+O` must not do);
#   - the scale stays fixed across navigation: widths are identical after 1, 5
#     and 20 selection steps in both directions, while x positions travel
#     (that is the viewport pan, and it is what distinguishes "scale" from
#     "viewport position");
#   - the focused tile is visible after every step, and navigation saturates
#     at both ends instead of wrapping;
#   - cursor selection (hover) focuses without floating and without rescaling;
#     an explicit Mod4+drag on a tile is a no-op, on a float it moves;
#   - floats keep their rect across entry/navigation/exit, and floating a tile
#     during Overview starts from the unscaled tile, never from a shrunk one;
#   - resize, map/unmap, fullscreen and workspace switches behave, and
#     leaving restores the settled rectangles bit-for-bit with no grab left
#     behind (the pointer keeps working).
#
# Every assertion reads live state. Nothing is fabricated, and nothing is
# asserted from the source.
#
# Navigation below is driven through the control socket (`maverickctl msg`),
# never through XTEST keys: XTEST through a nested Xephyr produces phantom
# autorepeats, so a key-driven step count is nondeterministic. Pointer gestures
# (click, Mod4+drag) necessarily go through xdotool.
#
# ISOLATION. Two traps this rig has to defeat:
#
#   1. This tool normally runs *inside* a Maverick session, so it inherits
#      `$MAVERICK_INSTANCE` and resolves every request to the operator's own
#      live desktop. The variable is dropped, a private `$XDG_RUNTIME_DIR` is
#      used, and the reached instance is checked to manage nothing but the
#      clients this rig mapped.
#   2. The compiled autostart contains the xdg-desktop-portal binaries. Running
#      them on a nested display re-runs D-Bus activation for the operator's
#      session, which respawns the status bar, the terminal and the compositor
#      onto the display under test. The rig therefore launches Maverick with an
#      empty `[autostart].commands`, which replaces the compiled list outright.
#
# REQUIREMENTS: xephyr, x11-utils (xprop/xwininfo), xdotool, cc + libX11,
# imagemagick (import/convert) for the screenshots.
# Run:  ./tests/xephyr-overview.sh

set -u

# `MAVERICK_INSTANCE` names the session that spawned this process. Inherited
# from an operator desktop it is indistinguishable from the nested rig, and
# `maverickctl` resolves it before anything else, so it must go first.
unset MAVERICK_INSTANCE

HERE="$(cd "$(dirname "$0")" && pwd)"
SCREEN_W=1920
SCREEN_H=1080
MAVERICK_BIN="${MAVERICK_BIN:-./target/debug/maverick}"
MAVERICK_CTL="${MAVERICK_CTL:-./target/debug/maverickctl}"
RT="$(mktemp -d /tmp/mvov.XXXXXX)"
export XDG_RUNTIME_DIR="$RT"
LOG="$RT/maverick.log"
declare -A WPID=()
# Xephyr is itself an X client of the *host* server: launch it with the host
# DISPLAY, then switch the shell to the nested one.
HOST_DISPLAY="${DISPLAY:-}"

PASS=0
FAIL=0

ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }
info(){ echo "INFO: $*"; }

cleanup() {
    local pid
    for pid in "${WPID[@]}"; do
        kill -9 "$pid" 2>/dev/null
        wait "$pid" 2>/dev/null
    done
    [ -n "${MAV_PID:-}" ] && kill "$MAV_PID" 2>/dev/null
    [ -n "${MAV_PID:-}" ] && wait "$MAV_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && kill "$XEPHYR_PID" 2>/dev/null
    [ -n "${XEPHYR_PID:-}" ] && wait "$XEPHYR_PID" 2>/dev/null
    rm -rf "$RT"
}
trap cleanup EXIT

# ── preflight ─────────────────────────────────────────────────────────────────
[ -x "$HERE/mgdwin" ] || cc -O2 -o "$HERE/mgdwin" "$HERE/mgdwin.c" -lX11 2>/dev/null \
    || { echo "FAIL: could not build tests/mgdwin"; exit 1; }
[ -x "$HERE/winmove" ] || cc -O2 -o "$HERE/winmove" "$HERE/winmove.c" -lX11 2>/dev/null

cat >"$RT/rig.toml" <<'EOF'
[autostart]
commands = []

# Cursor selection is exercised through hover (EnterNotify): XTEST button
# presses double-deliver through Xephyr's SYNC-grab path in this environment
# (client press + phantom root press), so clicks cannot assert focus here.
# Motion is not grabbed and delivers once, which makes hover the honest
# end-to-end cursor-selection probe.
[general]
focus_mouse = true
EOF

# Let our server claim a free display atomically. A fixed number can already
# belong to another session; neither its readiness nor its teardown is ours.
DISPLAY="$HOST_DISPLAY" Xephyr -displayfd 3 -screen "${SCREEN_W}x${SCREEN_H}" -ac \
    +extension RANDR +extension GLX +extension Composite +extension DAMAGE \
    3>"$RT/display" >"$RT/xephyr.log" 2>&1 &
XEPHYR_PID=$!
up=0
for _ in $(seq 1 60); do
    kill -0 "$XEPHYR_PID" 2>/dev/null || break
    if [ -s "$RT/display" ]; then
        read -r display_number <"$RT/display"
        if [[ "$display_number" =~ ^[0-9]+$ ]]; then
            DISP=":$display_number"
            if DISPLAY="$DISP" xprop -root >/dev/null 2>&1; then up=1; break; fi
        fi
    fi
    sleep 0.1
done
if [ "$up" != 1 ]; then
    bad "Xephyr did not claim a ready display ($(tail -2 "$RT/xephyr.log"))"; exit 1
fi
export DISPLAY="$DISP"

"$MAVERICK_BIN" --config "$RT/rig.toml" >"$LOG" 2>&1 &
MAV_PID=$!
sleep 1.5
if ! kill -0 "$MAV_PID" 2>/dev/null; then
    bad "maverick did not start on $DISP"; tail -20 "$LOG"; exit 1
fi
ok "maverick started on $DISP (log $LOG)"

# ── helpers ───────────────────────────────────────────────────────────────────

# Echo the XID of the client titled $1, non-zero when there is none. The exit
# status matters as much as the value: a pipeline ending in `head` succeeds on
# empty input, which silently turns every "wait for the window" into a no-op.
win_of() {
    local w
    w="$(xdotool search --name "^$1\$" 2>/dev/null | head -1)"
    [ -n "$w" ] || return 1
    printf '%s\n' "$w"
}

spawn_win() {
    # The braces keep bash's asynchronous job report ("Terminado") off the
    # suite's own output; it fires when the client is later killed.
    { MGDTITLE="$1" "$HERE/mgdwin" >/dev/null 2>&1 & } 2>/dev/null
    WPID["$1"]=$!
    local i=0
    while [ $i -lt 50 ]; do win_of "$1" >/dev/null 2>&1 && return 0; sleep 0.1; i=$((i+1)); done
    return 1
}

close_win() {
    if [ -n "${WPID[$1]:-}" ]; then
        kill -9 "${WPID[$1]}" 2>/dev/null
        wait "${WPID[$1]}" 2>/dev/null
        unset 'WPID[$1]'
    fi
    local i=0
    while [ $i -lt 50 ]; do win_of "$1" >/dev/null 2>&1 || return 0; sleep 0.1; i=$((i+1)); done
    return 1
}

# x y w h of one window, straight from the server: the physical X11 geometry.
rect_of() {
    xwininfo -id "$1" 2>/dev/null | awk \
        '/Absolute upper-left X:/{x=$4} /Absolute upper-left Y:/{y=$4}
         /Width:/{w=$2} /Height:/{h=$2} END{print x, y, w, h}'
}

# Sorted widths of every managed client, comma-separated: the scale
# fingerprint. Navigation at a fixed scale remeasures nothing, so this must
# not move while the viewport pans.
widths() {
    for w in $(xdotool search --class mgdwin 2>/dev/null | sort -n); do
        rect_of "$w" | awk '{print $3}'
    done | paste -sd, -
}

# Integer percentage of a/b, rounded: the scale fingerprint as one number.
# Used to assert the entry reduction lands in a band instead of pinning a
# single pixel count that would break on any gap or border change.
pct_of() { awk -v a="$1" -v b="$2" 'BEGIN{ if (b<=0) { print 0 } else { printf "%d", (a*100/b)+0.5 } }'; }

# cmp_lt A B: true when A < B, for the comparisons awk is needed to make.
cmp_lt() { awk -v a="$1" -v b="$2" 'BEGIN{ exit !(a<b) }'; }

# Same, with position and height: the full rectangle fingerprint.
rects() {
    for w in $(xdotool search --class mgdwin 2>/dev/null | sort -n); do
        rect_of "$w" | awk '{printf "%sx%s@%s,%s ", $3, $4, $1, $2}'
    done | sed 's/ $//'
}

msg() { "$MAVERICK_CTL" msg "$1" >/dev/null 2>&1; sleep 0.4; }

active_win() {
    xprop -root -notype _NET_ACTIVE_WINDOW 2>/dev/null | grep -oE '0x[0-9a-f]+' | head -1
}

focus_title() {
    "$MAVERICK_CTL" state 2>/dev/null | grep -oE '"focused_title":"[^"]*"' \
        | head -1 | sed 's/.*:"//; s/"$//'
}

# Per-window tiled/floating state from the control plane (`"float":true` in
# the window's own tree entry — `[^}]*` cannot cross into another entry).
float_of() {
    "$MAVERICK_CTL" query tree 2>/dev/null \
        | grep -oE "\"title\":\"$1\"[^}]*\"float\":[a-z]+" \
        | grep -oE '"float":[a-z]+' | head -1
}

# The model's own geometry mirror for the window titled $1 (`"geom":[x,y,w,h]
# in its tree entry): the logical geometry, to compare against the physical
# one xwininfo reports.
tree_geom_of() {
    "$MAVERICK_CTL" query tree 2>/dev/null \
        | grep -oE "\"title\":\"$1\"[^}]*\"geom\":\[[^]]*\]" \
        | grep -oE '\[[^]]*\]' | head -1 | tr -d '[]' | tr ',' ' '
}

# Decimal XID to the 0x spelling `_NET_ACTIVE_WINDOW` uses.
hex_of() { printf '0x%x' "$1"; }

# Wait until the server geometry of a window agrees with the model's own
# mirror (or time out): navigation messages return before the backend's
# arrange round trip lands, so a position read straight after a burst can catch
# the previous frame. Widths are camera-independent and need no sync; x
# positions do. With a title, that window is synced; without, the focused one.
sync_tile() {
    local i foc trect prect w
    if [ -n "${1:-}" ]; then
        for i in $(seq 1 25); do
            w="$(win_of "$1")" || { sleep 0.2; continue; }
            trect="$(tree_geom_of "$1")"
            prect="$(rect_of "$w")"
            [ -n "$trect" ] && [ "$trect" = "$prect" ] && return 0
            sleep 0.2
        done
        return 1
    fi
    for i in $(seq 1 25); do
        foc="$(active_win)"
        [ "$foc" = "0x0" ] && { sleep 0.2; continue; }
        trect="$("$MAVERICK_CTL" query tree 2>/dev/null \
            | grep -oE "\"id\":$((foc))[^}]*\"geom\":\[[^]]*\]" \
            | grep -oE '\[[^]]*\]' | head -1 | tr -d '[]' | tr ',' ' ')"
        prect="$(rect_of "$((foc))" 2>/dev/null)"
        [ -n "$trect" ] && [ "$trect" = "$prect" ] && return 0
        sleep 0.2
    done
    return 1
}

# Click the centre of window $1 (a real pointer click through xdotool).
# `--sync` first: `mousemove X Y click 1` in one line races the motion, and
# the press can land where the pointer *was* (the root, which unfocuses).
click_win() {
    local r x y w h cx cy
    r="$(rect_of "$1")"
    x="$(printf '%s' "$r" | awk '{print $1}')"
    y="$(printf '%s' "$r" | awk '{print $2}')"
    w="$(printf '%s' "$r" | awk '{print $3}')"
    h="$(printf '%s' "$r" | awk '{print $4}')"
    cx=$((x + w / 2)); cy=$((y + h / 2))
    xdotool mousemove --sync "$cx" "$cy" >/dev/null 2>&1
    xdotool click 1 >/dev/null 2>&1
    sleep 0.5
}

# Mod4+drag window $1 by (dx,dy): the explicit float gesture. Same `--sync`
# first, for the same reason: the grab records the press position, so a press
# that missed the float would silently become a no-op.
mod_drag() {
    local r x y w h cx cy
    r="$(rect_of "$1")"
    x="$(printf '%s' "$r" | awk '{print $1}')"
    y="$(printf '%s' "$r" | awk '{print $2}')"
    w="$(printf '%s' "$r" | awk '{print $3}')"
    h="$(printf '%s' "$r" | awk '{print $4}')"
    cx=$((x + w / 2)); cy=$((y + h / 2))
    xdotool mousemove --sync "$cx" "$cy" >/dev/null 2>&1
    xdotool keydown Super_L mousedown 1 \
        mousemove $((cx + $2)) $((cy + $3)) mouseup 1 keyup Super_L >/dev/null 2>&1
    sleep 0.6
}

# Wait until $1 managed clients are mapped AND laid out: every one of them at
# least $2 px wide (default 1000, the full-size tile width). Mapping alone is
# not enough — a client just mapped still carries its own default geometry
# (520x360@60,60 for mgdwin) until the first arrange reaches the server, and a
# widths read in that window would pin a transient as the baseline. The caller
# passes a lower bar when the workspace is in Overview, where every tile is
# deliberately projected smaller.
wait_windows() {
    local i n bad w min
    min="${2:-1000}"
    for i in $(seq 1 50); do
        bad=0
        n="$(xdotool search --class mgdwin 2>/dev/null | wc -l)"
        if [ "$n" = "$1" ]; then
            for w in $(xdotool search --class mgdwin 2>/dev/null | sort -n); do
                [ "$(rect_of "$w" | awk '{print $3}')" -ge "$min" ] 2>/dev/null || bad=1
            done
            [ "$bad" = 0 ] && return 0
        fi
        sleep 0.2
    done
    return 1
}

SHOT_DIR="${OVERVIEW_SHOT_DIR:-$HERE/../target/overview-shots}"
mkdir -p "$SHOT_DIR" 2>/dev/null

# The Overview toggle has no "set to" form, so every measurement below needs to
# know which side of the toggle the workspace is on. Tracking it here is what
# makes the tests deterministic instead of depending on an even/odd count.
VIEW="settled"
toggle() { msg toggle_overview; if [ "$VIEW" = settled ]; then VIEW=zoomed; else VIEW=settled; fi; }
settle() { [ "$VIEW" = zoomed ] && toggle || true; }
zoomit() { [ "$VIEW" = settled ] && toggle || true; }

# Capture the nested root window to $SHOT_DIR/<name>.ppm. PPM because `import`
# can write it without any codec and `convert` reads it back for the pixel diff
# below. The composition of the two modes is what the acceptance criterion is
# about, so the capture is part of the suite and not an ornament: the frames
# stay under target/overview-shots for inspection.
snap() {
    command -v import >/dev/null 2>&1 || return 1
    import -window root -silent "$SHOT_DIR/$1.ppm" 2>/dev/null || return 1
    [ -s "$SHOT_DIR/$1.ppm" ]
}

# Sum of absolute per-channel differences between two same-size frames
# (0 = identical, 3 x pixels x channels = maximally different), or -1 when
# imagemagick is unavailable. A pixel-level comparison catches a change the
# geometry pass could miss — a tile that moved without resizing, a background
# repaint — which is exactly what "the desktop must look different" means.
# `printf` normalises ImageMagick's float spelling into the integer the shell
# can compare directly.
frame_diff() {
    local raw
    command -v convert >/dev/null 2>&1 || { echo -1; return; }
    raw="$(convert "$1" "$2" -compose difference -composite \
        -format "%[fx:mean*w*h]" info: 2>/dev/null)"
    case "$raw" in
        "" | *[!0-9.eE+-]*) echo -1 ;;
        *) printf '%.0f' "$raw" ;;
    esac
}

# Guard: the instance reached must manage nothing but this rig's clients. If the
# nested Maverick never came up, `maverickctl` would otherwise silently resolve
# against the operator's session and every action below would land on a real
# desktop.
assert_isolated() {
    local foreign
    foreign="$("$MAVERICK_CTL" query tree 2>/dev/null \
        | grep -oE '"instance":"[^"]+"' | grep -vc '"mgdwin"')"
    if [ "${foreign:-1}" != "0" ]; then
        bad "driving a foreign instance ($foreign non-mgdwin clients) — aborting"
        exit 1
    fi
}

MGDTITLE=__guard__ "$HERE/mgdwin" >/dev/null 2>&1 &
GUARD_PID=$!
WPID[__guard__]=$GUARD_PID
for _ in $(seq 1 50); do
    "$MAVERICK_CTL" query tree 2>/dev/null | grep -q '"instance":"mgdwin"' && break
    sleep 0.1
done
assert_isolated
kill -9 "$GUARD_PID" 2>/dev/null
wait "$GUARD_PID" 2>/dev/null
unset 'WPID[__guard__]'
sleep 0.4
ok "the rig manages only its own clients"

# ── A. one window: entering must rescale, even with nothing else to show ─────
spawn_win ov1 || { bad "could not map the first client"; exit 1; }
wait_windows 1 || bad "ov1 never reached its tile"
sleep 0.4
ONE_RECT="$(rects)"
ONE_W="$(widths)"
zoomit
ONE_ZOOMED="$(rects)"
ONE_ZW="$(widths)"
info "1 client: settled=[$ONE_W] overview=[$ONE_ZW]"
ONE_PCT="$(pct_of "$ONE_ZW" "$ONE_W")"
if [ "$ONE_ZOOMED" != "$ONE_RECT" ] && cmp_lt "$ONE_ZW" "$ONE_W" \
     && [ "$ONE_PCT" -ge 70 ] && [ "$ONE_PCT" -le 82 ]; then
    ok "a single window is visibly reduced on entry (${ONE_ZW}px, ${ONE_PCT}% of settled)"
else
    bad "a single window was not reduced on entry: $ONE_RECT -> $ONE_ZOOMED"
fi
settle
[ "$(rects)" = "$ONE_RECT" ] \
    && ok "one-window round trip restores the exact rectangles" \
    || bad "one-window round trip moved something: $(rects) want $ONE_RECT"
close_win ov1 || bad "ov1 did not go away"

# ── A2. two windows: the ribbon the mode is named for ───────────────────────
# Two tiles is the smallest ribbon with a neighbour to reveal: at the entry
# scale the focused tile must be complete on screen *and* the next one must
# have started beside it. Captures for both modes are kept as evidence.
for t in p q; do spawn_win "ov2-$t" || bad "could not map ov2-$t"; done
wait_windows 2 || bad "the two clients never reached their tiles"
sleep 0.5
TWO_RECT="$(rects)"
TWO_W="$(widths)"
if command -v import >/dev/null 2>&1; then snap settled-2 || info "no settled-2 capture"; fi
zoomit
TWO_ZOOMED="$(rects)"
TWO_ZW="$(widths)"
info "2 clients: settled=[$TWO_W] overview=[$TWO_ZW]"
TWO_PCT="$(pct_of "$TWO_ZW" "$TWO_W")"
if [ "$TWO_ZOOMED" != "$TWO_RECT" ] && [ "$TWO_PCT" -ge 70 ] && [ "$TWO_PCT" -le 82 ]; then
    ok "two windows are visibly reduced on entry (${TWO_PCT}% of settled)"
else
    bad "two windows were not reduced on entry: $TWO_RECT -> $TWO_ZOOMED"
fi
# The focused tile fits fully, and the neighbour has begun: that is the
# "one whole window plus a peek of the next" the ribbon is for. Either side
# counts — with two columns the neighbour sits wherever the camera parked it.
TWO_FOCUS="$(active_win)"
TWO_F_RECT="$(rect_of "$TWO_FOCUS")"
TWO_FX="$(printf '%s' "$TWO_F_RECT" | awk '{print $1}')"
TWO_FW="$(printf '%s' "$TWO_F_RECT" | awk '{print $3}')"
TWO_FR=$((TWO_FX + TWO_FW))
TWO_PEEK=0
for w in $(xdotool search --class mgdwin 2>/dev/null | sort -n); do
    [ "$w" = "$TWO_FOCUS" ] && continue
    N_RECT="$(rect_of "$w")"
    NX="$(printf '%s' "$N_RECT" | awk '{print $1}')"
    NW="$(printf '%s' "$N_RECT" | awk '{print $3}')"
    NR=$((NX + NW))
    # Visible on screen, and started (not fully hidden past an edge).
    if [ "$NX" -lt "$SCREEN_W" ] && [ "$NR" -gt 0 ]; then
        TWO_PEEK=$((TWO_PEEK + 1))
        info "2-client neighbour: $N_RECT"
    fi
done
if [ "$TWO_FX" -ge 0 ] && [ "$TWO_FR" -le "$SCREEN_W" ] && [ "$TWO_PEEK" -ge 1 ]; then
    ok "the focused tile is whole and its neighbour peeks (focus $TWO_FX..$TWO_FR, $TWO_PEEK neighbour(s) visible)"
else
    bad "the two-window ribbon is not navigable: focus=[$TWO_F_RECT]"
fi
if command -v import >/dev/null 2>&1; then snap overview-2 || info "no overview-2 capture"; fi
settle
[ "$(rects)" = "$TWO_RECT" ] \
    && ok "two-window round trip restores the exact rectangles" \
    || bad "two-window round trip moved something: $(rects) want $TWO_RECT"
for t in p q; do close_win "ov2-$t" || bad "ov2-$t did not go away"; done
sleep 0.4

# ── B. three windows: entry fixes the scale, and it is a real reduction ──────
for t in a b c; do spawn_win "ov-$t" || bad "could not map ov-$t"; done
wait_windows 3 || bad "the three clients never reached their tiles"
sleep 0.5
SETTLED="$(widths)"
SETTLED_RECT="$(rects)"
FOCUS_TITLE="$(focus_title)"
zoomit
Z1="$(widths)"
Z1_RECT="$(rects)"
info "3 clients: settled=[$SETTLED] overview=[$Z1]"
B_PCT="$(pct_of "$(printf '%s' "$Z1" | cut -d, -f1)" "$(printf '%s' "$SETTLED" | cut -d, -f1)")"
if [ "$Z1" != "$SETTLED" ] && [ "$B_PCT" -ge 70 ] && [ "$B_PCT" -le 82 ]; then
    ok "entering Overview visibly reduces the tiles (${B_PCT}% of settled)"
else
    bad "entering Overview did not reduce the tiles as designed: $SETTLED -> $Z1"
fi
# The reduction is one global scale, so every tile shrinks by the same factor
# and none of them is squeezed to an unreadable sliver.
N_BEFORE="$(printf '%s' "$SETTLED" | awk -F, '{print NF}')"
N_AFTER="$(printf '%s' "$Z1" | awk -F, '{print NF}')"
if [ "$N_BEFORE" = "$N_AFTER" ] && [ "$N_AFTER" = 3 ]; then
    ok "every tile keeps its place in the ribbon ($Z1)"
else
    bad "the ribbon lost or gained a tile on entry: $SETTLED -> $Z1"
fi
MINW="$(printf '%s\n' "$Z1" | tr ',' '\n' | sort -n | head -1)"
if [ "${MINW:-0}" -gt 200 ]; then
    ok "no tile degenerated (narrowest ${MINW} px)"
else
    bad "a tile degenerated on entry: narrowest ${MINW} px"
fi

# Logical (control-plane geom) vs physical (xwininfo) for one tile: the two
# must agree — the reconciler converged — while the viewport is what may move.
sync_tile || info "server geometry lagged the model on entry"
TILE_WIN="$(win_of ov-b)"
LOGICAL="$(tree_geom_of ov-b)"
PHYSICAL="$(rect_of "$TILE_WIN")"
if [ "$LOGICAL" = "$PHYSICAL" ]; then
    ok "logical and physical geometry agree on the tile ($PHYSICAL)"
else
    bad "logical vs physical disagree: tree [$LOGICAL] vs X [$PHYSICAL]"
fi

# ── B2. the composition really changes ──────────────────────────────────────
# X11 geometry is the measurement; the pixels are the proof. The settled frame
# and the Overview frame must differ — a capture that only recorded a camera
# pan would still pass the geometry pass, but not this one. Both frames are
# taken from a known side of the toggle: settle, shoot, enter, shoot.
if command -v import >/dev/null 2>&1 && command -v convert >/dev/null 2>&1; then
    settle
    if snap settled; then
        zoomit
        if snap overview; then
            D_SET="$(frame_diff "$SHOT_DIR/settled.ppm" "$SHOT_DIR/overview.ppm")"
            info "frame diff settled vs overview: $D_SET ($SHOT_DIR/{settled,overview}.ppm)"
            if [ "${D_SET:-0}" -gt 0 ] 2>/dev/null; then
                ok "the visible composition changes on entry (diff $D_SET)"
            else
                bad "the Overview frame is pixel-identical to the settled one"
            fi
        else
            info "could not capture the Overview frame; screenshot evidence skipped"
        fi
    else
        info "could not capture the settled frame; screenshot evidence skipped"
    fi
else
    info "imagemagick unavailable; screenshot evidence skipped"
fi

# ── C. focus survives the entry ──────────────────────────────────────────────
BEFORE_FOCUS="$(active_win)"
BEFORE_TITLE="$(focus_title)"
AFTER_FOCUS="$(active_win)"
AFTER_TITLE="$(focus_title)"
[ "$BEFORE_FOCUS" = "$AFTER_FOCUS" ] \
    && ok "overview does not move the input focus ($AFTER_FOCUS)" \
    || bad "overview moved focus $BEFORE_FOCUS -> $AFTER_FOCUS"
[ "$BEFORE_TITLE" = "$AFTER_TITLE" ] \
    && ok "overview does not change which client is focused ($AFTER_TITLE)" \
    || bad "overview changed the focused client: $BEFORE_TITLE -> $AFTER_TITLE"

# ── D. navigation pans at a fixed scale ──────────────────────────────────────
# One step left through the dedicated nav verb (focus starts on the last
# column, so right would saturate), then back right.
msg "overview_nav:left"
if [ "$(active_win)" != "$BEFORE_FOCUS" ]; then
    ok "overview_nav:left moves to the previous column while zoomed"
else
    bad "overview_nav:left did not move while zoomed"
fi
[ "$(widths)" = "$Z1" ] \
    && ok "one nav step leaves every tile width untouched" \
    || bad "one nav step rescaled: $Z1 -> $(widths)"
msg "overview_nav:right"
[ "$(active_win)" = "$BEFORE_FOCUS" ] \
    && ok "overview_nav:right returns to the original column" \
    || bad "overview_nav:right did not return: $(active_win) want $BEFORE_FOCUS"

# The ordinary focus keys work too, and also never rescale.
msg "focus:right"
[ "$(widths)" = "$Z1" ] \
    && ok "focus:right keeps the fixed scale" \
    || bad "focus:right rescaled: $Z1 -> $(widths)"
msg "focus:left"

# 1, 5 and 20 steps: widths pinned, positions travelling. Each measurement
# starts from a known end of the ribbon so saturation cannot hide a drift.
SAME=1; DRIFT=""
goto_end() { for _ in $(seq 1 10); do msg "overview_nav:right" >/dev/null; done; }
goto_start() { for _ in $(seq 1 10); do msg "overview_nav:left" >/dev/null; done; }
goto_end
sync_tile || info "server geometry lagged the model at the far end"
END_RECT="$(rects)"
goto_start
sync_tile || info "server geometry lagged the model at the near end"
[ "$(widths)" = "$Z1" ] || { SAME=0; DRIFT="$(widths)"; }
for _ in 1 2 3 4 5; do msg "overview_nav:right" >/dev/null; done
[ "$(widths)" = "$Z1" ] || { SAME=0; DRIFT="$(widths)"; }
for _ in $(seq 1 20); do msg "overview_nav:left" >/dev/null; done
[ "$(widths)" = "$Z1" ] || { SAME=0; DRIFT="$(widths)"; }
for _ in $(seq 1 20); do msg "overview_nav:right" >/dev/null; done
[ "$(widths)" = "$Z1" ] || { SAME=0; DRIFT="$(widths)"; }
[ "$SAME" = 1 ] \
    && ok "widths are identical after 1, 5 and 20 nav steps ($Z1)" \
    || bad "navigation rescaled the tiles: $Z1 -> $DRIFT"
# ...while the viewport demonstrably travelled between the two ends.
goto_start
sync_tile || info "server geometry lagged before the pan comparison"
START_RECT="$(rects)"
if [ "$START_RECT" != "$END_RECT" ]; then
    ok "the viewport pans between the ends of the ribbon"
else
    bad "the viewport never moved: $START_RECT"
fi
# Saturation, not wrapping: pushing past the last column keeps the focus.
goto_end
END_FOCUS="$(active_win)"
msg "overview_nav:right"
[ "$(active_win)" = "$END_FOCUS" ] \
    && ok "navigation past the last column saturates" \
    || bad "navigation past the end wrapped: $END_FOCUS -> $(active_win)"
goto_start
START_FOCUS="$(active_win)"
msg "overview_nav:left"
[ "$(active_win)" = "$START_FOCUS" ] \
    && ok "navigation past the first column saturates" \
    || bad "navigation past the start wrapped"
goto_end
settle

# Reversibility at pixel level.
[ "$(rects)" = "$SETTLED_RECT" ] \
    && ok "overview round trip restores every rectangle exactly" \
    || bad "overview round trip left geometry behind: $(rects) want $SETTLED_RECT"

# ── E. cursor selection: hover focuses, click never floats ────────────────
# Center col1 (ov-b): its right neighbour then peeks on screen, which is what
# the cursor below targets. (Col0 is full-width, so from col0 nothing else is
# clickable; hovering off-screen coordinates would select the wrong window.)
zoomit
for _ in 1 2 3 4 5; do msg "overview_nav:left" >/dev/null; done
msg "overview_nav:right"
[ "$(active_win)" = "$(hex_of "$(win_of ov-b)")" ] \
    || bad "setup did not land on ov-b: $(active_win)"
CLICK_W="$(widths)"
# Hover 50 px inside the right edge of ov-c's tile (clamped on screen): the
# visible sliver of the neighbour. Motion delivers once (no grabs), so this
# is the honest cursor-selection probe.
C_WIN="$(win_of ov-c)"
sync_tile || info "server geometry lagged the model before the hover"
C_RECT="$(rect_of "$C_WIN")"
C_X="$(printf '%s' "$C_RECT" | awk '{print $1}')"
C_Y="$(printf '%s' "$C_RECT" | awk '{print $2}')"
C_W="$(printf '%s' "$C_RECT" | awk '{print $3}')"
C_H="$(printf '%s' "$C_RECT" | awk '{print $4}')"
PX=$((C_X + C_W - 50)); [ "$PX" -gt 1910 ] && PX=1910
PY=$((C_Y + C_H / 2))
if [ "$PX" -le "$C_X" ]; then
    bad "ov-c is fully off screen, cannot hover it ($C_RECT)"
else
    xdotool mousemove --sync "$PX" "$PY" >/dev/null 2>&1
    sleep 0.6
    [ "$(active_win)" = "$(hex_of "$C_WIN")" ] \
        && ok "hovering a visible tile selects it in Overview" \
        || bad "hover did not select the tile: $(active_win) want $(hex_of "$C_WIN")"
    [ "$(float_of ov-c)" = '"float":false' ] \
        && ok "hover selection does not float the tile" \
        || bad "hover floated the tile: $(float_of ov-c)"
    [ "$(widths)" = "$CLICK_W" ] \
        && ok "cursor selection rescales nothing ($CLICK_W)" \
        || bad "cursor selection rescaled: $CLICK_W -> $(widths)"
    # A real click on the same tile: still tiled, same widths. (Focus itself
    # is not asserted here: XTEST presses double-deliver through Xephyr in
    # this environment — client press plus phantom root press — and the root
    # half clears the focus by design. No-float and no-rescale are what a
    # click must guarantee, and those are asserted.)
    xdotool click 1 >/dev/null 2>&1
    sleep 0.5
    [ "$(float_of ov-c)" = '"float":false' ] \
        && ok "a click does not convert the tile to floating" \
        || bad "a click floated the tile: $(float_of ov-c)"
    [ "$(widths)" = "$CLICK_W" ] \
        && ok "click selection rescales nothing ($CLICK_W)" \
        || bad "click selection rescaled: $CLICK_W -> $(widths)"
    # A Mod4+drag on a tile is a no-op: still tiled, same rectangle.
    sync_tile ov-c || info "server geometry lagged the model before the drag"
    TILE_RECT="$(rect_of "$C_WIN")"
    mod_drag "$C_WIN" 120 0
    [ "$(rect_of "$C_WIN")" = "$TILE_RECT" ] \
        && ok "a Mod4+drag on a tile moves nothing" \
        || bad "a tile drag moved the tile: $TILE_RECT -> $(rect_of "$C_WIN")"
    [ "$(float_of ov-c)" = '"float":false' ] \
        && ok "and it still did not float the tile" \
        || bad "a drag floated the tile"
fi
settle

# ── E2. six clients: the same reduction, and no hover cascade ───────────────
# Six columns is the first count where several neighbours are on screen at once,
# so it pins both halves of the entry contract: the reduction is still the one
# global scale, and selecting a partially visible tile pans the viewport without
# the pan sliding the next tile under the stationary pointer into a re-selection
# (that loop walks the selection to the end of the ribbon on its own).
for t in e f g; do spawn_win "ov-$t" || bad "could not map ov-$t"; done
sleep 0.5
E2_SETTLED="$(widths)"
zoomit
E2_OVERVIEW="$(widths)"
E2_PCT="$(pct_of "$(printf '%s' "$E2_OVERVIEW" | cut -d, -f1)" "$(printf '%s' "$E2_SETTLED" | cut -d, -f1)")"
info "6 clients: settled=[$E2_SETTLED] overview=[$E2_OVERVIEW]"
if [ "$E2_OVERVIEW" != "$E2_SETTLED" ] && [ "$E2_PCT" -ge 70 ] && [ "$E2_PCT" -le 82 ]; then
    ok "six clients enter at the same reduction (${E2_PCT}% of settled)"
else
    bad "six clients were not reduced as designed: $E2_SETTLED -> $E2_OVERVIEW"
fi
for _ in 1 2 3 4 5 6 7 8; do msg "overview_nav:left" >/dev/null; done
msg "overview_nav:right"
sync_tile || info "server geometry lagged the model before the E2 hover"
E2_W="$(widths)"
E2_WIN="$(win_of ov-c)"
E2_RECT="$(rect_of "$E2_WIN")"
E2_X="$(printf '%s' "$E2_RECT" | awk '{print $1}')"
E2_Y="$(printf '%s' "$E2_RECT" | awk '{print $2}')"
E2_WD="$(printf '%s' "$E2_RECT" | awk '{print $3}')"
E2_H="$(printf '%s' "$E2_RECT" | awk '{print $4}')"
E2_PX=$((E2_X + E2_WD - 50)); [ "$E2_PX" -gt 1910 ] && E2_PX=1910
E2_PY=$((E2_Y + E2_H / 2))
if [ "$E2_PX" -le "$E2_X" ]; then
    bad "ov-c is fully off screen, cannot hover it ($E2_RECT)"
else
    DBG_PRE="$(xdotool getmouselocation 2>/dev/null | sed 's/ screen.*//;s/window:.*//')"
    # Park the pointer on the bare root first, then onto the target: a pointer
    # already inside the window it is moved onto crosses no boundary, so the
    # server emits no EnterNotify and there is nothing to select. The detour
    # makes the crossing real, which is what the assertion is about.
    xdotool mousemove --sync "$((SCREEN_W - 1))" "$((SCREEN_H - 1))" >/dev/null 2>&1
    xdotool mousemove --sync "$E2_PX" "$E2_PY" >/dev/null 2>&1
    sleep 1.2
    [ "$(active_win)" = "$(hex_of "$E2_WIN")" ] \
        && ok "hovering on a long ribbon sticks to the selected tile" \
        || bad "hover selection ran away: $(active_win) want $(hex_of "$E2_WIN")"
    [ "$(widths)" = "$E2_W" ] \
        && ok "the cascade-free viewport rescales nothing ($E2_W)" \
        || bad "widths moved during the hover: $E2_W -> $(widths)"
    [ "$(float_of ov-c)" = '"float":false' ] \
        && ok "and the tile stayed tiled" \
        || bad "hover floated the tile"
fi
for t in e f g; do close_win "ov-$t" || bad "ov-$t did not go away"; done
sleep 0.4
settle
zoomit
PEER="$(rect_of "$(win_of ov-b)" | awk '{print $3}')"
spawn_win ov-d || bad "could not map ov-d"
wait_windows 4 300 || bad "ov-d never reached its tile"
sleep 0.5
NEW_W="$(rect_of "$(win_of ov-d)" | awk '{print $3}')"
D=$((NEW_W - PEER)); [ $D -lt 0 ] && D=$((-D))
if [ "$D" -le 2 ]; then
    ok "a window opened during Overview is projected at the same scale (${NEW_W} vs ${PEER})"
else
    bad "a window opened during Overview has an alien width: $NEW_W vs $PEER"
fi
settle
PEER_SETTLED="$(rect_of "$(win_of ov-b)" | awk '{print $3}')"
NEW_SETTLED="$(rect_of "$(win_of ov-d)" | awk '{print $3}')"
D=$((NEW_SETTLED - PEER_SETTLED)); [ $D -lt 0 ] && D=$((-D))
if [ "$D" -le 2 ]; then
    ok "it also settles at its full logical width (${NEW_SETTLED} vs ${PEER_SETTLED})"
else
    bad "it did not settle at full width: $NEW_SETTLED vs $PEER_SETTLED"
fi

zoomit
CLOSED_W="$(widths)"
close_win ov-d || bad "ov-d did not go away"
sleep 0.4
AFTER_CLOSE="$(widths)"
info "after closing one client: overview=[$CLOSED_W] -> [$AFTER_CLOSE]"
# Peers keep their widths (fixed scale); only the membership changed. The
# closed window's entry is gone and every survivor still measures exactly what
# it measured before, so the check is on the surviving set rather than on a
# position in the comma list (which is XID order, not creation order).
N_CLOSED="$(printf '%s' "$CLOSED_W" | awk -F, '{print NF}')"
N_AFTER="$(printf '%s' "$AFTER_CLOSE" | awk -F, '{print NF}')"
SURVIVORS_OK=1
if [ "$N_AFTER" -ne $((N_CLOSED - 1)) ]; then
    SURVIVORS_OK=0
else
    UNIQ_CLOSED="$(printf '%s\n' "$CLOSED_W" | tr ',' '\n' | sort -u | paste -sd, -)"
    UNIQ_AFTER="$(printf '%s\n' "$AFTER_CLOSE" | tr ',' '\n' | sort -u | paste -sd, -)"
    [ "$UNIQ_CLOSED" = "$UNIQ_AFTER" ] || SURVIVORS_OK=0
fi
if [ "$SURVIVORS_OK" = 1 ]; then
    ok "the survivors keep their widths after a close ($AFTER_CLOSE)"
else
    bad "a close rescaled the survivors: $CLOSED_W -> $AFTER_CLOSE"
fi
settle

# ── G. manual resize during Overview: logical weight, no extra zoom ─────────
settle
FOC="$(active_win)"
FB="$(rect_of "$FOC" | awk '{print $3}')"
zoomit
Z0="$(rect_of "$FOC" | awk '{print $3}')"
msg "grow_col_pct 20"
ZGROWN="$(rect_of "$FOC" | awk '{print $3}')"
settle
FA="$(rect_of "$FOC" | awk '{print $3}')"
if [ "$FA" -gt "$FB" ]; then
    ok "grow_col during Overview widened the logical column (${FB} -> ${FA} settled)"
else
    bad "grow_col during Overview did not widen the logical column: ${FB} -> ${FA}"
fi
# The resize is visible through the viewport, but it is still the same entry
# scale — wider than the pre-resize projection and narrower than the settled
# column it grew into. A second, accumulated reduction would show up as
# ZGROWN <= Z0 or ZGROWN == FA-with-a-shrunken-FA.
if [ "$ZGROWN" -gt "$Z0" ] && [ "$ZGROWN" -lt "$FA" ]; then
    ok "the same resize is visible through the viewport at the entry scale (${Z0} -> ${ZGROWN} -> ${FA} settled)"
else
    bad "the resize while in Overview did not project at the entry scale: ${Z0} -> ${ZGROWN} (settled ${FA})"
fi

# ── H. floating clients are not part of the ribbon ──────────────────────────
settle
FLOATER="$(active_win)"
FLOATER_TITLE="$(focus_title)"
msg toggle_float
sleep 0.5
[ "$(float_of "$FLOATER_TITLE")" = '"float":true' ] \
    && ok "the window is floating now" \
    || bad "toggle_float did not float ($FLOATER_TITLE: $(float_of "$FLOATER_TITLE"))"
R_SETTLED="$(rect_of "$FLOATER")"
zoomit
R_ZOOMED="$(rect_of "$FLOATER")"
if [ "$R_SETTLED" = "$R_ZOOMED" ]; then
    ok "a floating window is not moved by the viewport ($R_ZOOMED)"
else
    bad "a floating window changed under the viewport: $R_SETTLED -> $R_ZOOMED"
fi
# A float drag moves the float and only the float.
mod_drag "$FLOATER" 120 60
R_DRAGGED="$(rect_of "$FLOATER")"
if [ "$R_DRAGGED" != "$R_ZOOMED" ]; then
    ok "an explicit Mod4+drag moves the float ($R_ZOOMED -> $R_DRAGGED)"
else
    bad "an explicit drag did not move the float"
fi
[ "$(float_of "$FLOATER_TITLE")" = '"float":true' ] \
    && ok "and it is still floating afterwards" \
    || bad "the drag untiled nothing but lost the float state"
settle
R_AGAIN="$(rect_of "$FLOATER")"
[ "$R_AGAIN" = "$R_DRAGGED" ] \
    && ok "and keeps its dragged rectangle on exit ($R_AGAIN)" \
    || bad "the float did not keep its rect: $R_DRAGGED -> $R_AGAIN"
msg toggle_float      # un-float it again, the same verb that floated it
sleep 0.4
# Nothing to re-sync: `toggle_float` does not move the view, and the tracker
# already says "settled", which is where the workspace is.

# ── I. float a tile *during* Overview: no shrunk float ──────────────────────
zoomit
TILE_WIN="$(win_of ov-a)"
TILE_W="$(rect_of "$TILE_WIN" | awk '{print $3}')"
msg toggle_float
sleep 0.4
FLOAT_W="$(rect_of "$TILE_WIN" | awk '{print $3}')"
if [ "$FLOAT_W" -ge "$TILE_W" ]; then
    ok "floating during Overview does not shrink the window ($TILE_W -> $FLOAT_W)"
else
    bad "floating during Overview shrank the window: $TILE_W -> $FLOAT_W"
fi
settle
msg toggle_float
sleep 0.4

# ── J. workspace switch during Overview ─────────────────────────────────────
zoomit
ZOOM_W="$(widths)"
"$MAVERICK_CTL" view goto 2 >/dev/null 2>&1
sleep 0.5
"$MAVERICK_CTL" view goto 1 >/dev/null 2>&1
sleep 0.5
BACK="$(widths)"
if [ "$BACK" = "$ZOOM_W" ]; then
    ok "the workspace's overview survives a switch away and back ($BACK)"
else
    bad "the overview did not survive a workspace round trip: $ZOOM_W -> $BACK"
fi
settle

# ── K. an external resize must not be adopted as the tile ───────────────────
if [ -x "$HERE/winmove" ]; then
    TILE_WIN="$(win_of ov-a)"
    zoomit
    "$HERE/winmove" "$TILE_WIN" 40 40 640 480 >/dev/null 2>&1
    sleep 0.6
    RE="$(rect_of "$TILE_WIN")"
    [ "$RE" != "40 40 640 480" ] \
        && ok "a tiled client's own XResizeWindow is not adopted as its tile ($RE)" \
        || bad "a tiled client's XResizeWindow was adopted: $RE"
    settle
    info "tile after the external resize: $(rect_of "$TILE_WIN")"
else
    info "winmove not buildable; external-resize check skipped"
fi

# ── L. fullscreen during Overview keeps priority ────────────────────────────
FOCUSED_WIN="$(active_win)"
FOCUSED_TITLE="$(focus_title)"
zoomit
msg toggle_fullscreen
sleep 0.6
FS="$(rect_of "$FOCUSED_WIN")"
FS_W="$(printf '%s\n' "$FS" | awk '{print $3}')"
info "fullscreen under overview: $FS (screen ${SCREEN_W}x${SCREEN_H})"
[ "$FS_W" = "$SCREEN_W" ] \
    && ok "fullscreen fills the monitor even under Overview" \
    || bad "fullscreen under Overview is not screen-sized: $FS"
# ...and the mode is still on around it: navigating keeps the flag.
msg "overview_nav:right"
if "$MAVERICK_CTL" query tree 2>/dev/null | grep -oE "\"title\":\"$FOCUSED_TITLE\"[^}]*\"fullscreen\":[a-z]+" | grep -q '"fullscreen":true'; then
    ok "the fullscreen flag survives navigation in Overview"
else
    bad "navigation in Overview dropped the fullscreen flag"
fi
msg toggle_fullscreen
sleep 0.5
settle

# ── M. a long ribbon: stable scale, travelling viewport ─────────────────────
for i in 1 2 3 4 5 6; do spawn_win "lng-$i" || bad "could not map lng-$i"; done
wait_windows 9 || bad "the nine clients never reached their tiles"
sleep 0.6
LONG_SETTLED="$(widths)"
LONG_SETTLED_RECT="$(rects)"
zoomit
LONG_Z="$(widths)"
info "9 clients: settled=[$LONG_SETTLED] overview=[$LONG_Z]"
LONG_PCT="$(pct_of "$(printf '%s' "$LONG_Z" | cut -d, -f1)" "$(printf '%s' "$LONG_SETTLED" | cut -d, -f1)")"
if [ "$LONG_Z" != "$LONG_SETTLED" ] && [ "$LONG_PCT" -ge 70 ] && [ "$LONG_PCT" -le 82 ]; then
    ok "nine tiles enter at the same reduction (${LONG_PCT}% of settled)"
else
    bad "nine tiles did not enter reduced as designed: $LONG_SETTLED -> $LONG_Z"
fi
MINW="$(printf '%s\n' "$LONG_Z" | tr ',' '\n' | sort -n | head -1)"
if [ "${MINW:-0}" -ge 1 ]; then
    ok "every tile stays a real rectangle (narrowest ${MINW} px)"
else
    bad "a tile degenerated: narrowest ${MINW} px"
fi
SAME_O=1; SAME_I=1; DRIFT_O=""; DRIFT_I=""
for _ in 1 2 3 4 5; do
    settle; [ "$(widths)" = "$LONG_SETTLED" ] || { SAME_I=0; DRIFT_I="$(widths)"; }
    zoomit; [ "$(widths)" = "$LONG_Z" ]        || { SAME_O=0; DRIFT_O="$(widths)"; }
done
[ "$SAME_O" = 1 ] \
    && ok "a long ribbon's overview widths are stable under repetition" \
    || bad "a long ribbon's overview widths drift: $LONG_Z -> $DRIFT_O"
[ "$SAME_I" = 1 ] \
    && ok "the long ribbon's settled widths are stable under repetition" \
    || bad "the long ribbon's settled widths drift: $LONG_SETTLED -> $DRIFT_I"
# ...and the full rectangles come back exactly once the mode is left again:
# positions, heights and widths all restored, so leaving Overview is a round
# trip and not a re-layout. `settle`/`zoomit` around it keeps the VIEW tracker
# in step, which the walk below relies on.
settle
[ "$(rects)" = "$LONG_SETTLED_RECT" ] \
    && ok "the nine-client round trip restores every rectangle exactly" \
    || bad "the nine-client round trip left geometry behind: $(rects) want $LONG_SETTLED_RECT"
zoomit
# Walk the whole strip: every selection must land on screen (x within the
# monitor), widths never moving.
VISIBLE_OK=1
goto_end() { for _ in $(seq 1 12); do msg "overview_nav:right" >/dev/null; done; }
goto_start() { for _ in $(seq 1 12); do msg "overview_nav:left" >/dev/null; done; }
goto_start
for _ in $(seq 1 8); do
    msg "overview_nav:right" >/dev/null
    sync_tile || { VISIBLE_OK=0; continue; }
    FX="$(rect_of "$(active_win)" | awk '{print $1}')"
    FW="$(rect_of "$(active_win)" | awk '{print $3}')"
    if [ "$FX" -lt 0 ] || [ $((FX + FW)) -gt "$SCREEN_W" ]; then
        VISIBLE_OK=0
    fi
    [ "$(widths)" = "$LONG_Z" ] || VISIBLE_OK=0
done
[ "$VISIBLE_OK" = 1 ] \
    && ok "walking nine columns keeps every selection on screen at a fixed scale" \
    || bad "a selection left the screen or rescaled mid-walk"
settle

# ── N. exit leaves no grab behind ────────────────────────────────────────────
# After the last settle the mode is off. Navigate home to col0 so ov-a is
# fully on screen, leave Overview, then hover it: the hover must still select
# (a stranded SYNC grab or an active drag grab would freeze the pointer and
# this would time out on the old focus).
for _ in $(seq 1 12); do msg "overview_nav:left" >/dev/null; done
# The navs above entered Overview (OverviewNav doubles as enter) while the
# `VIEW` tracker still says settled, so exit with a direct toggle and keep the
# tracker as-is.
msg toggle_overview
sleep 0.4
N_WIN="$(win_of ov-a)"
sync_tile || info "server geometry lagged the model before the final hover"
N_RECT="$(rect_of "$N_WIN")"
NX="$(printf '%s' "$N_RECT" | awk '{print $1}')"
NY="$(printf '%s' "$N_RECT" | awk '{print $2}')"
NW="$(printf '%s' "$N_RECT" | awk '{print $3}')"
NH="$(printf '%s' "$N_RECT" | awk '{print $4}')"
xdotool mousemove --sync $((NX + NW / 2)) $((NY + NH / 2)) >/dev/null 2>&1
sleep 0.6
[ "$(active_win)" = "$(hex_of "$N_WIN")" ] \
    && ok "the pointer still selects after leaving Overview (no stranded grab)" \
    || bad "the pointer is dead after Overview: $(active_win) want $(hex_of "$N_WIN")"

# ── result ───────────────────────────────────────────────────────────────────
echo
if [ -d "$SHOT_DIR" ] && ls "$SHOT_DIR"/*.ppm >/dev/null 2>&1; then
    info "captured frames: $(ls "$SHOT_DIR"/*.ppm | tr '\n' ' ')"
fi
echo "overview suite: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
