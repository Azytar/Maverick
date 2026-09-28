#!/usr/bin/env bash
# Security verification for Maverick sessions.
#
# Checks the four boundaries that keep one user's session out of another user's
# reach. Three of them can be checked as the session's own user; the fourth —
# an actual *other* user being refused — needs a second account and is not
# skipped silently: without one the script says so and exits non-zero, because
# a security check that quietly reports success without running is worse than
# no check.
#
# Usage:
#   tests/session-security.sh [session-name]
#
# With a second account available (set PROBE_USER, or pass one as $2):
#   sudo -u nobody PROBE_USER=nobody tests/session-security.sh
# but note `nobody` has no writable HOME/XDG_RUNTIME_DIR, so a real account is
# better; the script only needs to *attempt* the operations and see them denied.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
: "${MAVERICK_BIN:=${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/maverick}"
: "${MAVERICKCTL_BIN:=${CARGO_TARGET_DIR:-$REPO_ROOT/target}/debug/maverickctl}"
MAVERICK_BIN="$(realpath "$MAVERICK_BIN" 2>/dev/null || echo "$MAVERICK_BIN")"
MAVERICKCTL_BIN="$(realpath "$MAVERICKCTL_BIN" 2>/dev/null || echo "$MAVERICKCTL_BIN")"

SESSION="${1:-secprobe}"
PROBE_USER="${2:-${PROBE_USER:-}}"

PASS=0
FAIL=0
SKIP=0

ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; FAIL=$((FAIL+1)); }
skip() { printf '  \033[33mSKIP\033[0m  %s\n' "$*"; SKIP=$((SKIP+1)); }

mode_of() { stat -c '%a' "$1" 2>/dev/null || echo "?"; }

echo "== Maverick session security =="
echo "   maverickctl: $MAVERICKCTL_BIN"
echo "   session:     $SESSION"
echo

# ── 0. a session to attack ────────────────────────────────────────────────────
# A session that exists but is not *running* is worse than no session: every
# assertion below reads a file the running session would have written, so a
# crashed leftover produces two failures that say nothing about permissions or
# about authentication. `crashed` and `stopped` are exactly the states to
# recreate; `running` is the one to keep, since that is the state under test.
# Only existence was checked before, which is why a session killed by an
# unrelated test run turned this suite red with no product change involved.
SEC_STATE=$("$MAVERICKCTL_BIN" session list 2>/dev/null | awk -v s="$SESSION" '$1==s {print $NF; exit}')
if [ "$SEC_STATE" != "running" ]; then
    if [ -n "$SEC_STATE" ]; then
        echo "removing '$SESSION' (state: ${SEC_STATE:-unknown}) and creating it fresh…"
        "$MAVERICKCTL_BIN" session remove "$SESSION" --force >/dev/null 2>&1 || true
    else
        echo "creating session '$SESSION'…"
    fi
    "$MAVERICKCTL_BIN" session create "$SESSION" --binary "$MAVERICK_BIN" \
        --resolution 640x480 >/dev/null || { echo "could not create the session"; exit 1; }
fi

RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/maverick"
SESSION_DIR="$RUNTIME_DIR/$SESSION"
SOCK="$SESSION_DIR/control.sock"
XAUTH="$SESSION_DIR/Xauthority"
LOG="$SESSION_DIR/maverick.log"
RECORD="$SESSION_DIR/session.json"
DISPLAY_NUM="$("$MAVERICKCTL_BIN" session list --json 2>/dev/null |
    python3 -c 'import json,sys; print(next(s["display"] for s in json.load(sys.stdin)["sessions"] if s["name"]=="'"$SESSION"'"))' 2>/dev/null)"

# ── 1. permissions ────────────────────────────────────────────────────────────
echo "1. filesystem permissions"
[ "$(mode_of "$RUNTIME_DIR")" = "700" ] \
    && ok "runtime directory is 0700" \
    || bad "runtime directory is $(mode_of "$RUNTIME_DIR"), want 700"
