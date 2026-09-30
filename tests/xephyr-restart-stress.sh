#!/usr/bin/env bash
# xephyr-restart-stress.sh — deterministic repeated-`maverickctl restart` stress.
#
# WHAT THIS TESTS, AND WHY THE EARLIER VERSION OF THIS SCRIPT COULD NOT SEE IT.
#
# `maverickctl restart` re-execs the WM with `launch_args` only
# (src/backend/x11/actions.rs `restart()`), and `launch_args` is
# `std::env::args().skip(1)`. Unless `--session-id` was on the original command
# line, the re-exec'd process mints a brand-new random session id
# (maverick-sys identity::new_session_id) and therefore a new runtime dir, a
# new control socket and a new identity ficha, and the old ones are deleted.
#
# A terminal the WM opened inherits `MAVERICK_INSTANCE=<sid>` from
# `std::env::set_var` in src/main.rs, and it has its OWN controlling tty. So
# from the second restart onward the terminal's copy of the variable is stale,
# and `maverick_sys::ctl::resolve_target` falls through to its DISPLAY+tty
# context filter, which finds nothing because the WM is on a different tty.
#
# The earlier script could not see this: it explicitly unset
# `MAVERICK_INSTANCE` and ran every command from the very shell that started the
# WM, so the context filter always matched. This one reproduces the real
# topology — WM on its own pty (pty A, the login-shell case), user's terminal on
# a *different* pty (pty B) that inherited the sid — and ends with a control
# matrix that runs the same commands from the WM's own tty to show the old
# script's condition passing.
#
# USAGE
#   tests/xephyr-restart-stress.sh [BIN_DIR]      # default: <repo>/target/debug
#   BIN_DIR=/tmp/kilo/mv-baseline/target/debug RESTARTS=15 tests/xephyr-restart-stress.sh
#
# ENV
#   BIN_DIR   directory holding `maverick` and `maverickctl` (default target/debug)
#   RESTARTS  iterations in the main loop and the --session-id control (default 15)
#   DPY       Xephyr display number                          (default 98)
#   EXPECT    stale | fixed  — what this build *should* do   (default stale)
#   KEEP      1 to keep $WORK for post-mortem inspection
#
# EXIT: 0 when the observed behaviour matches EXPECT. The expectation is
# inverted from a normal test: on an unfixed build the main loop is EXPECTED to
# break at iteration 2, so an all-pass run is the bug, not the success.

set -uo pipefail

REPO=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
BIN_DIR=${1:-${BIN_DIR:-$REPO/target/debug}}
RESTARTS=${RESTARTS:-15}
DPY=${DPY:-98}
KEEP=${KEEP:-0}
EXPECT=${EXPECT:-stale}
WORK=${WORK:-$(mktemp -d /tmp/kilo/rs.XXXXXX)}

MAVERICK=$BIN_DIR/maverick
CTL=$BIN_DIR/maverickctl
MGDWIN=$WORK/mgdwin
PTRUN=$WORK/ptyrun.py
PINGPY=$WORK/ping.py
RT=$WORK/rt
TSV=$WORK/iterations.tsv
DISP=:$DPY
PY=python3
mkdir -p "$WORK" "$RT"

# The X display this harness *nests into* is whatever the caller had; the
# display this harness *owns* is $DISP. Every Maverick-visible process below
# runs with DISPLAY=$DISP, and the export is what makes the bare `mgdwin` and
# `xprop` calls land there too. Without it they inherit the caller's live
# session, the WM adopts the test windows into the user's real desktop, and
# every probe reports the live window list instead of the test one.
HOST_DISPLAY=${DISPLAY:-:0}
export DISPLAY=$DISP
[ "$HOST_DISPLAY" = "$DISP" ] && { log "FATAL: HOST_DISPLAY ($HOST_DISPLAY) equals the test display ($DISP); refusing to nest a Xephyr into itself."; exit 2; }

# ── lifecycle ───────────────────────────────────────────────────────────────
XPID=; WMPID=; TTY_A=; TTY_B=; TTY_C=
HOLDER_B=; HOLDER_C=
CLIENT_PIDS=()

cleanup() {
    local p
    # KEEP preserves $WORK for post-mortem only. Processes are always torn
    # down: leaving a Xephyr or a WM behind would poison the next run (and the
    # next agent's).
    for p in "${CLIENT_PIDS[@]:-}"; do kill -KILL "$p" 2>/dev/null; done
    [ -n "$WMPID" ] && kill -KILL "$WMPID" 2>/dev/null
    [ -n "$HOLDER_B" ] && "$PY" "$PTRUN" killp "$HOLDER_B" 2>/dev/null
    [ -n "$HOLDER_C" ] && "$PY" "$PTRUN" killp "$HOLDER_C" 2>/dev/null
    [ -n "$XPID" ] && kill -KILL "$XPID" 2>/dev/null
    pkill -KILL -f "$MGDWIN" 2>/dev/null
    rm -f "/tmp/.X${DPY}-lock" 2>/dev/null
    if [ "$KEEP" != 0 ]; then echo "keeping $WORK"; else rm -rf "$WORK" 2>/dev/null; fi
    return 0
}
trap cleanup EXIT INT TERM

log() { printf '%s\n' "$*"; }
hdr() { printf '\n=== %s ===\n' "$*"; }

