#!/usr/bin/env python3
"""The five-scene Maverick showcase story."""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import shutil
import subprocess
import time
from typing import Any, Callable

from harness import ROOT, Session, ShowcaseError, run


SCENES = ("workspace", "ribbon", "tools", "floating", "hero")


@dataclass(frozen=True)
class WindowRef:
    role: str
    window: str
    application: str


class Showcase:
    """Launch real clients once, then reveal one capability at a time."""

    def __init__(self, session: Session):
        self.session = session
        self.terminal = shutil.which("alacritty") or shutil.which("xterm")
        self.nvim = shutil.which("nvim")
        self.firefox = shutil.which("firefox")
        self.ids: dict[str, str] = {}
        self.apps: dict[str, WindowRef] = {}
        self.float_window: str | None = None
        if not self.terminal:
            raise ShowcaseError("the showcase needs alacritty or xterm for its terminal scene")

    def _add(
        self,
        role: str,
        process: subprocess.Popen[Any],
        application: str,
        predicate: Callable[[dict[str, Any]], bool],
    ) -> str:
        window = self.session.wait_window(f"{application} client", predicate)
        self.ids[role] = window
        self.apps[role] = WindowRef(role, window, application)
        self.session.stable([window])
        print(f"showcase: {role} = {application} (XID {window})", flush=True)
        return window

    def _match(self, *, instance: str | None = None, title: str | None = None, classes: tuple[str, ...] = ()) -> Callable[[dict[str, Any]], bool]:
        wanted_instance = (instance or "").lower()
        wanted_title = (title or "").lower()
        wanted_classes = tuple(value.lower() for value in classes)

        def matches(entry: dict[str, Any]) -> bool:
            actual_instance = str(entry.get("instance") or "").lower()
            actual_title = str(entry.get("title") or "").lower()
            actual_class = str(entry.get("class") or "").lower()
            if wanted_instance and actual_instance == wanted_instance:
                return True
            if wanted_title and wanted_title in actual_title:
                return True
            return bool(wanted_classes and any(value in actual_class for value in wanted_classes))

        return matches

    def _terminal_argv(self, label: str, title: str, instance: str, command: list[str]) -> list[str]:
        if self.terminal and Path(self.terminal).name == "alacritty":
            config = self.session.path / f"alacritty-{label}.toml"
            config.write_text(
                """[font]
size = 14
normal = { family = "DejaVu Sans Mono" }
[window]
padding = { x = 12, y = 10 }
[colors.primary]
background = "#1b2229"
foreground = "#e8eef0"
[colors.cursor]
text = "#1b2229"
cursor = "#9ed0d7"
""",
                encoding="utf-8",
            )
            return [self.terminal, "--config-file", config, "--title", title, "--class", f"Alacritty,{instance}", "-e", *command]
        return [
            self.terminal,
            "-name",
            instance,
            "-class",
            f"Showcase,{instance}",
            "-title",
            title,
            "-fa",
            "DejaVu Sans Mono",
            "-fs",
            "13",
            "-bg",
            "#1b2229",
            "-fg",
            "#e8eef0",
            "-e",
            *command,
        ]

    def launch_shell(self, role: str, title: str, instance: str) -> str:
        rcfile = self.session.path / f"{role}.bashrc"
        rcfile.write_text(
            "printf '\\nMAVERICK / TERMINAL\\n'\n"
            "printf 'layout: column\\n'\n"
            "printf 'camera: focus\\n'\n"
            "printf 'clients: real X11\\n'\n"
            "PS1='mav$ '\n",
            encoding="utf-8",
        )
        command = ["bash", "--noprofile", "--rcfile", str(rcfile), "-i"]
        process = self.session.spawn(
            self._terminal_argv(role, title, instance, command),
            role,
        )
        return self._add(
            role,
            process,
            "Alacritty terminal" if self.terminal and Path(self.terminal).name == "alacritty" else "xterm terminal",
            self._match(instance=instance, title=title),
        )

    def launch_code(self, role: str, title: str, instance: str, source: Path) -> str:
        if self.nvim:
            command = [
                self.nvim,
                "--clean",
                "-c",
                "set number",
                "-c",
                "set termguicolors",
                str(source),
            ]
            application = "Neovim in a real terminal"
        else:
            command = [
                "bash",
                "--noprofile",
                "--norc",
                "-c",
                f"printf 'SOURCE / {source.name}\\n\\n'; sed -n '1,120p' {source}; printf '\\n'; exec bash --noprofile --norc -i",
            ]
            application = "source viewer fallback"
        process = self.session.spawn(self._terminal_argv(role, title, instance, command), role)
        return self._add(role, process, application, self._match(instance=instance, title=title))

    def launch_browser(self) -> str:
        title = "Maverick / workspace reference"
        if self.firefox:
            profile = self.session.path / "firefox-profile"
            profile.mkdir(mode=0o700)
            (profile / "user.js").write_text(
                """user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("browser.startup.homepage", "about:blank");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
user_pref("browser.rights.3.shown", true);
user_pref("network.dns.disabled", true);
user_pref("network.proxy.type", 0);
""",
                encoding="utf-8",
            )
            url = (ROOT / "showcase/assets/reference.html").resolve().as_uri()
            process = self.session.spawn(
                [
                    self.firefox,
                    "--no-remote",
                    "--offline",
                    "--new-instance",
                    "--profile",
                    profile,
                    url,
                ],
                "browser",
            )
            try:
                return self._add(
                    "browser",
                    process,
                    "Firefox local offline page",
                    self._match(title="Maverick / workspace reference", classes=("firefox",)),
                )
            except ShowcaseError:
                self.session.abandon(process)
                print("showcase: Firefox did not map; using the offline reference viewer", flush=True)
        return self.launch_reference("browser", "browser-fallback")

    def launch_reference(self, role: str, label: str) -> str:
        asset = str(ROOT / "showcase/assets/reference.txt")
        command = [
            "bash",
            "--noprofile",
            "--norc",
            "-c",
            f"printf 'MAVERICK / OFFLINE REFERENCE\\n\\n'; cat {asset}; printf '\\n'; exec bash --noprofile --norc -i",
        ]
        process = self.session.spawn(
            self._terminal_argv(label, "Maverick Reference", "mav-reference", command),
            label,
        )
        return self._add(
            role,
            process,
            "Maverick reference terminal",
            self._match(instance="mav-reference", title="Maverick Reference"),
        )

    def launch_files(self) -> str:
        return self.launch_reference("files", "reference")

    def launch_metrics(self) -> str:
        return self.launch_code(
            "metrics",
            "Maverick / commands.rs",
            "mav-commands",
            ROOT / "src/core/commands.rs",
        )

    def launch_monitor(self) -> str:
        command = [
            shutil.which("python3") or "python3",
            str(ROOT / "showcase/apps/mav_monitor.py"),
            "--maverick-bin",
            self.session.binaries / "maverickctl",
            "--name",
            "showcase",
            "--interval",
            "0.8",
        ]
        process = self.session.spawn(
            self._terminal_argv("monitor", "Maverick Monitor", "mav-monitor", command),
            "monitor",
        )
        return self._add(
            "monitor",
            process,
            "Maverick live terminal monitor",
            self._match(instance="mav-monitor", title="Maverick Monitor"),
        )

    def _scroll(self) -> int:
        tree = self.session.tree()
        for monitor in tree.get("monitors", []):
            for workspace in monitor.get("workspaces", []):
                if workspace.get("active"):
                    return int(workspace.get("scroll", 0))
        raise ShowcaseError("Maverick did not report an active workspace")

    def workspace_scene(self) -> None:
        self.ids["terminal"] = self.launch_shell("terminal", "Maverick / terminal", "mav-terminal")
        self.ids["editor"] = self.launch_code("editor", "Maverick / layout.rs", "mav-editor", ROOT / "src/core/layout.rs")
        self.ids["browser"] = self.launch_browser()
        # Maverick intentionally gives the first window the full workarea;
        # grow_col is the real WM operation used to establish a balanced
        # three-column reference composition without hand-positioning clients.
        self.session.focus(self.ids["terminal"])
        usable = self.session.size[0] - 36
        self.session.action(f"grow_col:{-int(usable * 0.75)}")
        self.session.stable()
        self.session.focus(self.ids["editor"])
        self.session.stable()
        self.session.capture("workspace", "a clean three-column workspace with real clients")

    def ribbon_scene(self) -> None:
        self.ids["ribbon"] = self.launch_code("ribbon", "Maverick / engine.rs", "mav-ribbon", ROOT / "src/core/engine.rs")
        self.ids["files"] = self.launch_files()
        self.ids["monitor"] = self.launch_monitor()
        self.session.action("focus:left")
        self.session.stable()
        tree = self.session.tree()
        workspace = tree["monitors"][0]["workspaces"][0]
        if len(workspace.get("columns", [])) < 5 or int(workspace.get("scroll", 0)) <= 0:
            raise ShowcaseError("ribbon scene did not overflow the viewport")
        self.session.capture("ribbon", "the tiled ribbon continues beyond the visible viewport")

    def tools_scene(self) -> None:
        # The ribbon scene already crossed the viewport boundary. This capture
        # pauses on the resulting real-tool composition before the dedicated
        # floating proof adds another client.
        self.session.capture("tools", "real browser, reference terminal, editor and terminal in one scrollable workspace")

    def floating_scene(self) -> None:
        monitor = self.ids["monitor"]
        self.session.focus(monitor)
        self.session.action("toggle_float")
        self.session.wait_for(
            "Maverick Monitor becomes floating",
            lambda: any(
                str(entry["id"]) == monitor and entry.get("float")
                for entry in self.session.entries(self.session.tree())
            ),
            timeout=15,
        )
        width = min(560, self.session.size[0] // 3)
        height = min(390, self.session.size[1] // 2)
        x = int(self.session.size[0] * 0.48)
        y = int(self.session.size[1] * 0.25)
        float_before = self.session.place_float(monitor, width, height, x, y)
        self.session.focus(self.ids["terminal"])
        tiled_before = self.session.geometry(self.ids["terminal"])
        self.session.move_focus("right", 2)
        tiled_after = self.session.geometry(self.ids["terminal"])
        float_after = self.session.geometry(monitor)
        if float_before != float_after:
            raise ShowcaseError(f"floating window moved with the camera: {float_before} -> {float_after}")
        if tiled_before[:2] == tiled_after[:2]:
            raise ShowcaseError("floating scene did not move the tiled ribbon")
        self.float_window = monitor
        self.session.capture("floating", "a real floating monitor over a moving tiled ribbon")

    def hero_scene(self) -> None:
        self.session.focus(self.ids["editor"])
        self.session.action("grow_col:180")
        self.session.stable()
        self.session.focus(self.ids["browser"])
        self.session.stable()
        if self.float_window is None:
            raise ShowcaseError("hero scene requires the floating monitor")
        entries = self.session.entries(self.session.tree())
        if not any(str(entry["id"]) == self.float_window and entry.get("float") for entry in entries):
            raise ShowcaseError("hero scene lost the floating monitor")
        self.session.capture("hero", "the complete composition: varied columns, camera, real clients and a float")

    def run_until(self, target: str) -> None:
        if target not in SCENES:
            raise ShowcaseError(f"unknown scene {target!r}")
        for scene in SCENES[: SCENES.index(target) + 1]:
            getattr(self, f"{scene}_scene")()
            time.sleep(0.15)
