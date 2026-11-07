#!/usr/bin/env python3
"""Exercise live configuration reload on an owned Xvfb display."""
import argparse
import json
import math
import os
from pathlib import Path
import random
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
    args = parser.parse_args()
    processes = []
    with tempfile.TemporaryDirectory(prefix="mvl-", dir="/tmp") as tmp:
        tmp = Path(tmp)
        env = {k: v for k, v in os.environ.items() if not k.startswith("MAVERICK_")}
        for key in ("XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            path = tmp / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = tmp / "config.toml"
        values = {"gaps_inner": 4, "gaps_outer": 8, "border_width": 0}

        def write_config():
            fields = "\n".join(f"{key} = {json.dumps(value)}" for key, value in values.items())
            config.write_text(f"[general]\n{fields}\nfocus_mouse = false\nwarp_cursor = false\n"
                              "[autostart]\ncommands = []\n")

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
                probes = []
                for _ in range(6):
                    process = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                               stdout=subprocess.PIPE, stderr=log, text=True)
                    processes.append(process)
                    probes.append((process, int(line(process.stdout))))
                    wait_for(lambda: len(json.loads(ctl("query", "tree"))["monitors"][0]
                                         ["workspaces"][0]["columns"]) == len(probes),
                             "new tile was not managed")
                selected, window = probes[-1]

                def geometry(process=selected):
                    process.stdin.write("check\n")
                    process.stdin.flush()
                    return list(map(int, line(process.stdout).split()))[2:]

                def camera():
                    return json.loads(ctl("query", "inspect"))["layout"]["camera"]

                reloads = 0

                def reload():
                    nonlocal reloads
                    ctl("reload")
                    reloads += 1
                    wait_for(lambda: log_path.read_text().count("[INFO]  reload:") >= reloads,
                             "reload did not complete")

                def assert_visible(label, *, expected_outer=None, expected_border=None):
                    outer = min(values["gaps_outer"], 300) if expected_outer is None else expected_outer
                    border = values["border_width"] if expected_border is None else expected_border

                    def visible():
                        x, y, w, h, bw = geometry()
                        return (bw == border and y == outer
                                and h == max(1, 600 - 2 * outer - 2 * border)
                                and 0 <= x and x + w + 2 * bw <= 800
                                and 0 <= y and y + h + 2 * bw <= 600)

                    wait_for(visible, f"{label}: focused tile left the screen: {geometry()}")
                    actual = geometry()
                    time.sleep(0.08)
                    assert geometry() == actual, f"{label}: unstable geometry"
                    focused = int(subprocess.check_output(["xdotool", "getactivewindow"], env=env,
                                                          text=True, timeout=3))
                    assert focused == window, f"{label}: focus changed"
                    assert math.isfinite(camera()), f"{label}: non-finite camera"
                    print("PASS:", label, actual, "camera=", camera(), flush=True)

                assert_visible("initial layout")
                for inner, outer in ((200, 40), (500, 100), (0, 0), (40, 16)):
                    values.update(gaps_inner=inner, gaps_outer=outer)
                    write_config()
                    reload()
                    assert_visible(f"reload gaps {inner}/{outer}")

                # Out-of-range values are rejected individually. The parsed file is
                # still adopted, but those fields fall back to the compiled defaults
                # (4/8) rather than becoming enormous geometry or preserving the
                # previous live values.
                values.update(gaps_inner=4294967295, gaps_outer=4294967295)
                write_config()
                reload()
                wait_for(lambda: log_path.read_text().count("must be between 0 and 1000000") >= 2,
                         "out-of-range gaps were not diagnosed")
                assert_visible("reload rejects out-of-range gaps", expected_outer=8)

                values.update(gaps_inner=4, gaps_outer=8)
                write_config()
                reload()
                assert_visible("reload gaps 4/8")

                rng = random.Random(20261018)
                for case in range(16):
                    values.update(gaps_inner=rng.randrange(257), gaps_outer=rng.randrange(129),
                                  border_width=rng.randrange(9), smart_gaps=bool(case % 2),
                                  accordion_boost=rng.choice((0.0, 0.2, 0.8)),
                                  column_width=rng.choice((0.1, 0.4, 0.6, 1.0)))
                    write_config()
                    reload()
                    assert_visible(f"seeded reload {case}")

                ctl("msg", "page_snap:left")
                before_camera, before_geometry = camera(), geometry()
                reload()
                assert camera() == before_camera and geometry() == before_geometry, (
                    "an unchanged reload reset a deliberate camera pan")
                print("PASS: unchanged reload preserves a deliberate pan", flush=True)

                def assert_rejected_reload(label):
                    before_camera, before_geometry = camera(), geometry()
                    rejections = log_path.read_text().count("keeping current config")
                    ctl("reload")
                    wait_for(lambda: log_path.read_text().count("keeping current config") > rejections,
                             f"{label}: invalid reload was not rejected")
                    assert camera() == before_camera and geometry() == before_geometry
                    assert wm.poll() is None
                    print(f"PASS: {label} preserves the live layout", flush=True)

                for source in ("[general\ngaps_inner = broken\n",
                               "[general]\ngaps_inner = -0x10\n",
                               '[autostart]\ncommands = [["touch"], x"marker"]]\n'):
                    config.write_text(source)
                    assert_rejected_reload("malformed reload")
                config.unlink()
                assert_rejected_reload("missing file reload")
                config.mkdir()
                assert_rejected_reload("unreadable file reload")
                config.rmdir()

                values.update(gaps_inner=4, gaps_outer=8, border_width=0,
                              smart_gaps=False, accordion_boost=0.0)
                write_config()
                reload()
                assert_visible("valid reload recovers after malformed input")

                # Exercise the merged compatibility path through the live WM,
                # not just the parser: aliases must keep geometry, camera and
                # View count identical to their canonical spellings.
                values.update(gaps_inner=13, gaps_outer=13, border_width=3, view_count=6,
                              column_width=0.6)
                write_config()
                reload()
                assert_visible("canonical configuration before alias reload")
                before_camera, before_geometry = camera(), geometry()
                config.write_text('[general]\ngaps = 13\nborder_w = 3\nn_tags = 6\n'
                                  'auto_workspace_binds = true\nfocus_mouse = false\n'
                                  'warp_cursor = false\n[autostart]\napps = []\n')
                reload()
                assert camera() == before_camera and geometry() == before_geometry
                tree = json.loads(ctl("query", "tree"))
                assert len(tree["monitors"][0]["workspaces"]) == 6
                print("PASS: legacy aliases preserve canonical live geometry and Views", flush=True)
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