# ── pty primitive ───────────────────────────────────────────────────────────
# wm    : run a long-lived process on a brand-new controlling pty (pty A)
# alloc : create a pty that outlives the process that made it (pty B / pty C)
# exec  : run one command in that pty, with the pty as its controlling terminal
# ttynr : print the kernel tty_nr the kernel would report for that pty
# killp : kill a holder recorded by alloc/wm
cat >"$PTRUN" <<'PTRUN_EOF'
#!/usr/bin/env python3
"""Minimal pty primitives for the Maverick restart-stress harness.

  wm     <ttyfile> <pidfile> <logfile> -- <argv...>
         Detached and long-lived. Creates a pty (pty A), makes the child a
         session leader that re-opens that pty *after* setsid so it really is
         the controlling terminal, execs the WM on it, and drains the master
         end into a log. This is the "login shell / .xinitrc" launch, so the
         WM records a real non-zero tty_nr in its identity ficha.

  alloc  <ttyfile> <pidfile>
         Creates a pty and leaves a detached holder keeping both ends open so
         the slave path stays valid across unrelated short-lived commands.
         This is the "terminal emulator" (pty B / pty C): it persists for the
         whole run, exactly like a terminal window the WM opened once.

  exec   <ttyfile> <logfile> -- <argv...>
         One command in that pty. setsid + re-open makes the pty the
         controlling terminal; stdio is redirected to the log, which is
         faithful because /proc/<pid>/stat field 7 reports the *controlling
         terminal*, not whatever descriptor sits behind fd 0.

  ttynr  <ttyfile>
         Prints the tty device number the kernel encodes, the same value
         Maverick writes into the ficha's tty_nr.

  killp  <pidfile>
"""

import fcntl
import os
import select
import signal
import sys
import termios
import time

import pty


def _proc_tty_nr(pid):
    with open("/proc/%d/stat" % pid) as f:
        s = f.read()
    return int(s[s.rfind(")") + 1:].split()[4])  # state ppid pgrp session tty_nr


def _drain(master_fd, log, until_pid):
    while True:
        try:
            os.kill(until_pid, 0)
        except OSError:
            deadline = time.time() + 0.5
            while time.time() < deadline:
                r, _, _ = select.select([master_fd], [], [], 0.1)
                if not r:
                    continue
                try:
                    data = os.read(master_fd, 65536)
                except OSError:
                    return
                if not data:
                    return
                log.write(data)
                log.flush()
            return
        r, _, _ = select.select([master_fd], [], [], 1.0)
        for fd in r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            log.write(data)
            log.flush()


def _detach_stdio():
    null = os.open("/dev/null", os.O_RDWR)
    os.dup2(null, 0)
    os.dup2(null, 1)
    os.dup2(null, 2)
    if null > 2:
        os.close(null)


def _exec_on_ctty(name, argv, out_fd=None):
    os.setsid()
    fd = os.open(name, os.O_RDWR)
    if _proc_tty_nr(os.getpid()) == 0:
        try:
            fcntl.ioctl(fd, termios.TIOCSCTTY, 0)
        except OSError:
            pass
    os.dup2(fd, 0)
    os.dup2(fd, 1)
    os.dup2(fd, 2)
    if out_fd is not None:
        os.dup2(out_fd, 1)
        os.dup2(out_fd, 2)
    if fd > 2:
        os.close(fd)
    os.execvp(argv[0], argv)
    os._exit(127)


def cmd_wm(argv):
    ttyfile, pidfile, logfile = argv[0], argv[1], argv[2]
    rest = argv[3:]
    if rest and rest[0] == "--":
        rest = rest[1:]
    master, slave = pty.openpty()
    name = os.ttyname(slave)
    with open(ttyfile, "w") as f:
        f.write(name)
    wm = os.fork()
    if wm == 0:
        # Keep the inherited `slave` descriptor OPEN across setsid(): it was
        # opened before setsid so it can never become the ctty, but holding it
        # stops the pty from being destroyed if the parent reaches its _exit
        # first. The re-open BY NAME is what acquires the ctty — a session
        # leader with no controlling terminal adopts the first terminal it
        # opens — and only then is the inherited descriptor safe to drop.
        os.close(master)
        os.setsid()
        fd = os.open(name, os.O_RDWR)
        os.close(slave)
        if _proc_tty_nr(os.getpid()) == 0:
            try:
                fcntl.ioctl(fd, termios.TIOCSCTTY, 0)
            except OSError:
                pass
        os.dup2(fd, 0)
        os.dup2(fd, 1)
        os.dup2(fd, 2)
        if fd > 2:
            os.close(fd)
        os.execvp(rest[0], rest)
        os._exit(127)
    drainer = os.fork()
    if drainer == 0:
        _detach_stdio()
        os.close(slave)
        log = open(logfile, "wb")
        _drain(master, log, wm)
        os._exit(0)
    with open(pidfile, "w") as f:
        f.write(str(wm))
    os.close(master)
    os.close(slave)
    os._exit(0)


def cmd_alloc(argv):
    ttyfile, pidfile = argv[0], argv[1]
    master, slave = pty.openpty()
    name = os.ttyname(slave)
    holder = os.fork()
    if holder == 0:
        os.setsid()
        _detach_stdio()
        while True:
            time.sleep(3600)
    with open(pidfile, "w") as f:
        f.write(str(holder))
    with open(ttyfile, "w") as f:
        f.write(name)
    os.close(master)
    os.close(slave)
    os._exit(0)


