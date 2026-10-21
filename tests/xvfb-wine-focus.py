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
import hashlib
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
    parser.add_argument("--wm-revision", help="explicit tested revision; hash is always recorded")
    parser.add_argument("--probe", type=Path, default=ROOT / "tests/wine-focus-probe.c")
    parser.add_argument("--complete", default="wine-focus")
    parser.add_argument("--honor-initial-state", action="store_true")
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--config-via-xdg", action="store_true", help="historical WMs without --config")
    parser.add_argument("--no-trace", action="store_true", help="verify production behavior without trace recording")
    parser.add_argument("--observer", action="store_true", help="probe also supports --observe; log external X11 snapshots")
    args = parser.parse_args()
    started = time.monotonic()

    out = args.evidence or Path(tempfile.mkdtemp(prefix="wine-focus-", dir="/tmp"))
    out.mkdir(parents=True, exist_ok=True)
    print(f"EVIDENCE {out}", flush=True)
    helper = out / "wine-focus-probe"
    subprocess.run(
        ["cc", "-Wall", "-Wextra", "-Werror",
         str(args.probe.resolve()),
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
    env.update(MAVERICK_TRACE="1", MAVERICK_TRACE_PATH=str(out / "wm.trace"),
               MAVERICK_LOG="warn",
               DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(out / "no-bus"))
    if args.no_trace:
        env.pop("MAVERICK_TRACE", None)
        env.pop("MAVERICK_TRACE_PATH", None)
    config = out / "config.toml"
    config.write_text(
        "[general]\n"
        "focus_mouse = false\n"
        "warp_cursor = false\n"
        f"honor_initial_state = {str(args.honor_initial_state).lower()}\n"
        "[autostart]\n"
        'commands = [["/usr/bin/true"]]\n'
    )
    if args.config_via_xdg:
        installed = Path(env["XDG_CONFIG_HOME"]) / "maverick/config.toml"
        installed.parent.mkdir()
        installed.write_text(config.read_text())
    processes = []
    failures = []
    try:
        with (out / "events.log").open("w") as events, (out / "wm.log").open("w") as logs, (out / "observer.log").open("w") as observation:
            server = subprocess.Popen(
                ["Xvfb", "-displayfd", "1", "-screen", "0", "1280x800x24",
                 "-nolisten", "tcp", "-noreset"],
                stdout=subprocess.PIPE, stderr=events, text=True)
            processes.append(server)
            env["DISPLAY"] = ":" + line(server.stdout)
            wm = subprocess.Popen(
                [str(args.wm.resolve())] + ([] if args.config_via_xdg else ["--config", str(config)]),
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
            if args.observer:
                observer = subprocess.Popen([str(helper), "--observe", "60"], env=env,
                                            stdout=observation, stderr=observation)
                processes.append(observer)
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
            (out / "probe.stdout").write_text(run.stdout)
            (out / "probe.stderr").write_text(run.stderr)
            if f"COMPLETE {args.complete}" not in run.stdout.splitlines():
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
        revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                                  capture_output=True, text=True).stdout.strip()
        (out / "manifest.json").write_text(json.dumps({
            "command": sys.argv, "harness_commit": revision, "wm_revision": args.wm_revision,
            "wm": str(args.wm.resolve()),
            "wm_sha256": hashlib.sha256(args.wm.read_bytes()).hexdigest(),
            "probe_sha256": hashlib.sha256(args.probe.read_bytes()).hexdigest(),
            "publication_rounds": env.get("AUDIT_PUBLICATION_ROUNDS"),
            "publication_only": env.get("AUDIT_PUBLICATION_ONLY"),
            "trace_enabled": not args.no_trace,
            "duration_seconds": time.monotonic() - started,
            "failures": failures, "exception": str(sys.exc_info()[1]) if sys.exc_info()[1] else None,
        }, indent=2) + "\n")
    print(json.dumps({"failures": failures}), flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