[ "$(mode_of "$SESSION_DIR")" = "700" ] \
    && ok "session directory is 0700" \
    || bad "session directory is $(mode_of "$SESSION_DIR"), want 700"
[ "$(mode_of "$SOCK")" = "600" ] \
    && ok "control socket is 0600" \
    || bad "control socket is $(mode_of "$SOCK"), want 600"
[ "$(mode_of "$XAUTH")" = "600" ] \
    && ok "Xauthority is 0600" \
    || bad "Xauthority is $(mode_of "$XAUTH"), want 600"
[ "$(mode_of "$RECORD")" = "600" ] \
    && ok "session record is 0600" \
    || bad "session record is $(mode_of "$RECORD"), want 600"
[ "$(mode_of "$LOG")" = "600" ] \
    && ok "session log is 0600" \
    || bad "session log is $(mode_of "$LOG"), want 600"
echo

# ── 2. no secret in any readable output ───────────────────────────────────────
echo "2. no credential in any document the tool writes"
# A real cookie, read as this user, must not appear in any output.
COOKIE="$(python3 - "$XAUTH" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
# The file is a sequence of entries; the last field of each is the 16-byte
# secret. Take any 16 printable-hex-looking run, which is what a leak would be.
import re
m = re.search(rb'MIT-MAGIC-COOKIE-1.{16}', data, re.S)
print(m.group(0)[16:].hex() if m else '')
PY
)"
if [ -z "$COOKIE" ]; then
    bad "could not read the session's cookie to search for"
else
    LEAK=0
    for cmd in \
        "session list" "session list --json" "session status $SESSION" \
        "session status $SESSION --json" "inspect $SESSION" "inspect $SESSION --json" \
        "process list $SESSION --json" "window list $SESSION --json" "list"
    do
        out="$("$MAVERICKCTL_BIN" $cmd 2>&1)"
        case "$out" in
            *"$COOKIE"*) bad "'$cmd' leaked the X cookie"; LEAK=1 ;;
        esac
    done
    [ "$LEAK" = 0 ] && ok "no cookie in any listing, status, inspect or query output"
    # And not in the logs either: the log is 0600, but a leak *into* it would
    # still be a leak into anything that reads it.
    if grep -qF "$COOKIE" "$LOG" 2>/dev/null; then
        bad "the session log contains the X cookie"
    else
        ok "no cookie in the session log"
    fi
fi
echo

# ── 3. X11 authentication ─────────────────────────────────────────────────────
echo "3. X11 authentication on the session's display"
if [ -z "$DISPLAY_NUM" ]; then
    skip "no display known for '$SESSION'"
else
    # No cookie at all: refused.
    if DISPLAY="$DISPLAY_NUM" XAUTHORITY=/nonexistent xprop -root >/dev/null 2>&1; then
        bad "a client with no cookie was ACCEPTED on $DISPLAY_NUM"
    else
        ok "a client with no cookie is refused on $DISPLAY_NUM"
    fi
    # A wrong cookie: refused.
    BAD_COOKIE="$(mktemp)"; chmod 600 "$BAD_COOKIE"
    python3 - "$BAD_COOKIE" "$DISPLAY_NUM" <<'PY'
import struct, sys
def entry(fam, addr, num, name, data):
    out = struct.pack('>H', fam)
    for f in (addr, num, name, data):
        out += struct.pack('>H', len(f)) + f
    return out
secret = bytes(range(16))
name = b"MIT-MAGIC-COOKIE-1"
open(sys.argv[1], 'wb').write(
    entry(256, b'', sys.argv[2].encode(), name, secret) +
    entry(65535, b'', b'', name, secret))