def cmd_exec(argv):
    ttyfile, logfile = argv[0], argv[1]
    rest = argv[2:]
    if rest and rest[0] == "--":
        rest = rest[1:]
    name = open(ttyfile).read().strip()
    pid = os.fork()
    if pid == 0:
        _detach_stdio()
        out = os.open(logfile, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
        _exec_on_ctty(name, rest, out_fd=out)
    _, status = os.waitpid(pid, 0)
    os._exit(os.waitstatus_to_exitcode(status)
             if hasattr(os, "waitstatus_to_exitcode") else 0)


def cmd_ttynr(argv):
    st = os.stat(open(argv[0]).read().strip())
    print(os.major(st.st_rdev) * 256 + os.minor(st.st_rdev))


def cmd_killp(argv):
    try:
        pid = int(open(argv[0]).read().strip())
    except Exception:
        return
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except OSError:
            return
        time.sleep(0.2)


if __name__ == "__main__":
    cmd, rest = sys.argv[1], sys.argv[2:]
    {"wm": cmd_wm, "alloc": cmd_alloc, "exec": cmd_exec,
     "ttynr": cmd_ttynr, "killp": cmd_killp}[cmd](rest)
PTRUN_EOF

cat >"$PINGPY" <<'PING_EOF'
#!/usr/bin/env python3
"""Talk to a Maverick control socket directly, bypassing maverickctl entirely.

Used by V4 so "the socket answers" is proven without going through
resolve_target — which is the very thing under suspicion.
"""
import socket
import sys

sock_path, sid = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX)
s.settimeout(2)
s.connect(sock_path)
s.sendall(("ping %s\n" % sid).encode())
sys.stdout.write(s.recv(4096).decode(errors="replace").strip())
s.close()
PING_EOF

# ── environment helpers ─────────────────────────────────────────────────────
# Every Maverick-visible process gets a hermetic environment. The harness shell
# itself inherits MAVERICK_INSTANCE from the live :0 session, so `env -i` is
# load-bearing, not hygiene: without it every probe would be answered by an
# instance on another display.
hermetic() { env -i PATH="$PATH" HOME="$HOME" DISPLAY="$DISP" XDG_RUNTIME_DIR="$RT" "$@"; }

# Run one command inside a pty, as the "user terminal" living in that pty.
#   term <ttyfile> <MAVERICK_INSTANCE|""> <shell command>
# An empty second argument leaves the variable genuinely absent (env -i).
# Result: TERM_OUT (combined output), return code of the command.
term() {
    local ttyf=$1 mi=$2 cmd=$3
    local -a e=(DISPLAY="$DISP" XDG_RUNTIME_DIR="$RT" HOME="$HOME" PATH="$PATH")
    [ -n "$mi" ] && e+=(MAVERICK_INSTANCE="$mi")
    "$PY" "$PTRUN" exec "$ttyf" "$WORK/term.out" \
        /usr/bin/env -i "${e[@]}" /bin/sh -c "$cmd"
    local rc=$?
    TERM_OUT=$(cat "$WORK/term.out" 2>/dev/null)
    return $rc
}
# (Re)create the three managed clients and refresh the recorded XIDs.
# Idempotent: killing the old ones first matters because a *graceful* WM exit
# (SIGTERM -> begin_shutdown) sends WM_DELETE_WINDOW, which mgdwin honours and
# then exits — so a client that survives `restart` does not survive a restart
# of the harness's own WM, and the second WM would otherwise start with an
# empty tree and V2 would report a harness artefact as a detachment bug.
spawn_clients() {
    local p n
    for p in "${CLIENT_PIDS[@]:-}"; do kill -KILL "$p" 2>/dev/null; done
    CLIENT_PIDS=()
    for n in A B C; do
        MGDTITLE="RS_$n" "$MGDWIN" >"$WORK/client_$n.log" 2>&1 &
        CLIENT_PIDS+=($!)
    done
    local stable=0
    for _ in $(seq 1 80); do
        [ "$(client_list_ids | wc -l)" -ge 3 ] && break
        sleep 0.25
    done
    for _ in $(seq 1 60); do
        n=$(client_list_ids | wc -l)
        if [ "$n" -eq 3 ]; then
            stable=$((stable + 1))
            [ "$stable" -ge 4 ] && break
        else
            stable=0
        fi
        sleep 0.25
    done
    mapfile -t XIDS < <(client_list_ids)
    if [ "${#XIDS[@]}" -lt 3 ]; then
        # One wake, then look again. A list that only appears after this is a
        # publication lag; one that never appears is a real loss.
        poke_client_list
        for _ in $(seq 1 20); do
            [ "$(client_list_ids | wc -l)" -ge 3 ] && break
            sleep 0.25
        done
        mapfile -t XIDS < <(client_list_ids)
    fi
}
sock_alive() { "$PY" "$PINGPY" "$RT/maverick/$1/control.sock" "$1" >/dev/null 2>&1; }
alive_sids() { hermetic "$CTL" list 2>/dev/null | awk '$NF=="alive"{print $1}'; }
cur_sid()    { alive_sids | head -1; }
ficha_pid()  { hermetic "$CTL" --session "$1" identify 2>/dev/null |
               sed -n 's/.*"pid":\([0-9]*\).*/\1/p'; }
ficha_tty()  { hermetic "$CTL" --session "$1" identify 2>/dev/null |
               sed -n 's/.*"tty_nr":\([0-9]*\).*/\1/p'; }
client_list_ids() { xprop -root _NET_CLIENT_LIST 2>/dev/null |
                     grep -o '0x[0-9a-fA-F]\+' | tr 'A-F' 'a-f'; }
# The raw property, so "the WM published an empty list" and "the WM never
# published the property at all" stay distinguishable. They are different
# defects and only the second one is a conformance problem.
client_list_raw() { xprop -root _NET_CLIENT_LIST 2>/dev/null | head -1; }

