#!/usr/bin/env python3
"""Isolated X11 session and evidence helpers for the Maverick showcase.

The showcase owns every process it starts.  It creates a private Xephyr
session, a private XDG/runtime tree, and a private Maverick configuration;
none of the clients are allowed to inherit the user's configuration.
"""
from __future__ import annotations

import ctypes
import errno
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterable
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SIZE = (1920, 1080)
DEFAULT_EVIDENCE = Path("/tmp/opencode/mav-showcase-evidence")
CHORD_SPACING_SECONDS = 0.08
PR_SET_CHILD_SUBREAPER = 36


class ShowcaseError(RuntimeError):
    """A failure that should stop the showcase instead of being hidden."""


def run(
    argv: Iterable[str],
    *,
    env: dict[str, str] | None = None,
    check: bool = True,
    timeout: float = 30,
) -> subprocess.CompletedProcess[str]:
    """Run a local command with bounded output and a useful failure message."""
    command = [str(arg) for arg in argv]
    try:
        result = subprocess.run(
            command,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ShowcaseError(f"command failed to complete: {command!r}: {error}") from error
    if check and result.returncode:
        detail = (result.stderr or result.stdout).strip()
        raise ShowcaseError(
            f"command failed ({result.returncode}): {command!r}"
            + (f"\n{detail}" if detail else "")
        )
    return result


def wait_for(
    description: str,
    probe: Callable[[], Any],
    *,
    timeout: float = 30,
    interval: float = 0.15,
) -> Any:
    """Wait for a positive probe without hiding the last observed error."""
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            value = probe()
            if value:
                return value
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
            last_error = error
        time.sleep(interval)
    detail = f"; last error: {last_error}" if last_error else ""
    raise ShowcaseError(f"timed out waiting for {description}{detail}")


def parse_size(value: str) -> tuple[int, int]:
    try:
        width_text, height_text = value.lower().split("x", 1)
        width, height = int(width_text), int(height_text)
    except (ValueError, TypeError) as error:
        raise ShowcaseError(f"invalid size {value!r}; expected WIDTHxHEIGHT") from error
    if width < 640 or height < 480:
        raise ShowcaseError("showcase size must be at least 640x480")
    return width, height


def _child_pids(pid: int) -> list[int]:
    try:
        raw = Path(f"/proc/{pid}/task/{pid}/children").read_text()
    except (FileNotFoundError, PermissionError):
        return []
    return [int(item) for item in raw.split()]


def descendants(pid: int | None = None) -> list[int]:
    """Return the harness-owned process tree in child-before-parent order."""
    root = os.getpid() if pid is None else pid
    found: list[int] = []

    def visit(current: int) -> None:
        for child in _child_pids(current):
            if child not in found:
                visit(child)
                found.append(child)

    visit(root)
    return found


class Session:
    """An isolated Xephyr + Maverick process group and its evidence writer."""

    def __init__(self, binaries: Path, output: Path, evidence: Path, size: tuple[int, int]):
        self.binaries = binaries
        self.output = output
        self.evidence = evidence
        self.size = size
        self.output.mkdir(parents=True, exist_ok=True)
        self.evidence.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="mav-showcase-", dir="/tmp/opencode")
        self.path = Path(self.temp.name)
        # Unix socket paths are limited by SUN_LEN. Keep XDG_RUNTIME_DIR short
        # even though the rest of the private state has descriptive names.
        self.runtime = Path(tempfile.mkdtemp(prefix="mav-rt-", dir="/tmp/opencode"))
        self.processes: list[subprocess.Popen[Any]] = []
        self.logs: list[Any] = []
        self.log_paths: list[Path] = []
        self.windows: list[str] = []
        self.compositor_selection_owner: str | None = None
        self.wm: subprocess.Popen[Any] | None = None
        self.xephyr: subprocess.Popen[Any] | None = None
        self.env = self._private_environment()
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"):
            directory = self.runtime if key == "XDG_RUNTIME_DIR" else self.path / key.lower()
            directory.mkdir(mode=0o700, exist_ok=key == "XDG_RUNTIME_DIR")
            self.env[key] = str(directory)
        self._lock = (self.evidence / "run.lock").open("w")
        try:
            fcntl.flock(self._lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            self._lock.close()
            self.temp.cleanup()
            shutil.rmtree(self.runtime)
            if error.errno in (errno.EACCES, errno.EAGAIN):
                raise ShowcaseError("another showcase run holds the evidence lock") from error
            raise

    @staticmethod
    def _private_environment() -> dict[str, str]:
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("MAVERICK_", "MAV_"))
        }
        env["LC_ALL"] = "C.UTF-8"
        env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("NIRI_SOCKET", None)
        env["WINIT_UNIX_BACKEND"] = "x11"
        env["GDK_BACKEND"] = "x11"
        env["KITTY_DISABLE_WAYLAND"] = "1"
        env["MOZ_ENABLE_WAYLAND"] = "0"
        if os.environ.get("XAUTHORITY"):
            env["XAUTHORITY"] = os.environ["XAUTHORITY"]
        elif (Path.home() / ".Xauthority").exists():
            env["XAUTHORITY"] = str(Path.home() / ".Xauthority")
        return env

    def spawn(self, argv: Iterable[str], label: str, *, env: dict[str, str] | None = None, **kwargs: Any) -> subprocess.Popen[Any]:
        command = [str(arg) for arg in argv]
        log_path = self.path / f"{label}.log"
        log = log_path.open("w", encoding="utf-8")
        self.logs.append(log)
        self.log_paths.append(log_path)
        process = subprocess.Popen(
            command,
            env=env or self.env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            **kwargs,
        )
        self.processes.append(process)
        return process

    def abandon(self, process: subprocess.Popen[Any]) -> None:
        """Stop an optional client that failed to map without touching the WM."""
        owned: list[int] = []

        def visit(pid: int) -> None:
            for child in _child_pids(pid):
                visit(child)
            owned.append(pid)

        visit(process.pid)
        for pid in owned:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            for pid in owned:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass

    @staticmethod
    def _free_display() -> str:
        for number in range(90, 200):
            lock = Path(f"/tmp/.X{number}-lock")
            socket = Path(f"/tmp/.X11-unix/X{number}")
            if not lock.exists() and not socket.exists():
                return f":{number}"
        raise ShowcaseError("no free nested X11 display in :90..:199")

    def _write_private_config(self) -> Path:
        config = self.path / "config.toml"
        config.write_text(
            """[general]
border_width = 1
corner_radius = 10
gaps_inner = 8
gaps_outer = 10
column_width = 0.29
n_tags = 3
focus_mouse = false
warp_cursor = false
accordion_boost = 0.0
[colors]
normal = 0x3b4650
focused = 0x78b6c4
[animations]
enabled = true
stiffness = 220.0
damping = 30.0
[compositor]
enabled = true
backend = "opengl"
fullscreen_bypass = false
""",
            encoding="utf-8",
        )
        return config

    def start(self) -> None:
        if sys.platform != "linux":
            raise ShowcaseError("the showcase is Linux/X11-only")
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
            raise ShowcaseError("Linux PR_SET_CHILD_SUBREAPER is required for safe cleanup")
        required = ("Xephyr", "xdpyinfo", "xdotool", "xprop", "xsetroot", "import", "identify")
        missing = [program for program in required if shutil.which(program) is None]
        if missing:
            raise ShowcaseError("missing showcase prerequisites: " + ", ".join(missing))
        for binary in ("maverick", "maverickctl"):
            if not os.access(self.binaries / binary, os.X_OK):
                raise ShowcaseError(f"missing executable {self.binaries / binary}; build Maverick first")
        if not os.environ.get("DISPLAY"):
            raise ShowcaseError("DISPLAY must point to a reachable host X11 display")
        if run(["xdpyinfo"], env=self.env, check=False).returncode:
            raise ShowcaseError("cannot connect to host DISPLAY; check DISPLAY/XAUTHORITY")

        host_display = self.env["DISPLAY"]
        display = self._free_display()
        xephyr_env = dict(self.env)
        xephyr_env["DISPLAY"] = host_display
        self.xephyr = self.spawn(
            [
                "Xephyr",
                display,
                "-screen",
                f"{self.size[0]}x{self.size[1]}",
                "-nolisten",
                "tcp",
                "-ac",
                "+extension",
                "Composite",
                "+extension",
                "DAMAGE",
                "+extension",
                "XFIXES",
                "+extension",
                "GLX",
                "+extension",
                "RANDR",
            ],
            "xephyr",
            env=xephyr_env,
        )
        self.env["DISPLAY"] = display
        wait_for(
            "nested X11 readiness",
            lambda: run(["xdpyinfo"], env=self.env, check=False).returncode == 0,
            timeout=15,
        )
        config = self._write_private_config()
        run([self.binaries / "maverick", "--check-config", config], env=self.env, timeout=15)
        self.wm = self.spawn(
            [self.binaries / "maverick", "--config", config, "--name", "showcase"],
            "wm",
        )
        wait_for("Maverick IPC startup", lambda: self.state().get("monitors"), timeout=20)
        self.compositor_selection_owner = wait_for(
            "Maverick compositor selection ownership",
            self.compositor_owner,
            timeout=20,
        )
        run(["xsetroot", "-solid", "#15191f"], env=self.env)
        print(
            f"showcase: Xephyr {self.env['DISPLAY']} at {self.size[0]}x{self.size[1]}; "
            f"_NET_WM_CM_S0 owner {self.compositor_selection_owner}",
            flush=True,
        )

    def compositor_owner(self) -> str | None:
        if self.wm is None or self.wm.poll() is not None:
            return None
        # `_NET_WM_CM_S<n>` is a core X selection, not a root property;
        # `xprop -root` therefore reports "not found" even for a healthy
        # compositor. Query the selection owner through Xlib directly.
        x11 = ctypes.CDLL("libX11.so.6")
        x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x11.XOpenDisplay.restype = ctypes.c_void_p
        x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
        x11.XInternAtom.restype = ctypes.c_ulong
        x11.XGetSelectionOwner.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        x11.XGetSelectionOwner.restype = ctypes.c_ulong
        x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
        x11.XCloseDisplay.restype = ctypes.c_int
        display = x11.XOpenDisplay(self.env["DISPLAY"].encode())
        if not display:
            return None
        try:
            atom = x11.XInternAtom(display, b"_NET_WM_CM_S0", 0)
            owner = x11.XGetSelectionOwner(display, atom)
        finally:
            x11.XCloseDisplay(display)
        return f"0x{owner:x}" if owner else None

    def state(self) -> dict[str, Any]:
        result = run(
            [self.binaries / "maverickctl", "state", "--name", "showcase"],
            env=self.env,
            timeout=10,
        )
        return json.loads(result.stdout)

    def tree(self) -> dict[str, Any]:
        result = run(
            [self.binaries / "maverickctl", "query", "tree", "--name", "showcase"],
            env=self.env,
            timeout=10,
        )
        return json.loads(result.stdout)

    @staticmethod
    def entries(tree: dict[str, Any]) -> list[dict[str, Any]]:
        result: list[dict[str, Any]] = []
        for monitor in tree.get("monitors", []):
            for workspace in monitor.get("workspaces", []):
                for column in workspace.get("columns", []):
                    result.extend(column.get("windows", []))
                result.extend(workspace.get("floats", []))
        return result

    def wait_window(self, description: str, predicate: Callable[[dict[str, Any]], bool], timeout: float = 45) -> str:
        def probe() -> str | None:
            for entry in self.entries(self.tree()):
                if predicate(entry):
                    return str(entry["id"])
            return None

        window = wait_for(description, probe, timeout=timeout)
        self.windows.append(window)
        return window

    def wait_for(self, description: str, probe: Callable[[], Any], timeout: float = 30) -> Any:
        return wait_for(description, probe, timeout=timeout)

    def action(self, action: str) -> None:
        run(
            [self.binaries / "maverickctl", "msg", action, "--name", "showcase"],
            env=self.env,
            timeout=10,
        )

    def chord(self, key: str, count: int = 1) -> None:
        """Inject a real Super+Ctrl+key chord into the nested X server."""
        for index in range(count):
            if index:
                time.sleep(CHORD_SPACING_SECONDS)
            run(
                [
                    "xdotool",
                    "keydown",
                    "Super_L",
                    "keydown",
                    "Control_L",
                    "key",
                    key,
                    "keyup",
                    "Control_L",
                    "keyup",
                    "Super_L",
                ],
                env=self.env,
                timeout=10,
            )

    def focused(self) -> str | None:
        for monitor in self.state().get("monitors", []):
            focused = monitor.get("focused")
            if focused is not None:
                return str(focused)
        return None

    def focus(self, window: str) -> None:
        run(["xdotool", "windowactivate", "--sync", str(window)], env=self.env, timeout=10)
        wait_for(f"focus on {window}", lambda: self.focused() == str(window), timeout=10)
        self.stable([window])

    def geometry(self, window: str) -> tuple[int, int, int, int]:
        result = run(
            ["xdotool", "getwindowgeometry", "--shell", str(window)],
            env=self.env,
            timeout=10,
        )
        fields = dict(
            line.split("=", 1)
            for line in result.stdout.strip().splitlines()
            if "=" in line
        )
        return tuple(int(fields[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))  # type: ignore[return-value]

    def stable(self, windows: Iterable[str] | None = None) -> None:
        selected = list(windows if windows is not None else self.windows)
        if not selected:
            return
        last: list[tuple[int, int, int, int]] | None = None
        since = 0.0

        def probe() -> list[tuple[int, int, int, int]] | None:
            nonlocal last, since
            current = [self.geometry(window) for window in selected]
            if current != last:
                last, since = current, time.monotonic()
                return None
            if time.monotonic() - since >= 1.5:
                return current
            return None

        wait_for("stable client geometry", probe, timeout=20)

    def move_focus(self, direction: str, count: int = 1) -> None:
        for _ in range(count):
            self.action(f"focus:{direction}")
            self.stable()

    def place_float(self, window: str, width: int, height: int, x: int, y: int) -> tuple[int, int, int, int]:
        run(["xdotool", "windowsize", str(window), str(width), str(height)], env=self.env, timeout=10)
        self.stable([window])
        run(["xdotool", "windowmove", str(window), str(x), str(y)], env=self.env, timeout=10)
        self.stable([window])
        return self.geometry(window)

    def capture(self, name: str, description: str) -> dict[str, Any]:
        self.stable()
        run(
            ["xdotool", "mousemove", str(self.size[0] - 2), str(self.size[1] - 2)],
            env=self.env,
            timeout=10,
        )
        image = self.path / f"{name}.png"
        run(["import", "-display", self.env["DISPLAY"], "-window", "root", image], env=self.env, timeout=30)
        dimensions = run(["identify", "-format", "%wx%h", image], timeout=10).stdout
        expected = f"{self.size[0]}x{self.size[1]}"
        if dimensions != expected:
            raise ShowcaseError(f"{name}: captured {dimensions}, expected {expected}")
        destination = self.output / f"{name}.png"
        shutil.copyfile(image, destination)
        tree = self.tree()
        record = {
            "scene": name,
            "description": description,
            "display": self.env["DISPLAY"],
            "size": dimensions,
            "compositor": {
                "selection": "_NET_WM_CM_S0",
                "selection_owner": self.compositor_selection_owner,
            },
            "geometry": {window: list(self.geometry(window)) for window in self.windows},
            "state": self.state(),
            "tree": tree,
        }
        (self.evidence / f"{name}.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        print(f"showcase: captured {description} -> {destination}", flush=True)
        return record

    def close(self) -> None:
        def signal_owned(sig: int) -> None:
            for pid in descendants():
                try:
                    os.kill(pid, sig)
                except ProcessLookupError:
                    pass

        # Ask the WM to perform its normal shutdown while Xephyr is still
        # alive. This closes managed clients through Maverick's real lifecycle
        # and avoids manufacturing XIO errors by killing the server first.
        if self.wm is not None and self.wm.poll() is None and self.xephyr is not None and self.xephyr.poll() is None:
            try:
                result = run(
                    [self.binaries / "maverickctl", "quit", "--yes", "--name", "showcase"],
                    env=self.env,
                    check=False,
                    timeout=5,
                )
                if result.returncode:
                    print("showcase: graceful WM quit unavailable; forcing owned cleanup", file=sys.stderr)
            except ShowcaseError as error:
                print(f"showcase: graceful WM quit failed; forcing owned cleanup: {error}", file=sys.stderr)
            deadline = time.monotonic() + 5
            while self.wm.poll() is None and time.monotonic() < deadline:
                time.sleep(0.1)

        signal_owned(signal.SIGTERM)
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline and descendants():
            time.sleep(0.1)
        signal_owned(signal.SIGKILL)
        for process in self.processes:
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                pass
        deadline = time.monotonic() + 3
        while descendants() and time.monotonic() < deadline:
            try:
                os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                pass
            time.sleep(0.05)
        remaining = descendants()
        for log in self.logs:
            log.flush()
        for log_path in self.log_paths:
            destination = self.evidence / log_path.name
            if log_path.exists():
                shutil.copyfile(log_path, destination)
        for log in self.logs:
            log.close()
        self._lock.close()
        self.temp.cleanup()
        shutil.rmtree(self.runtime)
        if remaining:
            raise ShowcaseError(f"owned descendants survived cleanup: {remaining}")
        print("showcase: owned processes reaped; private session removed", flush=True)
