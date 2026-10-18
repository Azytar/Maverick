#!/usr/bin/env python3
"""Exercise window-manager restart on an owned Xvfb display."""
import argparse
import json
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
        time.sleep(0.03)
    raise AssertionError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    parser.add_argument("--ctl", default=str(ROOT / "target/release/maverickctl"))
    parser.add_argument("--pointer-focus", action="store_true",
                        help="also exercise hover focus and cursor warping")
    args = parser.parse_args()
    processes = []
    with tempfile.TemporaryDirectory(prefix="mvr-", dir="/tmp") as tmp:
        tmp = Path(tmp)
        env = {k: v for k, v in os.environ.items() if not k.startswith("MAVERICK_")}
        for key in ("XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            path = tmp / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = tmp / "config.toml"
        autostart_log = tmp / "autostart.log"
        autostart_script = tmp / "autostart.sh"
        autostart_script.write_text('printf "started\\n" >> "$1"\n')
        values = {"n_tags": 3, "gaps_inner": 4, "gaps_outer": 8, "border_width": 0}
        pointer_focus_enabled = False

        def write_config():
            fields = "\n".join(f"{key} = {json.dumps(value)}" for key, value in values.items())
            commands = [["/bin/sh", str(autostart_script), str(autostart_log)]]
            pointer = str(pointer_focus_enabled).lower()
            config.write_text(f"[general]\n{fields}\nfocus_mouse = {pointer}\nwarp_cursor = {pointer}\n"
                              f"[autostart]\ncommands = {json.dumps(commands)}\n")

        write_config()
        helper = tmp / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/stacking-probe.c"),
                        "-o", str(helper), "-lX11"], check=True)
        log_path = tmp / "wm.log"
        with log_path.open("w+") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                           "-nolisten", "tcp"], env=env, stdout=subprocess.PIPE,
                                          stderr=log, text=True)
                processes.append(server)
                env["DISPLAY"] = ":" + line(server.stdout)
                wm = subprocess.Popen([args.wm, "--config", str(config)], env=env,
                                      stdout=log, stderr=log)
                processes.append(wm)

                def ctl(*command):
                    return subprocess.check_output([args.ctl, *command], env=env, text=True, timeout=5)

                def ready():
                    if wm.poll() is not None:
                        raise RuntimeError("Maverick exited")
                    reply = subprocess.check_output(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"],
                                                    env=env, text=True, timeout=3)
                    return "window id #" in reply

                wait_for(ready, "Maverick did not claim the display")
                wait_for(autostart_log.exists, "opt-in autostart did not run")
                probes = []
                for _ in range(6):
                    process = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                               stdout=subprocess.PIPE, stderr=log, text=True)
                    processes.append(process)
                    probes.append((process, int(line(process.stdout))))
                    wait_for(lambda: len(json.loads(ctl("query", "tree"))["monitors"][0]
                                         ["workspaces"][0]["columns"]) == len(probes),
                             "new tile was not managed")
                selected, _ = probes[-1]

                def geometry(process=selected):
                    process.stdin.write("check\n")
                    process.stdin.flush()
                    return list(map(int, line(process.stdout).split()))[2:]

                def msg(action):
                    ctl("msg", action)

                def state():
                    tree = json.loads(ctl("query", "tree"))
                    # These caches observe the new connection, not persistent
                    # layout. Actual X geometry is checked separately below.
                    for mon in tree["monitors"]:
                        for ws in mon["workspaces"]:
                            windows = ws["floats"] + [w for c in ws["columns"] for w in c["windows"]]
                            for window in windows:
                                for field in ("desired", "applied", "real"):
                                    window.pop(field, None)
                    return tree

                def settled():
                    time.sleep(0.15)
                    return state()

                # Two exclusive windows on View 1, distinct column sizes and
                # a floating window on View 2. Keep every client alive.
                for _, win in probes[:2]:
                    msg(f"fullscreen_window:{win}")
                for _, win in probes[2:]:
                    msg(f"focus_window:{win}")
                    msg("move_to_ws:2")
                msg("view_create")
                msg("view:2")
                msg(f"focus_window:{probes[3][1]}")
                msg("grow_col:-123")
                msg(f"float_window:{probes[-1][1]}")
                msg(f"focus_window:{probes[4][1]}")
                msg("page_snap:left")
                if args.pointer_focus:
                    for x, y in ((20, 20), (400, 300)):
                        subprocess.run(["xdotool", "mousemove", str(x), str(y)], env=env, check=True)
                    pointer_focus_enabled = True
                    # Keep hover activation separate from actual pointer motion.
                    write_config()
                    ctl("reload")
                    wait_for(lambda: "[INFO]  reload:" in log_path.read_text(), "pointer settings did not reload")
                before = settled()
                assert before["monitors"][0]["active_ws"] == 1
                assert len(before["monitors"][0]["workspaces"][0]["columns"]) == 2
                assert len(before["monitors"][0]["workspaces"][1]["columns"]) == 3
                positions = [geometry(probe) for probe, _ in probes]
                def pointer_position():
                    return subprocess.check_output(["xdotool", "getmouselocation", "--shell"],
                                                   env=env, text=True).splitlines()[:2]
                pointer = pointer_position()
                before_active = int(subprocess.check_output(["xdotool", "getwindowfocus"], env=env, text=True))
                desktop = [subprocess.check_output(["xprop", "-id", str(win), "_NET_WM_DESKTOP"],
                                                  env=env, text=True) for _, win in probes]
                for attempt in range(3):
                    ctl("restart")
                    wait_for(lambda: log_path.read_text().count("maverick ready") >= attempt + 2,
                             "restart did not finish")
                    after = settled()
                    assert after == before, ("restart changed the layout", before, after)
                    assert [geometry(probe) for probe, _ in probes] == positions
                    assert pointer_position() == pointer, "restart moved the cursor"
                    assert [subprocess.check_output(["xprop", "-id", str(win), "_NET_WM_DESKTOP"],
                                                    env=env, text=True) for _, win in probes] == desktop
                    for _, win in probes[:2]:
                        prop = subprocess.check_output(["xprop", "-id", str(win), "_NET_WM_STATE"],
                                                       env=env, text=True)
                        assert "_NET_WM_STATE_FULLSCREEN" in prop, prop
                    assert all(probe.poll() is None for probe, _ in probes)
                    active = int(subprocess.check_output(["xdotool", "getwindowfocus"],
                                                         env=env, text=True))
                    assert active == before_active, "real X11 focus changed"
                    assert autostart_log.read_text().splitlines() == ["started"]
                    prop = subprocess.check_output(["xprop", "-root", "_NET_NUMBER_OF_DESKTOPS"],
                                                   env=env, text=True)
                    assert int(prop.rsplit("=", 1)[1]) == 4, prop
                    prop = subprocess.check_output(["xprop", "-root", "_MAVERICK_RESTART"],
                                                   env=env, text=True)
                    assert "(CARDINAL)" not in prop, "handoff was not consumed"
                    print("PASS: restart preserves Views, columns, widths, floats, cameras and fullscreen", attempt, flush=True)

                floating, floating_id = probes[-1]
                msg(f"focus_window:{floating_id}")
                float_geometry = geometry(floating)
                msg(f"fullscreen_window:{floating_id}")
                before = settled()
                ctl("restart")
                wait_for(lambda: log_path.read_text().count("maverick ready") >= 5,
                         "floating fullscreen restart did not finish")
                assert settled() == before
                msg(f"fullscreen_window:{floating_id}")
                time.sleep(0.15)
                assert geometry(floating) == float_geometry, "pre-fullscreen float geometry was lost"
                print("PASS: restart preserves the pre-fullscreen floating geometry", flush=True)

                msg("view:1")
                for _, win in probes[:2]:
                    msg(f"fullscreen_window:{win}")
                for probe, _ in probes[:2]:
                    assert geometry(probe)[2:4] != [800, 600]
                print("PASS: fullscreen can be left after restart", flush=True)

                msg("view:2")
                msg(f"focus_window:{probes[4][1]}")
                logical_geometry = [geometry(probe) for probe, _ in probes]
                msg("toggle_overview")

                def overview_visible():
                    return subprocess.run(["xdotool", "search", "--onlyvisible", "--name",
                                           "^Maverick Overview$"], env=env,
                                          capture_output=True, timeout=3).returncode == 0

                wait_for(overview_visible, "Overview did not enter")
                before = settled()
                ctl("restart")
                wait_for(lambda: log_path.read_text().count("maverick ready") >= 6,
                         "Overview restart did not finish")
                wait_for(overview_visible, "Overview was lost on restart")
                assert settled() == before
                assert [geometry(probe) for probe, _ in probes] == logical_geometry
                msg("toggle_overview")
                print("PASS: restart preserves Overview and logical client geometry", flush=True)

                values.update(gaps_inner=200, gaps_outer=40)
                write_config()
                ctl("restart")
                wait_for(lambda: log_path.read_text().count("maverick ready") >= 7,
                         "restart with changed settings did not finish")
                x, y, width, height, border = geometry(probes[4][0])
                assert y == 40 and height == 520 and border == 0
                assert 0 <= x and x + width <= 800, "changed settings left the selected tile off screen"
                print("PASS: restart applies new spacing and retargets the camera", flush=True)
                assert autostart_log.read_text().splitlines() == ["started"]
                ctl("quit", "--yes")
                assert wm.wait(timeout=5) == 0
            except Exception:
                log.flush()
                print(log_path.read_text())
                raise
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                        try:
                            process.wait(timeout=3)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait(timeout=3)


if __name__ == "__main__":
    main()
