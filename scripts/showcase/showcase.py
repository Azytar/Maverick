#!/usr/bin/env python3
import argparse
import ctypes
import colorsys
import math
import fcntl
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
SCENES = ("tiling", "navigation", "floating", "fullscreen", "compositor",
          "rounded", "tiled-spacing", "floating-scroll", "fullscreen-decoration",
          "rounded-focus", "rounded-focus-gl", "fullscreen-new-window", "fullscreen-new-window-gl",
          "fullscreen-transition", "fullscreen-transition-gl")
TRANSITION_SCENES = ("fullscreen-transition", "fullscreen-transition-gl")
FOCUS_SCENES = ("rounded-focus", "rounded-focus-gl")
FULLSCREEN_SCENES = ("fullscreen-new-window", "fullscreen-new-window-gl")
GL_SCENES = ("compositor", "rounded-focus-gl", "fullscreen-new-window-gl", "fullscreen-transition-gl")
# Render scale: the live Xephyr framebuffer is INTERNAL, the logical README
# asset is SIZE. Only 2x is supported by the authoritative path (other
# scales live in the supersample.py experiment driver, /tmp outputs only).
SUPER = 2
SIZE = (1440, 900)
INTERNAL = (SIZE[0] * SUPER, SIZE[1] * SUPER)
BORDER, RADIUS = 1 * SUPER, 18 * SUPER
FOCUSED, NORMAL = (137, 180, 250), (69, 71, 90)
CONTENT = (30, 30, 46)
FONT_FACE = "Fira Code"
FONT_FILE = "/usr/share/fonts/TTF/FiraCode-Regular.ttf"
# xterm has no OpenType shaping (libXft, no harfbuzz): Fira Code ligatures are
# present in the font (GSUB/liga) but are NOT rendered. Chosen for metrics and
# glyph clarity, not ligatures. Verified local-only: fc-match resolves the
# exact file above, no fallback; measured settled grid 56x57 in an 878x1764
# tiled window (vs 49x50 for Noto 11 at 1x).
FONT_SIZE = 22


def fold_line(line, width):
    """Fold one source line to fit `width` cells without mid-word cuts.

    Word-aware wrap; only an overlong whitespace-free token is hard-folded
    with a trailing ellipsis marker. Returns the list of continuation rows.
    """
    if len(line) <= width:
        return [line]
    rows, current = [], ""
    for word in line.split(" "):
        if not current:
            if len(word) <= width:
                current = word
            else:
                while len(word) > width - 1:
                    rows.append(word[:width - 1] + "…")
                    word = word[width - 1:]
                current = word
        elif len(current) + 1 + len(word) <= width:
            current += " " + word
        else:
            rows.append(current)
            if len(word) <= width:
                current = word
            else:
                while len(word) > width - 1:
                    rows.append(word[:width - 1] + "…")
                    word = word[width - 1:]
                current = word
    rows.append(current)
    return rows


def client(title, source):
    lines = (ROOT / source).read_text().splitlines()
    def draw(*_):
        columns, rows = shutil.get_terminal_size()
        width = max(10, columns - 7)
        budget = max(1, rows - 9)
        print("\033[2J\033[H\033[?25l\033[1;36m" + title + "\033[0m\n")
        print("MAVERICK / LIVE X11 CLIENT\n")
        print(source + "\n" + "─" * min(42, columns - 1))
        folded = []
        for index, line in enumerate(lines, 1):
            for position, part in enumerate(fold_line(line, width)):
                folded.append((index, position > 0, part))
                if len(folded) >= budget:
                    break
            if len(folded) >= budget:
                break
        shown = {index for index, _, _ in folded}
        if len(shown) < len(lines):
            folded = folded[:max(0, budget - 1)]
            shown = {index for index, _, _ in folded}
        for index, continuation, part in folded:
            if continuation:
                print(f"\033[90m    │\033[0m {part}")
            else:
                print(f"\033[90m{index:3} │\033[0m {part}")
        if len(shown) < len(lines):
            print(f"\033[90m    │ … +{len(lines) - len(shown)} lines\033[0m")
        sys.stdout.flush()
    signal.signal(signal.SIGWINCH, draw)
    draw()
    while True:
        signal.pause()


def run(args, env=None, check=True):
    result = subprocess.run(args, env=env, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=5)
    if check and result.returncode:
        raise RuntimeError(f"Command {args!r} failed ({result.returncode}):\n{result.stdout}{result.stderr}")
    return result


def wait_for(description, probe, timeout=15):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = probe()
            if value:
                return value
        except (RuntimeError, subprocess.SubprocessError, ValueError, OSError) as error:
            last = error
        time.sleep(0.15)
    raise RuntimeError(f"Timed out waiting for {description}: {last or 'not ready'}")


