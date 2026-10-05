#!/usr/bin/env python3
"""XQueryTree regression. Run after cargo build --release -p maverick.

Drives the *release* binary: it is the profile the installer builds and
installs, so a pass here is a statement about the artifact that ships. There is
deliberately no fallback to `target/debug` — a missing binary is a failure,
never a skip.
Uses its own Xvfb, runtime/config and PIDs; never touches a live session.
"""
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
WM_BIN = ROOT / "target/release/maverick"
WM_BUILD = "cargo build --release -p maverick"


def require_wm_binary():
    """Fail loudly and specifically when the release binary is absent.

    A bare `Popen` here raises FileNotFoundError from deep inside the harness
    and, worse, reads like a flake next to a live Xvfb. Name the profile and
    the command that produces it instead.
    """
    if WM_BIN.is_file() and os.access(WM_BIN, os.X_OK):
        return
    why = "is not executable" if WM_BIN.is_file() else "does not exist"
    raise SystemExit(
        f"error: {WM_BIN} {why}\n"
        "This regression drives the release profile and has no debug fallback.\n"
        f"Build it first from {ROOT}:\n\n    {WM_BUILD}\n"
    )


def line(pipe):
    if not select.select([pipe], [], [], 5)[0]:
        raise RuntimeError("response timed out")
    result = pipe.readline().strip()
    if not result:
        raise RuntimeError("probe/server exited")
    return result


def main():
    require_wm_binary()
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
        config = tmp / "config.toml"
        config.write_text('''[general]
focus_mouse = false
warp_cursor = false
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
                wm = subprocess.Popen([str(WM_BIN), "--config", str(config)],
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

                def fullscreen_peer(label):
                    """Regression: while A holds the exclusive fullscreen overlay,
                    a newly mapped ordinary tile B must reconcile X stacking
                    immediately — B below A, A's geometry/state untouched —
                    without any Mod+H/L navigation (the old cache skipped the
                    raise because the overlay-only order vector was unchanged)."""
                    command("peer")
                    time.sleep(0.2)
                    for _ in range(60):
                        wi, pi, px, py, pw, ph, pbw = map(
                            int, command("pcheck").split())
                        # A exclusive: geometry fullscreen, above the peer.
                        # B tiled: normal workarea geometry below A.
                        if (wi > pi >= 0 and 0 < pw < 800 and 0 < ph < 600):
                            break
                        time.sleep(0.05)
                    else:
                        raise AssertionError(f"{label}: A/B stack/geometry={wi} {pi} {px} {py} {pw} {ph} {pbw}")
                    time.sleep(0.3)
                    final = list(map(int, command("pcheck").split()))
                    assert final == [wi, pi, px, py, pw, ph, pbw], (
                        f"{label}: unstable {[wi, pi, px, py, pw, ph, pbw]} -> {final}")
                    a = list(map(int, command("check").split()))
                    assert a[2:] == [0, 0, 800, 600, 0], f"A lost fullscreen geometry: {a}"
                    active = subprocess.check_output(["xdotool", "getwindowfocus"],
                                                     env=env, text=True, timeout=3)
                    assert int(active) == int(win), f"B stole focus: {active}"
                    props = subprocess.check_output(["xprop", "-id", win, "_NET_WM_STATE"],
                                                    env=env, text=True, timeout=3)
                    assert "_NET_WM_STATE_FULLSCREEN" in props, props
                    print("PASS:", label, [wi, pi, px, py, pw, ph, pbw])
                    command("unpeer")
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

                # Regression: while A holds the exclusive fullscreen overlay
                # (entered above), a newly mapped ordinary tile B must
                # reconcile X stacking immediately — B below A, A's geometry
                # and state untouched — with no Mod+H/L. The old overlay-only
                # order cache skipped the raise because its cached vector did
                # not change on insertion.
                check("exclusive before peer insertion")
                fullscreen_peer("peer while exclusive stays below and A intact")
                check("exclusive survives peer lifecycle")

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
