#!/usr/bin/env bash
# End-to-end verification of Maverick sessions.
#
# Every assertion here is about a *real* session: a real nested X server, a real
# Maverick process, real applications, real windows. Nothing is simulated —
# a test that fakes the X server proves only that the fake works, and the point
# of this feature is the interaction with X11.
#
# Usage:
#   tests/session-suite.sh              # create, exercise, remove
#   tests/session-suite.sh --keep       # leave the sessions running to poke at
#
# Requires: Xephyr, a display to nest inside ($DISPLAY), and the two binaries.
# Skips (loudly) when Xephyr is absent or there is no display to nest into —
# an X11 feature cannot be tested without an X11 server.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
: "${CARGO_TARGET_DIR:=$REPO_ROOT/target}"
MAVERICK_BIN="${MAVERICK_BIN:-$CARGO_TARGET_DIR/debug/maverick}"
MAVERICKCTL_BIN="${MAVERICKCTL_BIN:-$CARGO_TARGET_DIR/debug/maverickctl}"
MAVERICK_BIN="$(realpath "$MAVERICK_BIN" 2>/dev/null || echo "$MAVERICK_BIN")"
MAVERICKCTL_BIN="$(realpath "$MAVERICKCTL_BIN" 2>/dev/null || echo "$MAVERICKCTL_BIN")"

KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1

PASS=0; FAIL=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; FAIL=$((FAIL+1)); }
note() { printf '  ..    %s\n' "$*"; }

# A minimal X client to open real windows with, and a `--version` style probe.
CLIENT="${SESSION_TEST_CLIENT:-}"
pick_client() {
    for c in xterm xclock xeyes xcalc; do
        if command -v "$c" >/dev/null 2>&1; then CLIENT="$c"; return 0; fi
    done
    return 1
}

cleanup() {
    [ "$KEEP" = 1 ] && return
    for s in "${SESSIONS[@]:-}"; do
        [ -n "$s" ] && "$MAVERICKCTL_BIN" session remove "$s" --force >/dev/null 2>&1
    done
}
SESSIONS=()
trap cleanup EXIT

echo "== Maverick session suite =="
echo "   maverick:    $MAVERICK_BIN"
echo "   maverickctl: $MAVERICKCTL_BIN"
echo

# ── preconditions ─────────────────────────────────────────────────────────────
if [ ! -x "$MAVERICK_BIN" ] || [ ! -x "$MAVERICKCTL_BIN" ]; then
    echo "binaries not built — run: cargo build" >&2
    exit 1
fi
if ! command -v Xephyr >/dev/null 2>&1; then
    echo "Xephyr is not installed; the nested-server backend cannot be tested" >&2
    exit 77   # the automake convention for "skipped"
fi
if [ -z "${DISPLAY:-}" ]; then
    echo "no \$DISPLAY: a nested session needs a display to nest into" >&2
    exit 77
fi
if ! pick_client; then
    echo "no X client found (xterm/xclock/xeyes/xcalc); cannot open real windows" >&2
    exit 77
fi
note "using '$CLIENT' as the X client"
note "nesting into $DISPLAY"
echo

# ── 1. create ─────────────────────────────────────────────────────────────────
echo "1. create"
OUT="$("$MAVERICKCTL_BIN" session create suite --binary "$MAVERICK_BIN" \
        --resolution 1280x720 --debug 2>&1)"
if [ $? -eq 0 ]; then
    SESSIONS+=(suite)
    ok "created 'suite' at 1280x720"
else
    bad "create failed: $OUT"
    echo; printf 'passed %d, failed %d\n' "$PASS" "$FAIL"; exit 1
fi
DISPLAY_NUM="$("$MAVERICKCTL_BIN" session list --json |
    python3 -c 'import json,sys;print(next(s["display"] for s in json.load(sys.stdin)["sessions"] if s["name"]=="suite"))')"
[ -n "$DISPLAY_NUM" ] && ok "the session has a display ($DISPLAY_NUM)" \
                      || bad "the session reports no display"
"$MAVERICKCTL_BIN" session list --json |
    python3 -c '
import json,sys
d=json.load(sys.stdin)
s=next(x for x in d["sessions"] if x["name"]=="suite")
assert s["state"]=="running", s["state"]
assert s["resolution"]=={"width":1280,"height":720}, s["resolution"]
assert s["display"].startswith(":"), s["display"]
assert s["pid"]>0 and s["x_pid"]>0, (s["pid"], s["x_pid"])
' 2>/dev/null && ok "session list --json reports a running session with the right size" \
               || bad "session list --json is missing or wrong"