class Session:
    def __init__(self, scene, binaries, output):
        self.scene, self.binaries, self.output = scene, binaries, output
        self.temp = tempfile.TemporaryDirectory(prefix="mav-showcase-", dir="/tmp")
        self.path = Path(self.temp.name)
        self.processes = []
        self.logs = []
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("MAVERICK_", "MAV_"))}
        self.env["MAVERICK_LOG"] = "info"
        self.env["MAV_COMP_TRACE"] = "1"
        if scene in (*FULLSCREEN_SCENES, *TRANSITION_SCENES):
            self.env["MAV_FLOAT_TRACE"] = "1"
        self.env["LC_ALL"] = "C.UTF-8"
        self.env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"):
            directory = self.path / key.lower()
            directory.mkdir(mode=0o700)
            self.env[key] = str(directory)
        if os.environ.get("XAUTHORITY"):
            self.env["XAUTHORITY"] = os.environ["XAUTHORITY"]
        elif (Path.home() / ".Xauthority").exists():
            self.env["XAUTHORITY"] = str(Path.home() / ".Xauthority")

    def spawn(self, args, label, env=None, **kwargs):
        log = (self.path / f"{label}.log").open("w")
        self.logs.append(log)
        process = subprocess.Popen(args, env=env or self.env, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=subprocess.STDOUT,
                                   start_new_session=True, **kwargs)
        self.processes.append(process)
        return process

    def start(self):
        read_fd, write_fd = os.pipe()
        try:
            self.xephyr = self.spawn(["Xephyr", "-displayfd", str(write_fd), "-screen",
                                     f"{INTERNAL[0]}x{INTERNAL[1]}", "-nolisten", "tcp", "-ac",
                                     "+extension", "GLX", "+extension", "Composite",
                                     "+extension", "DAMAGE"], "xephyr", pass_fds=(write_fd,))
            os.close(write_fd)
            write_fd = None
            os.set_blocking(read_fd, False)
            display = wait_for("Xephyr display allocation", lambda: os.read(read_fd, 64).strip())
        finally:
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)
        self.env["DISPLAY"] = ":" + display.decode()
        wait_for("X11 readiness", lambda: run(["xdpyinfo"], self.env, False).returncode == 0)
        config = self.path / "config.toml"
        config.write_text(f'''[general]
border_width = {BORDER}
corner_radius = {RADIUS if self.scene in ("compositor", "rounded", "tiled-spacing", "floating-scroll", "fullscreen-decoration", *FOCUS_SCENES, *FULLSCREEN_SCENES, *TRANSITION_SCENES) else 0}
gaps_inner = {(6 if self.scene == "tiled-spacing" else 4) * SUPER}
gaps_outer = {(10 if self.scene == "tiled-spacing" else 8) * SUPER}
column_width = 0.31
n_tags = 3
focus_mouse = false
warp_cursor = false
[colors]
normal = 0x45475a
focused = 0x89b4fa
[animations]
enabled = {str(self.scene == "fullscreen-transition-gl").lower()}
[compositor]
enabled = {str(self.scene in GL_SCENES).lower()}
backend = "opengl"
fullscreen_bypass = false
[autostart]
commands = [["/usr/bin/true"]]
[[rules]]
instance = "showcase3"
opacity = {0.78 if self.scene == "compositor" else 1.0}
''')
        if self.scene in (*FOCUS_SCENES, *FULLSCREEN_SCENES):
            with config.open("a") as stream:
                stream.write('\n[[rules]]\ninstance = "showcase6"\nfloat = true\n')
        run([str(self.binaries / "maverick"), "--check-config", str(config)], self.env)
        self.wm = self.spawn([str(self.binaries / "maverick"), "--config", str(config),
                              "--name", "showcase"], "wm")
        wait_for("Maverick IPC startup", lambda: self.state().get("monitors"))
        run(["xsetroot", "-solid", "#11111b"], self.env)
        print(f"{self.scene}: WM started on {self.env['DISPLAY']} (pid {self.wm.pid})", flush=True)

    def state(self):
        return json.loads(run([str(self.binaries / "maverickctl"), "state"], self.env).stdout)

    def tree(self):
        return json.loads(run([str(self.binaries / "maverickctl"), "query", "tree"], self.env).stdout)

    def gl_active(self):
        return self._gl_active(self.path / "wm.log")

    @staticmethod
    def _gl_active(log):
        try:
            text = Path(log).read_text()
        except FileNotFoundError:
            return False
        return "Backend: OpenGL/GLX" in text and "event=PresentEnd submitted=true" in text

    def action(self, action):
        result = run([str(self.binaries / "maverickctl"), "msg", action], self.env)
        print(f"  {action}: {result.stdout.strip()}", flush=True)

    def terminal(self, number, title, source):
        name = f"showcase{number}"
        background = ({2: "#542638", 3: "#245447"}.get(number, "#1e1e2e")
                      if self.scene in FULLSCREEN_SCENES else "#1e1e2e")
        self.spawn(["xterm", "-name", name, "-class", "Showcase", "-title", title,
                    "-fa", FONT_FACE, "-fs", str(FONT_SIZE), "-bg", background,
                    "-fg", "#cdd6f4", "-cr", background, "+sb", "-b", str(18 * SUPER),
                    "-geometry", "72x36", "-e", sys.executable, str(Path(__file__).resolve()),
                    "--client", title, source], name)
        return wait_for(f"client {number}", lambda: run(["xdotool", "search", "--onlyvisible",
                         "--classname", "^" + name + "$"], self.env).stdout.strip().splitlines())[0]

    def stable(self, windows):
        last, since = None, time.monotonic()
        def probe():
            nonlocal last, since
            value = [run(["xdotool", "getwindowgeometry", "--shell", window], self.env).stdout
                     for window in windows]
            if value != last:
                last, since = value, time.monotonic()
            return value if time.monotonic() - since > 0.8 else None
        return wait_for("stable client geometry", probe)

    def capture(self, windows):
        geometry = self.stable(windows)
        # Hires lives in the temp session dir (auto-cleaned with it); only the
        # final 1440x900 lands in docs/screenshots (no *-hires.png pollution).
        hires = self.path / f"{self.scene}-hires.png"
        final = self.output / f"{self.scene}.png"
        run(["import", "-display", self.env["DISPLAY"], "-window", "root", str(hires)], self.env)
        dimensions = run(["identify", "-format", "%wx%h", str(hires)]).stdout
        if dimensions != f"{INTERNAL[0]}x{INTERNAL[1]}":
            raise RuntimeError(f"Unexpected hires screenshot dimensions: {dimensions}")
        # Proven Phase-1 path: Lanczos downsample to logical size, sRGB, strip.
        run(["magick", str(hires), "-filter", "Lanczos", "-resize",
             f"{SIZE[0]}x{SIZE[1]}!", "-colorspace", "sRGB", "-strip", str(final)])
        dimensions = run(["identify", "-format", "%wx%h", str(final)]).stdout
        if dimensions != f"{SIZE[0]}x{SIZE[1]}":
            raise RuntimeError(f"Unexpected final screenshot dimensions: {dimensions}")
        print(f"  captured {final.relative_to(ROOT)} ({dimensions}, via hires {INTERNAL[0]}x{INTERNAL[1]})",
              flush=True)
        return geometry

    def close(self):
        def descendants(pid):
            try:
                children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
            except FileNotFoundError:
                return []
            result = []
            for child in children:
                result.extend(descendants(int(child)))
                result.append(int(child))
            return result

        def stop(sig):
            for pid in descendants(os.getpid()):
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass

        stop(signal.SIGTERM)
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline and any(p.poll() is None for p in self.processes):
            time.sleep(0.1)
        stop(signal.SIGKILL)
        for process in self.processes:
            process.wait(timeout=3)
        deadline = time.monotonic() + 3
        while descendants(os.getpid()) and time.monotonic() < deadline:
            try:
                os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                pass
            time.sleep(0.05)
        remaining = descendants(os.getpid())
        for log in self.logs:
            log.close()
        self.temp.cleanup()
        if remaining:
            raise RuntimeError(f"Owned descendants survived cleanup: {remaining}")
        print(f"{self.scene}: owned descendants reaped; temporary config/runtime removed", flush=True)


