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


def wait_display(server, timeout=30):
    """Read only Xvfb's readiness line, with a deadline for the entire record.

    A first server on a cold CI runner can exceed the probe's five-second
    response budget. Partial output must not turn readline into an unbounded
    wait, and an exited server must not be mistaken for a slow startup.
    """
    deadline = time.monotonic() + timeout
    record = b""
    while True:
        status = server.poll()
        if status is not None:
            raise RuntimeError(f"Xvfb exited before readiness (status {status})")
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(f"Xvfb did not report a display within {timeout:g}s")
        if not select.select([server.stdout], [], [], min(remaining, 0.1))[0]:
            continue
        data = os.read(server.stdout.fileno(), 64)
        if not data:
            raise RuntimeError("Xvfb closed its readiness pipe before reporting a display")
        record += data
        if len(record) > 32:
            raise RuntimeError("Xvfb reported an oversized display record")
        if b"\n" in record:
            number, _, tail = record.partition(b"\n")
            if not number.isdigit() or tail:
                raise RuntimeError(f"Xvfb reported an invalid display record: {record!r}")
            return ":" + number.decode("ascii")


def failure_diagnostics(log, env, display):
    # Print startup stderr before optional X queries; diagnostics must preserve
    # the original exception and must never query an inherited desktop display.
    log.flush()
    log.seek(0)
    print(log.read(), flush=True)
    if display is None:
        return
    try:
        result = subprocess.run(["xwininfo", "-root", "-tree"], env=env, timeout=5,
                                capture_output=True, text=True)
        print(result.stdout, end="", flush=True)
        print(result.stderr, end="", flush=True)
    except (OSError, subprocess.TimeoutExpired) as exc:
        print(f"xwininfo diagnostics unavailable: {exc}", flush=True)


def main():
    require_wm_binary()
    processes = []
    display = None
    with tempfile.TemporaryDirectory(prefix="mst-", dir="/tmp") as tmp:
        tmp = Path(tmp)
        env = os.environ.copy()
        for key in list(env):
            if key.startswith("MAVERICK_") or key in ("DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY"):
                del env[key]
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "HOME"):
            path = tmp / key
            path.mkdir(mode=0o700)
            env[key] = str(path)
        config = tmp / "config.toml"
        config.write_text('''[general]
focus_mouse = false
warp_cursor = false
honor_initial_state = true
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
[[keybindings]]
key = "Mod4+1"
action = "view:1"
[[keybindings]]
key = "Mod4+2"
action = "view:2"
[[keybindings]]
key = "Mod4+3"
action = "view:3"
''')
        helper = tmp / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/stacking-probe.c"),
                        "-o", str(helper), "-lX11"], check=True)
        with (tmp / "wm.log").open("w+") as log:
            try:
                server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                           "-nolisten", "tcp"], stdout=subprocess.PIPE,
                                          stderr=log, env=env)
                processes.append(server)
                display = env["DISPLAY"] = wait_display(server)
                print(f"Xvfb ready on {display}", flush=True)
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

                def command(cmd, process=probe):
                    process.stdin.write(cmd + "\n")
                    process.stdin.flush()
                    return line(process.stdout)

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

                for process in (peer, probe):
                    process.stdin.write("quit\n")
                    process.stdin.flush()
                    process.wait(timeout=3)

                def switch_view(number):
                    subprocess.run(["xdotool", "key", "--clearmodifiers", f"super+{number}"],
                                   env=env, check=True, timeout=5)
                    time.sleep(0.1)

                def workspace_check(process, dock, label, fullscreen=True):
                    for _ in range(60):
                        values = list(map(int, command(f"check {dock}", process).split()))
                        wi, di, x, y, w, h, bw = values
                        pixel = int(command("pixel", process))
                        if fullscreen:
                            valid = (wi > di >= 0 and (x, y, w, h, bw) == (0, 0, 800, 600, 0)
                                     and pixel == 0xffffff)
                        else:
                            valid = di >= 0 and pixel == 0xff0000
                        if valid:
                            break
                        time.sleep(0.05)
                    else:
                        raise AssertionError(f"{label}: stack/geometry={values}, pixel={pixel:#x}")
                    time.sleep(0.2)
                    assert list(map(int, command(f"check {dock}", process).split())) == values
                    assert int(command("pixel", process)) == pixel
                    print("PASS:", label, values, f"pixel={pixel:#x}")

                for first_mode, second_mode in (
                    ("ribbon", "exclusive"), ("exclusive", "ribbon"),
                    ("ribbon", "ribbon"), ("exclusive", "exclusive"),
                    ("exclusive", "client"), ("client", "exclusive"),
                ):
                    pair = []
                    for number, mode in ((1, first_mode), (2, second_mode)):
                        switch_view(number)
                        args = [str(helper)] + (["--fullscreen"] if mode == "ribbon" else [])
                        process = subprocess.Popen(args, env=env, stdin=subprocess.PIPE,
                                                   stdout=subprocess.PIPE, stderr=log,
                                                   text=True, bufsize=1)
                        processes.append(process)
                        pair.append(process)
                        window = line(process.stdout)
                        for _ in range(100):
                            desktop = subprocess.check_output(
                                ["xprop", "-id", window, "_NET_WM_DESKTOP"],
                                env=env, text=True, timeout=3)
                            if desktop.rstrip().endswith(f"= {number - 1}"):
                                break
                            time.sleep(0.05)
                        else:
                            raise AssertionError(f"window {window} not managed on View {number}")
                        if mode == "exclusive":
                            subprocess.run(["xdotool", "windowactivate", "--sync", window],
                                           env=env, check=True, timeout=5)
                            subprocess.run(["xdotool", "key", "--clearmodifiers", "super+shift+f"],
                                           env=env, check=True, timeout=5)
                        elif mode == "client":
                            command("fs", process)
                    first, second = pair
                    command("dock", first)
                    dock = int(command("dockid", first))
                    label = f"{first_mode}/{second_mode}"
                    for number, process in ((1, first), (2, second), (1, first), (2, second)):
                        switch_view(number)
                        workspace_check(process, dock, f"{label}: View {number} covers dock")
                    command("remove", first)
                    command("dock", first)
                    dock = int(command("dockid", first))
                    workspace_check(second, dock, f"{label}: dock restart stays below fullscreen")
                    switch_view(3)
                    workspace_check(second, dock, f"{label}: empty View restores dock", False)
                    switch_view(1)
                    workspace_check(first, dock, f"{label}: return restores fullscreen")
                    for process in pair:
                        process.stdin.write("quit\n")
                        process.stdin.flush()
                        process.wait(timeout=3)
            except Exception:
                failure_diagnostics(log, env, display)
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
