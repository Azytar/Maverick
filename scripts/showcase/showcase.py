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
# Phase-3 real-application scenes (same Session/capture pipeline, real clients).
# Kept separate from SCENES so the supersample experiment driver (which imports
# SCENES) keeps its exact scene list; run.sh/main accepts both.
REAL_SCENES = ("real-desktop", "real-scroll-a", "real-scroll-b", "real-focus",
               "real-floating", "real-compositor")
REAL_GL_SCENES = ("real-compositor",)
REAL_DOC = Path(__file__).resolve().parent / "maverick-doc.html"
# Alacritty runs at the 2x INTERNAL framebuffer; 16pt keeps an 878px column at
# a readable ~80-cell grid after the Lanczos downsample to 1440x900.
REAL_ALACRITTY_FONT_SIZE = 16
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
# Maverick Showcase palette (deterministic, offline).
# Very dark neutral blue/charcoal foundation; terminal surface one step up;
# single restrained electric-cyan/blue accent. Tuned at 2880x1800, verified
# after Lanczos to 1440x900 and at ~720px README width.
ROOT_BG = "#0D1118"       # root gradient top
ROOT_BG_BOTTOM = "#0A0D12"  # root gradient bottom
SURFACE = "#161B26"       # terminal background (distinguishable, same family)
SURFACE_ALT = "#3A2B45"   # secondary client tint (fullscreen B, compositor float)
SURFACE_ALT2 = "#1E3A40"  # tertiary client tint (fullscreen-new-window C)
FG = "#E8EAF0"            # primary text (off-white, not max white)
DIM = "#9AA3B2"           # secondary text
ACCENT = "#4CC3FF"        # single Maverick accent (electric cyan/blue)
FOCUSED, NORMAL = (76, 195, 255), (58, 67, 86)
CONTENT = (22, 27, 38)
FRAME_BG = "#0B0D12"      # external presentation chrome only
# Presentation frame (external chrome only; Maverick pixels inside untouched).
# No baked caption: README already captions each image below it.
FRAME_MARGIN = 48
FRAME_PAD = 40
FRAME_RADIUS = 24
FRAMED = (SIZE[0] + 2 * (FRAME_MARGIN + FRAME_PAD),
          SIZE[1] + 2 * (FRAME_MARGIN + FRAME_PAD))
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


def _rgb(hexcolor):
    hexcolor = hexcolor.lstrip("#")
    return tuple(int(hexcolor[i:i + 2], 16) for i in (0, 2, 4))


def _fg_seq(hexcolor):
    r, g, b = _rgb(hexcolor)
    return f"\033[38;2;{r};{g};{b}m"


_BG_CACHE = {}


def write_showcase_background(path, width, height):
    """Deterministic showcase background: vertical gradient #0D1118 -> #0A0D12
    with one restrained diagonal light band and a faint cyan depth glow.
    Pure stdlib (zlib/crc PNG writer), no RNG, no external files.

    The INTERNAL size is fixed, so the bytes are identical for every scene:
    render once per process and reuse them (a full 2880x1800 per-pixel pass
    costs ~6s; without the cache an `all` run would pay it ~15 times)."""
    import struct
    import zlib
    key = (width, height)
    png = _BG_CACHE.get(key)
    if png is None:
        top = (13, 17, 24)
        bottom = (10, 13, 18)
        glow_cx, glow_cy, glow_r = 0.78 * width, 0.78 * height, 350 * SUPER
        rows = []
        for y in range(height):
            t = y / max(1, height - 1)
            r0 = round(top[0] + (bottom[0] - top[0]) * t)
            g0 = round(top[1] + (bottom[1] - top[1]) * t)
            b0 = round(top[2] + (bottom[2] - top[2]) * t)
            line = bytearray(width * 3 + 1)
            line[0] = 0  # filter 0
            dy = y - glow_cy
            for x in range(width):
                cy = height * 0.30 - x * 0.12
                d = abs(y - cy)
                add = round(7 * max(0.0, 1.0 - d / (130 * SUPER)))
                dx = x - glow_cx
                glow = max(0.0, 1.0 - (dx * dx + dy * dy) ** 0.5 / glow_r)
                glow *= glow
                o = 1 + x * 3
                line[o] = min(255, r0 + add)
                line[o + 1] = min(255, g0 + add + round(5 * glow))
                line[o + 2] = min(255, b0 + add + round(11 * glow))
            rows.append(bytes(line))
        raw = b"".join(rows)
        def chunk(tag, data):
            c = struct.pack(">I", len(data)) + tag + data
            return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
               + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))
        _BG_CACHE[key] = png
    Path(path).write_bytes(png)