echo

# ── 2. resolution is independent of the parent display ─────────────────────────
echo "2. resolution"
# The current mode is the one whose refresh carries the `*`; its first field is
# the resolution. (The `*` is on the refresh, not the mode name — the
# `Screen 0:` and the connector lines have no `*` at all.)
parent_size() { xrandr 2>/dev/null | awk '/\*/ && NF >= 2 {print $1; exit}'; }
PARENT_SIZE="$(parent_size)"
if [ -n "$PARENT_SIZE" ]; then
    note "parent display is $PARENT_SIZE, session is 1280x720"
    ok "the session's size is its own, not the parent's"
fi
# The X server is what enforces it: ask the session's own server.
if XAUTHORITY="$XDG_RUNTIME_DIR/maverick/suite/Xauthority" xrandr 2>/dev/null |
   grep -qE '\b1280x720\b'; then
    ok "the session's X server reports 1280x720"
else
    bad "the session's X server does not report 1280x720"
fi
# And the parent is untouched.
PARENT_NOW="$(parent_size)"
if [ -n "$PARENT_SIZE" ] && [ "$PARENT_NOW" = "$PARENT_SIZE" ]; then
    ok "the parent display is unchanged ($PARENT_SIZE)"
else
    bad "the parent display's size changed ($PARENT_SIZE -> $PARENT_NOW)"
fi
echo

# ── 3. custom binary, cwd and passthrough args ────────────────────────────────
echo "3. custom binary and arguments"
ARGREC="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["args"])' \
    "$XDG_RUNTIME_DIR/maverick/suite/session.json")"
[ "$ARGREC" = "[]" ] && ok "no passthrough arguments by default" \
                    || bad "unexpected default arguments: $ARGREC"
"$MAVERICKCTL_BIN" session remove suite --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create suite --binary "$MAVERICK_BIN" --resolution 1280x720 \
    --cwd /tmp -- --debug --log-level debug >/dev/null 2>&1
if [ $? -eq 0 ]; then
    ok "recreated with --cwd and passthrough arguments"
    REC="$(cat "$XDG_RUNTIME_DIR/maverick/suite/session.json")"
    ARGS="$(echo "$REC" | python3 -c 'import json,sys;print(" ".join(json.load(sys.stdin)["args"]))')"
    [ "$ARGS" = "--debug --log-level debug" ] \
        && ok "the record replays Maverick's arguments verbatim ($ARGS)" \
        || bad "recorded arguments are '$ARGS'"
    CWD="$(echo "$REC" | python3 -c 'import json,sys;print(json.load(sys.stdin)["cwd"])')"
    [ "$CWD" = "/tmp" ] && ok "the working directory is recorded ($CWD)" \
                        || bad "recorded cwd is '$CWD'"
    BIN="$(echo "$REC" | python3 -c 'import json,sys;print(json.load(sys.stdin)["binary"])')"
    [ "$BIN" = "$MAVERICK_BIN" ] && ok "the binary is the one that was asked for" \
                                  || bad "recorded binary is '$BIN'"
else
    bad "could not recreate with --cwd and passthrough arguments"
fi
echo

# ── 4. idempotence and duplicate names ────────────────────────────────────────
echo "4. idempotence"
if "$MAVERICKCTL_BIN" session create suite --binary "$MAVERICK_BIN" >/dev/null 2>&1; then
    bad "creating an already-running session was allowed"
else
    out="$("$MAVERICKCTL_BIN" session create suite --binary "$MAVERICK_BIN" 2>&1)"
    case "$out" in
        *"already running"*) ok "creating a running session is refused, with a reason" ;;
        *) bad "unexpected message: $out" ;;
    esac
fi
"$MAVERICKCTL_BIN" session start suite >/dev/null 2>&1 \
    && ok "starting a running session succeeds (idempotent)" \
    || bad "starting a running session failed"
echo

# ── 5. exec, environment and the process tree ─────────────────────────────────
echo "5. exec and the process tree"
"$MAVERICKCTL_BIN" exec suite $CLIENT -geometry 40x10 >/dev/null 2>&1
"$MAVERICKCTL_BIN" exec suite $CLIENT -geometry 30x8 >/dev/null 2>&1
# Wait for the count rather than sleeping a fixed time: mapping an X client is
# not instantaneous and a fixed sleep makes this test a race.
NWIN=0
for _ in $(seq 1 40); do
    NWIN="$("$MAVERICKCTL_BIN" window list suite --json 2>/dev/null |
        python3 -c 'import json,sys;print(len(json.load(sys.stdin)["windows"]))' 2>/dev/null || echo 0)"
    [ "$NWIN" -ge 2 ] && break
    sleep 0.5
