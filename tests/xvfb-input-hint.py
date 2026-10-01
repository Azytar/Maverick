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
import ctypes
import ctypes.util
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import traceback
import threading
import time

ROOT = Path(__file__).resolve().parent.parent

# ICCCM 4.1.2.4, WM_HINTS.flags.
INPUT_HINT = 1
# ICCCM 4.1.2.4, WM_HINTS.initial_state.
NORMAL_STATE = 1
XA_STRING = 31
REPLACE = 0
# `XNextEvent` writes one `XEvent`, a union of 24 `long`s on LP64.
XEVENT_BYTES = 192
# Xlib is not thread-safe and the probe's Display is shared: the harness thread
# rewrites properties while each client drains its events. Every call goes
# through this lock, or a raced Display corrupts itself and Xlib's IO-error
# handler takes the whole harness down with "X connection to :N broken".
XLOCK = threading.Lock()


class XWMHints(ctypes.Structure):
    """The C `XWMHints` of Xutil.h — the structure a `WM_HINTS` property is."""

    _fields_ = [
        ("flags", ctypes.c_long),
        ("input", ctypes.c_int),
        ("initial_state", ctypes.c_int),
        ("icon_pixmap", ctypes.c_ulong),
        ("icon_window", ctypes.c_ulong),
        ("icon_x", ctypes.c_int),
        ("icon_y", ctypes.c_int),
        ("icon_mask", ctypes.c_ulong),
        ("window_group", ctypes.c_ulong),
    ]


def load_xlib():
    """Bind the handful of Xlib entry points this harness needs.

    The windows are driven through Xlib rather than `xprop`, because `xprop
    -set` always writes a property with type CARDINAL while the WM reads
    `WM_HINTS` behind an explicit type filter — a property written that way is
    invisible to it, so a `xprop`-driven harness would silently test nothing.
    """
    lib = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6", use_errno=True)
    handle = ctypes.c_void_p
    window = ctypes.c_ulong
    lib.XOpenDisplay.restype = handle
    lib.XOpenDisplay.argtypes = [ctypes.c_char_p]
    lib.XCloseDisplay.argtypes = [handle]
    lib.XDefaultScreen.restype = ctypes.c_int
    lib.XDefaultScreen.argtypes = [handle]
    lib.XRootWindow.restype = window
    lib.XRootWindow.argtypes = [handle, ctypes.c_int]
    lib.XBlackPixel.restype = window
    lib.XBlackPixel.argtypes = [handle, ctypes.c_int]
    lib.XWhitePixel.restype = window
    lib.XWhitePixel.argtypes = [handle, ctypes.c_int]
    lib.XCreateSimpleWindow.restype = window
    lib.XCreateSimpleWindow.argtypes = [handle, window, ctypes.c_int, ctypes.c_int,
                                        ctypes.c_uint, ctypes.c_uint, ctypes.c_uint,
                                        window, window]
    lib.XDestroyWindow.argtypes = [handle, window]
    lib.XMapWindow.argtypes = [handle, window]
    lib.XUnmapWindow.argtypes = [handle, window]
    lib.XInternAtom.restype = window
    lib.XInternAtom.argtypes = [handle, ctypes.c_char_p, ctypes.c_int]
    lib.XStoreName.argtypes = [handle, window, ctypes.c_char_p]
    # `c_char_p` (not a bare `c_void_p`) for the payload: ctypes copies a
    # `bytes` object straight through, where an untyped pointer argument can
    # arrive as a pointer to the wrong place and store a property full of
    # garbage that still *reads back* — silently wrong, not loud.
    lib.XChangeProperty.argtypes = [handle, window, window, window, ctypes.c_int,
                                    ctypes.c_int, ctypes.c_char_p, ctypes.c_int]
    lib.XDeleteProperty.argtypes = [handle, window, window]
    lib.XFlush.argtypes = [handle]
    lib.XPending.restype = ctypes.c_int
    lib.XPending.argtypes = [handle]
    lib.XNextEvent.argtypes = [handle, ctypes.c_void_p]
    lib.XFree.restype = None
    lib.XFree.argtypes = [handle]
    # `XGetWindowProperty` takes `unsigned long **data_return`: Xlib allocates
    # the payload and hands back the pointer, so the caller's slot receives an
    # address rather than the words. Passing a plain buffer here leaves it
    # untouched and puts a heap address in its first slot, which reads back as
    # a plausible-looking property full of zeros. The payload is XFree'd here.
    lib.XGetWindowProperty.restype = ctypes.c_int
    lib.XGetWindowProperty.argtypes = [handle, window, window, ctypes.c_long,
                                       ctypes.c_long, ctypes.c_int, window,
                                       ctypes.POINTER(window), ctypes.POINTER(ctypes.c_int),
                                       ctypes.POINTER(window), ctypes.POINTER(window),
                                       ctypes.POINTER(ctypes.POINTER(window))]
    return lib


