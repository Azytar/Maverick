#!/usr/bin/env python3
"""Regression for unmanaged focus and Globally Active protocol offers.

This models a client that focuses an auxiliary override-redirect window.
InputOnly pointer clipping, XI2 and keyboard delivery are tested separately
by xvfb-input.py; a pointer grab does not imply a keyboard focus transfer.

Uses tests/wine-focus-probe.c. Starts its own Xvfb, config and runtime dir;
never touches a live session. Run after `cargo build -p maverick`:

    python3 tests/xvfb-wine-focus.py [--wm target/debug/maverick]

Exits non-zero if any check fails.
"""
import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def line(pipe):
    if not select.select([pipe], [], [], 10)[0]:
        raise RuntimeError("the X server never reported its display")
    text = pipe.readline().strip()
    if not text:
        raise RuntimeError("the X server exited before reporting its display")
    return text


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wm", type=Path, default=ROOT / "target/debug/maverick")
    args = parser.parse_args()

    out = Path(tempfile.mkdtemp(prefix="wine-focus-", dir="/tmp"))
    print(f"EVIDENCE {out}", flush=True)
    helper = out / "wine-focus-probe"
    subprocess.run(
        ["cc", "-Wall", "-Wextra", "-Werror",
         str(ROOT / "tests/wine-focus-probe.c"),
         "-o", str(helper), "-lX11"],
        check=True,
    )
    env = os.environ.copy()
    for name in list(env):
        if name.startswith("MAVERICK_") or name in (
            "DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XAUTHORITY"
        ):
            del env[name]
    for name in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME",
                 "XDG_STATE_HOME", "XDG_CACHE_HOME"):
        path = out / name
        path.mkdir(mode=0o700)
        env[name] = str(path)
    env.update(MAVERICK_LOG="warn",
               DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(out / "no-bus"))
    config = out / "config.toml"
    config.write_text(
        "[general]\n"
        "focus_mouse = false\n"
        "warp_cursor = false\n"
        "[autostart]\n"
        'commands = [["/usr/bin/true"]]\n'
    )
    processes = []
    failures = []
    try:
        with (out / "events.log").open("w") as events, (out / "wm.log").open("w") as logs:
            server = subprocess.Popen(
                ["Xvfb", "-displayfd", "1", "-screen", "0", "1280x800x24",
                 "-nolisten", "tcp", "-noreset"],
                stdout=subprocess.PIPE, stderr=events, text=True)
            processes.append(server)
            env["DISPLAY"] = ":" + line(server.stdout)
            wm = subprocess.Popen(
                [str(args.wm.resolve()), "--config", str(config)],
                env=env, stdout=logs, stderr=logs)
            processes.append(wm)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                probe = subprocess.run(
                    ["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"],
                    env=env, capture_output=True, text=True, timeout=20)
                if "window id #" in probe.stdout:
                    break
                time.sleep(0.1)
            else:
                raise RuntimeError("the WM never claimed the screen")
            time.sleep(0.5)
            run = subprocess.run([str(helper)], env=env, capture_output=True,
                                 text=True, timeout=60)
            if wm.poll() is not None:
                failures.append("wm-exit-status")
            print(run.stdout, flush=True)
            print(run.stderr, file=sys.stderr, flush=True)
            if run.returncode != 0:
                print(f"probe exited with status {run.returncode}",
                      file=sys.stderr, flush=True)
                failures.append("probe-exit-status")
            if "COMPLETE wine-focus" not in run.stdout.splitlines():
                failures.append("probe-incomplete")
            for raw in run.stdout.splitlines():
                if not raw.startswith("RESULT "):
                    continue
                detail = json.loads(raw[len("RESULT "):])
                name = detail.pop("name")
                passed = detail.pop("passed")
                print(json.dumps({"check": name, "passed": passed, **detail}),
                      flush=True)
                if not passed:
                    failures.append(name)
            if not any(True for _ in failures) and not run.stdout.count("RESULT"):
                failures.append("probe-produced-no-results")
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    print(json.dumps({"failures": failures}), flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