def float_geometry(session, window):
    """Screen-space geometry of a window via xdotool, or None."""
    try:
        shell = run(["xdotool", "getwindowgeometry", "--shell", window], session.env).stdout
        fields = dict(line.split("=", 1) for line in shell.strip().splitlines() if "=" in line)
        return (int(fields["X"]), int(fields["Y"]), int(fields["WIDTH"]), int(fields["HEIGHT"]))
    except (RuntimeError, KeyError):
        return None


def color_match(pixel, target, root):
    backgrounds = (CONTENT, root)
    alternate = NORMAL if target == FOCUSED else FOCUSED
    def residual(color):
        best = float("inf")
        for background in backgrounds:
            delta = tuple(c - b for c, b in zip(color, background))
            weight = sum((p - b) * d for p, b, d in zip(pixel, background, delta)) / sum(d * d for d in delta)
            if 0.18 <= weight <= 1.1:
                best = min(best, math.dist(pixel, tuple(b + min(weight, 1) * d
                                                       for b, d in zip(background, delta))))
        return best
    if min(math.dist(pixel, background) for background in backgrounds) < 12:
        return False
    if target == FOCUSED:
        hue = colorsys.rgb_to_hsv(*(value / 255 for value in pixel))[0]
        if abs(hue - colorsys.rgb_to_hsv(*(value / 255 for value in FOCUSED))[0]) > 0.04:
            return False
    score = residual(target)
    return score <= 12 and score + 4 < residual(alternate)