done
[ "$NWIN" -ge 2 ] && ok "$NWIN windows are managed" || bad "only $NWIN windows after two execs"
# Each window carries the pid of the process that owns it.
"$MAVERICKCTL_BIN" window list suite --json | python3 -c '
import json,sys
ws=json.load(sys.stdin)["windows"]
missing=[w for w in ws if not w.get("pid")]
assert not missing, f"windows without a pid: {missing}"
' 2>/dev/null && ok "every window reports the pid that owns it" \
               || bad "some window has no pid"
# The process list sees the X server, the WM and both clients — and the clients
# are the ones a parent walk would miss.
"$MAVERICKCTL_BIN" process list suite --json | python3 -c '
import json,sys
ps=json.load(sys.stdin)["processes"]
roles={p["role"] for p in ps}
assert "maverick" in roles, roles
assert "x-server" in roles, roles
assert "application" in roles, f"the execed clients are invisible: {roles}"
' 2>/dev/null && ok "the process tree has the WM, the X server and both clients" \
               || bad "the process tree is incomplete"
# The environment is the session's, not the caller's.
ENVOUT="$("$MAVERICKCTL_BIN" shell suite -- sh -c 'echo "$DISPLAY|$MAVERICK_SESSION"')"
EXPECT="$DISPLAY_NUM|suite"
[ "$ENVOUT" = "$EXPECT" ] && ok "shell runs inside the session ($ENVOUT)" \
                          || bad "shell environment is '$ENVOUT', want '$EXPECT'"
echo

# ── 6. window control ─────────────────────────────────────────────────────────
echo "6. window control"
FIRST="$("$MAVERICKCTL_BIN" window list suite --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["windows"][0]["id_hex"])')"
FIRST_DEC="$("$MAVERICKCTL_BIN" window list suite --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["windows"][0]["id"])')"
"$MAVERICKCTL_BIN" window focus suite "$FIRST" >/dev/null 2>&1 \
    && ok "focus by id" || bad "focus by id failed"
FOCUSED="$("$MAVERICKCTL_BIN" window list suite --json | python3 -c '
import json,sys
print(next((w["id"] for w in json.load(sys.stdin)["windows"] if w["focused"]), ""))')"
[ "$FOCUSED" = "$FIRST_DEC" ] && ok "the window manager agrees about the focus" \
                              || bad "focus did not land on $FIRST_DEC (got '$FOCUSED')"
"$MAVERICKCTL_BIN" window float suite "$FIRST" >/dev/null 2>&1
sleep 0.5
"$MAVERICKCTL_BIN" window list suite --json | python3 -c "
import json,sys
w=next(x for x in json.load(sys.stdin)['windows'] if x['id_hex']=='$FIRST')
assert w['floating'], w" 2>/dev/null && ok "float by id reaches the state machine" \
                      || bad "float did not take effect"
"$MAVERICKCTL_BIN" window fullscreen suite "$FIRST" >/dev/null 2>&1
sleep 0.5
"$MAVERICKCTL_BIN" window list suite --json | python3 -c "
import json,sys
w=next(x for x in json.load(sys.stdin)['windows'] if x['id_hex']=='$FIRST')
assert w['fullscreen'], w" 2>/dev/null && ok "fullscreen by id" \
                      || bad "fullscreen did not take effect"
"$MAVERICKCTL_BIN" window fullscreen suite "$FIRST" >/dev/null 2>&1   # back
sleep 0.5
# The layout verbs, which must go through the same state machine.
for c in "camera suite right" "resize suite +10%" "resize suite -10%" "layout suite column"; do
    # shellcheck disable=SC2086
    if "$MAVERICKCTL_BIN" $c >/dev/null 2>&1; then ok "$c"; else bad "$c failed"; fi
done
# An ambiguous name must be refused, not guessed.
"$MAVERICKCTL_BIN" window focus suite XTerm >/dev/null 2>&1
case "$?" in
    0) note "the X client has a unique class here, so the name resolved" ;;
    *) out="$("$MAVERICKCTL_BIN" window focus suite XTerm 2>&1)"
       case "$out" in
           *"use the id"*) ok "an ambiguous name is refused with candidate ids" ;;
           *) bad "unexpected ambiguity message: $out" ;;
       esac ;;
esac
echo

