#!/usr/bin/env python3
"""Check private session teardown with deterministic unreaped child processes."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        return int(fields[19]), fields[0]
    except FileNotFoundError:
        return None


def running(pid, start):
    value = identity(pid)
    return value is not None and value[0] == start and value[1] != "Z"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    parser.add_argument("--ctl", default=str(ROOT / "target/release/maverickctl"))
    parser.add_argument("--exec-client", action="store_true")
    parser.add_argument("--stale-group", action="store_true")
    args = parser.parse_args()
    # Adopt the servers after the one-shot CLI exits, and deliberately leave
    # them unreaped until assertions finish. A zombie is not a running session.
    assert ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0
    with tempfile.TemporaryDirectory(prefix="mst-", dir="/tmp") as directory:
        directory = Path(directory)
        env = {key: value for key, value in os.environ.items() if not key.startswith("MAVERICK_")}
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            path = directory / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = directory / "config.toml"
        config.write_text("[autostart]\ncommands = []\n")
        refs = {}
        outside = subprocess.Popen(["sleep", "60"], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL, start_new_session=True)

        def ctl(*words):
            return subprocess.run([args.ctl, "--json", *words], env=env,
                                  capture_output=True, text=True, timeout=30)

        try:
            created = ctl("session", "create", "teardown-test", "--backend", "xvfb",
                          "--binary", args.wm, "--resolution", "800x600", "--", "--config", str(config))
            assert created.returncode == 0, created.stdout + created.stderr
            record = json.loads(created.stdout)
            for pid in (record["pid"], record["x_pid"]):
                refs[pid] = identity(pid)[0]
            client = None
            if args.exec_client:
                launched = ctl("exec", "teardown-test", "sh", "-c", "sleep 60 & wait")
                assert launched.returncode == 0, launched.stdout + launched.stderr
                client = json.loads(launched.stdout)["pid"]
                refs[client] = identity(client)[0]
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    listed = ctl("process", "list", "teardown-test")
                    assert listed.returncode == 0, listed.stdout + listed.stderr
                    children = [item["pid"] for item in json.loads(listed.stdout)["processes"]
                                if item["role"] == "child"]
                    if children:
                        for pid in children:
                            refs[pid] = identity(pid)[0]
                        break
                    time.sleep(0.02)
                else:
                    raise AssertionError("registered application child was not listed")
            if args.stale_group:
                record_path = Path(env["XDG_RUNTIME_DIR"]) / "maverick/teardown-test/session.json"
                stored = json.loads(record_path.read_text())
                stored["pgrps"] = [f"{outside.pid}:{identity(outside.pid)[0] + 1}"]
                record_path.write_text(json.dumps(stored))
            stopped = ctl("session", "stop", "teardown-test")
            print(json.dumps({"returncode": stopped.returncode, "stderr": stopped.stderr,
                              "states": {pid: identity(pid) for pid in refs}}), flush=True)
            if args.stale_group:
                assert stopped.returncode != 0 and "unverified application group" in stopped.stderr
            else:
                assert stopped.returncode == 0, stopped.stdout + stopped.stderr
            assert all(not running(pid, start) for pid, start in refs.items()), "owned process survived stop"
            assert outside.poll() is None, "teardown stopped an unrelated process"
            print("PASS: session stop accepts unreaped zombies and preserves an unrelated process")
            if client is not None:
                print("PASS: session stop terminates the registered exec client and its child")
            if args.stale_group:
                print("PASS: stale group identity grants no authority over an unrelated process")
        finally:
            ctl("session", "kill", "teardown-test")
            for pid, start in refs.items():
                if running(pid, start):
                    os.kill(pid, 9)
            deadline = time.monotonic() + 5
            pending = set(refs)
            while pending and time.monotonic() < deadline:
                for pid in list(pending):
                    try:
                        reaped, _ = os.waitpid(pid, os.WNOHANG)
                        if reaped:
                            pending.remove(pid)
                    except ChildProcessError:
                        pending.remove(pid)
                if pending:
                    time.sleep(0.02)
            outside.terminate()
            outside.wait(timeout=5)


if __name__ == "__main__":
    main()