def open_display(lib, name):
    """A `Display *` as a `c_void_p`, never a Python int.

    ctypes narrows an unannotated Python `int` argument to C `int`, which
    truncates the 64-bit handle and hands Xlib a pointer into low memory: the
    next call dies as a SIGSEGV, or comes back with reply fields that are right
    and payload bytes that are not. Every call below therefore goes through this
    handle, and every function above declares its arguments.
    """
    return ctypes.c_void_p(lib.XOpenDisplay(name))


class Client:
    """A managed top-level window that can restate its input model on demand."""

    def __init__(self, lib, display, name):
        self.lib = lib
        self.dpy = display  # shared with every other client; see XLOCK
        screen = lib.XDefaultScreen(display)
        self.win = lib.XCreateSimpleWindow(
            display, lib.XRootWindow(display, screen), 40, 40, 400, 300, 2,
            lib.XBlackPixel(display, screen), lib.XWhitePixel(display, screen))
        self.hints = lib.XInternAtom(display, b"WM_HINTS", False)
        self.wm_class = lib.XInternAtom(display, b"WM_CLASS", False)
        label = name.encode()
        # WM_CLASS is one STRING property holding both names NUL-separated.
        lib.XChangeProperty(display, self.win, self.wm_class, XA_STRING, 8, REPLACE,
                            label + b"\0probe\0", len(label) + 7)
        lib.XStoreName(display, self.win, label)
        self.declare_input(False)
        lib.XMapWindow(display, self.win)
        lib.XFlush(display)
        # An undrained client stalls once its event queue fills, which reads as
        # "the WM never answered", so keep the connection served.
        self.running = True
        self.pump = threading.Thread(target=self._drain, daemon=True)
        self.pump.start()

    def _drain(self):
        buffer = ctypes.create_string_buffer(XEVENT_BYTES)
        while self.running:
            with XLOCK:
                pending = self.lib.XPending(self.dpy)
                if pending:
                    self.lib.XNextEvent(self.dpy, ctypes.cast(buffer, ctypes.c_void_p))
            if not pending:
                time.sleep(0.01)

    def close(self):
        self.running = False
        self.pump.join(timeout=2)

    def _write(self, hints):
        raw = ctypes.string_at(ctypes.byref(hints), ctypes.sizeof(XWMHints))
        with XLOCK:
            self.lib.XChangeProperty(self.dpy, self.win, self.hints, self.hints, 32,
                                     REPLACE, raw, len(raw) // 4)
            self.lib.XFlush(self.dpy)

    def declare_input(self, value):
        """Restate `WM_HINTS.input`. `Replace` mode is the only one ICCCM 4.1.2
        allows, and the whole structure must be written with it."""
        self._write(XWMHints(INPUT_HINT, int(value), NORMAL_STATE, 0, 0, 0, 0, 0, 0))

    def withdraw_hints(self):
        with XLOCK:
            self.lib.XDeleteProperty(self.dpy, self.win, self.hints)
            self.lib.XFlush(self.dpy)

    def server_input(self):
        """The `input` word as the server holds it, read back off the property.

        None when there is nothing to read: the property is absent (a client
        that sends no `WM_HINTS`) or not in the CARD32 shape this wrote.
        """
        actual_type = ctypes.c_ulong()
        actual_format = ctypes.c_int()
        nitems = ctypes.c_ulong()
        after = ctypes.c_ulong()
        payload = ctypes.POINTER(ctypes.c_ulong)()
        with XLOCK:
            status = self.lib.XGetWindowProperty(
                self.dpy, self.win, self.hints, 0, 16, False, 0,
                ctypes.byref(actual_type), ctypes.byref(actual_format),
                ctypes.byref(nitems), ctypes.byref(after), ctypes.byref(payload))
            words = None
            if status == 0 and payload:
                try:
                    if actual_format.value == 32 and nitems.value >= 2:
                        words = (bool(payload[0] & 1), bool(payload[1]))
                finally:
                    self.lib.XFree(ctypes.cast(payload, ctypes.c_void_p))
        return words

    def withdraw(self):
        with XLOCK:
            self.lib.XUnmapWindow(self.dpy, self.win)
            self.lib.XFlush(self.dpy)

    def reoffer(self):
        with XLOCK:
            self.lib.XMapWindow(self.dpy, self.win)
            self.lib.XFlush(self.dpy)

    def destroy(self):
        with XLOCK:
            self.lib.XDestroyWindow(self.dpy, self.win)
            self.lib.XFlush(self.dpy)


def line(pipe):
    if not select.select([pipe], [], [], 10)[0]:
        raise RuntimeError("the X server never reported its display")
    text = pipe.readline().strip()
    if not text:
        raise RuntimeError("the X server exited before reporting its display")
    return text


def sh(cmd, env):
    return subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=20)