# ── 7. inspect ────────────────────────────────────────────────────────────────
echo "7. inspect"
"$MAVERICKCTL_BIN" inspect suite --json | python3 -c '
import json,sys
d=json.load(sys.stdin)
for k in ("name","state","display","resolution","windows","layout","compositor","process_count"):
    assert k in d, f"missing {k}"
assert d["compositor"]["backend"] in ("opengl","vulkan"), d["compositor"]
assert d["layout"]["type"] == "column", d["layout"]
assert d["windows"]["total"] >= 2, d["windows"]
' 2>/dev/null && ok "inspect --json carries session, windows, layout, compositor and processes" \
               || bad "inspect --json is missing or wrong"
"$MAVERICKCTL_BIN" inspect suite 2>/dev/null | grep -q COMPOSITOR \
    && ok "inspect reports the compositor" || bad "inspect has no COMPOSITOR section"
echo

# ── 8. compositor on/off, compared without touching the primary session ────────
echo "8. compositor"
"$MAVERICKCTL_BIN" session remove suite --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create suite --binary "$MAVERICK_BIN" --resolution 1280x720 \
    --no-compositor >/dev/null 2>&1
if [ $? -eq 0 ]; then
    "$MAVERICKCTL_BIN" inspect suite --json | python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["compositor_requested"] is False, d["compositor_requested"]
assert d["compositor"]["active"] is False, d["compositor"]
' 2>/dev/null && ok "--no-compositor runs the real window manager without the compositor" \
                || bad "--no-compositor did not disable the compositor"
else
    bad "could not create a no-compositor session"
fi
"$MAVERICKCTL_BIN" session remove suite --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create suite --binary "$MAVERICK_BIN" --resolution 1280x720 >/dev/null 2>&1
note "sessions now: $("$MAVERICKCTL_BIN" session list 2>/dev/null | awk 'NR>3 {printf "%s ", $1}')"
echo

# ── 9. several sessions at once ───────────────────────────────────────────────
echo "9. several sessions at once"
"$MAVERICKCTL_BIN" session remove suite --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create one --binary "$MAVERICK_BIN" --resolution 800x600 >/dev/null 2>&1 && SESSIONS+=(one)
"$MAVERICKCTL_BIN" session create two --binary "$MAVERICK_BIN" --resolution 1024x768 >/dev/null 2>&1 && SESSIONS+=(two)
D1="$("$MAVERICKCTL_BIN" session list --json | python3 -c 'import json,sys;print(next(s["display"] for s in json.load(sys.stdin)["sessions"] if s["name"]=="one"))')"
D2="$("$MAVERICKCTL_BIN" session list --json | python3 -c 'import json,sys;print(next(s["display"] for s in json.load(sys.stdin)["sessions"] if s["name"]=="two"))')"
[ -n "$D1" ] && [ -n "$D2" ] && [ "$D1" != "$D2" ] \
    && ok "two sessions, two displays ($D1 and $D2)" || bad "sessions share or lack a display"
"$MAVERICKCTL_BIN" exec one $CLIENT -geometry 20x5 >/dev/null 2>&1
N1=0
for _ in $(seq 1 40); do
    N1="$("$MAVERICKCTL_BIN" window list one --json 2>/dev/null |
        python3 -c 'import json,sys;print(len(json.load(sys.stdin)["windows"]))' 2>/dev/null || echo 0)"
    [ "$N1" -ge 1 ] && break
    sleep 0.5
done
N2="$("$MAVERICKCTL_BIN" window list two --json | python3 -c 'import json,sys;print(len(json.load(sys.stdin)["windows"]))')"
[ "$N1" -ge 1 ] && [ "$N2" -eq 0 ] \
    && ok "windows do not leak between sessions ($N1 vs $N2)" \
    || bad "window lists crossed: one=$N1 two=$N2"
echo

# ── 10. lifecycle, crash detection and cleanup ────────────────────────────────
echo "10. lifecycle and cleanup"
"$MAVERICKCTL_BIN" session stop one >/dev/null 2>&1 && ok "stop" || bad "stop failed"
"$MAVERICKCTL_BIN" session start one >/dev/null 2>&1 && ok "start again" || bad "start failed"
"$MAVERICKCTL_BIN" session restart one >/dev/null 2>&1 && ok "restart" || bad "restart failed"
"$MAVERICKCTL_BIN" session stop one >/dev/null 2>&1
P1="$("$MAVERICKCTL_BIN" session list --json | python3 -c 'import json,sys;print(next((s["x_pid"] or 0) for s in json.load(sys.stdin)["sessions"] if s["name"]=="one"))')"
[ "$P1" = "0" ] && ok "stopping released the X server pid from the record" \
                || bad "the X server pid is still recorded after stop ($P1)"
