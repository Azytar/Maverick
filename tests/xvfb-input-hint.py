#!/usr/bin/env python3
"""Focus eligibility against a live X server: the `WM_HINTS` input model.

Focus eligibility has exactly one input — the `input` field of the client's
`WM_HINTS` (ICCCM 4.1.2.4) — and that property may be rewritten at any time
while the window is up (ICCCM 4.1.2: "the window manager will retain no memory
of the old value"). So every check below is about the WM reading the client's
CURRENT declaration:

  * a window that declares `input = False` is refused the focus and never
    receives the X input focus;
  * a window that re-declares `input = True` is focusable again — the flip a
    cached flag beside the field cannot express, because a flag can only be set;
  * a window that never set the `InputHint` bit, and one that sends no
    `WM_HINTS` at all, are focusable (the documented WM default);
  * the map-time read (Withdrawn -> Normal) and the mid-life `PropertyNotify`
    read reach the same decision for the same declaration;
  * removing the focused window reconciles the focus onto the survivor.

It starts its own Xvfb, config, runtime dir and PIDs, and never touches a live
session. Run after `cargo build -p maverick`:

    python3 tests/xvfb-input-hint.py [--wm target/debug/maverick]

Exits non-zero if any check fails.
"""
import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import traceback
import time

ROOT = Path(__file__).resolve().parent.parent


class Probe:
    """One persistent C/Xlib client; each command returns exactly one JSON line."""

    def __init__(self, process):
        self.process = process
        if json.loads(line(process.stdout)) != {"ready": True}:
            raise RuntimeError("invalid probe handshake")

    def command(self, text):
        self.process.stdin.write(text + "\n")
        self.process.stdin.flush()
        return json.loads(line(self.process.stdout))


class Client:
    def __init__(self, probe, index):
        self.probe, self.index = probe, index
        self.win = probe.command(f"create {index}")["window"]

    def command(self, name):
        return self.probe.command(f"{name} {self.index}")

    def declare_input(self, value):
        self.probe.command(f"input {self.index} {int(value)}")

    def snapshot(self):
        return self.command("snapshot")

    def server_input(self):
        hints = self.snapshot()["hints"]
        return tuple(hints) if hints is not None else None

    def withdraw_hints(self):
        self.command("drop")

    def withdraw(self):
        self.command("unmap")

    def reoffer(self):
        self.command("map")

    def destroy(self):
        self.command("destroy")


def line(pipe):
    if not select.select([pipe], [], [], 10)[0]:
        raise RuntimeError("process response timed out")
    text = pipe.readline().strip()
    if not text:
        raise RuntimeError("process exited before its response")
    return text


def sh(cmd, env):
    return subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=20, check=True)


def window_count(env, ctl):
    """The WM's own count of managed windows, read over the control socket — so
    every poll also proves the WM is still answering its event loop."""
    try:
        doc = json.loads(sh([ctl, "state"], env).stdout)
    except json.JSONDecodeError:
        return None
    if not isinstance(doc, dict) or "monitors" not in doc:
        return None
    return doc["monitors"][0]["workspaces"][0]["windows"]


