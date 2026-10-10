#!/usr/bin/env python3
"""Check root cursor startup and ownership on an isolated Xvfb display."""
import argparse
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def line(pipe):
    if not select.select([pipe], [], [], 5)[0]:
        raise RuntimeError("probe or server response timed out")
    result = pipe.readline().strip()
    if not result:
        raise RuntimeError("probe or server exited")
    return result


def wait_for(predicate, message):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    args = parser.parse_args()
    processes = []
    with tempfile.TemporaryDirectory(prefix="mcur-", dir="/tmp") as scratch:
        scratch = Path(scratch)
        env = {k: v for k, v in os.environ.items() if not k.startswith("MAVERICK_")}
        env["XDG_RUNTIME_DIR"] = str(scratch)
        config = scratch / "config.toml"
        config.write_text("[general]\nwarp_cursor = false\n[autostart]\ncommands = []\n")
        helper = scratch / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/cursor-probe.c"),
                        "-o", str(helper), "-lX11", "-lXfixes"], check=True)
        with (scratch / "wm.log").open("w+") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                           "-nolisten", "tcp"], stdout=subprocess.PIPE,
                                          stderr=log, text=True)
                processes.append(server)
                env["DISPLAY"] = ":" + line(server.stdout)
                probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
                processes.append(probe)
                initial = list(map(int, line(probe.stdout).split()))

                def command(text):
                    probe.stdin.write(text + "\n")
                    probe.stdin.flush()
                    return list(map(int, line(probe.stdout).split()))

                blank = command("blank")
                assert blank[2] == 0, "the inherited cursor is not transparent"
                wm = subprocess.Popen([args.wm, "--config", str(config)], env=env,
                                      stdout=log, stderr=log)
                processes.append(wm)

                def ready():
                    if wm.poll() is not None:
                        raise RuntimeError("Maverick exited during startup")
                    prop = subprocess.run(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"],
                                          env=env, capture_output=True, text=True, timeout=3)
                    return "window id #" in prop.stdout

                wait_for(ready, "Maverick did not claim the display")
                arrow = command("sample")
                assert arrow[2] > 0, f"empty desktop cursor has no visible pixels: {arrow}"
                print(f"PASS: startup replaces an inherited transparent cursor with the standard arrow "
                      f"({arrow[0]}x{arrow[1]}, {arrow[2]} visible pixels)")
                time.sleep(0.15)
                assert command("sample") == arrow
                print("PASS: the root cursor remains visible without opening an application")
                command("client")
                wait_for(lambda: command("sample") != arrow, "the client's own cursor was replaced")
                assert command("sample")[2] > 0
                print("PASS: applications retain their own cursor")
                command("destroy")
                wait_for(lambda: command("sample") == arrow, "the empty desktop lost its arrow")
                print("PASS: closing the last application restores the root cursor")
                wm.terminate()
                assert wm.wait(timeout=5) == 0, "Maverick did not shut down cleanly"
                assert command("sample") == initial, "shutdown did not restore the server default"
                print("PASS: shutdown releases the root cursor to the server default")
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read())
                raise
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                    try:
                        process.wait(timeout=4)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    main()
