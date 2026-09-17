#!/usr/bin/env python3
import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


class Lines:
    def __init__(self, pipe, log):
        self.pipe = pipe
        self.log = log
        self.buffer = b""

    def line(self):
        deadline = time.monotonic() + 8
        while b"\n" not in self.buffer:
            if not select.select([self.pipe], [], [], max(0, deadline - time.monotonic()))[0]:
                raise RuntimeError("event/log barrier timed out")
            data = os.read(self.pipe.fileno(), 65536)
            if not data:
                raise RuntimeError("process exited before barrier")
            self.buffer += data
        result, self.buffer = self.buffer.split(b"\n", 1)
        result = result.decode(errors="replace")
        self.log.write(result + "\n")
        self.log.flush()
        return result

    def drain(self):
        """Consume already-buffered lines so a later barrier cannot match a
        stale one. Event-driven: returns as soon as the pipe is empty."""
        while self.buffer and b"\n" in self.buffer:
            result, self.buffer = self.buffer.split(b"\n", 1)
            result = result.decode(errors="replace")
            self.log.write(result + "\n")
            self.log.flush()

    def until(self, marker):
        result = []
        while True:
            value = self.line()
            result.append(value)
            if marker in value:
                return result

    def drain(self):
        # Consume every line already buffered (or arriving within a short
        # non-blocking window) so a later `until` can only match a FRESH line.
        drained = []
        while True:
            ready = select.select([self.pipe], [], [], 0.05)[0]
            if not ready:
                return drained
            data = os.read(self.pipe.fileno(), 65536)
            if not data:
                return drained
            self.buffer += data
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                text = line.decode(errors="replace")
                self.log.write(text + "\n")
                self.log.flush()
                drained.append(text)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wm", type=Path, default=ROOT / "target/debug/maverick")
    args = parser.parse_args()
    out = Path(tempfile.mkdtemp(prefix="keyboard-b-", dir="/tmp/kilo"))
    print(f"EVIDENCE {out}", flush=True)
    helper = out / "probe"
    subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/keyboard-probe.c"),
                    "-o", str(helper), "-lX11", "-lXtst"], check=True)
    env = os.environ.copy()
    for name in list(env):
        if name.startswith("MAVERICK_") or name in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XAUTHORITY"):
            del env[name]
    for name in ("HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
        path = out / name
        path.mkdir(mode=0o700)
        env[name] = str(path)
    env.update(MAVERICK_NO_COMPOSITOR="1", MAVERICK_LOG="debug", DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(out / "no-bus"))
    config = out / "config.toml"
    text = '''[general]
focus_mouse = false
warp_cursor = false
auto_workspace_binds = false
[compositor]
enabled = false
[animations]
enabled = false
[autostart]
commands = [["/usr/bin/true"]]
'''
    for prefix, suffix in (("Mod4", ""), ("Mod4+Shift", ""), ("Mod4+Control", ""),
                           ("Mod4+Alt", ""), ("Mod4+Control+Shift", ""), ("Mod4+Alt+Shift", ""),
                           ("Mod4+Alt+Control", ""), ("Mod4+Alt+Control+Shift", "")):
        for name, target in (("h", 2), ("j", 3), ("d", 4), ("c", 5), ("bracketleft", 6)):
            text += f'[[keybindings]]\nkey = "{prefix}+{name}{suffix}"\naction = "view:{target}"\n'
    config.write_text(text)
    processes = []
    results = []
    with (out / "events.log").open("w") as events, (out / "wm.log").open("w") as logs:
        try:
            server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24", "-nolisten", "tcp", "-noreset"],
                                      stdout=subprocess.PIPE, stderr=events)
            processes.append(server)
            env["DISPLAY"] = ":" + Lines(server.stdout, events).line()
            probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=events)
            processes.append(probe)
            lines = Lines(probe.stdout, events)
            lines.until("READY")

            def cmd(value, marker):
                events.write("COMMAND " + value + "\n")
                probe.stdin.write((value + "\n").encode())
                probe.stdin.flush()
                return lines.until(marker)

            def layout(variant, dynamic=False):
                command = ["setxkbmap", "-layout", "us" if "," not in variant else "us,us", "-variant", variant, "-option", "", "-option", "grp:rctrl_toggle"]
                events.write("LAYOUT " + repr(command) + "\n")
                subprocess.run(command, env=env, stdout=events, stderr=events, check=True)
                if dynamic:
                    wm_lines.until("keyboard: keymap refreshed")
                cmd("map", "STATE")

            def spawn_wm():
                wm = subprocess.Popen([str(args.wm.resolve()), "--config", str(config)], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
                processes.append(wm)
                stream = Lines(wm.stdout, logs)
                stream.until("maverick ready")
                return wm, stream

            def start(variant):
                layout(variant)
                return spawn_wm()

            def test(label, code, mask, expected, locks=0):
                if locks:
                    cmd(f"lock {locks}", "STATE")
                grab = cmd(f"grab {code} {mask | locks}", "GRAB")
                output = cmd(f"test {code} {mask}", "DONE")
                actual = next((int(line.split()[1]) for line in output if line.startswith("DESKTOP ")), None)
                result = dict(label=label, code=code, mask=mask, locks=locks, expected=expected, actual=actual,
                              passed=actual == expected, grab=grab, events=output)
                results.append(result)
                print(json.dumps(result), flush=True)

            wm, wm_lines = start("")
            test("startup-qwerty-super-h", 43, 64, 1)
            test("startup-qwerty-shift-h", 43, 65, 1)
            test("startup-qwerty-shift-bracket", 34, 65, 5)
            layout("dvorak", True)
            test("dynamic-map-dvorak-control-h", 44, 68, 1)
            layout("", True)
            test("dynamic-map-qwerty-alt-h", 43, 72, 1)
            wm.terminate()
            wm.wait(timeout=5)
            wm, wm_lines = start("dvorak")
            test("startup-dvorak-super-h", 44, 64, 1)
            test("startup-dvorak-shift-h", 44, 65, 1)
            test("startup-dvorak-caps-control-h", 44, 68, 1, 2)
            test("startup-dvorak-num-alt-h", 44, 72, 1, 16)
            layout(",dvorak", True)
            cmd("group 1", "STATE")
            test("group-dvorak-super-h", 44, 64, 1)
            test("group-dvorak-shift-h", 44, 65, 1)
            test("group-dvorak-control-h", 44, 68, 1)
            test("group-dvorak-alt-h", 44, 72, 1)
            test("group-dvorak-caps-ctrlshift-h", 44, 69, 1, 2)
            test("group-dvorak-num-altshift-h", 44, 73, 1, 16)
            test("group-dvorak-c", 31, 76, 4)
            cmd("group 0", "STATE")
            test("group-back-qwerty-h", 43, 77, 1)
            wm.terminate()
            wm.wait(timeout=5)
            # Session 4: Scroll Lock's column present when the WM reads the
            # keyboard. Order is load-bearing: the layout() inside start() runs
            # setxkbmap, which WIPES runtime modifier assignments (verified:
            # mod3 resets to ISO_Level5_Shift), and this server does not deliver
            # core MappingNotify for modifier-map changes (verified with a bare
            # X client), so the xmodmap must land AFTER the last setxkbmap and
            # BEFORE the WM starts reading the map.
            layout("")
            subprocess.run(["xmodmap", "-e", "add mod3 = Scroll_Lock"], env=env, stdout=events, stderr=events, check=True)
            subprocess.run(["xmodmap", "-pm"], env=env, stdout=events, stderr=events, check=True)
            wm, wm_lines = spawn_wm()
            test("qwerty-scroll-lock-h", 43, 76, 1, 32)
            wm.terminate()
            wm.wait(timeout=5)
            wm, wm_lines = start("dvorak,")
            cmd("group 1", "STATE")
            test("reverse-order-group-qwerty-h", 43, 64, 1)
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            (out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    return int(any(not result["passed"] for result in results))


if __name__ == "__main__":
    raise SystemExit(main())
