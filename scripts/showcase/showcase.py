#!/usr/bin/env python3
import argparse
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
SCENES = ("tiling", "navigation", "floating", "fullscreen", "compositor")
SIZE = (1440, 900)


def client(title, source):
    lines = (ROOT / source).read_text().splitlines()
    def draw(*_):
        columns, rows = shutil.get_terminal_size()
        print("\033[2J\033[H\033[?25l\033[1;36m" + title + "\033[0m\n")
        print("MAVERICK / LIVE X11 CLIENT\n")
        print(source + "\n" + "─" * min(42, columns - 1))
        for index, line in enumerate(lines[:max(1, rows - 9)], 1):
            print(f"\033[90m{index:3} │\033[0m {line[:max(1, columns - 7)]}")
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
                                     f"{SIZE[0]}x{SIZE[1]}", "-nolisten", "tcp", "-ac",
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
border_width = 1
corner_radius = {18 if self.scene == "compositor" else 0}
gaps_inner = 4
gaps_outer = 8
column_width = 0.31
n_tags = 3
focus_mouse = false
warp_cursor = false
[colors]
normal = 0x45475a
focused = 0x89b4fa
[animations]
enabled = false
[compositor]
enabled = {str(self.scene == "compositor").lower()}
backend = "opengl"
fullscreen_bypass = false
[autostart]
commands = [["/usr/bin/true"]]
[[rules]]
instance = "showcase3"
opacity = {0.78 if self.scene == "compositor" else 1.0}
''')
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
        self.spawn(["xterm", "-name", name, "-class", "Showcase", "-title", title,
                    "-fa", "DejaVu Sans Mono", "-fs", "11", "-bg", "#1e1e2e",
                    "-fg", "#cdd6f4", "-cr", "#1e1e2e", "+sb", "-b", "18",
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
        path = self.output / f"{self.scene}.png"
        run(["import", "-display", self.env["DISPLAY"], "-window", "root", str(path)], self.env)
        dimensions = run(["identify", "-format", "%wx%h", str(path)]).stdout
        if dimensions != f"{SIZE[0]}x{SIZE[1]}":
            raise RuntimeError(f"Unexpected screenshot dimensions: {dimensions}")
        print(f"  captured {path.relative_to(ROOT)} ({dimensions})", flush=True)
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
    evidence.mkdir(parents=True, exist_ok=True)
    lock = (evidence / "run.lock").open("w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        parser.error("Another showcase run is already active (evidence lock is held)")
    for scene in SCENES if args.scene == "all" else (args.scene,):
        session = Session(scene, binaries, output)
        try:
            session.start()
            windows = [session.terminal(1, "01 / Configuration", "config/config.toml"),
                       session.terminal(2, "02 / Workspace", "Cargo.toml"),
                       session.terminal(3, "03 / X11 client", "tests/realwin.c")]
            session.action("focus:left")
            session.action("focus:left")
            session.action("grow_col:-994")
            session.action("focus:right")
            session.action("focus:right")
            session.stable(windows)
            if scene == "navigation":
                windows.extend([session.terminal(4, "04 / Rendering", "maverick-gl/Cargo.toml"),
                                session.terminal(5, "05 / IPC", "maverick-sys/Cargo.toml")])
                session.action("focus:left")
                session.action("focus:left")
            elif scene in ("floating", "compositor"):
                session.action("toggle_float")
                session.stable(windows)
                run(["xdotool", "windowsize", windows[-1], "680", "510"], session.env)
                session.stable(windows)
                run(["xdotool", "windowmove", windows[-1], "590", "290"], session.env)
            elif scene == "fullscreen":
                session.action("toggle_fullscreen")
            else:
                session.action("focus:left")
            if scene == "compositor":
                wait_for("actual GL renderer and submitted frame (fallback is not accepted)", session.gl_active)
            geometry = session.capture(windows)
            record = {"scene": scene, "display": session.env["DISPLAY"],
                      "wm_pid": session.wm.pid, "geometry": geometry,
                      "state": session.state(), "tree": session.tree(),
                      "gl_active": session.gl_active(),
                      "sha256": hashlib.sha256((output / f"{scene}.png").read_bytes()).hexdigest()}
            (evidence / f"{scene}.json").write_text(json.dumps(record, indent=2) + "\n")
            if scene == "compositor":
                (evidence / "compositor-wm.log").write_text((session.path / "wm.log").read_text())
        except Exception:
            for name in ("xephyr", "wm"):
                log = session.path / f"{name}.log"
                if log.exists():
                    print(f"{name} log:\n{log.read_text()[-6000:]}", file=sys.stderr)
            raise
        finally:
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