def corner_pixels(session, evidence, stage, checks, fullscreen=False):
    session.stable([window for window, _ in checks])
    run(["xdotool", "mousemove", str(INTERNAL[0] - 1), str(INTERNAL[1] - 1)], session.env)
    image = evidence / f"{session.scene}-{stage}.png"
    run(["import", "-display", session.env["DISPLAY"], "-window", "root", str(image)], session.env)
    raw = subprocess.run(["convert", str(image), "-alpha", "off", "-colorspace", "sRGB",
                          "-depth", "8", "rgb:-"], stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, check=True, timeout=15).stdout
    if len(raw) != INTERNAL[0] * INTERNAL[1] * 3:
        raise RuntimeError(f"Unexpected raw RGB byte count: {len(raw)}")
    def pixel(x, y):
        offset = (y * INTERNAL[0] + x) * 3
        return tuple(raw[offset:offset + 3])
    root = (0, 0, 0) if session.scene in GL_SCENES else (17, 17, 27)
    report = {"stage": stage, "outer_radius": 0 if fullscreen else RADIUS,
              "inner_radius": 0 if fullscreen else max(RADIUS - BORDER, 0),
              "border": 0 if fullscreen else BORDER, "root_color": root, "windows": []}
    failures = []
    for window, focused in checks:
        geometry = float_geometry(session, window)
        if geometry is None:
            raise RuntimeError(f"No geometry for {window}")
        x, y, width, height = geometry
        if not fullscreen:
            width += 2 * BORDER
            height += 2 * BORDER
        if x < 0 or y < 0 or x + width > INTERNAL[0] or y + height > INTERNAL[1]:
            raise RuntimeError(f"Corner probe requires fully visible outer frame: {geometry}")
        if fullscreen and (x, y, width, height) != (0, 0, *INTERNAL):
            raise RuntimeError(f"Fullscreen did not cover monitor: {geometry}")
        result = {"id": window, "focused": focused, "outer_geometry": [x, y, width, height], "corners": {}}
        for name, right, bottom in (("tl", False, False), ("tr", True, False),
                                    ("bl", False, True), ("br", True, True)):
            samples, blue, normal = [], [], []
            for v in range(RADIUS):
                for u in range(RADIUS):
                    px = x + (width - 1 - u if right else u)
                    py = y + (height - 1 - v if bottom else v)
                    rgb = pixel(px, py)
                    samples.append(rgb)
                    if (2 <= u < RADIUS - 2 and 2 <= v < RADIUS - 2
                            and abs(math.hypot(RADIUS - u - 0.5, RADIUS - v - 0.5)
                                    - (RADIUS - BORDER / 2)) <= 2):
                        if color_match(rgb, FOCUSED, root):
                            blue.append([u, v, *rgb])
                        if color_match(rgb, NORMAL, root):
                            normal.append([u, v, *rgb])
            crop_x = x + width - RADIUS if right else x
            crop_y = y + height - RADIUS if bottom else y
            crop = evidence / f"{session.scene}-{stage}-{window}-{name}.png"
            run(["convert", str(image), "-crop", f"{RADIUS}x{RADIUS}+{crop_x}+{crop_y}",
                 "+repage", "-filter", "point", "-resize", "800%", str(crop)])
            result["corners"][name] = {"blue_arc_pixels": blue, "normal_arc_pixels": normal,
                                       "outermost_rgb": samples[0], "crop": str(crop)}
            if fullscreen:
                if any(color_match(rgb, FOCUSED, root) for rgb in samples) or any(
                        math.dist(rgb, CONTENT) > 8 for rgb in samples):
                    failures.append(f"{window}/{name}: fullscreen corner not square undecorated content")
            else:
                ring = blue if focused else normal
                if len(ring) < 3 or len({p[0] for p in ring}) < 2 or len({p[1] for p in ring}) < 2:
                    failures.append(f"{window}/{name}: missing {'focused' if focused else 'normal'} curved arc")
                if not focused and blue:
                    failures.append(f"{window}/{name}: unfocused arc retains blue")
        if fullscreen:
            edges = [pixel(width // 2, 0), pixel(width // 2, height - 1),
                     pixel(0, height // 2), pixel(width - 1, height // 2)]
            result["edge_pixels"] = edges
            if any(math.dist(rgb, CONTENT) > 8 for rgb in edges):
                failures.append(f"{window}: fullscreen still has edge decoration")
        report["windows"].append(result)
    report["failures"] = failures
    (evidence / f"{session.scene}-{stage}.json").write_text(json.dumps(report, indent=2) + "\n")
    if failures:
        raise RuntimeError("; ".join(failures) + f"; evidence: {image}")
    print(f"  {stage}: all four {'square fullscreen corners' if fullscreen else 'curved corner rings'} verified", flush=True)
    return report


def rounded_focus(session, windows, evidence):
    def activate(window):
        run(["xdotool", "windowactivate", "--sync", window], session.env)
        wait_for(f"focus on {window}", lambda: any(
            monitor.get("focused") == int(window) for monitor in session.state()["monitors"]))
        session.stable(windows)

    def tiled_checks(window):
        visible = []
        for other in windows:
            x, y, width, height = float_geometry(session, other)
            if other != window and x >= 0 and y >= 0 and x + width + 2 * BORDER <= INTERNAL[0] and y + height + 2 * BORDER <= INTERNAL[1]:
                visible.append((other, False))
        if not visible:
            raise RuntimeError("No fully visible unfocused tile for corner comparison")
        return [(window, True), *visible]

    if session.scene in GL_SCENES:
        wait_for("actual GL renderer and submitted frame (fallback is not accepted)", session.gl_active)
    elif session.gl_active() or "Backend: OpenGL/GLX" in (session.path / "wm.log").read_text():
        raise RuntimeError("rounded-focus must run without a compositor")
    reports = [corner_pixels(session, evidence, "tiles", [(windows[2], True), (windows[1], False)])]
    sizes = [float_geometry(session, window)[2:] for window in windows]
    session.action("focus:left")
    reports.append(corner_pixels(session, evidence, "focus-change", [(windows[1], True), (windows[2], False)]))
    if sizes != [float_geometry(session, window)[2:] for window in windows]:
        raise RuntimeError("Focus change altered tile sizes with B1 R18")
    before_scroll = float_geometry(session, windows[0])
    windows.extend([session.terminal(4, "04 / Rendering", "maverick-gl/Cargo.toml"),
                    session.terminal(5, "05 / IPC", "maverick-sys/Cargo.toml")])
    activate(windows[4])
    session.action("focus:left")
    session.stable(windows)
    reports.append(corner_pixels(session, evidence, "scrolled", tiled_checks(windows[3])))
    if float_geometry(session, windows[0])[:2] == before_scroll[:2]:
        raise RuntimeError("Scrolled test did not move the ribbon camera")
    session.action("toggle_fullscreen")
    reports.append(corner_pixels(session, evidence, "fullscreen", [(windows[3], True)], fullscreen=True))
    session.action("toggle_fullscreen")
    session.stable(windows)
    reports.append(corner_pixels(session, evidence, "fullscreen-exit", tiled_checks(windows[3])))
    floating = session.terminal(6, "06 / Floating isolation", "Cargo.toml")
    windows.append(floating)
    activate(floating)
    run(["xdotool", "windowsize", floating, str(480 * SUPER), str(360 * SUPER)], session.env)
    session.stable(windows)
    run(["xdotool", "windowmove", floating, str(480 * SUPER), str(270 * SUPER)], session.env)
    reports.append(corner_pixels(session, evidence, "floating", [(floating, True)]))
    before = float_geometry(session, floating)
    tiled_before = float_geometry(session, windows[0])
    for _ in range(4):
        session.action("focus:left")
    session.stable(windows)
    if before != float_geometry(session, floating):
        raise RuntimeError("Floating window moved during ribbon scroll")
    if tiled_before[:2] == float_geometry(session, windows[0])[:2]:
        raise RuntimeError("Floating isolation test did not actually scroll tiles")
    reports.append(corner_pixels(session, evidence, "floating-unfocused", [(floating, False)]))
    activate(floating)
    reports.append(corner_pixels(session, evidence, "floating-refocused", [(floating, True)]))
    before = float_geometry(session, floating)
    tiled_before = [float_geometry(session, window) for window in windows[:-1]]
    x, y, width, height = before
    run(["xdotool", "mousemove", str(x + width // 2), str(y + height // 2),
         "keydown", "Super_L", "mousedown", "1", "sleep", "0.2",
         "mousemove", str(x + width // 2 + 80 * SUPER), str(y + height // 2 + 50 * SUPER),
         "sleep", "0.2", "mouseup", "1", "keyup", "Super_L"], session.env)
    session.stable(windows)
    after = float_geometry(session, floating)
    if after[:2] != (before[0] + 80 * SUPER, before[1] + 50 * SUPER):
        raise RuntimeError(f"Floating drag did not follow pointer delta: {before} -> {after}")
    if tiled_before != [float_geometry(session, window) for window in windows[:-1]]:
        raise RuntimeError("Native floating drag changed tiled geometry")
    reports.append(corner_pixels(session, evidence, "floating-drag", [(floating, True)]))
    reports[-1]["drag_geometry"] = {"before": before, "after": after}
    run(["xdotool", "windowclose", floating], session.env)
    windows.remove(floating)
    activate(windows[3])
    reports.append(corner_pixels(session, evidence, "final", tiled_checks(windows[3])))
    return reports


def x_stack(session):
    lib = ctypes.CDLL("libX11.so.6")
    window_type = ctypes.c_ulong
    lib.XOpenDisplay.argtypes = [ctypes.c_char_p]
    lib.XOpenDisplay.restype = ctypes.c_void_p
    lib.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
    lib.XDefaultRootWindow.restype = window_type
    lib.XQueryTree.argtypes = [ctypes.c_void_p, window_type, ctypes.POINTER(window_type),
                              ctypes.POINTER(window_type), ctypes.POINTER(ctypes.POINTER(window_type)),
                              ctypes.POINTER(ctypes.c_uint)]
    lib.XGetInputFocus.argtypes = [ctypes.c_void_p, ctypes.POINTER(window_type), ctypes.POINTER(ctypes.c_int)]
    lib.XFree.argtypes = [ctypes.c_void_p]
    lib.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = lib.XOpenDisplay(session.env["DISPLAY"].encode())
    if not display:
        raise RuntimeError("XOpenDisplay failed for actual stack evidence")
    children = ctypes.POINTER(window_type)()
    try:
        root, parent, focus = window_type(), window_type(), window_type()
        count, revert = ctypes.c_uint(), ctypes.c_int()
        if not lib.XQueryTree(display, lib.XDefaultRootWindow(display), ctypes.byref(root),
                              ctypes.byref(parent), ctypes.byref(children), ctypes.byref(count)):
            raise RuntimeError("XQueryTree failed")
        lib.XGetInputFocus(display, ctypes.byref(focus), ctypes.byref(revert))
        return {"source": "XQueryTree(root)", "bottom_to_top": list(children[:count.value]),
                "input_focus": focus.value, "root": root.value}
    finally:
        if children:
            lib.XFree(children)
        lib.XCloseDisplay(display)


def repaint(session):
    """Ask every visible top-level to repaint (XClearArea on the root with
    exposures=True), then give the clients a moment to finish.

    fullscreen_new_window compares two screenshots of the same fullscreen
    window across a client insertion and fails on a single changed pixel. That
    comparison is only meaningful if both frames are fully painted: an xterm
    that grew from the tiled rectangle to the monitor keeps one partially
    painted cell from the pre-fullscreen column layout until something exposes
    it again, which is a client repaint artefact, not a Maverick behaviour.
    XClearArea generates the exposures through the X server only: no synthetic
    window is mapped, so stacking, focus and geometry stay untouched."""
    lib = ctypes.CDLL("libX11.so.6")
    lib.XOpenDisplay.argtypes = [ctypes.c_char_p]
    lib.XOpenDisplay.restype = ctypes.c_void_p
    lib.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
    lib.XDefaultRootWindow.restype = ctypes.c_ulong
    lib.XClearArea.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
                               ctypes.c_uint, ctypes.c_uint, ctypes.c_int]
    lib.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    lib.XCloseDisplay.argtypes = [ctypes.c_void_p]
    display = lib.XOpenDisplay(session.env["DISPLAY"].encode())
    if not display:
        raise RuntimeError("XOpenDisplay failed for the presentation repaint")
    try:
        lib.XClearArea(display, lib.XDefaultRootWindow(display), 0, 0, 0, 0, 1)
        lib.XSync(display, 0)
    finally:
        lib.XCloseDisplay(display)
    time.sleep(0.5)


def fullscreen_new_window(session, windows, evidence):
    reports = []
    owner = windows[0]

    def entries(tree):
        return {str(window["id"]): window
                for monitor in tree["monitors"] for workspace in monitor["workspaces"]
                for window in [*(w for col in workspace["columns"] for w in col["windows"]),
                               *workspace["floats"]]}

    def pixels(path):
        raw = subprocess.run(["convert", str(path), "-alpha", "off", "-colorspace", "sRGB",
                              "-depth", "8", "rgb:-"], stdout=subprocess.PIPE,
                             stderr=subprocess.PIPE, check=True, timeout=15).stdout
        if len(raw) != INTERNAL[0] * INTERNAL[1] * 3:
            raise RuntimeError("Unexpected fullscreen RGB byte count")
        return raw

    def snapshot(stage, overlay=None, focused=None, pending=None, reference=None):
        session.stable(windows)
        last, since = None, time.monotonic()
        def stable_stack():
            nonlocal last, since
            value = x_stack(session)
            if value != last:
                last, since = value, time.monotonic()
            return value if time.monotonic() - since > 0.8 else None
        stack = wait_for("stable actual QueryTree stack and X focus", stable_stack)
        repaint(session)
        tree, state = session.tree(), session.state()
        objects = entries(tree)
        geometry = {w: float_geometry(session, w) for w in windows}
        scroll = [[ws["scroll"] for ws in mon["workspaces"]] for mon in tree["monitors"]]
        image = evidence / f"{session.scene}-{stage}.png"
        run(["xdotool", "mousemove", str(INTERNAL[0] - 1), str(INTERNAL[1] - 1)], session.env)
        run(["import", "-display", session.env["DISPLAY"], "-window", "root", str(image)], session.env)
        raw = pixels(image)
        failures = []
        record = {"stage": stage, "state": state, "tree": tree, "x_geometry": geometry,
                  "actual_stack": stack, "camera": scroll, "image": str(image),
                  "gl_active": session.gl_active(), "failures": failures}
        for window in windows:
            obj = objects.get(window, {})
            # A freshly adopted float (client-driven resize/move) keeps its
            # pre-move projection in `desired` until the next full arrange —
            # the float-authority model (layout.rs). Tiles must satisfy the
            # strict equality; floats only need applied/real == X11.
            fields = ("applied", "real") if obj.get("float") else ("desired", "applied", "real")
            for field in fields:
                if obj.get(field) != list(geometry[window] or ()):
                    failures.append(f"{window}: {field} {obj.get(field)} != actual {geometry[window]}")
            if int(window) not in stack["bottom_to_top"]:
                failures.append(f"{window}: missing from root top-level QueryTree")
        if overlay is not None:
            obj = objects.get(owner, {})
            if not obj.get("fullscreen") or obj.get("overlay") != overlay:
                failures.append(f"A fullscreen/overlay expected true/{overlay}: {obj}")
            if geometry[owner] != (0, 0, *INTERNAL):
                failures.append(f"A does not cover monitor: {geometry[owner]}")
            order = stack["bottom_to_top"]
            for window in windows[1:]:
                if int(owner) in order and int(window) in order and order.index(int(owner)) <= order.index(int(window)):
                    failures.append(f"A is not above {window} in actual XQueryTree")
        if focused is not None:
            if not objects.get(focused, {}).get("focus") or not objects.get(focused, {}).get("x11_focus"):
                failures.append(f"Expected logical and observed X focus on {focused}")
            if stack["input_focus"] != int(focused):
                failures.append(f"Actual XGetInputFocus {stack['input_focus']} != {focused}")
        actual_pending = sorted(w for w, obj in objects.items() if obj.get("pending"))
        if actual_pending != ([] if pending is None else [pending]):
            failures.append(f"Pending focus {actual_pending} != {pending}")
        for window in windows[1:]:
            obj = objects.get(window, {})
            if not obj.get("float") and (obj.get("fullscreen") or obj.get("overlay") or
                                         not geometry[window] or geometry[window][2] >= INTERNAL[0]):
                failures.append(f"{window}: new tile did not retain normal ribbon geometry")
        if reference is not None:
            before = pixels(Path(reference["image"]))
            # Row-chunk compare: identical result to the old per-pixel loop
            # (~5.2M iterations) at a fraction of the cost. The cursor parking
            # rect is excluded by masking that corner out of both buffers.
            row = INTERNAL[0] * 3
            excl = INTERNAL[0] - 32 * SUPER
            masked, masked_before = bytearray(raw), bytearray(before)
            for y in range(INTERNAL[1] - 32 * SUPER, INTERNAL[1]):
                off = y * row + excl * 3
                masked[off:off + 32 * SUPER * 3] = bytes(32 * SUPER * 3)
                masked_before[off:off + 32 * SUPER * 3] = bytes(32 * SUPER * 3)
            masked, masked_before = bytes(masked), bytes(masked_before)
            # A real insertion disturbance (shift, ghost, border) moves
            # thousands of pixels. A handful of AA-edge pixels is xterm/Xft
            # double-blend noise from the forced repaint above (deterministic
            # per stage: 4 pixels at Δ21 for new-b, 8 at Δ35 for new-c, same
            # glyph column) and is invisible after the Lanczos downsample, so
            # it is recorded, not failed. Counting caps at 4096 to bound the
            # failure path instead of walking all 5.2M pixels on a real break.
            changed, samples, maxdelta, capped = 0, [], 0, False
            for y in range(INTERNAL[1]):
                a, b = masked[y * row:(y + 1) * row], masked_before[y * row:(y + 1) * row]
                if a != b:
                    for x in range(INTERNAL[0]):
                        o = x * 3
                        pa, pb = a[o:o + 3], b[o:o + 3]
                        if pa != pb:
                            changed += 1
                            delta = max(abs(u - v) for u, v in zip(pa, pb))
                            if delta > maxdelta:
                                maxdelta = delta
                            if len(samples) < 12:
                                samples.append([x, y, list(pb), list(pa)])
                            if changed > 4096:
                                capped = True
                                break
                    if capped:
                        break
            record["pixel_comparison"] = {"reference": reference["image"], "changed_pixels": changed,
                                           "capped": capped, "max_channel_delta": maxdelta,
                                           "excluded_cursor_rect": [INTERNAL[0] - 32 * SUPER, INTERNAL[1] - 32 * SUPER, 32 * SUPER, 32 * SUPER],
                                           "first_differences": samples}
            if capped or not (changed <= 64 and maxdelta <= 64):
                failures.append(f"Fullscreen content changed after insertion: {changed} pixels"
                                f" (max Δ {maxdelta})")
            elif changed:
                record["pixel_comparison"]["accepted_aa_noise"] = True
        if session.scene in GL_SCENES and not session.gl_active():
            failures.append("Real OpenGL/GLX backend and submitted frame required")
        reports.append(record)
        (evidence / f"{session.scene}-{stage}.json").write_text(json.dumps(record, indent=2) + "\n")
        (evidence / f"{session.scene}.json").write_text(json.dumps(
            {"scene": session.scene, "checks": reports, "failures": failures}, indent=2) + "\n")
        if failures:
            raise RuntimeError("; ".join(failures))
        return record

    def activate(window):
        run(["xdotool", "windowactivate", "--sync", window], session.env)
        wait_for(f"focus on {window}", lambda: any(
            mon.get("focused") == int(window) for mon in session.state()["monitors"]))
        session.stable(windows)

    session.stable(windows)
    if session.scene in GL_SCENES:
        wait_for("actual GL renderer and submitted frame (fallback is not accepted)", session.gl_active)
    snapshot("a-only", focused=owner)
    session.action("toggle_fullscreen")
    baseline = snapshot("a-fullscreen", overlay=True, focused=owner)
    windows.append(session.terminal(2, "B / MAGENTA NEW RIBBON CLIENT", "Cargo.toml"))
    snapshot("new-b-principal", overlay=True, focused=owner, pending=windows[1],
             reference=baseline)
    windows.append(session.terminal(3, "C / GREEN NEW RIBBON CLIENT", "tests/realwin.c"))
    snapshot("new-c", overlay=True, focused=owner, pending=windows[2],
             reference=baseline)
    floating = session.terminal(6, "06 / Floating isolation", "Cargo.toml")
    windows.append(floating)
    snapshot("new-unrelated-float", overlay=True, focused=owner, pending=floating,
             reference=baseline)
    if not entries(session.tree())[floating].get("float"):
        raise RuntimeError("showcase6 rule did not create a floating window")
    session.action("focus:right")
    snapshot("navigate-l", focused=windows[1])
    session.action("focus:left")
    snapshot("return-a-ribbon", overlay=False, focused=owner)
    session.action("toggle_fullscreen")
    # IPC acknowledgement precedes action dispatch; do not activate another
    # client until the focused-window command has actually completed.
    wait_for("A has exited fullscreen", lambda: not entries(session.tree())[owner]["fullscreen"])
    activate(floating)
    run(["xdotool", "windowsize", floating, str(480 * SUPER), str(360 * SUPER)], session.env)
    session.stable(windows)
    run(["xdotool", "windowmove", floating, str(480 * SUPER), str(270 * SUPER)], session.env)
    snapshot("float-focus", focused=floating)
    before = float_geometry(session, floating)
    activate(windows[1])
    session.action("focus:left")
    session.stable(windows)
    snapshot("float-unfocus", focused=owner)
    windows.extend([session.terminal(4, "04 / Rendering", "maverick-gl/Cargo.toml"),
                    session.terminal(5, "05 / IPC", "maverick-sys/Cargo.toml")])
    activate(windows[-1])
    tiled_before = float_geometry(session, windows[0])
    for _ in range(4):
        session.action("focus:left")
    session.stable(windows)
    isolation = snapshot("floating-scroll-isolation")
    isolation["float_before"] = before
    isolation["float_after"] = float_geometry(session, floating)
    isolation["tile_before"] = tiled_before
    isolation["tile_after"] = float_geometry(session, windows[0])
    failures = isolation["failures"]
    if before != isolation["float_after"]:
        failures.append("Floating geometry moved with ribbon camera")
    if tiled_before[:2] == isolation["tile_after"][:2]:
        failures.append("Floating isolation did not actually scroll tiles")
    (evidence / f"{session.scene}-floating-scroll-isolation.json").write_text(json.dumps(isolation, indent=2) + "\n")
    if failures:
        raise RuntimeError("; ".join(failures))
    activate(floating)
    for window in windows[2:]:
        run(["xdotool", "windowclose", window], session.env)
    windows[:] = windows[:2]
    wait_for("extra clients removed", lambda: set(entries(session.tree())) == set(windows))
    activate(owner)
    snapshot("a-b-before-fullscreen", focused=owner)
    session.action("toggle_fullscreen")
    snapshot("a-b-fullscreen-a", overlay=True, focused=owner)
    return reports


def fullscreen_transition(session, windows, evidence):
    gl = session.scene in GL_SCENES
    log = session.path / "wm.log"
    screen = float_geometry(session, str(x_stack(session)["root"]))
    if screen != (0, 0, *INTERNAL):
        raise RuntimeError(f"Unexpected Xephyr screen: {screen}")
    reports = []
    rect_pattern = r"Rect \{ x: (-?\d+), y: (-?\d+), w: (\d+), h: (\d+) \}"
    pattern = re.compile(r"\[TRANSFORM\] frame=(\d+) win=(0x[0-9a-f]+) old_transform="
                         + rect_pattern + r" new_transform=" + rect_pattern
                         + r" .*?radius=(\d+) transition=(true|false)")

    def traces(window, offset=0):
        text = log.read_text()[offset:]
        presented = {int(frame) for frame in re.findall(r"\[PRESENT\] frame=(\d+) submitted=true", text)}
        records = []
        for match in pattern.finditer(text):
            frame, win, *values = match.groups()
            if int(win, 16) == int(window) and int(frame) in presented:
                records.append({"frame": int(frame), "old": tuple(map(int, values[:4])),
                                "rect": tuple(map(int, values[4:8])), "radius": int(values[8]),
                                "transition": values[9] == "true", "submitted": True,
                                "line": match.group(0)})
        return records

    def entry(window):
        return next(w for mon in session.tree()["monitors"] for ws in mon["workspaces"]
                    for col in ws["columns"] for w in col["windows"] if w["id"] == int(window))

    def focused(window):
        return any(mon.get("focused") == int(window) for mon in session.state()["monitors"])

    def settled(window):
        session.stable(windows)
        obj = entry(window)
        geometry = float_geometry(session, window)
        if any(obj[field] != list(geometry) for field in ("desired", "applied", "real")):
            raise RuntimeError(f"Settled/X11 geometry mismatch: {obj}, X11={geometry}")
        border = 0 if obj["fullscreen"] else BORDER
        outer = (*geometry[:2], geometry[2] + 2 * border, geometry[3] + 2 * border)
        radius = 0 if outer == screen else RADIUS
        def probe():
            records = traces(window)
            return records[-1] if records and not records[-1]["transition"] and (
                records[-1]["rect"], records[-1]["radius"]) == (outer, radius) else None
        trace = wait_for(f"settled submitted GL frame for {window}", probe) if gl else None
        return {"window": window, "geometry": geometry, "outer": outer, "radius": radius,
                "entry": obj, "trace": trace}

    def perform(stage, command, window, target, fullscreen):
        before = settled(window)
        offset = len(log.read_text())
        report = {"stage": stage, "command": command, "before": before, "target": target,
                  "log_offset": offset, "animation_required": gl, "failures": []}
        reports.append(report)
        report_path = evidence / f"{session.scene}-{stage}.json"
        try:
            session.action(command)
            wait_for(f"{stage}: action dispatched", lambda: focused(window)
                     and entry(window)["fullscreen"] == fullscreen)
            geometry = float_geometry(session, window)
            report["first_geometry_after_dispatch"] = geometry
            if geometry != target:
                raise RuntimeError(f"{stage}: X geometry not immediately at target: {geometry} != {target}")
            obj = entry(window)
            if any(obj[field] != list(target) for field in ("desired", "applied", "real")):
                raise RuntimeError(f"{stage}: settled-goal/X11 disagreement: {obj}")
            outer = (*target[:2], target[2] + (0 if fullscreen else 2 * BORDER),
                     target[3] + (0 if fullscreen else 2 * BORDER))
            radius = 0 if outer == screen else RADIUS
            if gl:
                def probe():
                    records = traces(window, offset)
                    report["frames"] = records
                    return records if records and not records[-1]["transition"] and (
                        records[-1]["rect"], records[-1]["radius"]) == (outer, radius) else None
                records = wait_for(f"{stage}: exact settled submitted endpoint", probe)
                intermediates = [r for r in records if r["transition"] and
                                 r["rect"] not in (before["outer"], outer) and 0 < r["radius"] < RADIUS]
                if not intermediates:
                    raise RuntimeError(f"{stage}: no submitted intermediate transform with fading radius")
                if records[0]["old"] != before["outer"]:
                    raise RuntimeError(f"{stage}: trace does not start at the settled source")
                radii = [before["radius"], *(r["radius"] for r in records)]
                direction = 1 if radius > before["radius"] else -1
                if any(not 0 <= r <= RADIUS for r in radii) or any(
                        direction * (b - a) < 0 for a, b in zip(radii, radii[1:])):
                    raise RuntimeError(f"{stage}: inconsistent radius progression: {radii}")
                if any(not r["transition"] and (r["rect"], r["radius"]) != (outer, radius)
                       for r in records if r["frame"] >= intermediates[0]["frame"]):
                    raise RuntimeError(f"{stage}: inactive transition before exact endpoint")
                report["intermediate_count"] = len(intermediates)
                report["sample"] = intermediates[len(intermediates) // 2]
                report["final"] = records[-1]
                print(f"  {stage}: {len(intermediates)} submitted intermediate frames; exact endpoint R{radius}", flush=True)
            report["after"] = settled(window)
            if report["after"]["geometry"] != target:
                raise RuntimeError(f"{stage}: endpoint changed after settling")
        except Exception as error:
            report["failures"].append(str(error))
            raise
        finally:
            report_path.write_text(json.dumps(report, indent=2) + "\n")

    if gl:
        wait_for("actual GL renderer and submitted frame (fallback is not accepted)", session.gl_active)
    elif session.gl_active() or "Backend:" in log.read_text():
        raise RuntimeError("fullscreen-transition must run without a compositor")
    a, b = windows
    wait_for("initial focus on B", lambda: focused(b))
    tiled = settled(b)["geometry"]
    perform("toggle-enter", "toggle_fullscreen", b, screen, True)
    perform("toggle-exit", "toggle_fullscreen", b, tiled, False)
    perform("navigation-setup", "toggle_fullscreen", b, screen, True)
    session.action("focus:left")
    wait_for("tiled A focused while B remains fullscreen", lambda: focused(a)
             and not entry(a)["fullscreen"] and entry(b)["fullscreen"])
    away = settled(b)
    if away["outer"] == screen or away["radius"] != RADIUS:
        raise RuntimeError(f"Navigation did not leave B's fullscreen presentation: {away}")
    reports.append({"stage": "navigation-away", "a": settled(a), "b": away})
    perform("navigation-enter", "focus:right", b, screen, True)
    if not gl and "[TRANSFORM]" in log.read_text():
        raise RuntimeError("Compositor-OFF scene unexpectedly emitted presentation transforms")
    return reports


def main():
    parser = argparse.ArgumentParser(description="Capture real Maverick windows in an isolated Xephyr server.")
    parser.add_argument("scene", choices=(*SCENES, "all"))
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
    parser.add_argument("--evidence", type=Path, default=Path("/tmp/kilo/showcase-evidence"),
                        help="Where JSON evidence and the compositor WM log are written")
    args = parser.parse_args()
    if sys.platform != "linux" or ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        parser.error("Linux PR_SET_CHILD_SUBREAPER is required for safe detached-child cleanup")
    required = ("Xephyr", "xdpyinfo", "xdotool", "xterm", "xsetroot", "import", "identify")
    if args.scene in (*FOCUS_SCENES, *FULLSCREEN_SCENES, "all"):
        required += ("convert",)
    missing = [program for program in required if not shutil.which(program)]
    if missing:
        parser.error("Missing prerequisites: " + ", ".join(missing))
    if not os.environ.get("DISPLAY"):
        parser.error("DISPLAY must point to a working host X11 display for Xephyr")
    if run(["xdpyinfo"], check=False).returncode:
        parser.error("Cannot connect to host DISPLAY; check DISPLAY and XAUTHORITY")
    binaries = args.bin_dir.resolve()
    for binary in ("maverick", "maverickctl"):
        if not os.access(binaries / binary, os.X_OK):
            parser.error(f"Missing {binaries / binary}; run cargo build --workspace")
    output = ROOT / "docs/screenshots"
    output.mkdir(parents=True, exist_ok=True)
    evidence = args.evidence.resolve()
    if args.scene in (*FOCUS_SCENES, *FULLSCREEN_SCENES, "all") and evidence != Path("/tmp/kilo/showcase-evidence"):
        parser.error("Regression scene evidence must use /tmp/kilo/showcase-evidence")
    evidence.mkdir(parents=True, exist_ok=True)
    lock = (evidence / "run.lock").open("w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        parser.error("Another showcase run is already active (evidence lock is held)")
    for scene in SCENES if args.scene == "all" else (args.scene,):
        session = Session(scene, binaries, output)
        try:
            if scene in FULLSCREEN_SCENES:
                (evidence / f"{scene}.json").write_text(json.dumps(
                    {"scene": scene, "checks": [], "failures": []}, indent=2) + "\n")
            session.start()
            windows = [session.terminal(1, "01 / Configuration", "config/config.toml")]
            pixel_checks = None
            if scene in TRANSITION_SCENES:
                windows.append(session.terminal(2, "B / Fullscreen presentation", "Cargo.toml"))
            elif scene not in FULLSCREEN_SCENES:
                windows.extend([session.terminal(2, "02 / Workspace", "Cargo.toml"),
                                session.terminal(3, "03 / X11 client", "tests/realwin.c")])
                session.action("focus:left")
                session.action("focus:left")
                session.action(f"grow_col:{-994 * SUPER}")
                session.action("focus:right")
                session.action("focus:right")
                session.stable(windows)
            if scene in TRANSITION_SCENES:
                transition_checks = fullscreen_transition(session, windows, evidence)
            elif scene in FULLSCREEN_SCENES:
                pixel_checks = fullscreen_new_window(session, windows, evidence)
            elif scene in FOCUS_SCENES:
                pixel_checks = rounded_focus(session, windows, evidence)
            elif scene == "navigation":
                windows.extend([session.terminal(4, "04 / Rendering", "maverick-gl/Cargo.toml"),
                                session.terminal(5, "05 / IPC", "maverick-sys/Cargo.toml")])
                session.action("focus:left")
                session.action("focus:left")
            elif scene in ("floating", "compositor"):
                session.action("toggle_float")
                session.stable(windows)
                run(["xdotool", "windowsize", windows[-1], str(680 * SUPER), str(510 * SUPER)], session.env)
                session.stable(windows)
                run(["xdotool", "windowmove", windows[-1], str(590 * SUPER), str(290 * SUPER)], session.env)
            elif scene == "fullscreen":
                session.action("toggle_fullscreen")
            elif scene == "floating-scroll":
                session.action("toggle_float")
                session.stable(windows)
                run(["xdotool", "windowsize", windows[-1], str(480 * SUPER), str(360 * SUPER)], session.env)
                session.stable(windows)
                run(["xdotool", "windowmove", windows[-1], str(480 * SUPER), str(270 * SUPER)], session.env)
                # Record the float's screen geometry, scroll the ribbon two
                # columns, record again, then scroll back: the float must not
                # have moved (it is isolated from the ribbon camera).
                before = float_geometry(session, windows[-1])
                session.action("focus:left")
                session.action("focus:left")
                session.stable(windows)
                after = float_geometry(session, windows[-1])
                if before is None or before != after:
                    raise RuntimeError(
                        f"floating window moved during ribbon scroll: {before} -> {after}")
                print("  floating isolation verified during scroll", flush=True)
            elif scene == "fullscreen-decoration":
                session.action("toggle_fullscreen")
                session.stable(windows)
                session.action("toggle_fullscreen")
                session.stable(windows)
            else:
                session.action("focus:left")
            if scene in GL_SCENES:
                wait_for("actual GL renderer and submitted frame (fallback is not accepted)", session.gl_active)
            geometry = session.capture(windows)
            record = {"scene": scene, "display": session.env["DISPLAY"],
                      "wm_pid": session.wm.pid, "geometry": geometry,
                      "state": session.state(), "tree": session.tree(),
                      "gl_active": session.gl_active(),
                      "sha256": hashlib.sha256((output / f"{scene}.png").read_bytes()).hexdigest(),
                      "render": {"internal": list(INTERNAL), "final": list(SIZE),
                                 "scale": SUPER,
                                 "downsample": "Lanczos", "colorspace": "sRGB",
                                 "stripped": True,
                                 "font_face": FONT_FACE, "font_file": FONT_FILE,
                                 "font_size": FONT_SIZE}}
            if scene in TRANSITION_SCENES:
                record["transition_checks"] = transition_checks
            if pixel_checks is not None:
                record["pixel_checks"] = pixel_checks
            if scene in FULLSCREEN_SCENES:
                record["failures"] = []
            (evidence / f"{scene}.json").write_text(json.dumps(record, indent=2) + "\n")
            if scene in GL_SCENES:
                (evidence / f"{scene}-wm.log").write_text((session.path / "wm.log").read_text())
        except Exception as error:
            if scene in FULLSCREEN_SCENES:
                report = evidence / f"{scene}.json"
                record = json.loads(report.read_text()) if report.exists() else {"scene": scene}
                record["failures"] = [str(error)]
                report.write_text(json.dumps(record, indent=2) + "\n")
            for name in ("xephyr", "wm"):
                log = session.path / f"{name}.log"
                if log.exists():
                    print(f"{name} log:\n{log.read_text()[-6000:]}", file=sys.stderr)
            raise
        finally:
            if scene in (*FULLSCREEN_SCENES, *TRANSITION_SCENES) and (session.path / "wm.log").exists():
                (evidence / f"{scene}-wm.log").write_text((session.path / "wm.log").read_text())
            session.close()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--client":
        client(*sys.argv[2:])
    else:
        def interrupted(signum, frame):
            raise KeyboardInterrupt(f"signal {signum}")
        signal.signal(signal.SIGTERM, interrupted)
        try:
            main()
        except (RuntimeError, OSError, subprocess.SubprocessError, KeyboardInterrupt) as error:
            print(f"showcase: {error}", file=sys.stderr)
            sys.exit(1)
