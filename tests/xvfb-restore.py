#!/usr/bin/env python3
"""Check normal fullscreen exit and explicit presentation restoration on X11."""
import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
FULLSCREEN = "_NET_WM_STATE_FULLSCREEN"
VERT = "_NET_WM_STATE_MAXIMIZED_VERT"
HORZ = "_NET_WM_STATE_MAXIMIZED_HORZ"
ATTENTION = "_NET_WM_STATE_DEMANDS_ATTENTION"


def line(pipe):
    if not select.select([pipe], [], [], 5)[0]:
        raise RuntimeError("X server or probe response timed out")
    value = pipe.readline().strip()
    if not value:
        raise RuntimeError("X server or probe exited")
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
    args = parser.parse_args()
    processes = []
    with tempfile.TemporaryDirectory(prefix="mrestore-", dir="/tmp") as directory:
        directory = Path(directory)
        env = {key: value for key, value in os.environ.items() if not key.startswith("MAVERICK_")}
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            path = directory / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = directory / "config.toml"
        config.write_text("[general]\nfocus_mouse = false\nwarp_cursor = false\n"
                          "gaps_outer = 10\nborder_width = 2\n[autostart]\ncommands = []\n")
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
                wm = subprocess.Popen([args.wm, "--config", str(config), "--session-id", "restore"],
                                      env=env, stdout=log, stderr=log)
                processes.append(wm)

                def ctl(*words, check=True):
                    return subprocess.run([args.ctl, *words, "--session", "restore"], env=env,
                                          capture_output=True, text=True, timeout=5, check=check).stdout

                def ready():
                    if wm.poll() is not None:
                        raise RuntimeError("Maverick exited during startup")
                    return "window id #" in subprocess.check_output(
                        ["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env=env, text=True, timeout=3)

                wait_for(ready, "WM did not claim the display")
                probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
                processes.append(probe)
                win = line(probe.stdout)

                def command(text):
                    probe.stdin.write(text + "\n")
                    probe.stdin.flush()
                    return line(probe.stdout)

                def geometry():
                    return list(map(int, command("check").split()))[2:]

                def properties():
                    return subprocess.check_output(["xprop", "-id", win, "_NET_WM_STATE",
                                                    "_NET_WM_BYPASS_COMPOSITOR"],
                                                   env=env, text=True, timeout=3)

                def has(*atoms):
                    value = properties()
                    return all(atom in value for atom in atoms)

                def restored():
                    return not any(atom in properties() for atom in (FULLSCREEN, VERT, HORZ))

                wait_for(lambda: json.loads(ctl("query", "inspect"))["windows"]["total"] == 1,
                         "probe was not managed")
                for action in (3, 0xFFFFFFFF):
                    for atom in (FULLSCREEN, HORZ, ATTENTION):
                        command(f"state {action} {atom}")
                        command(f"state 1 {VERT}")
                        wait_for(lambda: has(VERT), "valid state sentinel was not processed")
                        assert not has(atom), f"invalid action {action} changed {atom}"
                        ctl("msg", "restore_layout")
                        wait_for(restored, "sentinel maximize was not restored")
                command("fs")
                wait_for(lambda: has(FULLSCREEN), "fullscreen was not applied")
                command(f"state 3 {FULLSCREEN}")
                command(f"state 1 {VERT}")
                wait_for(lambda: has(VERT), "fullscreen sentinel was not processed")
                assert has(FULLSCREEN), "invalid action exited fullscreen"
                ctl("msg", "restore_layout")
                wait_for(restored, "fullscreen sentinel was not restored")
                print("PASS: invalid EWMH actions change no fullscreen, maximize or attention state")
                command("dock")
                wait_for(lambda: geometry()[1] == 34, "dock workarea was not applied")
                original = geometry()
                for floating in (False, True):
                    if floating:
                        ctl("msg", "toggle_float")
                    baseline = geometry()
                    command(f"state 1 {VERT} {HORZ}")
                    wait_for(lambda: has(VERT, HORZ) and geometry() == [0, 24, 800, 576, 0],
                             "client maximize request was not applied")
                    maximized = geometry()
                    command("fs")
                    wait_for(lambda: has(FULLSCREEN) and geometry() == [0, 0, 800, 600, 0],
                             "client fullscreen request was not applied")
                    command(f"state 0 {FULLSCREEN}")
                    wait_for(lambda: not has(FULLSCREEN) and has(VERT, HORZ)
                             and geometry() == maximized,
                             "normal video exit did not restore maximization")
                    print(f"PASS: normal fullscreen exit restores maximized state (floating={floating})")
                    command("fs")
                    wait_for(lambda: has(FULLSCREEN), "second fullscreen request was rejected")
                    ctl("msg", "restore_layout")
                    wait_for(lambda: restored() and geometry() == baseline,
                             "explicit restore did not recover original placement")
                    assert "= 2" not in properties(), "fullscreen compositor bypass survived restore"
                    print(f"PASS: explicit restore clears both states and restores geometry (floating={floating})")
                    command("events")
                    tree = json.loads(ctl("query", "tree"))
                    for _ in range(5):
                        ctl("msg", "restore_layout")
                    assert json.loads(ctl("query", "tree")) == tree
                    assert command("events") == "0 0", "repeated restore changed client properties or geometry"
                    print("PASS: repeating restore produces no client configure/property events")
                    command("fs")
                    wait_for(lambda: has(FULLSCREEN), "a later video could not enter fullscreen")
                    subprocess.run(["xdotool", "key", "--clearmodifiers", "super+shift+Escape"],
                                   env=env, check=True, timeout=5)
                    wait_for(lambda: restored() and geometry() == baseline,
                             "default restore shortcut did not restore placement")
                    print("PASS: fullscreen can be requested again and the WM shortcut exits it")
                    for axis in (VERT, HORZ):
                        command(f"state 1 {axis}")
                        wait_for(lambda: has(axis) and geometry() != baseline,
                                 "per-axis maximize request was not applied")
                        axis_geometry = geometry()
                        command("fs")
                        wait_for(lambda: has(FULLSCREEN) and geometry() == [0, 0, 800, 600, 0],
                                 "per-axis maximized window did not enter fullscreen")
                        command(f"state 0 {FULLSCREEN}")
                        wait_for(lambda: not has(FULLSCREEN) and has(axis)
                                 and geometry() == axis_geometry,
                                 "normal fullscreen exit did not restore the maximized axis")
                        ctl("msg", "restore_layout")
                        wait_for(lambda: restored() and geometry() == baseline,
                                 "per-axis maximize did not restore placement")
                    print(f"PASS: independent maximized axes restore placement (floating={floating})")
                ctl("msg", "toggle_float")
                wait_for(lambda: geometry() == original, "returning to tiling changed the layout")
                print("PASS: video fullscreen cycles preserve the original tiled layout")
                ctl("quit", "--yes", check=False)
                assert wm.wait(timeout=5) == 0
            except Exception:
                if "probe" in locals() and probe.poll() is None:
                    print("CLIENT", geometry(), properties())
                    print("STATE", ctl("query", "tree"))
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