# _NET_CLIENT_LIST is published from a dirty flag drained at the top of the
# event loop, so on a WM that has just gone idle it can still be unpublished.
# Nudging the WM with one control query (which is itself a wake) separates
# "the windows were lost" from "the property was never written", instead of
# scoring a publication lag as a detached window.
poke_client_list() {
    local sid
    [ "$(client_list_ids | wc -l)" -ge 3 ] && return 0
    sid=$(cur_sid)
    [ -n "$sid" ] && hermetic "$CTL" --session "$sid" query tree >/dev/null 2>&1
    return 0
}

# Fall back to the WM's own tree when the EWMH property is missing entirely, so
# an unpublished `_NET_CLIENT_LIST` is reported as exactly that instead of being
# scored as "the windows were lost" (or aborting the run).
xids_from_tree() {
    local sid tree
    sid=$(cur_sid)
    [ -n "$sid" ] || return 1
    tree=$(hermetic "$CTL" --session "$sid" query tree 2>/dev/null) || return 1
    printf '%s' "$tree" | grep -o '"id":[0-9]*' | sed 's/"id"://' |
        while read -r n; do printf '0x%x\n' "$n"; done | sort
}
wmcheck_window()  { xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null |
                     sed -n 's/.*window id # \(0x[0-9a-fA-F]*\).*/\1/p'; }

# Readiness gate. `state` is useless here: the WM blocks in poll() when idle
# and only publishes a snapshot after a wake, so it answers {} on a quiet WM.
# `query tree` pushes a command, which is itself a wake, and every reply is a
# JSON document — so it is the only honest "the new image is serving" probe.
# The sid is discovered from the runtime dir rather than assumed, because after
# a restart the caller's idea of the sid is exactly what is in question.
wait_ready() {
    local limit=${1:-25}
    local deadline=$((SECONDS + limit))
    local sid
    while [ "$SECONDS" -lt "$deadline" ]; do
        sid=$(cur_sid)
        if [ -n "$sid" ] && [ "$(alive_sids | wc -l)" -eq 1 ] &&
           hermetic "$CTL" --session "$sid" query tree 2>/dev/null |
               head -c 1 | grep -q '{'; then
            printf '%s' "$sid"; return 0
        fi
        sleep 0.25
    done
    return 1
}

TITLES=(RS_A RS_B RS_C)
XIDS=()
V1=; V2=; V3=; V4=; V5=; V_VERDICT=; V_LINE=; V_WMCHECK=; V_ALIVE_N=; V_PID=; V_DETAIL=

# V1..V5 for the terminal described by ($2 ttyfile, $3 MAVERICK_INSTANCE),
# against the live instance $4. Sets globals; prints nothing.
verify() {
    local ttyf=$1 mi=$2 sid=$3 label=$4
    local v2a v2b tree cl wmcheck pid i t

    # V1 — the user's own maverickctl, resolved the way the user's terminal
    #      resolves it, answers with a JSON document.
    if term "$ttyf" "$mi" "$CTL query tree" &&
       [ "$(printf '%s' "$TERM_OUT" | head -c 1)" = "{" ]; then
        V1=PASS
    else
        V1=FAIL
    fi

    # V2 — the pre-restart clients are still managed: present in
    #      _NET_CLIENT_LIST (the X server's own EWMH state) AND in the WM tree.
    #      V2a and V2b are reported apart, because a WM that manages the windows
    #      but never publishes the EWMH property is a different defect from one
    #      that dropped them.
    poke_client_list
    cl=$(client_list_ids)
    v2a=PASS
    for i in "${XIDS[@]}"; do
        printf '%s\n' "$cl" | grep -qx "$i" || v2a=FAIL
    done
    tree=$(hermetic "$CTL" --session "$sid" query tree 2>/dev/null)
    v2b=PASS
    for t in "${TITLES[@]}"; do
        printf '%s' "$tree" | grep -q "\"$t\"" || v2b=FAIL
    done
    if [ "$v2a" = PASS ] && [ "$v2b" = PASS ]; then V2=PASS; else V2=FAIL; fi

    # V3 — somebody owns the screen.
    wmcheck=$(wmcheck_window)
    if [ -n "$wmcheck" ]; then V3=PASS; else V3=FAIL; fi

    # V4 — exactly one live instance, and its socket answers a raw ping.
    V_ALIVE_N=$(alive_sids | wc -l)
    if [ "$V_ALIVE_N" -eq 1 ] && sock_alive "$sid"; then V4=PASS; else V4=FAIL; fi

    # V5 — the process behind the ficha is alive.
    pid=$(ficha_pid "$sid")
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then V5=PASS; else V5=FAIL; fi

    V_WMCHECK=${wmcheck:-none}
    V_PID=${pid:-none}
    if [ "$V1$V2$V3$V4$V5" = "PASSPASSPASSPASSPASS" ]; then
        V_VERDICT=PASS
        V_DETAIL=
    else
        V_VERDICT=FAIL
        # Keep it to one short line: a failed V1 can dump a whole tree snapshot.
        V_DETAIL=$(printf '%s' "$TERM_OUT" | head -1 | cut -c1-120)
    fi
    V_LINE=$(printf '%-8s V1=%-4s V2=%-4s (prop=%s tree=%s) V3=%-4s V4=%-4s V5=%-4s wmcheck=%s alive=%s pid=%s' \
        "$label" "$V1" "$V2" "$v2a" "$v2b" "$V3" "$V4" "$V5" "$V_WMCHECK" "$V_ALIVE_N" "$V_PID")
    return 0
}

