#!/usr/bin/env python3
"""Isolated X11 focus protocol and input ownership regression."""
import argparse
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def display(server):
    if not select.select([server.stdout], [], [], 10)[0]:
        raise RuntimeError("X server startup timed out")
    number = server.stdout.readline().strip()
    if not number:
        raise RuntimeError("X server exited before allocating a display")
    return ":" + number


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wm", type=Path, default=ROOT / "target/release/maverick")
    parser.add_argument("--case", choices=["protocol", "ownership", "child"], default="protocol")
    parser.add_argument("--trace", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="maverick-input-") as temp:
        directory = Path(temp)
        helper = directory / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/input-probe.c"),
                        "-o", str(helper), "-lX11"], check=True)
        env = os.environ.copy()
        for name in list(env):
            if name.startswith("MAVERICK_") or name in ("DISPLAY", "WAYLAND_DISPLAY", "XAUTHORITY"):
                del env[name]
        for name in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            path = directory / name
            path.mkdir(mode=0o700)
            env[name] = str(path)
        env["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=" + str(directory / "no-bus")
        if args.trace:
            env["MAVERICK_TRACE"] = "1"
            env["MAVERICK_TRACE_PATH"] = str(args.trace.resolve())
        config = directory / "config.toml"
        config.write_text('[general]\nfocus_mouse = false\nwarp_cursor = false\n[autostart]\ncommands = []\n')
        processes = []
        with (directory / "wm.log").open("w+") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "1280x800x24",
                                           "-nolisten", "tcp", "-noreset"], stdout=subprocess.PIPE,
                                          stderr=log, text=True)
                processes.append(server)
                env["DISPLAY"] = display(server)
                wm = subprocess.Popen([str(args.wm.resolve()), "--config", str(config)], env=env,
                                      stdout=log, stderr=log)
                processes.append(wm)
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    ready = subprocess.run(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env=env,
                                           capture_output=True, text=True, timeout=3)
                    if "window id #" in ready.stdout:
                        break
                    if wm.poll() is not None:
                        raise RuntimeError("Maverick exited during startup")
                    time.sleep(0.02)
                else:
                    raise RuntimeError("Maverick startup timed out")
                subprocess.run([str(helper), args.case], env=env, timeout=30, check=True)
            except Exception:
                log.seek(0)
                print(log.read(), end="")
                raise
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    main()
