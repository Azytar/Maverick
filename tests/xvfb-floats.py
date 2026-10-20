#!/usr/bin/env python3
"""Check floating-window layers and focus on a private Xvfb display."""
import argparse
import json
import os
from pathlib import Path
import re
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def line(pipe):
    if not select.select([pipe], [], [], 5)[0]:
        raise RuntimeError("server or probe response timed out")
    value = pipe.readline().strip()
    if not value:
        raise RuntimeError("server or probe exited")
    return value


def wait_for(predicate, message):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.03)
    raise AssertionError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    parser.add_argument("--ctl", default=str(ROOT / "target/release/maverickctl"))
    parser.add_argument("--case", choices=("layers", "focus", "transients", "metadata"), default="focus")
    args = parser.parse_args()
    processes = []
    with tempfile.TemporaryDirectory(prefix="mfloat-", dir="/tmp") as directory:
        directory = Path(directory)
        env = {key: value for key, value in os.environ.items() if not key.startswith("MAVERICK_")}
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            path = directory / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = directory / "config.toml"
        config.write_text("[general]\nfocus_mouse = false\nwarp_cursor = false\n"
                          "[autostart]\ncommands = []\n")
        helper = directory / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/stacking-probe.c"),
                        "-o", str(helper), "-lX11"], check=True)
        with (directory / "wm.log").open("w+") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                           "-nolisten", "tcp"], stdout=subprocess.PIPE,
                                          stderr=log, text=True, env=env)
                processes.append(server)
                env["DISPLAY"] = ":" + line(server.stdout)
                wm = subprocess.Popen([args.wm, "--config", str(config), "--session-id", "floats"],
                                      env=env, stdout=log, stderr=log)
                processes.append(wm)

                def ctl(*words, check=True):
                    return subprocess.run([args.ctl, *words, "--session", "floats"], env=env,
                                          capture_output=True, text=True, timeout=5, check=check).stdout

                def tree():
                    return json.loads(ctl("query", "tree"))

                def ready():
                    if wm.poll() is not None:
                        raise RuntimeError("Maverick exited during startup")
                    return "window id #" in subprocess.check_output(
                        ["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env=env, text=True, timeout=3)

                wait_for(ready, "WM did not claim the private display")
                probes = {}

                def launch(parent=None):
                    words = [str(helper)]
                    if parent is not None:
                        words += ["--transient-for", str(parent)]
                    probe = subprocess.Popen(words, env=env, stdin=subprocess.PIPE,
                                             stdout=subprocess.PIPE, stderr=log, text=True)
                    processes.append(probe)
                    win = int(line(probe.stdout))
                    probes[win] = probe
                    wait_for(lambda: json.loads(ctl("query", "inspect"))["windows"]["total"] == len(probes),
                             "client was not managed")
                    return win

                def command(win, text):
                    probe = probes[win]
                    probe.stdin.write(text + "\n")
                    probe.stdin.flush()
                    return line(probe.stdout)

                def above(upper, lower):
                    first, second, *_ = map(int, command(upper, f"check {lower}").split())
                    if not first > second >= 0:
                        return False
                    if args.case == "metadata":
                        value = subprocess.check_output(["xprop", "-root", "_NET_CLIENT_LIST_STACKING"],
                                                        env=env, text=True, timeout=3)
                        order = [int(win, 16) for win in re.findall(r"0x[0-9a-fA-F]+", value)]
                        assert len(order) == len(set(order)), "stack metadata contains duplicate windows"
                        return upper in order and lower in order and order.index(upper) > order.index(lower)
                    return True

                def focus(win):
                    ctl("msg", f"focus_window {win}")
                    wait_for(lambda: f"0x{win:x}" in subprocess.check_output(
                        ["xprop", "-root", "_NET_ACTIVE_WINDOW"], env=env, text=True, timeout=3),
                        "focus did not reach the requested window")

                a = launch()
                ctl("msg", f"float_window {a}")
                b = launch()
                wait_for(lambda: above(a, b), "new tile covered an existing float")
                focus(b)
                wait_for(lambda: above(a, b), "focusing a tile covered an existing float")
                print("PASS: a new or focused tile stays below an existing float", flush=True)
                if args.case != "layers":
                    ctl("msg", f"float_window {b}")
                    if args.case in ("focus", "metadata"):
                        for win, other in ((a, b), (b, a), (a, b)):
                            focus(win)
                            wait_for(lambda: above(win, other), "focused float stayed behind its peer")
                        print("PASS: focusing either float raises it above the other", flush=True)
                        c = launch()
                        wait_for(lambda: above(a, c) and above(b, c), "new tile covered a float")
                        focus(c)
                        ctl("msg", "toggle_fullscreen")
                        wait_for(lambda: above(c, a) and above(c, b), "fullscreen did not cover floats")
                        focus(a)
                        wait_for(lambda: above(a, c), "focused float did not peek above fullscreen")
                        focus(c)
                        wait_for(lambda: above(c, a), "fullscreen did not regain its layer")
                        if args.case == "metadata":
                            command(a, "dock")
                            dock = int(command(a, "dockid"))
                            wait_for(lambda: above(c, dock), "published stack put a dock above fullscreen")
                            print("PASS: published stack matches X11 for float focus, fullscreen and docks", flush=True)
                        print("PASS: fullscreen and focused floating peek retain their layers", flush=True)
                    else:
                        child = launch(a)
                        grandchild = launch(child)
                        focus(a)
                        wait_for(lambda: above(grandchild, child) and above(child, a) and above(a, b),
                                 "focusing a float buried it or one of its transient descendants")
                        focus(b)
                        wait_for(lambda: above(b, grandchild), "unrelated focused float stayed buried")
                        print("PASS: transient descendants stay above their raised parent", flush=True)
                ctl("quit", "--yes", check=False)
                assert wm.wait(timeout=5) == 0
            except Exception:
                if "wm" in locals() and wm.poll() is None:
                    print("STATE", tree())
                    print(subprocess.check_output(["xprop", "-root", "_NET_CLIENT_LIST_STACKING"],
                                                  env=env, text=True, timeout=3))
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