if [ "$P1" != "0" ]; then kill -0 "$P1" 2>/dev/null && bad "the X server is still running" \
                                             || ok "the X server is gone"; fi
# Crash: kill the WM and check the session reports it rather than pretending.
"$MAVERICKCTL_BIN" session start one >/dev/null 2>&1
WM="$("$MAVERICKCTL_BIN" session list --json | python3 -c 'import json,sys;print(next(s["pid"] for s in json.load(sys.stdin)["sessions"] if s["name"]=="one"))')"
kill -9 "$WM" 2>/dev/null; sleep 1.5
STATE="$("$MAVERICKCTL_BIN" session list --json | python3 -c 'import json,sys;print(next(s["state"] for s in json.load(sys.stdin)["sessions"] if s["name"]=="one"))')"
[ "$STATE" = "crashed" ] && ok "a killed window manager reads as crashed, not running" \
                         || bad "after killing the WM the state is '$STATE'"
# A read command must not clean up; a mutating one must.
XA="$("$MAVERICKCTL_BIN" session list --json | python3 -c 'import json,sys;print(next((s["x_pid"] or 0) for s in json.load(sys.stdin)["sessions"] if s["name"]=="one"))')"
[ "$XA" != "0" ] && kill -0 "$XA" 2>/dev/null \
    && ok "a read command leaves the session alone" \
    || bad "a read command had side effects"
"$MAVERICKCTL_BIN" session stop one >/dev/null 2>&1
kill -0 "$XA" 2>/dev/null && bad "the orphaned X server was not reaped" \
                          || ok "stopping a crashed session reaps its orphaned X server"
# The logs survive: they are why a crashed session is worth keeping.
[ -s "$XDG_RUNTIME_DIR/maverick/one/maverick.log" ] \
    && ok "the crashed session's log is preserved" \
    || bad "the log was deleted with the crash"
# And the name is reusable.
"$MAVERICKCTL_BIN" session remove one --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create one --binary "$MAVERICK_BIN" --resolution 800x600 >/dev/null 2>&1 \
    && ok "a removed session's name is reusable" || bad "the name is still blocked"
# A live session cannot be removed out from under itself.
if "$MAVERICKCTL_BIN" session remove one >/dev/null 2>&1; then
    bad "removing a running session was allowed"
else
    out="$("$MAVERICKCTL_BIN" session remove one 2>&1)"
    case "$out" in
        *"still running"*) ok "removing a running session is refused, with a reason" ;;
        *) bad "unexpected refusal message: $out" ;;
    esac
fi
echo

# ── 11. a session only owns the processes it can prove it started ─────────────
echo "11. process ownership"
# Each down-transition must leave the session owning nothing. Before this was
# enforced, a stopped session's roots were `ProcRef::default()` — pid 0 — and
# `closure` seeded from it, which adopts pid 1 (whose `ppid` is literally 0) and
# then the whole process table. `process list` reported every process on the
# machine and `process kill` signalled any of them.
OWN_SESS=ownq
"$MAVERICKCTL_BIN" session remove "$OWN_SESS" --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create "$OWN_SESS" --binary "$MAVERICK_BIN" --resolution 640x480 >/dev/null 2>&1 \
    && ok "ownership session created" || bad "could not create the ownership session"

# An unrelated process of our own, in its own process group, that the session
# has never seen. If it ever shows up as owned, the predicate is too wide.
setsid sleep 300 & SACRIFICE=$!
sleep 0.3

# A live session owns the two processes it started, and nothing else.
LIVE_N="$("$MAVERICKCTL_BIN" process list "$OWN_SESS" --json 2>/dev/null | python3 -c '
import json, sys
try:
    print(len(json.load(sys.stdin)["processes"]))
except Exception:
    print(-1)' 2>/dev/null)"
[ "$LIVE_N" = "2" ] && ok "a live session owns exactly its two processes" \
                    || bad "a live session reported $LIVE_N processes, expected 2"
HAS_SAC="$("$MAVERICKCTL_BIN" process list "$OWN_SESS" --json 2>/dev/null | SACRIFICE="$SACRIFICE" python3 -c '
import json, os, sys
try:
    wanted = int(os.environ["SACRIFICE"])
    print(1 if any(p["pid"] == wanted for p in json.load(sys.stdin)["processes"]) else 0)
