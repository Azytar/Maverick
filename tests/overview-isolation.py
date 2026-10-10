#!/usr/bin/env python3
"""Exercise the Overview rig's startup/teardown without a graphical desktop.

Tool doubles stop the rig before its geometry scenarios. They make global
pkill requests observable without allowing them to signal real sessions, and
test both a ready child display and a child that dies while xprop still succeeds.
Run: python3 tests/overview-isolation.py [path/to/xephyr-overview.sh]
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


TOOLS = r'''
import json
import os
from pathlib import Path
import signal
import sys
import time

state = Path(os.environ["OVERVIEW_FIXTURE"])
tool = Path(sys.argv[0]).name
if tool == "pkill":
    with (state / "global-signals").open("a") as log:
        log.write(json.dumps(sys.argv[1:]) + "\n")
elif tool == "xprop":
    with (state / "displays").open("a") as log:
        log.write(os.environ["DISPLAY"] + "\n")
elif tool == "maverick":
    (state / "wm-started").touch()
    raise SystemExit(12)
elif tool == "Xephyr":
    (state / "server-pid").write_text(str(os.getpid()))
    if os.environ["OVERVIEW_SERVER"] == "dead":
        raise SystemExit(13)
    if "-displayfd" in sys.argv:
        fd = int(sys.argv[sys.argv.index("-displayfd") + 1])
        os.write(fd, b"197\n")
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    while True:
        time.sleep(1)
'''


def run_case(source, mode):
    with tempfile.TemporaryDirectory(prefix="overview-isolation-") as scratch:
        root = Path(scratch)
        rig = root / "rig"
        rig.mkdir()
        shutil.copyfile(source, rig / "xephyr-overview.sh")
        regression = source.with_name("xvfb-overview.py")
        if regression.exists():
            (rig / "xvfb-overview.py").symlink_to(regression.resolve())
        tool_dir = root / "bin"
        tool_dir.mkdir()
        for name in ("Xephyr", "pkill", "xprop", "maverick"):
            path = tool_dir / name
            path.write_text(f"#!{sys.executable}\n" + TOOLS)
            path.chmod(0o755)
        env = os.environ.copy()
        env.update(
            PATH=str(tool_dir) + os.pathsep + env["PATH"],
            OVERVIEW_FIXTURE=str(root),
            OVERVIEW_SERVER=mode,
            MAVERICK_BIN=str(tool_dir / "maverick"),
        )
        result = subprocess.run(
            ["bash", str(rig / "xephyr-overview.sh")], cwd=rig, env=env,
            capture_output=True, text=True, timeout=15,
        )
        assert result.returncode == 1, result.stdout + result.stderr
        pid = int((root / "server-pid").read_text())
        assert not Path(f"/proc/{pid}").exists(), "the rig leaked its X server child"
        assert not (root / "global-signals").exists(), "the rig requested global process kills"
        if mode == "ready":
            assert (root / "wm-started").exists(), "the owned display was never used"
            assert set((root / "displays").read_text().splitlines()) == {":197"}, (
                "readiness must use the display claimed by this server"
            )
        else:
            assert not (root / "wm-started").exists(), (
                "a foreign display answered xprop after our server died"
            )
        print(f"PASS: {mode} server startup is isolated and its child is reaped")


if __name__ == "__main__":
    source = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("xephyr-overview.sh")
    for mode in ("ready", "dead"):
        run_case(source, mode)
