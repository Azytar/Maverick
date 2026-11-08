#!/usr/bin/env python3
"""Exercise the pinned client against one private Xephyr WM on the host desktop."""
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def wait_for(predicate, reason):
    end = time.monotonic() + 8
    while time.monotonic() < end:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(reason)


def main():
    children = []
    with tempfile.TemporaryDirectory(prefix="mclive-") as scratch:
        scratch = Path(scratch)
        env = {k: v for k, v in os.environ.items() if not k.startswith("MAVERICK_")}
        for key in ("HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
            directory = scratch / key
            directory.mkdir(mode=0o700)
            env[key] = str(directory)
        config = scratch / "config.toml"
        config.write_text("[general]\nview_count=3\n[autostart]\ncommands=[]\n")
        with (scratch / "server.log").open("w+") as log, (scratch / "events").open("w+") as events:
            try:
                server = subprocess.Popen(["Xephyr", "-displayfd", "1", "-screen", "1280x720",
                                           "-resizeable", "-no-host-grab", "-ac", "-nolisten", "tcp"],
                                          env=env, stdout=subprocess.PIPE, stderr=log, text=True)
                children.append(server)
                assert select.select([server.stdout], [], [], 10)[0], "Xephyr readiness timeout"
                number = server.stdout.readline().strip()
                assert number.isdecimal(), "invalid owned display"
                env["DISPLAY"] = ":" + number
                env.pop("XAUTHORITY", None)
                wm = subprocess.Popen([str(ROOT / "target/release/maverick"), "--config", str(config),
                                       "--session-id", "stage18-live"], env=env, stdout=log, stderr=log)
                children.append(wm)

                def ctl(*words, check=True):
                    return subprocess.run([str(ROOT / "target/release/maverickctl"), *words,
                                           "--session", "stage18-live"], env=env, capture_output=True,
                                          text=True, timeout=5, check=check)

                wait_for(lambda: ctl("state", check=False).returncode == 0, "control startup")
                assert len(json.loads(ctl("query", "tree").stdout)["monitors"]) == 1
                subscription = subprocess.Popen([str(ROOT / "target/release/maverickctl"), "subscribe",
                                                  "--session", "stage18-live"], env=env,
                                                 stdout=events, stderr=log)
                children.append(subscription)

                def registered():
                    assert subscription.poll() is None, "subscription exited"
                    ctl("msg", "view", "1")
                    ctl("msg", "view", "2")
                    return "workspace 1 0" in (scratch / "events").read_text()

                wait_for(registered, "subscriber received no workspace event")
                ctl("msg", "view", "3")
                wait_for(lambda: "workspace 2 0" in (scratch / "events").read_text(), "missing next event")
                wait_for(lambda: json.loads(ctl("state").stdout)["monitors"][0]["active_ws"] == 2,
                         "state did not publish the selected View")
                print("PASS: pinned client queries, actions and live workspace subscriptions", flush=True)
                modes = subprocess.check_output(["xrandr", "--query"], env=env, text=True)
                print(modes, flush=True)
                mode = "1024x768"
                assert mode in modes, "required nested RandR mode unavailable"
                subprocess.run(["xrandr", "--output", "default", "--mode", mode], env=env, check=True)
                wait_for(lambda: "1024, 768" in subprocess.check_output(
                    ["xprop", "-root", "_NET_DESKTOP_GEOMETRY"], env=env, text=True), "RandR geometry stale")
                print("PASS: nested RandR resize updates EWMH geometry", flush=True)
                for attempt in range(50):
                    ctl("restart")
                    wait_for(lambda: (scratch / "server.log").read_text().count("maverick ready")
                             >= attempt + 2 and ctl("state", check=False).returncode == 0,
                             "restart did not complete a new startup")
                    assert wm.poll() is None
                ctl("reload")
                ctl("quit", "--yes")
                assert wm.wait(timeout=8) == 0
                assert (scratch / "server.log").read_text().count("maverick ready") == 51
                print("PASS: 50 acknowledged control restarts, reload and owned shutdown", flush=True)
            finally:
                for child in reversed(children):
                    if child.poll() is None:
                        child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
                log.seek(0)
                print(log.read(), flush=True)


if __name__ == "__main__":
    main()