def client(title, source):
    lines = (ROOT / source).read_text().splitlines()
    def draw(*_):
        columns, rows = shutil.get_terminal_size()
        width = max(10, columns - 7)
        budget = max(1, rows - 9)
        print("\033[2J\033[H\033[?25l" + _fg_seq(ACCENT) + title + "\033[0m\n")
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
                print(f"{_fg_seq(DIM)}    │\033[0m {part}")
            else:
                print(f"{_fg_seq(DIM)}{index:3} │\033[0m {part}")
        if len(shown) < len(lines):
            print(f"{_fg_seq(DIM)}    │ … +{len(lines) - len(shown)} lines\033[0m")
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


def frame_shot(raw, final):
    """Wrap the authentic 1440x900 capture in external presentation chrome.

    Maverick pixels inside are never altered (no rounding/cropping of the
    shot itself): a FRAME_BG margin block with rounded outer corners and a
    soft drop shadow is composited around it on transparency. No caption is
    baked in; README already captions each image.
    """
    bordered_w = SIZE[0] + 2 * FRAME_MARGIN
    bordered_h = SIZE[1] + 2 * FRAME_MARGIN
    # Shot sits at (framed_x, framed_y) inside the explicit canvas; the blur
    # (sigma 12, +12 downward shift) never reaches the canvas edge, so the
    # output dimensions are exactly FRAMED every run.
    framed_x, framed_y = 44, 32
    bordered = str(final) + ".bordered.png"
    mask = str(final) + ".mask.png"
    rounded = str(final) + ".rounded.png"
    try:
        run(["magick", str(raw), "-bordercolor", FRAME_BG, "-border",
             f"{FRAME_MARGIN}x{FRAME_MARGIN}", "-colorspace", "sRGB",
             "+repage", bordered])
        run(["magick", "-size", f"{bordered_w}x{bordered_h}", "xc:none",
             "-fill", "white", "-draw",
             f"roundrectangle 0,0 {bordered_w - 1},{bordered_h - 1} "
             f"{FRAME_RADIUS},{FRAME_RADIUS}",
             mask])
        run(["magick", bordered, mask, "-alpha", "off",
             "-compose", "CopyOpacity", "-composite", "-strip", rounded])
        run(["magick", "-size", f"{FRAMED[0]}x{FRAMED[1]}", "xc:none",
             "(", rounded, "-alpha", "extract", "-fill", "black",
             "-colorize", "100", "-blur", "0x12", ")",
             "-geometry", f"+{framed_x}+{framed_y + 12}",
             "-compose", "Over", "-composite",
             rounded, "-geometry", f"+{framed_x}+{framed_y}",
             "-compose", "Over", "-composite",
             "-colorspace", "sRGB", "-strip", str(final)])
    finally:
        for temp in (bordered, mask, rounded):
            try:
                Path(temp).unlink()
            except FileNotFoundError:
                pass


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
        # Real GL clients (Alacritty/winit, Kitty/glfw, Firefox) prefer Wayland
        # when the host session offers it (host here is Wayland+Xwayland).
        # Xephyr is X11-only, so force the X11 backends for every scene; xterm
        # ignores these variables, real apps need them to map at all.
        self.env.pop("WAYLAND_DISPLAY", None)
        self.env.pop("NIRI_SOCKET", None)
        self.env["WINIT_UNIX_BACKEND"] = "x11"
        self.env["GDK_BACKEND"] = "x11"
        self.env["KITTY_DISABLE_WAYLAND"] = "1"
        self.env["MOZ_ENABLE_WAYLAND"] = "0"
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
        # Procedural deterministic background (stdlib PNG, no external file).
        # Installed as Maverick's native wallpaper so BOTH paths share it:
        # plain-X11 scenes via rootwall, compositor scenes via the GL painter.
        # A flat xsetroot color remains underneath for the pre-wallpaper moment.
        bg = self.path / "showcase-bg.png"
        write_showcase_background(bg, *INTERNAL)
        config = self.path / "config.toml"
        rounded_here = self.scene in ("compositor", "rounded", "tiled-spacing", "floating-scroll",
                                      "fullscreen-decoration", *FOCUS_SCENES, *FULLSCREEN_SCENES,
                                      *TRANSITION_SCENES, *REAL_GL_SCENES)
        config.write_text(f'''[general]
border_width = {BORDER}
corner_radius = {RADIUS if rounded_here else 0}
gaps_inner = {(6 if self.scene == "tiled-spacing" else 4) * SUPER}
gaps_outer = {(10 if self.scene == "tiled-spacing" else 8) * SUPER}
column_width = 0.31
n_tags = 3
focus_mouse = false
warp_cursor = false
[colors]
normal = 0x3a4356
focused = 0x4cc3ff
[animations]
enabled = {str(self.scene == "fullscreen-transition-gl").lower()}
[compositor]
enabled = {str(self.scene in (*GL_SCENES, *REAL_GL_SCENES)).lower()}
backend = "opengl"
fullscreen_bypass = false
[wallpaper]
path = "{bg}"
mode = "fill"
[autostart]
commands = [["/usr/bin/true"]]
[[rules]]
instance = "showcase3"
opacity = {0.78 if self.scene == "compositor" else 1.0}
''')
        if self.scene in REAL_GL_SCENES:
            # Same 0.78 opacity the xterm compositor scene uses, but matched to
            # the real floating client so GL transparency reads on real pixels.
            with config.open("a") as stream:
                stream.write('\n[[rules]]\ninstance = "realfloat"\nopacity = 0.78\n')
        if self.scene in (*FOCUS_SCENES, *FULLSCREEN_SCENES):
            with config.open("a") as stream:
                stream.write('\n[[rules]]\ninstance = "showcase6"\nfloat = true\n')
        run([str(self.binaries / "maverick"), "--check-config", str(config)], self.env)
        self.wm = self.spawn([str(self.binaries / "maverick"), "--config", str(config),
                              "--name", "showcase"], "wm")
        wait_for("Maverick IPC startup", lambda: self.state().get("monitors"))
        run(["xsetroot", "-solid", ROOT_BG], self.env)
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
        if self.scene in FULLSCREEN_SCENES:
            background = ({2: SURFACE_ALT, 3: SURFACE_ALT2}.get(number, SURFACE))
        elif self.scene == "compositor" and number == 3:
            # Tinted float over same-hue tiles: the rule-set 0.78 opacity
            # blends two distinguishable surfaces, so the real GL
            # transparency reads without touching compositor semantics.
            background = SURFACE_ALT
        else:
            background = SURFACE
        self.spawn(["xterm", "-name", name, "-class", "Showcase", "-title", title,
                    "-fa", FONT_FACE, "-fs", str(FONT_SIZE), "-bg", background,
                    "-fg", FG, "-cr", background, "+sb", "-b", str(18 * SUPER),
                    "-geometry", "72x36", "-e", sys.executable, str(Path(__file__).resolve()),
                    "--client", title, source], name)
        return wait_for(f"client {number}", lambda: run(["xdotool", "search", "--onlyvisible",
                         "--classname", "^" + name + "$"], self.env).stdout.strip().splitlines())[0]

    # -- Phase-3 real-application harness (same WM, same pipeline) --
    def wait_tree_for(self, description, predicate, timeout=60):
        """Wait until `predicate(tree)` returns a truthy window id."""
        def probe():
            try:
                tree = self.tree()
            except RuntimeError:
                return None
            for monitor in tree.get("monitors", []):
                for workspace in monitor.get("workspaces", []):
                    for column in workspace.get("columns", []):
                        for window in column.get("windows", []):
                            if predicate(window):
                                return str(window["id"])
                    for window in workspace.get("floats", []):
                        if predicate(window):
                            return str(window["id"])
            return None
        return wait_for(description, probe, timeout=timeout)

    def real_alacritty_config(self):
        """Deterministic Alacritty config: Fira Code, dark Maverick surface."""
        path = self.path / "real-alacritty.toml"
        path.write_text(f'''[font]
size = {REAL_ALACRITTY_FONT_SIZE}
normal = {{ family = "{FONT_FACE}" }}
[colors.primary]
background = "{SURFACE}"
foreground = "{FG}"
''')
        return path

    def real_alacritty(self, description, title, command, instance="Alacritty"):
        """Launch real Alacritty (authentic terminal chrome, not xterm).

        Each caller passes a distinct WM_CLASS instance so concurrent
        Alacritty windows are distinguishable via the Maverick tree."""
        config = self.real_alacritty_config()
        label = "real-" + re.sub(r"[^a-z0-9]+", "-", description.lower()).strip("-")
        self.spawn(["alacritty", "--config-file", str(config), "--title", title,
                    "--class", f"Alacritty,{instance}",
                    "-e", *command], label)
        return self.wait_tree_for(
            f"alacritty {description}",
            lambda w, inst=instance: w.get("instance") == inst)

    def real_firefox_profile(self):
        """Isolated Firefox profile pinned to offline file:// operation."""
        profile = self.path / "real-firefox-profile"
        profile.mkdir(mode=0o700, exist_ok=True)
        (profile / "prefs.js").write_text(
            'user_pref("browser.shell.checkDefaultBrowser", false);\n'
            'user_pref("browser.aboutwelcome.enabled", false);\n'
            'user_pref("browser.startup.homepage", "about:blank");\n'
            'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
            'user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);\n'
            'user_pref("browser.rights.3.shown", true);\n')
        return profile

    def real_firefox(self, description, url):
        """Launch real Firefox on a local file:// URL (no network)."""
        if not url.startswith("file://"):
            raise RuntimeError("Firefox showcase must use a local file:// URL")
        profile = self.real_firefox_profile()
        label = "real-" + re.sub(r"[^a-z0-9]+", "-", description.lower()).strip("-")
        self.spawn(["firefox", "--no-remote", "--profile", str(profile), url], label)
        return self.wait_tree_for(
            f"firefox {description}",
            lambda w: (w.get("class") or "").lower() == "firefox", timeout=90)

    def real_zed_settings(self):
        """Isolated Zed config preferring a dark theme (best effort)."""
        zed_conf = Path(self.env["XDG_CONFIG_HOME"]) / "zed"
        zed_conf.mkdir(parents=True, exist_ok=True)
        (zed_conf / "settings.json").write_text(
            '{"theme":{"mode":"dark","dark":"One Dark","light":"One Light"},'
            '"vim_mode":false}\n')
        data = self.path / "real-zed-data"
        data.mkdir(mode=0o700, exist_ok=True)
        return data

    def real_zed(self, description, target):
        """Launch real Zed on a local file; dismiss trust dialog via Enter."""
        data = self.real_zed_settings()
        label = "real-" + re.sub(r"[^a-z0-9]+", "-", description.lower()).strip("-")
        self.spawn(["zeditor", "--user-data-dir", str(data), str(target)], label)
        window = self.wait_tree_for(
            f"zed {description}",
            lambda w: "zed" in (w.get("class") or "").lower(), timeout=90)
        # Fresh user-data-dir always shows the Restricted Mode modal for an
        # unrecognized project. A real Enter keypress ("Trust and Continue")
        # is authentic WM input, not post-processing.
        time.sleep(2.0)
        try:
            run(["xdotool", "windowactivate", "--sync", window], self.env)
            run(["xdotool", "key", "Return"], self.env)
        except RuntimeError:
            pass
        time.sleep(2.0)
        return window

    def real_tool(self, description, argv, match, timeout=30):
        """Launch a real desktop tool (file manager, mixer, monitor)."""
        label = "real-" + re.sub(r"[^a-z0-9]+", "-", description.lower()).strip("-")
        self.spawn(argv, label)
        def predicate(window):
            haystack = " ".join(str(window.get(k) or "") for k in ("class", "instance", "title"))
            return match.lower() in haystack.lower()
        return self.wait_tree_for(f"tool {description}", predicate, timeout=timeout)

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
        # framed presentation asset lands in docs/screenshots.
        hires = self.path / f"{self.scene}-hires.png"
        raw = self.path / f"{self.scene}-raw.png"
        final = self.output / f"{self.scene}.png"
        run(["import", "-display", self.env["DISPLAY"], "-window", "root", str(hires)], self.env)
        dimensions = run(["identify", "-format", "%wx%h", str(hires)]).stdout
        if dimensions != f"{INTERNAL[0]}x{INTERNAL[1]}":
            raise RuntimeError(f"Unexpected hires screenshot dimensions: {dimensions}")
        # Proven Phase-1 path: Lanczos downsample to logical size, sRGB, strip.
        run(["magick", str(hires), "-filter", "Lanczos", "-resize",
             f"{SIZE[0]}x{SIZE[1]}!", "-colorspace", "sRGB", "-strip", str(raw)])
        dimensions = run(["identify", "-format", "%wx%h", str(raw)]).stdout
        if dimensions != f"{SIZE[0]}x{SIZE[1]}":
            raise RuntimeError(f"Unexpected raw screenshot dimensions: {dimensions}")
        frame_shot(raw, final)
        dimensions = run(["identify", "-format", "%wx%h", str(final)]).stdout
        if dimensions != f"{FRAMED[0]}x{FRAMED[1]}":
            raise RuntimeError(f"Unexpected framed dimensions: {dimensions}")
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
    root = (0, 0, 0) if session.scene in GL_SCENES else (13, 17, 24)
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