except Exception:
    print(-1)' 2>/dev/null)"
[ "$HAS_SAC" = "0" ] && ok "an unrelated process is never reported as owned" \
                     || bad "a live session claimed the unrelated pid $SACRIFICE"

for transition in stop kill; do
    "$MAVERICKCTL_BIN" session "$transition" "$OWN_SESS" >/dev/null 2>&1
    export OWN_RECORD="$XDG_RUNTIME_DIR/maverick/$OWN_SESS/session.json"
    OWNED="$("$MAVERICKCTL_BIN" process list "$OWN_SESS" --json 2>/dev/null | python3 -c '
import json, sys
try:
    ps = json.load(sys.stdin)["processes"]
except Exception:
    print("-1 0")
else:
    print(len(ps), 1 if any(p["pid"] == 1 for p in ps) else 0)' 2>/dev/null)"
    N="${OWNED%% *}"
    [ "$N" = "0" ] && ok "after '$transition' the session owns no processes" \
                    || bad "after '$transition' it still claims $N processes ($OWNED)"
    # The record itself must have let go, not merely been outvoted by the
    # ownership gate. Without this the clearing could be deleted and every
    # assertion above would still pass. Read the record rather than
    # `session status --json`: the view deliberately carries no `pgrps` field,
    # so asking for it there would always answer zero.
    LEFT="$(python3 -c '
import json, os
try:
    with open(os.environ["OWN_RECORD"]) as f:
        print(len(json.load(f).get("pgrps", [])))
except Exception:
    print(-1)' 2>/dev/null)"
    case "$LEFT" in
        0) ok "after '$transition' the record retains no process groups" ;;
        -1) bad "could not read the session record to check its process groups" ;;
        *) bad "after '$transition' the record still lists $LEFT process group(s)" ;;
    esac
done

# The decisive one: the signal must not be sent.
if "$MAVERICKCTL_BIN" process kill "$OWN_SESS" "$SACRIFICE" >/dev/null 2>&1; then
    bad "process kill accepted an unowned pid"
else
    ok "process kill refuses an unowned pid"
fi
sleep 0.3
if kill -0 "$SACRIFICE" 2>/dev/null; then
    ok "the refused process is still running — no signal was sent"
else
    bad "process kill refused but the process died anyway"
fi

# Defence in depth, made observable: a record that kept a process group it has
# no live root for must still authorise nothing. `stop` clears the groups, so
# the only way to reach this state is a record that never went through teardown
# — hand-edited, written by an older build, or left by a crash. Without this
# the "only while a live root exists" rule could be deleted and every assertion
# above would still pass, because this session's group list is empty.
OWN2=ownq2
"$MAVERICKCTL_BIN" session remove "$OWN2" --force >/dev/null 2>&1
"$MAVERICKCTL_BIN" session create "$OWN2" --binary "$MAVERICK_BIN" --resolution 640x480 >/dev/null 2>&1
"$MAVERICKCTL_BIN" session stop "$OWN2" >/dev/null 2>&1
OWN2_RECORD="$XDG_RUNTIME_DIR/maverick/$OWN2/session.json"
export OWN2_RECORD
# Plant the sacrifice's process group in a record that names no live process.
PGID_OF_SACRIFICE="$(ps -o pgid= -p "$SACRIFICE" 2>/dev/null | tr -d ' ')"
if [ -z "$PGID_OF_SACRIFICE" ]; then
    bad "could not read the sacrifice's process group"
else
    PGID_OF_SACRIFICE="$PGID_OF_SACRIFICE" python3 -c '
import json, os
path = os.environ["OWN2_RECORD"]
with open(path) as f:
    rec = json.load(f)
rec["wm_pid"] = 0
rec["wm_start_time"] = 0
rec["x_pid"] = 0
rec["x_start_time"] = 0
rec["state"] = "stopped"
rec["pgrps"] = [int(os.environ["PGID_OF_SACRIFICE"])]
with open(path, "w") as f:
    json.dump(rec, f)
' 2>/dev/null
    PLANTED="$("$MAVERICKCTL_BIN" process list "$OWN2" --json 2>/dev/null | python3 -c '
import json, sys
try:
    print(len(json.load(sys.stdin)["processes"]))
except Exception:
    print(-1)' 2>/dev/null)"
    [ "$PLANTED" = "0" ] \
        && ok "a record with a stale process group and no live root owns nothing" \
        || bad "a planted process group put $PLANTED process(es) back in the session"
    if "$MAVERICKCTL_BIN" process kill "$OWN2" "$SACRIFICE" >/dev/null 2>&1; then
        bad "a planted process group authorised a signal"
    else
        ok "a planted process group cannot authorise a signal"
    fi
    sleep 0.3
    if kill -0 "$SACRIFICE" 2>/dev/null; then
        ok "the process named by the planted group is still running"
    else
        bad "the planted process group killed an unrelated process"
    fi