# ── preflight ───────────────────────────────────────────────────────────────
hdr "preflight"
for b in "$MAVERICK" "$CTL"; do
    [ -x "$b" ] || { log "FATAL: $b missing or not executable"; exit 2; }
done
[ "$DPY" != 0 ] || { log "FATAL: refusing to run on the live display :0"; exit 2; }
command -v Xephyr >/dev/null || { log "FATAL: Xephyr not installed"; exit 2; }
log "bin dir   : $BIN_DIR"
log "restarts  : $RESTARTS    expect=$EXPECT"
log "display   : $DISP"
log "work dir  : $WORK"
cc -O1 -o "$MGDWIN" "$REPO/tests/mgdwin.c" -lX11 || { log "FATAL: mgdwin build failed"; exit 2; }
printf 'iter\trc\tsid_changed\tsid\tV1\tV2\tV3\tV4\tV5\twmcheck\tnote\n' >"$TSV"

# ── Xephyr ──────────────────────────────────────────────────────────────────
hdr "xephyr $DISP"
# A display already in use would silently hand the WM somebody else's server
# (and a stale Xephyr keeps a SubstructureRedirect grab, which fails as
# "another WM is already running"). Wait it out rather than stomping it.
for _ in $(seq 1 40); do
    DISPLAY=$DISP xprop -root >/dev/null 2>&1 || break
    sleep 1
done
if DISPLAY=$DISP xprop -root >/dev/null 2>&1; then
    log "FATAL: $DISP is already in use by another X server; refusing to share it."
    exit 2
fi
rm -f "/tmp/.X${DPY}-lock"
DISPLAY=$HOST_DISPLAY Xephyr "$DISP" -screen 1280x800 -resizeable -nolisten tcp \
    >"$WORK/xephyr.log" 2>&1 &
XPID=$!
for _ in $(seq 1 60); do
    DISPLAY=$DISP xprop -root >/dev/null 2>&1 && break
    sleep 0.25
done
DISPLAY=$DISP xprop -root >/dev/null 2>&1 || { log "FATAL: Xephyr never came up"; exit 2; }
log "xephyr pid $XPID"

# ── the WM on pty A ─────────────────────────────────────────────────────────
# No --session-id: the ordinary user flow, so every restart mints a new random
# session id. This is the condition under investigation.
start_wm() {
    local ttyf=$1 pidf=$2 logf=$3; shift 3
    # Hermetic env: the WM must land on this harness's Xephyr and this
    # harness's runtime dir, never on whatever the invoking shell pointed at.
    "$PY" "$PTRUN" wm "$ttyf" "$pidf" "$logf" -- \
        /usr/bin/env -i PATH="$PATH" HOME="$HOME" DISPLAY="$DISP" \
        XDG_RUNTIME_DIR="$RT" "$@"
    local i
    for i in $(seq 1 160); do
        [ -n "$(wmcheck_window)" ] && [ -n "$(cur_sid)" ] && return 0
        sleep 0.25
    done
    return 1
}
hdr "wm on pty A (login-shell / .xinitrc launch)"
start_wm "$WORK/A.tty" "$WORK/A.pid" "$WORK/wm.log" "$MAVERICK" || {
    log "FATAL: WM never took the display"; tail -20 "$WORK/wm.log"; exit 2; }
TTY_A=$WORK/A.tty
WMPID=$(cat "$WORK/A.pid")
SID0=$(cur_sid)
log "pty A        : $(cat "$TTY_A")   tty_nr=$("$PY" "$PTRUN" ttynr "$TTY_A")"
if [ -e "/proc/$WMPID" ]; then WM_STATE="alive"; else WM_STATE="dead"; fi
log "wm pid       : $WMPID  ($WM_STATE)"
log "session id   : $SID0"
log "ficha tty_nr : $(ficha_tty "$SID0")   socket: $RT/maverick/$SID0/control.sock"
# Prove the WM is on the harness display and not the caller's live session.
FICHADISP=$(hermetic "$CTL" --session "$SID0" identify | sed -n 's/.*"display":"\([^"]*\)".*/\1/p')
log "ficha display: $FICHADISP  (expected $DISP)"
[ "$FICHADISP" = "$DISP" ] || { log "FATAL: WM landed on $FICHADISP, not $DISP"; exit 2; }

# ── managed clients ─────────────────────────────────────────────────────────
hdr "3 managed clients"
CLIENT_LIST_ABSENT=0
spawn_clients
log "raw _NET_CLIENT_LIST: $(client_list_raw)"
if [ "${#XIDS[@]}" -eq 3 ]; then
    log "client xids  : ${XIDS[*]}  (3, from _NET_CLIENT_LIST)"
else
    CLIENT_LIST_ABSENT=1
    log "*** _NET_CLIENT_LIST not published even after a WM wake ***"
    log "*** falling back to the WM's own tree for the XIDs ***"
    mapfile -t XIDS < <(xids_from_tree)
    log "client xids  : ${XIDS[*]}  (${#XIDS[@]}, from the WM tree)"
fi
if [ "${#XIDS[@]}" -ne 3 ]; then
    log "--- root window tree ---"
    xwininfo -root -children 2>&1 | sed -n '1,30p' | sed 's/^/    /'
    log "FATAL: expected 3 managed clients, got ${#XIDS[@]}"
    exit 2
fi