def x_active_window(env):
    """`_NET_ACTIVE_WINDOW`: the window the WM has published as focused."""
    out = sh(["xprop", "-root", "_NET_ACTIVE_WINDOW"], env).stdout
    for token in out.split("#"):
        parts = token.split()
        if len(parts) == 1 and parts[0].startswith("0x"):
            return int(parts[0], 16)
    return None


def x_input_focus(env):
    """`GetInputFocus`: where the keyboard actually is, server-side.

    `None` for the "focus is on the root, not a window" answers — the server
    reports the root, or `PointerRoot`/`None`, and neither is a client window.
    """
    out = sh(["xdotool", "getwindowfocus"], env).stdout.strip()
    try:
        window = int(out, 0)
    except ValueError:
        return None
    return window or None


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


def read_stably(read, attempts=6, pause=0.2):
    """A single observation, retried until it yields a value.

    `xprop`/`xdotool` open their own connections per call and can lose a race
    under load; a `None` here would silently satisfy a "the focus is not on
    this window" assertion, so the readers retry instead of reporting nothing.
    """
    for _ in range(attempts):
        value = read()
        if value is not None:
            return value
        time.sleep(pause)
    return None


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
    args = parser.parse_args()

    lib = load_xlib()
    processes = []
    failures = []
    runtime = Path(tempfile.mkdtemp(prefix="input-hint-", dir="/tmp"))
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

    def settle():
        time.sleep(0.6)

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

            probe = open_display(lib, display.encode())
            alpha = Client(lib, probe, "alpha")
            beta = Client(lib, probe, "beta")
            if alpha.server_input() != (True, False):
                raise RuntimeError(
                    f"the probe's own WM_HINTS write did not land: {alpha.server_input()}")
            if not wait_for(lambda: (window_count(env, ctl) or 0) == 2, 20):
                raise RuntimeError("the WM never managed both probe windows")
            print(json.dumps({"alpha": hex(alpha.win), "beta": hex(beta.win)}), flush=True)

            step("both probe windows mapped")
            # 1. Both windows declared input = False: the WM focuses neither,
            #    and the keyboard is on neither of them.
            focus_request(alpha.win)
            settle()
            focused = logical_focus()
            active = read_stably(lambda: x_active_window(env))
            real = read_stably(lambda: x_input_focus(env))
            check("an-input-false-window-is-never-focused", focused != alpha.win,
                  logical_focus=focused, x_active_window=active,
                  x_input_focus=real, alpha=hex(alpha.win))

            step("asking for an input = False window")
            # 2. The flip. The client re-declares input = True on the mapped
            #    window; the focus request must be honoured in the rewrite.
            alpha.declare_input(True)
            if not wait_for(lambda: alpha.server_input() == (True, True), 5):
                raise RuntimeError(
                    f"the client's rewrite is not visible on the server: {alpha.server_input()}")
            settle()
            focus_request(alpha.win)
            settle()
            focused = logical_focus()
            active = read_stably(lambda: x_active_window(env))
            real = read_stably(lambda: x_input_focus(env))
            check("re-declaring-input-true-restores-eligibility",
                  focused == alpha.win and active == alpha.win and real == alpha.win,
                  logical_focus=focused, x_active_window=active,
                  x_input_focus=x_input_focus(env), expected=hex(alpha.win))

            step("alpha re-declared input = True")
            # 3. A peer that wants input, so every refusal below is measured
            #    from a window the focus can actually sit on.
            beta.declare_input(True)
            settle()
            focus_request(beta.win)
            settle()
            focused = logical_focus()
            check("an-input-true-window-is-focused", focused == beta.win,
                  logical_focus=focused, x_active_window=x_active_window(env),
                  expected=hex(beta.win))

            step("peer beta wants input")
            # 4. Withdrawing the request takes the eligibility away again. The
            #    focus is on the peer, so a refusal is observable: a window that
            #    has just said it does not want the keyboard must not be able to
            #    pull it over — and a peer that is still eligible keeps it.
            alpha.declare_input(False)
            if not wait_for(lambda: alpha.server_input() == (True, False), 5):
                raise RuntimeError(
                    f"the withdrawal is not visible on the server: {alpha.server_input()}")
            settle()
            focus_request(alpha.win)
            settle()
            focused = logical_focus()
            active = read_stably(lambda: x_active_window(env))
            real = read_stably(lambda: x_input_focus(env))
            check("withdrawing-the-input-request-refuses-the-focus",
                  focused == beta.win and active == beta.win,
                  logical_focus=focused, x_active_window=active, x_input_focus=real,
                  alpha=hex(alpha.win), expected=hex(beta.win))

            step("alpha withdrew its input request")
            step("keyboard navigation toward the input = False window")
            # 5. The same window reached by keyboard navigation, which is the
            #    one focus route that moves the WM's own focus model before the
            #    eligibility gate sees the request. Whatever the logical focus
            #    ends up naming, the X input focus must not land on a window that
            #    asked not to receive it — that is the whole of ICCCM 4.1.7's
            #    request, and it is the state the reconcile repair is for.
            sh([ctl, "msg", "focus:left"], env)
            settle()
            active = read_stably(lambda: x_active_window(env))
            real = read_stably(lambda: x_input_focus(env))
            focused = logical_focus()
            check("keyboard-focus-may-name-an-input-false-window-but-never-focuses-it",
                  real is not None and active is not None
                  and real != alpha.win and active != alpha.win,
                  logical_focus=focused, x_active_window=active, x_input_focus=real,
                  alpha=hex(alpha.win), expected="anything but " + hex(alpha.win))

            # 6. …and re-declaring once more is honoured, so eligibility is
            #    re-derived from the client's current words in both directions
            #    rather than latched either way. The keyboard-navigation step
            #    above left the WM's focus model naming alpha, so put the focus
            #    back on the peer first: the transition has to be a real one for
            #    the check to say anything, and it has to include the X input
            #    focus, which is what a stale latch kept out of reach.
            focus_request(beta.win)
            settle()
            alpha.declare_input(True)
            settle()
            focus_request(alpha.win)
            settle()
            focused = logical_focus()
            active = read_stably(lambda: x_active_window(env))
            check("eligibility-is-re-derived-not-latched",
                  focused == alpha.win and active == alpha.win,
                  logical_focus=focused, x_active_window=active,
                  x_input_focus=read_stably(lambda: x_input_focus(env)),
                  expected=hex(alpha.win))

            step("alpha re-declared input = True again")
            # 7. A client whose `WM_HINTS` disappears entirely — the property is
            #    removed, not rewritten — is the documented WM default
            #    (ICCCM 4.1.2.4: assume convenient values), not a refusal, and
            #    not the withdrawal of a request either.
            beta.withdraw_hints()
            if not wait_for(lambda: beta.server_input() is None, 5):
                raise RuntimeError(f"the property removal is not visible: {beta.server_input()}")
            settle()
            focus_request(beta.win)
            settle()
            focused = logical_focus()
            check("no-wm-hints-at-all-is-the-default-and-focusable",
                  focused == beta.win,
                  logical_focus=focused, x_active_window=x_active_window(env),
                  expected=hex(beta.win))

            step("beta dropped its WM_HINTS")
            # 8. The map-time read: a window that withdraws, re-offers, and maps
            #    with input = False must come back ineligible, so the decision is
            #    the same one whichever read of the property produced it.
            alpha.declare_input(False)
            settle()
            alpha.withdraw()
            if not wait_for(lambda: (window_count(env, ctl) or 0) == 1, 15):
                raise RuntimeError("the withdrawal did not unmanage the window")
            alpha.reoffer()
            if not wait_for(lambda: (window_count(env, ctl) or 0) == 2, 15):
                raise RuntimeError("the re-offer did not re-manage the window")
            settle()
            focus_request(alpha.win)
            settle()
            focused = logical_focus()
            check("map-time-input-false-is-honoured-on-remap", focused != alpha.win,
                  logical_focus=focused, x_active_window=x_active_window(env),
                  alpha=hex(alpha.win))

            step("alpha remapped with input = False")
            # 9. …and the mid-life read of that same window restores it.
            alpha.declare_input(True)
            settle()
            focus_request(alpha.win)
            settle()
            focused, active = logical_focus(), x_active_window(env)
            check("post-remap-flip-restores-eligibility", focused == alpha.win,
                  logical_focus=focused, x_active_window=active, expected=hex(alpha.win))

            step("alpha flipped its input request back on the remap")
            # 10. Removing the focused window reconciles onto the survivor.
            focus_request(beta.win)
            settle()
            beta.close()
            beta.destroy()
            if not wait_for(lambda: (window_count(env, ctl) or 0) == 1, 15):
                raise RuntimeError("the destroyed window was never unmanaged")
            settle()
            focused = logical_focus()
            active = read_stably(lambda: x_active_window(env))
            real = read_stably(lambda: x_input_focus(env))
            check("removing-the-focused-window-reconciles-to-the-survivor",
                  focused == alpha.win and active == alpha.win and real == alpha.win,
                  logical_focus=focused, x_active_window=active,
                  x_input_focus=real, expected=hex(alpha.win))

            alpha.close()
            beta.close()
            lib.XCloseDisplay(probe)
            probe = None
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
        # Xlib's IO-error handler calls C `exit()` on a broken display, which
        # would discard this traceback, so report before the teardown gets there.
        traceback.print_exc()
        code = 2
    sys.exit(code)
