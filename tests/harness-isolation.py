#!/usr/bin/env python3
"""Prove shell test cleanup cannot select or stop an unrelated desktop instance."""
import argparse
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def alive(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[0] != "Z"
    except FileNotFoundError:
        return False


def wait_for(predicate):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.03)
    raise AssertionError("owned test process did not reach the expected state")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--common", type=Path, default=ROOT / "tests/common.sh")
    parser.add_argument("--matrix", type=Path, default=ROOT / "tests/compat-matrix.sh")
    parser.add_argument("--wm", type=Path, default=ROOT / "target/release/maverick")
    parser.add_argument("--ctl", type=Path, default=ROOT / "target/release/maverickctl")
    args = parser.parse_args()
    processes, owned_pids = [], []
    with tempfile.TemporaryDirectory(prefix="mhi-", dir="/tmp") as directory:
        directory = Path(directory)
        env = os.environ.copy()
        tools = directory / "bin"
        tools.mkdir()
        attempts = directory / "global-signals"
        control_log = directory / "control.jsonl"
        env.update(HARNESS_GLOBAL_LOG=str(attempts), HARNESS_CONTROL_LOG=str(control_log),
                   HARNESS_REAL_CTL=str(args.ctl.resolve()),
                   HARNESS_REAL_XPROP=shutil.which("xprop") or "xprop",
                   HARNESS_REAL_XVFB=shutil.which("Xvfb") or "Xvfb",
                   MAVERICK_BIN=str(args.wm.resolve()))

        def tool(name, source):
            path = tools / name
            path.write_text(f"#!{sys.executable}\n" + source)
            path.chmod(0o755)
            return path

        for name in ("pkill", "killall"):
            tool(name, 'import os,sys\nfrom pathlib import Path\n'
                 'with Path(os.environ["HARNESS_GLOBAL_LOG"]).open("a") as log:\n'
                 ' log.write(repr(sys.argv)+"\\n")\n')
        ctl = tool("ctl", '''import os,sys,json
with open(os.environ["HARNESS_CONTROL_LOG"], "a") as log:
 log.write(json.dumps(sys.argv[1:])+"\\n")
if os.environ.get("HARNESS_DROP_QUIT") == "1" and "quit" in sys.argv:
 sys.exit(1)
os.execv(os.environ["HARNESS_REAL_CTL"], [os.environ["HARNESS_REAL_CTL"], *sys.argv[1:]])
''')
        # Exercise the actual shell lifecycle without requiring a host desktop.
        tool("Xephyr", '''import os,sys
if os.environ.get("HARNESS_REFUSE_SERVER") == "1": sys.exit(12)
if os.environ.get("HARNESS_FAILED_CHILD") == "1":
 import time
 with open("/tmp/.X"+sys.argv[1][1:]+"-lock", "x") as lock: lock.write(str(os.getpid())+"\\n")
 time.sleep(0.3)
 sys.exit(13)
args=sys.argv[1:]
pos=args.index("-screen")
args[pos:pos+2]=["-screen", "0", args[pos+1]+"x24"]
os.execv(os.environ["HARNESS_REAL_XVFB"], ["Xvfb", *args, "-nolisten", "tcp"])
''')
        tool("xprop", '''import os,sys
if os.environ.get("HARNESS_REFUSE_SERVER") == "1": sys.exit(0)
os.execv(os.environ["HARNESS_REAL_XPROP"], ["xprop", *sys.argv[1:]])
''')
        unavailable_wm = tool("unavailable-wm", "raise SystemExit(17)\n")
        env.update(PATH=str(tools) + os.pathsep + env["PATH"], MAVERICK_CTL=str(ctl))

        # This probe is safe even against the old harness: global-kill tools
        # only record requests, and no child PIDs have been registered.
        subprocess.run(["bash", "-c", 'source "$1"; '
                        'if declare -F mav_preflight >/dev/null; then mav_preflight; fi; '
                        'mav_cleanup', "probe", str(args.common.resolve())], env=env,
                       capture_output=True, text=True, timeout=10, check=True)
        assert not attempts.exists(), "harness requested process-name kills: " + attempts.read_text()
        print("PASS: preflight and empty cleanup request no global signals", flush=True)

        outside = directory / "outside"
        outside.mkdir(mode=0o700)
        marker = outside / "keep"
        marker.write_text("live desktop state\n")
        outside_env = env.copy()
        outside_env.update(XDG_RUNTIME_DIR=str(outside), XDG_CONFIG_HOME=str(outside / "config"),
                           XDG_STATE_HOME=str(outside / "state"), XDG_CACHE_HOME=str(outside / "cache"))
        for name in ("MAVERICK_INSTANCE", "MAVERICK_SESSION", "MAVERICK_RESTART_PID"):
            outside_env.pop(name, None)
        config = directory / "config.toml"
        config.write_text("[autostart]\ncommands = []\n")
        with (directory / "outside.log").open("w") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                           "-nolisten", "tcp"], env=outside_env,
                                          stdout=subprocess.PIPE, stderr=log, text=True)
                processes.append(server)
                assert select.select([server.stdout], [], [], 5)[0]
                outside_env["DISPLAY"] = ":" + server.stdout.readline().strip()
                outside_lock = Path(f"/tmp/.X{outside_env['DISPLAY'][1:]}-lock")
                outside_lock_bytes = outside_lock.read_bytes() if outside_lock.exists() else None
                wm = subprocess.Popen([str(args.wm), "--session-id", "outside-test", "--config", str(config)],
                                      env=outside_env, stdout=log, stderr=log)
                processes.append(wm)

                def outside_answers():
                    result = subprocess.run([str(args.ctl), "query", "inspect", "--session", "outside-test"],
                                            env=outside_env, capture_output=True, text=True, timeout=3)
                    return result.returncode == 0 and json.loads(result.stdout)["windows"]["total"] == 0

                wait_for(outside_answers)
                env.update(outside_env)
                env.update(MAVERICK_INSTANCE="outside-test", MAVERICK_SESSION="outside-test",
                           MAVERICK_RESTART_PID=str(wm.pid), FOREIGN_PID=str(wm.pid))
                script = '''set -eu
source "$1"
trap mav_cleanup EXIT
if mav_track "$FOREIGN_PID"; then echo "registered foreign PID" >&2; exit 1; fi
mav_stop "$FOREIGN_PID"
server=$(start_xephyr "$2" 800 600)
wm=$(mav_launch "$2")
child=$(sleep 60 >/dev/null 2>&1 & p=$!; mav_track "$p"; echo "$p")
sleep 60 >/dev/null 2>&1 & stale=$!
mav_track "$stale"
sed -i '1s/.*/0/' "$MAV_TEST_RUNTIME/pids/$stale"
printf 'OWNED %s %s %s %s %s\\n' "$server" "$wm" "$child" "$stale" "$MAV_TEST_RUNTIME"
mav_cleanup
mav_cleanup
'''
                for drop in ("0", "1"):
                    # High display numbers avoid the allocator used by other CI
                    # tests. The shell still refuses locks or a foreign server.
                    display = next(f":{n}" for n in range(200, 600)
                                   if not Path(f"/tmp/.X{n}-lock").exists()
                                   and not Path(f"/tmp/.X11-unix/X{n}").exists())
                    env["HARNESS_DROP_QUIT"] = drop
                    result = subprocess.run(["bash", "-c", script, "harness", str(args.common.resolve()), display],
                                            env=env, capture_output=True, text=True, timeout=25)
                    print(result.stderr, end="")
                    records = [line.split()[1:] for line in result.stdout.splitlines() if line.startswith("OWNED ")]
                    for record in records:
                        owned_pids.extend(map(int, record[:4]))
                    assert result.returncode == 0, result.stdout + result.stderr
                    assert len(records) == 1, result.stdout
                    server_pid, wm_pid, child_pid, stale_pid = map(int, records[0][:4])
                    wait_for(lambda: not any(alive(pid) for pid in (server_pid, wm_pid, child_pid)))
                    assert alive(stale_pid), "cleanup ignored the process start-time guard"
                    assert not Path(records[0][4]).exists()
                    assert wm.poll() is None and server.poll() is None and outside_answers()
                    assert marker.read_text() == "live desktop state\n"
                    assert (outside_lock.read_bytes() if outside_lock.exists() else None) == outside_lock_bytes
                    print(f"PASS: owned cleanup (IPC failure={drop}) preserves a live foreign WM and stale PID", flush=True)
                    os.kill(stale_pid, 15)

                # An occupied display must fail before launching a WM on it.
                refusal = '''set -eu
source "$1"
trap mav_cleanup EXIT
if start_xephyr "$2" 800 600; then exit 1; fi
if mav_launch "$2"; then exit 1; fi
'''
                result = subprocess.run(["bash", "-c", refusal, "refuse", str(args.common.resolve()),
                                         outside_env["DISPLAY"]], env=env, capture_output=True, text=True, timeout=10)
                assert result.returncode == 0, result.stdout + result.stderr
                assert outside_answers() and marker.exists()
                failed_env = env.copy()
                failed_env["HARNESS_FAILED_CHILD"] = "1"
                result = subprocess.run(["bash", "-c", refusal, "failed-child", str(args.common.resolve()), display],
                                        env=failed_env, capture_output=True, text=True, timeout=10)
                assert result.returncode == 0, result.stdout + result.stderr
                assert not Path(f"/tmp/.X{display[1:]}-lock").exists(), "failed child left its owned lock"
                assert (outside_lock.read_bytes() if outside_lock.exists() else None) == outside_lock_bytes
                calls = [json.loads(line) for line in control_log.read_text().splitlines()]
                quits = [call for call in calls if "quit" in call]
                assert len(quits) == 2 and all("--session" in call and "--yes" in call for call in quits), quits
                assert all("outside-test" not in call for call in calls), calls
                assert not attempts.exists(), "a global signal request escaped the lifecycle checks"
                print("PASS: quit targets private sessions; failed children clean only their own locks; occupied displays are refused", flush=True)

                matrix_env = env.copy()
                matrix_env.update(HARNESS_REFUSE_SERVER="1", MAVERICK_BIN=str(unavailable_wm),
                                  HOSTILE=str(unavailable_wm))
                result = subprocess.run(["bash", str(args.matrix)], cwd=ROOT, env=matrix_env,
                                        capture_output=True, text=True, timeout=10)
                assert marker.exists(), "compatibility harness deleted the inherited runtime directory"
                assert result.returncode != 0 and "occupied" in result.stderr, result.stdout + result.stderr
                assert outside_answers() and wm.poll() is None and server.poll() is None
                for path in (ROOT / "tests").glob("*.sh"):
                    tokens = shlex.split(path.read_text(), comments=True)
                    assert not {"pkill", "killall"}.intersection(tokens), path
                print("PASS: compatibility startup preserves the inherited runtime; shell scripts contain no global-kill commands", flush=True)
            finally:
                for pid in owned_pids:
                    if alive(pid):
                        os.kill(pid, 9)
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=3)


if __name__ == "__main__":
    main()
