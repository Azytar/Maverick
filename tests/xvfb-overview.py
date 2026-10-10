#!/usr/bin/env python3
"""Observe pixels and client events across Overview, using only owned processes.

The default target is the release binary. --display lets the Xephyr wrapper
reuse its own nested server; --compositor checks an external Picom instance.
"""
import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent


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
    parser.add_argument("--display")
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    parser.add_argument("--ctl", default=str(ROOT / "target/release/maverickctl"))
    parser.add_argument("--compositor", action="store_true")
    parser.add_argument("--without-composite", action="store_true")
    args = parser.parse_args()
    processes = []
    with tempfile.TemporaryDirectory(prefix="mov-", dir="/tmp") as scratch:
        scratch = Path(scratch)
        env = {k: v for k, v in os.environ.items() if not k.startswith("MAVERICK_")}
        env["XDG_RUNTIME_DIR"] = str(scratch)
        config = scratch / "config.toml"
        config.write_text("""[general]
focus_mouse = true
warp_cursor = false
gaps_inner = 8
gaps_outer = 8
border_w = 2
accordion_boost = 0.3
[autostart]
commands = []
""")
        with (scratch / "wm.log").open("w+") as log:
            try:
                if args.display:
                    env["DISPLAY"] = args.display
                else:
                    command = ["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24", "-nolisten", "tcp"]
                    if args.without_composite:
                        command.extend(["-extension", "Composite"])
                    server = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
                    processes.append(server)
                    env["DISPLAY"] = ":" + line(server.stdout)
                if args.compositor:
                    compositor = subprocess.Popen(["picom", "--backend", "xrender", "--config", "/dev/null"],
                                                  env=env, stdout=log, stderr=log)
                    processes.append(compositor)
                    time.sleep(0.2)
                    assert compositor.poll() is None, "Picom exited during startup"
                wm = subprocess.Popen([args.wm, "--config", str(config)], env=env, stdout=log, stderr=log)
                processes.append(wm)
                time.sleep(0.05)

                def ctl(*command):
                    return subprocess.run([args.ctl, *command], env=env, text=True, capture_output=True,
                                          check=True, timeout=3).stdout

                def state():
                    return json.loads(ctl("state"))

                def ready():
                    if wm.poll() is not None:
                        raise RuntimeError("Maverick exited during startup")
                    root = subprocess.run(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"],
                                          env=env, text=True, capture_output=True, timeout=3)
                    return "window id #" in root.stdout

                wait_for(ready, "Maverick did not publish its monitor")
                helper = scratch / "probe"
                subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(HERE / "overview-probe.c"),
                                "-o", str(helper), "-lX11", "-lXcomposite"], check=True)
                probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
                processes.append(probe)
                first = int(line(probe.stdout))

                def command(text):
                    probe.stdin.write(text + "\n")
                    probe.stdin.flush()
                    return line(probe.stdout)

                def stats(index=0):
                    return list(map(int, command(f"stats {index}").split()))

                def bounds(color=0xffffff):
                    return list(map(int, command(f"bounds {color:x}").split()))

                def key(chord):
                    subprocess.run(["xdotool", "key", "--clearmodifiers", chord], env=env,
                                   check=True, timeout=3)

                def stable():
                    before = stats()
                    time.sleep(0.1)
                    return before == stats() and before[2] > 400

                wait_for(stable, "the client never reached a stable tile")
                owner = command("owner")
                assert (owner != "0") == args.compositor
                baseline = stats()[:5]
                normal = bounds()
                assert normal[2] > 0 and normal[3] > 0, "the normal client image is missing"
                command("reset")
                key("super+o")
                if args.without_composite:
                    time.sleep(0.3)
                    assert wm.poll() is None
                    assert stats()[:5] == baseline and stats()[5:] == [0] * 5
                    assert bounds() == normal
                    assert command("owner") == "0"
                    print("PASS: unavailable Composite leaves the client and displayed image unchanged")
                    return
                wait_for(lambda: bounds()[2] < normal[2] * 0.85, "Mod+O did not reduce the displayed image")
                image = bounds()
                assert image[2] > normal[2] * 0.65 and image[3] > normal[3] * 0.65
                assert abs(image[2] / normal[2] - image[3] / normal[3]) < 0.02
                assert stats()[:5] == baseline and stats()[5:] == [0] * 5, stats()
                assert command("owner") == owner, "Overview replaced the compositor"
                print(f"PASS: Mod+O reduces pixels {normal[2:]} -> {image[2:]}; client remains {baseline[2:4]} with zero configure/map/unmap events")
                sample_x = image[0] + image[2] * 3 // 4
                sample_y = image[1] + image[3] * 3 // 4
                command("paint ff0000")
                wait_for(lambda: int(command(f"pixel {sample_x} {sample_y}")) & 0xffffff == 0xff0000,
                         "the preview did not update after client damage")
                assert stats()[5:] == [0] * 5
                print("PASS: client repaint updates the reduced image without resizing")
                subprocess.run(["xdotool", "mousemove", "--sync", str(sample_x), str(sample_y), "click", "1"],
                               env=env, check=True, timeout=3)
                time.sleep(0.1)
                assert stats()[9] == 0, "Overview replayed its selection click to the application"
                key("super+o")
                wait_for(lambda: bounds(0xff0000)[2] == normal[2], "leaving Overview did not restore the normal image")
                assert stats()[:5] == baseline and stats()[5:] == [0] * 5
                print("PASS: entering and leaving preserves exact geometry and consumes selection clicks")
                command("paint ffffff")
                peer = int(command("new"))
                wait_for(lambda: sum(ws["windows"] for mon in state()["monitors"] for ws in mon["workspaces"]) == 2, "the peer was not managed")
                time.sleep(0.2)
                before = [stats(0)[:5], stats(1)[:5]]
                command("reset")
                key("super+o")
                time.sleep(0.15)
                ctl("msg", f"focus_window:{first}")
                for _ in range(8):
                    ctl("msg", "overview_nav:right")
                    ctl("msg", "overview_nav:left")
                time.sleep(0.15)
                for index in (0, 1):
                    assert stats(index)[:5] == before[index] and stats(index)[5:] == [0] * 5, stats(index)
                assert command("owner") == owner
                print("PASS: navigation and focus with accordion enabled leave both client geometries unchanged")
                fresh = int(command("new"))
                wait_for(lambda: sum(ws["windows"] for mon in state()["monitors"] for ws in mon["workspaces"]) == 3, "a new client did not join Overview")
                wait_for(lambda: stats(2)[2] > 400, "the new client did not get its initial logical layout")
                for index in (0, 1):
                    assert stats(index)[:5] == before[index] and stats(index)[5:] == [0] * 5, stats(index)
                command("destroy 2")
                wait_for(lambda: sum(ws["windows"] for mon in state()["monitors"] for ws in mon["workspaces"]) == 2, "the destroyed client remained in Overview")
                assert fresh != peer
                print("PASS: mapping and destroying a client preserves the existing applications")
                ctl("msg", "view:2")
                time.sleep(0.15)
                assert wm.poll() is None
                ctl("msg", "view:1")
                time.sleep(0.15)
                assert wm.poll() is None
                ctl("msg", "overview_enter")
                time.sleep(0.15)
                assert wm.poll() is None and command("owner") == owner
                print("PASS: View switches and Overview exit release the temporary presentation")
                for verb in ("float_window", "fullscreen_window"):
                    ctl("msg", f"{verb}:{first}")
                    time.sleep(0.2)
                    preserved = stats()[:5]
                    prefs = subprocess.run(["xprop", "-id", str(first), "_NET_WM_STATE"], env=env,
                                           text=True, capture_output=True, check=True, timeout=3).stdout
                    command("reset")
                    ctl("msg", "toggle_overview")
                    time.sleep(0.15)
                    assert stats()[:5] == preserved and stats()[5:] == [0] * 5
                    current = subprocess.run(["xprop", "-id", str(first), "_NET_WM_STATE"], env=env,
                                             text=True, capture_output=True, check=True, timeout=3).stdout
                    assert current == prefs
                    if verb == "float_window":
                        command("paintwin 0 ff")
                        wait_for(lambda: bounds(0xff)[2] > 0, "the floating image is missing")
                        old_image = bounds(0xff)
                        command(f"resize 0 {preserved[2] + 60} {preserved[3] + 20}")
                        wait_for(lambda: stats()[2:4] == [preserved[2] + 60, preserved[3] + 20],
                                 "Overview rejected the floating application's resize")
                        command("paintwin 0 ff")
                        wait_for(lambda: bounds(0xff)[2] > old_image[2] + 20,
                                 "Overview clipped the application's new logical size")
                        preserved = stats()[:5]
                        command("reset")
                        print("PASS: a floating application's requested resize updates the image at the stored scale")
                    ctl("msg", "toggle_overview")
                    time.sleep(0.15)
                    assert stats()[:5] == preserved and stats()[5:] == [0] * 5
                    ctl("msg", f"{verb}:{first}")
                    time.sleep(0.15)
                    print(f"PASS: Overview preserves {verb} client geometry and EWMH state")
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