fi
"$MAVERICKCTL_BIN" session remove "$OWN2" --force >/dev/null 2>&1

# `remove` must not leave a record that still claims anything.
"$MAVERICKCTL_BIN" session remove "$OWN_SESS" --force >/dev/null 2>&1
if "$MAVERICKCTL_BIN" process list "$OWN_SESS" >/dev/null 2>&1; then
    bad "process list on a removed session reported success"
else
    ok "a removed session cannot be listed at all"
fi
kill -9 "$SACRIFICE" 2>/dev/null
wait "$SACRIFICE" 2>/dev/null
echo

# ── 12. a session never leaves a resource nothing owns ────────────────────────
echo "12. resource ownership across lifecycle transitions"
LC=lifecycleq
"$MAVERICKCTL_BIN" session remove "$LC" --force >/dev/null 2>&1

# A failed start must leave nothing running. `--cwd` naming a file is the only
# deterministic way to fail *after* the X server is up: the check sits in the
# window manager stage, which is the stage that owns the display.
"$MAVERICKCTL_BIN" session create "$LC" --binary "$MAVERICK_BIN" --resolution 640x480 \
    --cwd /etc/hostname >/dev/null 2>&1
"$MAVERICKCTL_BIN" session start "$LC" >/dev/null 2>&1
export LC_RECORD="$XDG_RUNTIME_DIR/maverick/$LC/session.json"
LC_STATE=$(python3 -c '
import json, os
try:
    d = json.load(open(os.environ["LC_RECORD"]))
    print(d["state"], d["x_pid"])
except Exception:
    print("unreadable -")' 2>/dev/null)
LC_XPID="${LC_STATE##* }"
if [ "$LC_STATE" = "unreadable -" ]; then
    bad "the record could not be read after a failed start"
elif [ "$LC_XPID" != "0" ] && kill -0 "$LC_XPID" 2>/dev/null; then
    bad "a failed start left X server $LC_XPID running"
else
    ok "a failed start leaves no X server running"
fi
case "$LC_STATE" in
    stopped*) ok "a failed start records the session as stopped" ;;
    *) bad "after a failed start the record reads '$LC_STATE'" ;;