PY
    if DISPLAY="$DISPLAY_NUM" XAUTHORITY="$BAD_COOKIE" xprop -root >/dev/null 2>&1; then
        bad "a client with a WRONG cookie was ACCEPTED on $DISPLAY_NUM"
    else
        ok "a client with a wrong cookie is refused on $DISPLAY_NUM"
    fi
    rm -f "$BAD_COOKIE"
    # The right cookie: accepted, or the check is not testing anything.
    if DISPLAY="$DISPLAY_NUM" XAUTHORITY="$XAUTH" xprop -root >/dev/null 2>&1; then
        ok "the session's own cookie is accepted (so the refusals above mean something)"
    else
        bad "the session's own cookie was refused — the test is not measuring auth"
    fi
    # The display is not on the network.
    if ss -ltnp 2>/dev/null | grep -q ":${DISPLAY_NUM#:} "; then
        bad "$DISPLAY_NUM appears to be listening on TCP"
    else
        ok "$DISPLAY_NUM is not listening on TCP"
    fi
fi
echo

# ── 4. a different user ───────────────────────────────────────────────────────
echo "4. a different user"
if [ -z "$PROBE_USER" ]; then
    skip "no probe user: set PROBE_USER=<name>, or pass one as the second argument"
    echo "        (the cross-user half cannot be verified from one account)"
else
    if ! id "$PROBE_USER" >/dev/null 2>&1; then
        skip "probe user '$PROBE_USER' does not exist"
    elif [ "$(id -u "$PROBE_USER")" = "$(id -u)" ]; then
        skip "probe user '$PROBE_USER' is this user"
    else
        # Run every privileged operation as the other user and require each to
        # be refused. `su`/`runuser` is used because that is what a real second
        # account can do; nothing here needs privileges to *run*, only to switch.
        probe() {
            local desc="$1"; shift
            if su -s /bin/sh -c "$*" "$PROBE_USER" >/dev/null 2>&1; then
                bad "$PROBE_USER was ALLOWED to $desc"
            else
                ok "$PROBE_USER is refused: $desc"
            fi
        }
        SU="su -s /bin/sh -c"
        # A private runtime dir for the probe user, so the refusal is about
        # *our* session and not about the probe having nowhere to look.
        probe "read the session list" \
            "$SU 'XDG_RUNTIME_DIR=$RUNTIME_DIR \"$MAVERICKCTL_BIN\" session list --json'"
        probe "read the session record" \
            "$SU 'cat \"$RECORD\"'"
        probe "read the session log" \
            "$SU 'cat \"$LOG\"'"
        probe "read the X cookie" \
            "$SU 'cat \"$XAUTH\"'"
        probe "send an action to the window manager" \
            "$SU 'printf \"focus-left\\n\" | timeout 5 nc -U \"$SOCK\"'"
        probe "quit the window manager" \
            "$SU 'printf \"quit\\n\" | timeout 5 nc -U \"$SOCK\"'"
        probe "connect to the session's display" \
            "$SU 'XAUTHORITY=\"$XAUTH\" DISPLAY=\"$DISPLAY_NUM\" xprop -root'"
        WM_PID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["wm_pid"])' "$RECORD" 2>/dev/null || echo "")"
        if [ -n "$WM_PID" ]; then
            # Signalling the WM is the sharpest test: it is the operation that
            # would be most damaging if another user could do it.
            probe "signal the window manager" \
                "$SU 'kill -0 $WM_PID'"
        fi
        # And the WM must still be there: a refusal that killed the session
        # would be a different failure.
        if "$MAVERICKCTL_BIN" session list 2>/dev/null | grep -qE "^${SESSION}[[:space:]].*running"; then
            ok "the session survived every attempt"
        else
            bad "the session did not survive the cross-user attempts"
        fi
    fi
fi
echo

echo "-------------------------------------------"
printf 'passed %d, failed %d, skipped %d\n' "$PASS" "$FAIL" "$SKIP"
[ "$FAIL" -eq 0 ] || exit 1
# A skipped cross-user half is a real gap: say so in the exit status when it
# was the only thing missing, so CI cannot report a green run that never
# checked the boundary it exists to check.
if [ "$SKIP" -gt 0 ]; then
    echo "NOTE: something was skipped; see the SKIP lines above."
    exit 2
fi
exit 0
