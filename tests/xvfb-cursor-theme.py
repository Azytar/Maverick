#!/usr/bin/env python3
"""Measure root cursor theme/size and client ownership on a private Xvfb."""
import argparse
import os
from pathlib import Path
import struct
import subprocess
import tempfile

from importlib.util import module_from_spec, spec_from_file_location

ROOT = Path(__file__).resolve().parent.parent
spec = spec_from_file_location("cursor_test", ROOT / "tests/xvfb-cursor.py")
cursor_test = module_from_spec(spec)
spec.loader.exec_module(cursor_test)
line, wait_for = cursor_test.line, cursor_test.wait_for


def write_cursor(path):
    # Static Xcursor images with distinct, known pixels at each nominal size.
    sizes = (24, 32, 48)
    offset = 16 + 12 * len(sizes)
    chunks, toc, samples = [], [], {}
    for size in sizes:
        pixel = 0xFF204000 + size
        toc.append(struct.pack("<III", 0xFFFD0002, size, offset))
        chunk = struct.pack("<9I", 36, 0xFFFD0002, size, 1, size, size, 0, 0, 0)
        chunk += struct.pack("<I", pixel) * (size * size)
        chunks.append(chunk)
        offset += len(chunk)
        checksum = 14695981039346656037
        for _ in range(size * size):
            checksum = ((checksum ^ pixel) * 1099511628211) & ((1 << 64) - 1)
        samples[size] = [size, size, size * size, checksum]
    path.write_bytes(struct.pack("<4I", 0x72756358, 16, 1, len(sizes))
                     + b"".join(toc) + b"".join(chunks))
    return samples


def run_case(args, scratch, helper, name, settings, resources, expected, render=True):
    processes = []
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("MAVERICK_", "XCURSOR_"))}
    for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
        path = scratch / name / key
        path.mkdir(parents=True, mode=0o700)
        env[key] = str(path)
    env.update(XCURSOR_PATH=str(scratch / "themes"), XENVIRONMENT=str(scratch / "empty"))
    env.update(settings)
    config = scratch / "config.toml"
    config.write_text("[general]\nwarp_cursor = false\nfocus_mouse = false\n"
                      "[autostart]\ncommands = []\n")
    with (scratch / f"{name}.log").open("w+") as log:
        try:
            server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24",
                                       "-nolisten", "tcp", "-noreset",
                                       *([] if render else ["-extension", "RENDER"])],
                                      stdout=subprocess.PIPE, stderr=log, text=True, env=env)
            processes.append(server)
            env["DISPLAY"] = ":" + line(server.stdout)
            subprocess.run(["xprop", "-root", "-f", "RESOURCE_MANAGER", "8s", "-set",
                            "RESOURCE_MANAGER", resources], env=env, check=True)
            probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1)
            processes.append(probe)
            line(probe.stdout)

            def command(text):
                probe.stdin.write(text + "\n")
                probe.stdin.flush()
                return list(map(int, line(probe.stdout).split()))

            command("blank")
            wm = subprocess.Popen([args.wm, "--config", str(config), "--session-id", "cursor-theme"],
                                  env=env, stdout=log, stderr=log)
            processes.append(wm)

            def ctl(*words, check=True):
                return subprocess.run([args.ctl, *words, "--session", "cursor-theme"],
                                      env=env, capture_output=True, text=True, timeout=5, check=check)

            def ready():
                if wm.poll() is not None:
                    raise RuntimeError("Maverick exited during startup")
                return "window id #" in subprocess.check_output(
                    ["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env=env, text=True, timeout=3)

            wait_for(ready, "Maverick did not claim the display")
            arrow = command("sample")
            assert arrow[2] > 0, f"invisible root cursor: {arrow}"
            if expected is not None:
                assert arrow == expected, f"{name}: expected {expected}, got {arrow}"
            command("client")
            wait_for(lambda: command("enter")[2] > 0 and command("sample") != arrow,
                     "client cursor did not become visible")
            client = command("sample")
            assert client[2] > 0 and client != arrow, "WM replaced the client's cursor"
            ctl("msg", "view_create")
            ctl("msg", "view:2")
            wait_for(lambda: command("sample") == arrow, "empty View changed the root cursor")
            ctl("msg", "view:1")
            wait_for(lambda: command("enter") == client, "returning to the client changed its cursor")
            command("destroy")
            wait_for(lambda: command("sample") == arrow, "closing the client changed the root cursor")
            ctl("quit", "--yes", check=False)
            assert wm.wait(timeout=5) == 0
            print(f"PASS: {name}: root {arrow[0]}x{arrow[1]}, pixels, View switches and client ownership")
        except Exception:
            log.flush()
            log.seek(0)
            print(log.read())
            raise
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    parser.add_argument("--ctl", default=str(ROOT / "target/release/maverickctl"))
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="mct-", dir="/tmp") as directory:
        scratch = Path(directory)
        cursors = scratch / "themes/maverick-test/cursors"
        cursors.mkdir(parents=True)
        samples = write_cursor(cursors / "left_ptr")
        default = scratch / "themes/default"
        default.mkdir()
        (default / "index.theme").write_text("[Icon Theme]\nInherits=maverick-test\n")
        broken = scratch / "themes/broken/cursors"
        broken.mkdir(parents=True)
        (broken / "left_ptr").write_bytes(b"invalid cursor")
        (scratch / "empty").write_text("")
        helper = scratch / "probe"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/cursor-probe.c"),
                        "-o", str(helper), "-lX11", "-lXfixes"], check=True)
        resources = "Xcursor.theme: maverick-test\nXcursor.size: 24\n"
        run_case(args, scratch, helper, "environment-precedence",
                 {"XCURSOR_THEME": "maverick-test", "XCURSOR_SIZE": "48"},
                 "Xcursor.theme: broken\nXcursor.size: 24\n", samples[48])
        run_case(args, scratch, helper, "resources", {}, resources, samples[24])
        run_case(args, scratch, helper, "dpi", {},
                 "Xcursor.theme: maverick-test\nXft.dpi: 144\n", samples[32])
        run_case(args, scratch, helper, "session-default", {}, "Maverick.test: cursor\n", samples[24])
        for theme in ("missing", "broken", "core"):
            settings = {"XCURSOR_THEME": theme}
            if theme == "missing":
                settings["XCURSOR_PATH"] = str(scratch / "empty-themes")
            run_case(args, scratch, helper, theme, settings, resources, None)
        run_case(args, scratch, helper, "without-render", {}, resources, None, render=False)


if __name__ == "__main__":
    main()