# ── the user's terminal on pty B ────────────────────────────────────────────
# A fresh pty whose MAVERICK_INSTANCE is the sid the running WM exports. Nothing
# re-syncs it afterwards, which is the entire point of the test.
hdr "user terminal on pty B (different pty, inherited the WM's sid)"
"$PY" "$PTRUN" alloc "$WORK/B.tty" "$WORK/B.pid"
HOLDER_B=$WORK/B.pid
TTY_B=$WORK/B.tty
TERM_SID=$SID0
log "pty B        : $(cat "$TTY_B")   tty_nr=$("$PY" "$PTRUN" ttynr "$TTY_B")"
log "pty A tty_nr : $("$PY" "$PTRUN" ttynr "$TTY_A")   <-- different, as in the real setup"
log "terminal MI  : $TERM_SID"
verify "$TTY_B" "$TERM_SID" "$SID0" baseline
log "baseline     : $V_LINE"

# ── main loop: restart N times from pty B, never reopened ───────────────────
# The headline number is the iteration at which `maverickctl restart` ITSELF
# returns non-zero — that is the user's symptom ("I have to close the terminal").
# V1 is tracked separately because it fails one iteration *earlier*: the moment
# the first restart rotates the sid, the terminal's copy is already stale, so it
# cannot even run `query tree`, let alone `restart`.
hdr "MAIN LOOP — $RESTARTS restarts, one terminal (pty B), sid never re-read"
printf '%-6s %-6s %-11s %s\n' ITER RC SID-CHANGE "V1..V5"
FIRST_RC_FAIL=0; FIRST_RC_ERR=; FIRST_RC_SID=
FIRST_V_FAIL=0; FIRST_V_WHAT=; FIRST_V_SID=
SPURIOUS=0; FIRST_SPURIOUS=0; FIRST_SPURIOUS_ERR=
RUN=0
for i in $(seq 1 "$RESTARTS"); do
    RUN=$i
    before=$(cur_sid)
    term "$TTY_B" "$TERM_SID" "$CTL restart"
    rc=$?
    note=
    [ "$rc" -ne 0 ] && note=$(printf '%s' "$TERM_OUT" | head -1 | cut -c1-120)
    [ "$FIRST_RC_FAIL" -eq 0 ] && [ "$rc" -ne 0 ] && { FIRST_RC_FAIL=$i; FIRST_RC_ERR=$note; FIRST_RC_SID=$before; }
    sleep 0.3
    after=$(wait_ready 25)
    # A restart that took effect but was *reported* as a failure is a different
    # defect from one that did not take effect; counting them together would
    # hide the second inside the first.
    if [ "$rc" -ne 0 ] && [ -n "$after" ] && [ "$before" = "$after" ]; then
        SPURIOUS=$((SPURIOUS + 1))
        [ "$FIRST_SPURIOUS" -eq 0 ] && { FIRST_SPURIOUS=$i; FIRST_SPURIOUS_ERR=$note; }
    fi
    if [ -z "$after" ]; then
        printf '%-6s %-6s %-11s %s\n' "$i" "rc=$rc" "?" "WM DID NOT COME BACK"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$i" "$rc" "?" "" "FAIL" "?" "?" "?" "?" "" "wm did not come back" >>"$TSV"
        [ "$FIRST_RC_FAIL" -eq 0 ] && { FIRST_RC_FAIL=$i; FIRST_RC_ERR="wm did not come back"; }
        break
    fi
    verify "$TTY_B" "$TERM_SID" "$after" "it$i"
    chg=SAME; [ "$before" != "$after" ] && chg=CHANGED
    printf '%-6s %-6s %-11s %s  %s\n' "$i" "rc=$rc" "$chg" "$V_LINE" "$V_VERDICT"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$i" "$rc" "$chg" "$after" "$V1" "$V2" "$V3" "$V4" "$V5" "$V_WMCHECK" "$note" >>"$TSV"
    if [ "$V_VERDICT" = FAIL ] && [ "$FIRST_V_FAIL" -eq 0 ]; then
        FIRST_V_FAIL=$i
        FIRST_V_WHAT=$(printf 'V1=%s V2=%s V3=%s V4=%s V5=%s %s' \
            "$V1" "$V2" "$V3" "$V4" "$V5" "$V_DETAIL")
        FIRST_V_SID=$after
    fi
done

hdr "main loop outcome"
FINAL_SID=$(cur_sid)
log "iterations run              : $RUN"
log "restarts that took effect   : $((RUN - FIRST_RC_FAIL))/$RUN"
log "restarts reported as FAILED but which took effect: $SPURIOUS"
log "first restart that FAILED   : ${FIRST_RC_FAIL:-none}   <-- the user's symptom"
log "  exact stderr              : ${FIRST_RC_ERR:-n/a}"
log "  sid in use at that moment : ${FIRST_RC_SID:-n/a}"
[ "$SPURIOUS" -gt 0 ] && log "  spurious ack loss at it   : ${FIRST_SPURIOUS}  ${FIRST_SPURIOUS_ERR}"
log "FIRST health-check FAILURE  : ${FIRST_V_FAIL:-none}"
log "  which check               : ${FIRST_V_WHAT:-n/a}"
log "terminal's sid              : $TERM_SID"
log "stale now?                  : $([ "$TERM_SID" = "$FINAL_SID" ] && echo no || echo YES)"
log "live sid now                : $FINAL_SID"
log "runtime dirs                : $(ls "$RT/maverick" 2>/dev/null | wc -l) (one per restart; the old one is emptied, not removed)"
log "wm pid unchanged            : $([ "$WMPID" = "$(ficha_pid "$FINAL_SID")" ] && echo yes || echo no)  (restart execs in place)"
log "clients still in CLIENT_LIST: $(client_list_ids | wc -l) of 3  [$(client_list_raw)]"
[ "$CLIENT_LIST_ABSENT" = 1 ] && log "*** this build never published _NET_CLIENT_LIST; V2 is scored from the WM's own tree ***"
log "empty stale runtime dirs    : $(find "$RT/maverick" -mindepth 1 -maxdepth 1 -type d -empty 2>/dev/null | wc -l)"

