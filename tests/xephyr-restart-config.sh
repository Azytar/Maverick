#!/usr/bin/env bash
#
# F2 — config-reuse across `maverick-msg restart` (CRITICAL).
#
# Validates that `restart` re-execs with EXACTLY the same `--config <path>`:
#   1. launch maverick --config <distinctive file>
#   2. assert the distinctive setting (n_tags=5) is live via `maverickctl state`
#   3. assert the process cmdline carries --config <path>
#   4. restart once; assert the new instance is alive, same config (state + cmdline)
#   5. loop restart 3x; assert config stays identical every time
#
# Run: bash tests/xephyr-restart-config.sh

set -u
source "$(dirname "$0")/common.sh"
mav_preflight
build_helpers
trap mav_cleanup EXIT ERR

PASS=0; FAIL=0
ok()  { echo "PASS: $*"; PASS=$((PASS+1)); }
bad() { echo "FAIL: $*"; FAIL=$((FAIL+1)); }

DISP=":98"
XEP="$(start_xephyr "$DISP" 1280 720)"

CFG="$(mktemp /tmp/maverick-f2.XXXX.toml)"
cat > "$CFG" <<'EOF'
[general]
n_tags = 5
border_width = 9

[colors]
focused = 0x0badf0
EOF
echo "F2 config: $CFG (n_tags=5)"

MAV="$(mav_launch "$DISP" --config "$CFG")"
echo "maverick pid=$MAV"

NWS="$(ws_count "$DISP")"
[ "$NWS" = "5" ] && ok "config loaded: n_tags=5 (state reports $NWS workspaces)" \
                  || bad "config NOT loaded: state reports $NWS workspaces (expected 5)"
tr '\0' ' ' </proc/$MAV/cmdline 2>/dev/null | grep -q -- "--config $CFG" \
    && ok "cmdline carries --config" \
    || bad "cmdline missing --config ($(tr '\0' ' ' </proc/$MAV/cmdline 2>/dev/null))"

# ── single restart ───────────────────────────────────────────────────────────
DISPLAY="$DISP" "$MAVERICK_CTL" restart >/dev/null 2>&1
sleep 3
if alive "$MAV"; then ok "alive after restart (re-exec kept pid $MAV)"; else bad "maverick DIED on restart"; fi
NWS2="$(ws_count "$DISP")"
[ "$NWS2" = "5" ] && ok "config REUSED after restart: n_tags still 5" \
                   || bad "config LOST after restart: n_tags=$NWS2"
tr '\0' ' ' </proc/$MAV/cmdline 2>/dev/null | grep -q -- "--config $CFG" \
    && ok "cmdline still carries --config after restart" \
    || bad "cmdline --config lost after restart"

# ── repeated restart (3x) keeps the same config ───────────────────────────────
for i in 1 2 3; do
    DISPLAY="$DISP" "$MAVERICK_CTL" restart >/dev/null 2>&1
    sleep 2.5
    alive "$MAV" || { bad "maverick died on restart iteration $i"; break; }
    N="$(ws_count "$DISP")"
    if [ "$N" = "5" ] && tr '\0' ' ' </proc/$MAV/cmdline 2>/dev/null | grep -q -- "--config $CFG"; then
        ok "restart #$i: alive, n_tags=5, --config intact"
    else
        bad "restart #$i: broken (n_tags=$N, --config=$(grep -qa -- "--config $CFG" /proc/$MAV/cmdline 2>/dev/null && echo yes || echo no))"
    fi
done

echo "────────────────────────────────────"
echo "F2 config-reuse: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