def run_real_desktop(session):
    """Everyday developer desktop: Neovim + Firefox + Zed + shell.

    Four real applications in Maverick columns (not floating boxes). The
    ribbon must overflow the viewport so the capture reads as a viewport
    onto a larger column sequence, not a fitted mosaic."""
    windows = []
    windows.append(session.real_alacritty(
        "neovim", "Neovim / layout.rs",
        ["nvim", "--clean", "-c", "set number", "--",
         str(ROOT / "src/core/layout.rs")],
        instance="realnvim"))
    windows.append(session.real_firefox("docs", f"file://{REAL_DOC}"))
    windows.append(session.real_zed("repo", ROOT / "Cargo.toml"))
    root = str(ROOT)
    windows.append(session.real_alacritty(
        "shell", "Shell / git status",
        ["bash", "--noprofile", "--norc", "-c",
         f'export PS1="mav$ "; cd {root} && git status --short --branch;'
         ' exec bash --noprofile --norc -i'],
        instance="realshell"))
    session.stable(windows)
    tree = session.tree()
    workspace = tree["monitors"][0]["workspaces"][0]
    scroll = workspace["scroll"]
    ncols = len(workspace["columns"])
    print(f"  real-desktop: {ncols} columns, scroll={scroll}", flush=True)
    if ncols < 4:
        raise RuntimeError(f"real-desktop expected 4 columns, got {ncols}")
    if scroll <= 0:
        raise RuntimeError(f"real-desktop did not overflow the viewport: scroll={scroll}")
    return windows


