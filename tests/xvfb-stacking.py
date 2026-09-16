#!/usr/bin/env python3
"""XQueryTree regression. Run after cargo build -p maverick.
Uses its own Xvfb, runtime/config and PIDs; never touches a live session.
"""
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def line(pipe):
    if not select.select([pipe], [], [], 5)[0]:
        raise RuntimeError("response timed out")
    result = pipe.readline().strip()
    if not result:
        raise RuntimeError("probe/server exited")
    return result


def main():
    processes = []
    with tempfile.TemporaryDirectory(prefix="mst-", dir="/tmp") as tmp:
        tmp = Path(tmp)
        env = os.environ.copy()
        for key in list(env):
            if key.startswith("MAVERICK_"):
                del env[key]
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "HOME"):
            path = tmp / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        env["MAVERICK_NO_COMPOSITOR"] = "1"
        config = tmp / "config.toml"
        config.write_text('''[general]
focus_mouse = false
warp_cursor = false
[compositor]
enabled = false
[animations]
enabled = false
[autostart]
commands = []
[[keybindings]]
key = "Mod4+Shift+f"
action = "toggle_fullscreen"
[[keybindings]]
key = "Mod4+h"
action = "focus:left"
[[keybindings]]
key = "Mod4+l"
action = "focus:right"
''')
        helper = tmp / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/stacking-probe.c"),
                        "-o", str(helper), "-lX11"], check=True)
        with (tmp / "wm.log").open("w+") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                           "-nolisten", "tcp"], stdout=subprocess.PIPE,
                                          stderr=log, text=True)
                processes.append(server)
                env["DISPLAY"] = ":" + line(server.stdout)
                wm = subprocess.Popen([str(ROOT / "target/debug/maverick"), "--config", str(config)],
                                      env=env, stdout=log, stderr=log)
                processes.append(wm)
                for _ in range(100):
                    ready = subprocess.run(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"],
                                           env=env, capture_output=True, text=True, timeout=3)
                    if "window id #" in ready.stdout:
                        break
                    if wm.poll() is not None:
                        raise RuntimeError("WM exited during startup")
                    time.sleep(0.05)
                else:
                    raise RuntimeError("WM startup timed out")
                probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
                processes.append(probe)
                win = line(probe.stdout)
                time.sleep(0.3)

                def command(cmd):
                    probe.stdin.write(cmd + "\n")
                    probe.stdin.flush()
                    return line(probe.stdout)

                def toggle():
                    subprocess.run(["xdotool", "windowactivate", "--sync", win], env=env,
                                   check=True, timeout=5)
                    subprocess.run(["xdotool", "key", "--clearmodifiers", "super+shift+f"],
                                   env=env, check=True, timeout=5)
                    time.sleep(0.2)

                def check(label, fullscreen=True):
                    for _ in range(60):
                        values = list(map(int, command("check").split()))
                        wi, di, x, y, w, h, bw = values
                        valid = wi >= 0 and di >= 0
                        if fullscreen:
                            valid &= wi > di and (x, y, w, h, bw) == (0, 0, 800, 600, 0)
                        else:
                            valid &= wi < di
                        if valid:
                            break
                        time.sleep(0.05)
                    else:
                        raise AssertionError(f"{label}: stack/geometry={values}")
                    time.sleep(0.3)
                    final = list(map(int, command("check").split()))
                    assert final == values, f"{label}: unstable {values} -> {final}"
                    print("PASS:", label, values)

                command("dock")
                toggle()
                check("exclusive after dock")
                command("remove")
                time.sleep(0.2)
                command("dock")
                check("dock created after exclusive")
                toggle()
                check("exit exclusive restores dock", False)
                command("remove")
                time.sleep(0.2)
                command("fs")
                time.sleep(0.2)
                command("dock")
                check("dock created after ribbon covering")
                toggle()
                check("exit covering restores dock", False)
                toggle()
                check("exclusive after covering transition")
                command("remove")
                time.sleep(0.2)
                command("dock")
                check("dock restart after covering and exclusive")

                # B maps behind A's exclusive overlay; explicit keyboard focus
                # must override that deferral and reveal B, not scroll under A.
                peer = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
                processes.append(peer)
                peer_win = int(line(peer.stdout))
                time.sleep(0.3)
                for key in ("h", "l"):
                    subprocess.run(["xdotool", "key", "--clearmodifiers", "super+" + key],
                                   env=env, check=True, timeout=5)
                    for _ in range(60):
                        a = list(map(int, command("check").split()))
                        peer.stdin.write("check\n")
                        peer.stdin.flush()
                        b = list(map(int, line(peer.stdout).split()))
                        active = subprocess.check_output(["xdotool", "getactivewindow"], env=env,
                                                         text=True, timeout=3)
                        if (int(active) == peer_win and b[2] >= 0 and b[2] + b[4] <= 800
                                and (a[2] + a[4] <= b[2] or a[2] >= b[2] + b[4])):
                            break
                        time.sleep(0.05)
                    else:
                        raise AssertionError(f"Mod+{key}: A={a}, B={b}, active={active}")
                    print(f"PASS: Mod+{key} releases A and reveals B", a, b)
                    # on_key_press suppresses the same binding within 60 ms.
                    time.sleep(0.1)
                    subprocess.run(["xdotool", "key", "--clearmodifiers", "super+" + key],
                                   env=env, check=True, timeout=5)
                    check("return to A keeps fullscreen")
            except Exception:
                subprocess.run(["xwininfo", "-root", "-tree"], env=env, timeout=5)
                log.flush()
                log.seek(0)
                print(log.read())
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