# ── workaround: close the terminal, open a new one ──────────────────────────
# The user's workaround. A freshly opened terminal is spawned by the WM that is
# running *now*, so it inherits the *current* sid and the first restart works.
# The second one cannot: the sid it was launched with is now the stale one.
hdr "WORKAROUND — close the terminal, open a fresh one (pty C)"
"$PY" "$PTRUN" alloc "$WORK/C.tty" "$WORK/C.pid"
HOLDER_C=$WORK/C.pid
TTY_C=$WORK/C.tty
NEWSID=$(cur_sid)
log "pty C          : $(cat "$TTY_C")   tty_nr=$("$PY" "$PTRUN" ttynr "$TTY_C")"
log "new terminal MI: $NEWSID  (inherited from the WM running now)"
WC_OK=0; WC_FAIL=0; WC_SPURIOUS=0
for i in $(seq 1 "$RESTARTS"); do
    term "$TTY_C" "$NEWSID" "$CTL restart"
    rc=$?
    before=$(cur_sid)
    sleep 0.3
    after=$(wait_ready 25)
    if [ -z "$after" ] || [ "$before" != "$after" ]; then
        log "pty C restart #$i -> rc=$rc, the restart did not take effect"
        WC_FAIL=$i
        break
    fi
    verify "$TTY_C" "$NEWSID" "$after" "wc$i"
    if [ "$rc" -eq 0 ]; then
        WC_OK=$((WC_OK + 1))
    else
        WC_SPURIOUS=$((WC_SPURIOUS + 1))
    fi
    if [ "$rc" -ne 0 ] || [ $i -le 2 ] || [ $i -eq "$RESTARTS" ]; then
        log "pty C restart #$i -> rc=$rc  $V_LINE  $V_DETAIL"
    fi
done
log "reopened terminal: $WC_OK/$RESTARTS restarts took effect; $WC_SPURIOUS of them were reported as failures anyway"
# ── control matrix ──────────────────────────────────────────────────────────
# These rows separate the two hypotheses. The "pty A" rows are the exact
# conditions the previous test ran under, and on an unfixed build they pass —
# which is why that test concluded the bug did not exist.
#
# The "MI=stale" row is only meaningful on a build that actually rotates the
# sid. A build that keeps the sid stable has no stale value to be stale about,
# so the row is labelled N/A there instead of being scored as a mismatch.
hdr "CONTROL MATRIX — one restart per row, on the live WM"
SID_BEFORE_MATRIX=$(cur_sid)
STALE=$SID0
[ "$STALE" = "$SID_BEFORE_MATRIX" ] && STALE=$TERM_SID
row() {  # row <label> <ttyfile> <MI|""> <PASS|FAIL|N/A>
    local label=$1 ttyf=$2 mi=$3 want=$4 rc after got mark err before
    before=$(cur_sid)
    term "$ttyf" "$mi" "$CTL restart"; rc=$?
    err=$(printf '%s' "$TERM_OUT" | head -1 | cut -c1-110)
    sleep 0.3
    after=$(wait_ready 25)
    got=PASS; [ "$rc" -ne 0 ] && got=FAIL
    if [ "$want" = N/A ]; then
        mark="N/A (sid is stable on this build; there is no stale value)"
    elif [ "$got" != "$want" ]; then
        mark="MISMATCH (wanted $want)"
    else
        mark=as-expected
    fi
    printf '%-44s rc=%s  %-4s  %s\n' "$label" "$rc" "$got" "$mark"
    [ -n "$err" ] && printf '%-44s   stderr: %s\n' "" "$err"
    return 0
}
if [ "$SID_BEFORE_MATRIX" = "$TERM_SID" ]; then
    # This build never rotated the sid, so the terminal's copy is still valid.
    STALE_EXPECT=N/A
else
    STALE_EXPECT=FAIL
fi
row "pty B, MI unset      (tty mismatch alone)"    "$TTY_B" ""           FAIL
row "pty B, MI=stale      (the WM's own terminal)" "$TTY_B" "$TERM_SID" "$STALE_EXPECT"
row "pty A, MI unset      (previous test's shell)" "$TTY_A" ""           PASS
row "pty A, MI=stale      (fallback rescues it)"   "$TTY_A" "$STALE"     PASS
SIDNOW=$(cur_sid)
row "pty B, MI=current    (reopened terminal)"     "$TTY_B" "$SIDNOW"    PASS

# ── --session-id control ────────────────────────────────────────────────────
# Same binary, same pty topology, same terminal, same inherited env — the only
# difference is that the WM was launched with a fixed --session-id, so restart
# re-execs into the SAME identity and the terminal's copy never goes stale.
# This isolates "the sid rotates" from "restart itself is broken".
hdr "CONTROL — WM launched with --session-id stresssid"
"$PY" "$PTRUN" killp "$WORK/A.pid" 2>/dev/null
WMPID=
for _ in $(seq 1 80); do
    kill -0 "$(cat "$WORK/A.pid" 2>/dev/null || echo 0)" 2>/dev/null || break
    sleep 0.25