def main():
    parser = argparse.ArgumentParser(description="Capture real Maverick windows in an isolated Xephyr server.")
    parser.add_argument("scene", choices=(*SCENES, "real-desktop", "all"))
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
    parser.add_argument("--evidence", type=Path, default=Path("/tmp/kilo/showcase-evidence"),
                        help="Where JSON evidence and the compositor WM log are written")
    args = parser.parse_args()
    if sys.platform != "linux" or ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        parser.error("Linux PR_SET_CHILD_SUBREAPER is required for safe detached-child cleanup")
    required = ("Xephyr", "xdpyinfo", "xdotool", "xterm", "xsetroot", "import", "identify")
    if args.scene in (*FOCUS_SCENES, *FULLSCREEN_SCENES, "all"):
        required += ("convert",)
    if args.scene in ("real-desktop",):
        required += ("alacritty", "firefox", "zeditor", "nvim")
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
            if scene == "real-desktop":
                windows = run_real_desktop(session)
                pixel_checks = None
                transition_checks = None
                if scene in (*GL_SCENES, *REAL_GL_SCENES):
                    wait_for("actual GL renderer and submitted frame (fallback is not accepted)", session.gl_active)
                geometry = session.capture(windows)
                record = {"scene": scene, "display": session.env["DISPLAY"],
                          "wm_pid": session.wm.pid, "geometry": geometry,
                          "state": session.state(), "tree": session.tree(),
                          "gl_active": session.gl_active(),
                          "applications": ["alacritty+nvim", "firefox", "zed", "alacritty+shell"],
                          "sha256": hashlib.sha256((output / f"{scene}.png").read_bytes()).hexdigest(),
                          "render": {"internal": list(INTERNAL), "final": list(SIZE),
                                     "framed": list(FRAMED), "scale": SUPER,
                                     "downsample": "Lanczos", "colorspace": "sRGB",
                                     "stripped": True,
                                     "font_face": FONT_FACE, "font_file": FONT_FILE,
                                     "font_size": FONT_SIZE,
                                     "frame": {"margin": FRAME_MARGIN, "pad": FRAME_PAD,
                                               "radius": FRAME_RADIUS,
                                               "exterior": FRAME_BG}}}
                (evidence / f"{scene}.json").write_text(json.dumps(record, indent=2) + "\n")
                continue
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
                                 "framed": list(FRAMED), "scale": SUPER,
                                 "downsample": "Lanczos", "colorspace": "sRGB",
                                 "stripped": True,
                                 "font_face": FONT_FACE, "font_file": FONT_FILE,
                                 "font_size": FONT_SIZE,
                                 "frame": {"margin": FRAME_MARGIN, "pad": FRAME_PAD,
                                           "radius": FRAME_RADIUS,
                                           "exterior": FRAME_BG}}}
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