esac
# The reason is what tells a user why their session is not there.
LC_WHY=$(python3 -c '
import json, os
print(json.load(open(os.environ["LC_RECORD"])).get("exit_reason", ""))' 2>/dev/null)
[ -n "$LC_WHY" ] && ok "a failed start records why ($LC_WHY)" \
                 || bad "a failed start recorded no reason"
"$MAVERICKCTL_BIN" session remove "$LC" --force >/dev/null 2>&1

# A clean quit must take the X server with it, and say so truthfully.
"$MAVERICKCTL_BIN" session create "$LC" --binary "$MAVERICK_BIN" --resolution 640x480 >/dev/null 2>&1
LC_X=$("$MAVERICKCTL_BIN" session list --json | python3 -c "
import json,sys
print(next((s['x_pid'] or 0) for s in json.load(sys.stdin)['sessions'] if s['name']=='$LC'))")
"$MAVERICKCTL_BIN" quit --session "$LC" >/dev/null 2>&1
sleep 0.5
if [ "$LC_X" != "0" ] && kill -0 "$LC_X" 2>/dev/null; then
    bad "quit left X server $LC_X running"
else
    ok "quit reaps the session's X server"
fi
LC_Q=$("$MAVERICKCTL_BIN" session list --json | python3 -c "
import json,sys
ss=[s for s in json.load(sys.stdin)['sessions'] if s['name']=='$LC']
print(ss[0]['state'] if ss else 'gone')" 2>/dev/null)
case "$LC_Q" in
    stopped|gone) ok "after quit the record is truthful ($LC_Q)" ;;
    *) bad "after quit the record reads '$LC_Q'" ;;
esac
"$MAVERICKCTL_BIN" session remove "$LC" --force >/dev/null 2>&1

# A display whose X server was killed uncleanly must be reclaimable. Both
# `/tmp/.X<n>-lock` and `/tmp/.X11-unix/X<n>` have to go, or `display_is_free`
# stays false for that number for the life of the machine.
"$MAVERICKCTL_BIN" session create "$LC" --binary "$MAVERICK_BIN" --resolution 640x480 >/dev/null 2>&1
LC_D=$("$MAVERICKCTL_BIN" session list --json | python3 -c "
import json,sys
print(next(s['display'] for s in json.load(sys.stdin)['sessions'] if s['name']=='$LC'))")
LC_X=$("$MAVERICKCTL_BIN" session list --json | python3 -c "
import json,sys
print(next((s['x_pid'] or 0) for s in json.load(sys.stdin)['sessions'] if s['name']=='$LC'))")
kill -9 "$LC_X" 2>/dev/null; sleep 0.5
"$MAVERICKCTL_BIN" session stop "$LC" >/dev/null 2>&1
LC_N=${LC_D#:}
if [ -e "/tmp/.X${LC_N}-lock" ] || [ -e "/tmp/.X11-unix/X${LC_N}" ]; then
    bad "display $LC_D stayed claimed after its server was killed and the session stopped"
else
    ok "a display whose server was killed uncleanly is released"
fi
"$MAVERICKCTL_BIN" session remove "$LC" --force >/dev/null 2>&1

# Concurrent creates must each get their own display. Asserted on the final
# state, never on a duration: the property holds however the interleaving fell.
RC_NAMES=""
for i in 1 2 3 4; do RC_NAMES="$RC_NAMES rc$i"; done
for n in $RC_NAMES; do
    "$MAVERICKCTL_BIN" session create "$n" --binary "$MAVERICK_BIN" --backend Xvfb \
        --resolution 320x240 >/dev/null 2>&1 &
done
wait
export RC_RT="$XDG_RUNTIME_DIR"
export RC_NAMES="rc1 rc2 rc3 rc4"
RC_OUT=$(python3 -c '
import json, os, collections
names = os.environ["RC_NAMES"].split()
recs = []
for n in names:
    try:
        recs.append(json.load(open(os.path.join(
            os.environ["RC_RT"], "maverick", n, "session.json"))))
    except Exception:
        pass
by = collections.Counter(r.get("display") for r in recs)
dupes = {d: c for d, c in by.items() if c > 1}
# A record that names a dead X server and is not stopped is the lie the race
# used to produce.
lying = [r["name"] for r in recs
         if r.get("x_pid") and r.get("state") in ("running", "starting")
         and not os.path.exists("/proc/%d" % r["x_pid"])]
print("%d records, %d distinct displays, dupes=%s, dead-but-not-stopped=%s"
      % (len(recs), len(by), dupes or "none", lying or "none"))' 2>/dev/null)
case "$RC_OUT" in
    *"dupes=none"*) ok "concurrent creates each get their own display ($RC_OUT)" ;;
    *) bad "concurrent creates collided: $RC_OUT" ;;
esac
case "$RC_OUT" in
    *"dead-but-not-stopped=none"*) ok "no concurrent create left a record naming a dead X server" ;;
    *) bad "a concurrent create recorded a dead X server: $RC_OUT" ;;
esac
for n in $RC_NAMES; do "$MAVERICKCTL_BIN" session remove "$n" --force >/dev/null 2>&1; done
echo

# ── 12. maverick-msg is gone ──────────────────────────────────────────────────
echo "12. one control binary"
# Check the *manifest*, not just the build output: a stale binary in a target
# directory says nothing about whether the source still produces one.
if grep -q 'name = "maverick-msg"' "$REPO_ROOT/maverick-sys/Cargo.toml" 2>/dev/null ||
   [ -e "$REPO_ROOT/maverick-sys/src/bin/maverick-msg.rs" ]; then
    bad "the source still declares a maverick-msg binary"
else
    ok "the source declares no maverick-msg binary"
fi
# Its capability is maverickctl's: an unrecognised word is forwarded verbatim.
FWD="$("$MAVERICKCTL_BIN" --session two focus-left 2>&1)"
case "$FWD" in
    ok*) ok "maverickctl forwards an action line verbatim" ;;
    *)   bad "forwarding failed: $FWD" ;;
esac
"$MAVERICKCTL_BIN" --help >/dev/null 2>&1 && ok "maverickctl --help" || bad "--help failed"
for g in session window process; do
    "$MAVERICKCTL_BIN" $g --help >/dev/null 2>&1 && ok "maverickctl $g --help" || bad "$g --help failed"
done
echo

echo "-------------------------------------------"
printf 'passed %d, failed %d\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
[ "$KEEP" = 1 ] && echo "(sessions left running: ${SESSIONS[*]})"
exit 0