done
sleep 0.5
start_wm "$WORK/A2.tty" "$WORK/A2.pid" "$WORK/wm2.log" "$MAVERICK" --session-id stresssid || {
    log "FATAL: --session-id WM never took the display"; tail -20 "$WORK/wm2.log"; exit 2; }
WMPID=$(cat "$WORK/A2.pid")
TTY_A=$WORK/A2.tty
# The first WM's clients did NOT survive: SIGTERM is a *graceful* exit, so
# begin_shutdown sends WM_DELETE_WINDOW and mgdwin (which advertises it) quits.
# Re-create them, or V2 would measure the harness instead of the WM.
spawn_clients
FIXED_SID=$(cur_sid)
log "fixed sid    : $FIXED_SID"
log "pty A2       : $(cat "$TTY_A")  tty_nr=$("$PY" "$PTRUN" ttynr "$TTY_A")"
log "client xids  : ${XIDS[*]} (${#XIDS[@]}) — recreated after the first WM exited"
log "terminal MI  : $FIXED_SID"
FIX_OK=0; FIX_BAD=0; FIX_FIRSTBAD=0; FIX_NOREPLY=0
for i in $(seq 1 "$RESTARTS"); do
    before=$(cur_sid)
    term "$TTY_B" "$FIXED_SID" "$CTL restart"; rc=$?
    err=$(printf '%s' "$TERM_OUT" | head -1 | cut -c1-100)
    sleep 0.3
    after=$(wait_ready 25)
    # Two different failures must not be conflated:
    #   * the restart did not take effect (sid rotated, or the WM is gone)
    #   * the restart took effect but maverickctl reported failure anyway
    if [ -z "$after" ] || [ "$before" != "$after" ]; then
        FIX_BAD=$((FIX_BAD + 1))
        [ "$FIX_FIRSTBAD" -eq 0 ] && FIX_FIRSTBAD=$i
    else
        FIX_OK=$((FIX_OK + 1))
    fi
    [ "$rc" -ne 0 ] && [ -n "$after" ] && FIX_NOREPLY=$((FIX_NOREPLY + 1))
    verify "$TTY_B" "$FIXED_SID" "${after:-$(cur_sid)}" "fx$i"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "fix$i" "$rc" "$([ "$before" = "$after" ] && echo SAME || echo CHANGED)" \
        "${after:-none}" "$V1" "$V2" "$V3" "$V4" "$V5" "$V_WMCHECK" "$err" >>"$TSV"
    if [ "$rc" -ne 0 ] || [ $i -le 2 ] || [ $i -eq "$RESTARTS" ] || [ "$FIX_BAD" -gt 0 ]; then
        log "  fix$i rc=$rc sid_before=$before sid_after=${after:-timeout} $V_LINE $err"
    fi
    [ "$FIX_BAD" -gt 0 ] && break
done
log "--session-id control: sid never rotated on $FIX_OK/$((FIX_OK + FIX_BAD)) restarts (first rotation: ${FIX_FIRSTBAD:-none})"
log "--session-id control: maverickctl reported FAILURE on $FIX_NOREPLY/$((FIX_OK + FIX_BAD)) restarts that actually took effect"
[ "$FIX_NOREPLY" -gt 0 ] && log "  ^ a lost acknowledgement, not a lost restart: see RESTART-STRESS.md, 'no reply from the instance'"

# ── verdict ─────────────────────────────────────────────────────────────────
# Two independent dimensions, reported separately because they have different
# fixes: whether the restart *happened*, and whether maverickctl *said* so.
hdr "VERDICT"
log "A. did the restart happen?"
log "   first restart that did not take effect : ${FIRST_RC_FAIL:-none}"
log "     exact stderr                        : ${FIRST_RC_ERR:-n/a}"
log "   reopened terminal                     : $WC_OK/$RESTARTS took effect"
log "   --session-id control                  : $FIX_OK/$((FIX_OK + FIX_BAD)) took effect"
log "B. did maverickctl report it correctly?"
log "   main loop   spurious failures         : $SPURIOUS/$RUN"
log "   reopened    spurious failures         : $WC_SPURIOUS/$RESTARTS"
log "   --session-id spurious failures        : $FIX_NOREPLY/$((FIX_OK + FIX_BAD))"
[ "$SPURIOUS" -gt 0 ] && log "   first at iteration ${FIRST_SPURIOUS}: ${FIRST_SPURIOUS_ERR}"
if [ "$EXPECT" = stale ]; then
    if [ "$FIRST_RC_FAIL" = 2 ]; then
        log 'REPRODUCED: restart works once, then fails from iteration 2 onward, exactly as predicted.'
        exit 0
    fi
    log "NOT REPRODUCED as predicted (first restart that did not take effect: ${FIRST_RC_FAIL:-none})."
    exit 1
fi
if [ "$FIRST_RC_FAIL" -eq 0 ] && [ "$FIX_BAD" -eq 0 ] && [ "$WC_FAIL" -eq 0 ] && [ "$WC_OK" -ge "$RESTARTS" ]; then
    if [ "$((SPURIOUS + WC_SPURIOUS + FIX_NOREPLY))" -eq 0 ]; then
        log 'FIXED: every restart took effect and was reported as a success.'
        exit 0
    fi
    log "PARTIALLY FIXED: every restart took effect, but $((SPURIOUS + WC_SPURIOUS + FIX_NOREPLY)) of them were reported as failures."
    exit 0
fi
log "STILL BROKEN: first restart that did not take effect ${FIRST_RC_FAIL:-none}, reopened-terminal $WC_OK/$RESTARTS, --session-id $FIX_OK/$((FIX_OK + FIX_BAD))."
exit 1