def wait_for(predicate, timeout=15.0, interval=0.1):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--wm", type=Path, default=ROOT / "target/debug/maverick")
    parser.add_argument("--ctl", type=Path, default=ROOT / "target/debug/maverickctl")
    parser.add_argument("--probe", type=Path, default=ROOT / "tests/input-hint-probe.c")
    parser.add_argument("--sanitize", action="store_true")
    args = parser.parse_args()

    processes = []
    failures = []
    runtime = Path(tempfile.mkdtemp(prefix="input-hint-", dir="/tmp"))
    print(f"EVIDENCE {runtime}", flush=True)
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("MAVERICK_") or key in ("DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY"):
            del env[key]
    for key in ("HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
        path = runtime / key
        path.mkdir(mode=0o700)
        env[key] = str(path)
    env["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=" + str(runtime / "no-bus")
    config = runtime / "config.toml"
    config.write_text("""[general]
focus_mouse = false
warp_cursor = false
[autostart]
commands = [["/usr/bin/true"]]
""")
    ctl = str(args.ctl)
    display = None

    def check(name, ok, **detail):
        print(json.dumps({"check": name, "passed": bool(ok), **detail}), flush=True)
        if not ok:
            failures.append(name)

    def step(label):
        print(f"… {label}", file=sys.stderr, flush=True)

    def focus_request(window):
        sh([ctl, "msg", "focus_window", hex(window)], env)

    def logical_focus():
        return json.loads(sh([ctl, "state"], env).stdout)["monitors"][0]["focused"]

    try:
        with (runtime / "wm.log").open("w") as log:
            server = subprocess.Popen(
                ["Xvfb", "-displayfd", "1", "-screen", "0", "1024x768x24",
                 "-nolisten", "tcp", "-noreset"],
                stdout=subprocess.PIPE, stderr=log, text=True)
            processes.append(server)
            display = ":" + line(server.stdout)
            env["DISPLAY"] = display
            processes.append(subprocess.Popen([str(args.wm.resolve()), "--config", str(config)],
                                             env=env, stdout=log, stderr=log))
            if not wait_for(lambda: "window id #" in sh(
                    ["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env).stdout, 20):
                raise RuntimeError("the WM never claimed the screen")

            helper = runtime / "probe"
            compiler = ["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-Wshadow", "-Wformat=2"]
            if args.sanitize:
                compiler += ["-fsanitize=address,undefined", "-fno-omit-frame-pointer", "-g"]
                env["ASAN_OPTIONS"] = "detect_leaks=1:abort_on_error=1"
                env["UBSAN_OPTIONS"] = "halt_on_error=1:print_stacktrace=1"
            subprocess.run([*compiler, str(args.probe.resolve()), "-o", str(helper), "-lX11"],
                           check=True, timeout=30)
            process = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
            processes.append(process)
            probe = Probe(process)
            alpha, beta = Client(probe, 0), Client(probe, 1)

            if alpha.server_input() != (True, False):
                raise RuntimeError(
                    f"the probe's own WM_HINTS write did not land: {alpha.server_input()}")
            if not wait_for(lambda: window_count(env, ctl) == 2, 20):
                raise RuntimeError("the WM never managed both probe windows")
            print(json.dumps({"alpha": hex(alpha.win), "beta": hex(beta.win)}), flush=True)

            def expect(name, target, *, forbidden=False, logical=True, unselected=False):
                """Wait for positive transitions; observe every result for 600ms.

                Refusals keep the full observation interval. A missing active
                property is allowed only in the initial, logically unselected state.
                X queries do not fence the WM's own connection.
                """
                def matches(snapshot):
                    active, real = snapshot["active"], snapshot["focus"]
                    if active is None:
                        return unselected and real in (0, 1, snapshot["root"])
                    if forbidden:
                        return active != target and real != target
                    return active == target and real == target

                if not forbidden:
                    if not wait_for(lambda: matches(alpha.snapshot())
                                    and (not logical or logical_focus() == target),
                                    timeout=5, interval=0.02):
                        check(name, False, snapshot=alpha.snapshot(), logical_focus=logical_focus(),
                              expected=target, reason="focus did not converge")
                        raise RuntimeError("failed prerequisite: " + name)
                deadline = time.monotonic() + 0.6
                stable = True
                while time.monotonic() < deadline:
                    snapshot = alpha.snapshot()
                    stable = matches(snapshot) and stable
                    time.sleep(0.02)
                focused = logical_focus()
                logical_ok = (focused != target if forbidden else focused == target) if logical else True
                if unselected:
                    logical_ok = focused is None
                check(name, stable and logical_ok, snapshot=snapshot, logical_focus=focused,
                      expected=target, forbidden=forbidden)
                if not stable or not logical_ok:
                    raise RuntimeError("failed prerequisite: " + name)

            step("both probe windows mapped without input eligibility")
            focus_request(alpha.win)
            expect("an-input-false-window-is-never-focused", alpha.win, forbidden=True, unselected=True)

            # Same client rewrites False -> True -> False -> True. Every
            # eligible destination must receive both EWMH and real X focus.
            alpha.declare_input(True)
            if alpha.server_input() != (True, True):
                raise RuntimeError("input rewrite did not reach the server")
            focus_request(alpha.win)
            expect("re-declaring-input-true-restores-eligibility", alpha.win)
            beta.declare_input(True)
            focus_request(beta.win)
            expect("an-input-true-window-is-focused", beta.win)
            alpha.declare_input(False)
            if alpha.server_input() != (True, False):
                raise RuntimeError("input withdrawal did not reach the server")
            focus_request(alpha.win)
            expect("withdrawing-the-input-request-refuses-the-focus", beta.win)

            # Keyboard navigation may name the ineligible client internally,
            # but neither published nor real X focus may move onto it.
            sh([ctl, "msg", "focus:left"], env)
            expect("keyboard-focus-may-name-an-input-false-window-but-never-focuses-it",
                   alpha.win, forbidden=True, logical=False)
            focus_request(beta.win)
            if not wait_for(lambda: logical_focus() == beta.win
                            and alpha.snapshot()["active"] == beta.win
                            and alpha.snapshot()["focus"] == beta.win, timeout=5, interval=0.02):
                raise RuntimeError("peer was not logically selected before re-declaration")
            alpha.declare_input(True)
            focus_request(alpha.win)
            expect("eligibility-is-re-derived-not-latched", alpha.win)

            beta.withdraw_hints()
            if beta.server_input() is not None:
                raise RuntimeError("WM_HINTS removal did not reach the server")
            focus_request(beta.win)
            expect("no-wm-hints-at-all-is-the-default-and-focusable", beta.win)

            # The previous docstring promised the missing InputHint case, but
            # never exercised it. False in an unflagged field is not refusal.
            beta.probe.command("input 1 2")
            if beta.server_input() != (False, False):
                raise RuntimeError("flagless hints did not reach the server")
            focus_request(alpha.win)
            expect("flagless-hints-precondition-peer-focused", alpha.win)
            focus_request(beta.win)
            expect("missing-input-hint-bit-is-default-and-focusable", beta.win)

            alpha.declare_input(False)
            alpha.withdraw()
            if not wait_for(lambda: window_count(env, ctl) == 1, 15):
                raise RuntimeError("withdrawal did not unmanage the window")
            alpha.reoffer()
            if not wait_for(lambda: window_count(env, ctl) == 2, 15):
                raise RuntimeError("re-offer did not manage the window")
            focus_request(alpha.win)
            expect("map-time-input-false-is-honoured-on-remap", alpha.win, forbidden=True)
            alpha.declare_input(True)
            focus_request(alpha.win)
            expect("post-remap-flip-restores-eligibility", alpha.win)

            focus_request(beta.win)
            expect("destroy-precondition-peer-focused", beta.win)
            beta.destroy()
            if not wait_for(lambda: window_count(env, ctl) == 1, 15):
                raise RuntimeError("destroyed window was never unmanaged")
            expect("removing-the-focused-window-reconciles-to-the-survivor", alpha.win)

            if any(p.poll() is not None for p in processes[:-1]):
                raise RuntimeError("WM or X server exited during the test")
            process.stdin.close()
            if process.wait(timeout=5) != 0:
                raise RuntimeError("probe exited unsuccessfully")
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    print(json.dumps({"failures": failures, "display": display}), flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    try:
        code = main()
    except Exception:  # a harness reports a broken setup; it does not swallow it
        traceback.print_exc()
        code = 2
    sys.exit(code)
