#!/usr/bin/env python3
"""Owned-Xvfb stacking equivalence and separated request/wait/latency campaign."""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import select
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def line(pipe):
    if not select.select([pipe], [], [], 8)[0]:
        raise RuntimeError("private server/probe response timed out")
    value = pipe.readline()
    if not value:
        raise RuntimeError("private server/probe exited")
    return value.strip()


def wait_for(function, message):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        result = function()
        if result:
            return result
        time.sleep(0.005)
    raise AssertionError(message)


def distribution(values):
    return {"count": len(values), "median": statistics.median(values) if values else None,
            "min": min(values) if values else None, "max": max(values) if values else None}


def cpu_ticks(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    return int(fields[11]) + int(fields[12])


class Session:
    def __init__(self, directory, helper, shim, wm, ctl, mode, count, instrumented):
        self.directory, self.ctl_path, self.mode = directory, ctl, mode
        directory.mkdir(mode=0o700)
        self.processes = []
        self.log = (directory / "session.log").open("w+")
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith("MAVERICK_") and k not in ("DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "LD_PRELOAD")}
        for key, short in (("HOME", "home"), ("XDG_RUNTIME_DIR", "rt"), ("XDG_CONFIG_HOME", "cfg"),
                           ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache")):
            path = directory / short
            path.mkdir(mode=0o700)
            self.env[key] = str(path)
        config = directory / "config.toml"
        config.write_text('[general]\nfocus_mouse = false\nwarp_cursor = false\n'
                          '[autostart]\ncommands = []\n[[rules]]\nclass = "stage11"\n'
                          f'float = {str(mode != "tiled").lower()}\ntrue_fullscreen = true\n'
                          + ('sticky = true\n' if mode == 'sticky' else ''))
        try:
            server = self.spawn(["Xvfb", "-displayfd", "1", "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
                                stdout=subprocess.PIPE, stderr=self.log, text=True)
            self.env["DISPLAY"] = ":" + line(server.stdout)
            wm_env = dict(self.env)
            if instrumented:
                wm_env.update(LD_PRELOAD=str(shim), MAVERICK_XCB_PROFILE_PATH=str(directory / "requests.jsonl"))
            self.wm = self.spawn([wm, "--config", str(config), "--session-id", "stage11"],
                                 env=wm_env, stdout=self.log, stderr=self.log)
            wait_for(lambda: self.ctl("query", "inspect", check=False).returncode == 0, "WM did not become ready")
            self.probe = self.spawn([str(helper)], env=self.env, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=self.log, text=True)
            self.first = int(line(self.probe.stdout))
            self.windows = self.command(f"many {count} {int(mode == 'transient')}")
            assert len(self.windows) == count
            self.settle(count)
            if mode in ("fullscreen", "maximize"):
                self.focus(self.first)
                self.ctl("msg", f"fullscreen_window {self.first}" if mode == "fullscreen" else "toggle_maximize")
                self.ctl("query", "tree")  # FIFO command barrier; not an X11 completion claim.
                self.settle(count)
        except Exception:
            self.close()
            raise

    def spawn(self, *args, **kwargs):
        process = subprocess.Popen(*args, **kwargs)
        self.processes.append(process)
        return process

    def ctl(self, *words, check=True):
        return subprocess.run([self.ctl_path, *words, "--session", "stage11"], env=self.env,
                              text=True, capture_output=True, timeout=5, check=check)

    def command(self, text):
        self.probe.stdin.write(text + "\n")
        self.probe.stdin.flush()
        value = line(self.probe.stdout)
        return {"ok": True} if value == "ok" else json.loads(value)

    def settle(self, count):
        def snapshot():
            value = self.command("stack")
            if not value["equal"] or len(value["managed"]) != count:
                return None
            if self.mode == "transient":
                for parent, child in zip(self.windows[::2], self.windows[1::2]):
                    assert value["real"].index(parent) < value["real"].index(child)
            return value
        return wait_for(snapshot, "published stacking differs from independent QueryTree")

    def focus(self, target):
        self.command("arm")
        self.ctl("msg", f"focus_window {target}")
        result = self.command(f"complete {target}")
        assert result["complete"], "WM action did not acquire real and active focus"
        return result["focus_ack_ns"]

    def close(self):
        if hasattr(self, "wm") and self.wm.poll() is None:
            self.wm.terminate()
            try:
                self.wm.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.wm.kill()
                self.wm.wait(timeout=3)
        for process in reversed(self.processes):
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        self.log.flush()
        self.log.close()


def measure(session, count, mode, pattern, actions, instrumented):
    # Warm all protocol/geometry caches, and leave the repeat target selected.
    session.focus(session.windows[-1])
    session.settle(count)
    start_cpu = cpu_ticks(session.wm.pid)
    start = time.monotonic_ns()
    latency = []
    for action in range(actions):
        target = session.windows[action % count] if pattern == "cycle" else session.windows[-1]
        latency.append(session.focus(target))
        # Equivalence is a separate eventual-publication endpoint. Sleep is
        # outside the focus acknowledgement timer and never used as latency.
        time.sleep(0.005)
        state = session.settle(count)
        assert state["active"] == target and state["focus"] == target
    end = time.monotonic_ns()
    ticks = cpu_ticks(session.wm.pid) - start_cpu
    result = dict(clients=count, mode=mode, pattern=pattern, instrumented=instrumented, actions=actions)
    if instrumented:
        records = [json.loads(row) for row in (session.directory / "requests.jsonl").read_text().splitlines()]
        measured = [row for row in records if start <= row["ns"] <= end]
        submits = [row for row in measured if row["kind"] == "submit"]
        replies = [row for row in measured if row["kind"] == "reply_wait"]
        checks = [row for row in measured if row["kind"] == "checked_void"]
        requests = {row["sequence"]: row["opcode"] for row in records if row["kind"] == "submit"}
        tree_waits = [row for row in replies if requests.get(row["sequence"]) == 15]
        assert len([row for row in submits if row["opcode"] == 42]) >= actions
        assert all(row["success"] for row in replies + checks), "X11 reply/check failed during workload"
        result.update(raw_submissions=dict(sorted(Counter(row["opcode"] for row in submits).items())),
                      successful_reply_waits=len(replies), checked_void_calls=len(checks),
                      query_tree_wait_ns=distribution([row["wait_ns"] for row in tree_waits]),
                      checked_void_wait_ns=distribution([row["wait_ns"] for row in checks]),
                      query_tree_wait_process_cpu_ns=sum(row["cpu_ns"] for row in tree_waits))
    else:
        result.update(focus_ack_ns=distribution(latency), campaign_wall_ns=end-start,
                      wm_cpu_ticks=ticks, clock_ticks_per_second=os.sysconf("SC_CLK_TCK"))
    return result


def external_cases(session):
    count = len(session.windows)
    target = session.windows[-1]
    session.focus(target)
    before = session.settle(count)
    # Ordinary requests are redirected: actual order must stay authoritative.
    session.command(f"external {target} 0 0")
    time.sleep(0.03)
    assert session.settle(count)["real"] == before["real"]
    # A real external restack bypasses redirect. No WM action follows it; a
    # stale property cannot be rescued by an unrelated focus/layout command.
    session.command(f"external {target} 1 0")
    changed = wait_for(lambda: (v if v["real"] != before["real"] else None)
                       if (v := session.command("stack")) else None, "external restack did not reach X11")
    updated = session.settle(count)
    assert updated["real"] == changed["real"]
    print("PASS: external restack updates published stacking without another WM action", flush=True)
    # Burst commands are deliberately not settled individually.
    for index in range(40):
        session.ctl("msg", f"focus_window {session.windows[index % count]}")
    session.ctl("query", "tree")
    session.focus(target)
    state = session.settle(count)
    assert state["focus"] == target and state["active"] == target
    session.command("dock")
    dock = session.command("dockid")
    state = session.settle(count)
    assert dock in state["published"]
    session.command(f"external {dock} 1 0")
    wait_for(lambda: session.command("stack")["real"][0] == dock, "external dock lower did not reach X11")
    assert session.settle(count)["real"][0] == dock
    session.command("remove")
    session.settle(count)
    print("PASS: rapid focus and external dock restacking preserve server equivalence", flush=True)
    session.command(f"destroy-id {target}")
    session.settle(count - 1)
    print("PASS: destruction removes the client from the real and published stack", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", default=str(ROOT / "target/release/maverick"))
    parser.add_argument("--ctl", default=str(ROOT / "target/release/maverickctl"))
    parser.add_argument("--clients", type=int, nargs="+", default=[2, 10, 50])
    parser.add_argument("--actions", type=int, default=12)
    parser.add_argument("--modes", nargs="+", choices=["float", "tiled", "transient", "fullscreen", "maximize"],
                        default=["float", "tiled", "transient", "fullscreen", "maximize"])
    parser.add_argument("--external", action="store_true")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    if not all(2 <= n <= 200 for n in args.clients) or not 1 <= args.actions <= 1000:
        parser.error("expected 2..200 clients and 1..1000 actions")
    with tempfile.TemporaryDirectory(prefix="ms11-", dir="/tmp") as tmp:
        directory = Path(tmp)
        helper, shim = directory / "probe", directory / "requests.so"
        subprocess.run(["cc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/stacking-probe.c"),
                        "-o", str(helper), "-lX11"], check=True)
        subprocess.run(["cc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC",
                        str(ROOT / "tests/xcb-request-profile.c"), "-o", str(shim), "-ldl"], check=True)
        rows = []
        for count in args.clients:
            for mode in args.modes:
                for instrumented in (False, True):
                    session = Session(directory / f"s{len(rows)}", helper, shim, args.wm, args.ctl, mode, count, instrumented)
                    try:
                        for pattern in ("cycle", "repeat"):
                            row = measure(session, count, mode, pattern, args.actions, instrumented)
                            rows.append(row)
                            print("STACKING_MEASUREMENT " + json.dumps(row), flush=True)
                    except Exception:
                        session.log.flush()
                        print((session.directory / "session.log").read_text()[-6000:], flush=True)
                        raise
                    finally:
                        session.close()
        evidence = dict(schema=1, complete=True, revision=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                        wm_sha256=hashlib.sha256(Path(args.wm).read_bytes()).hexdigest(), kernel=platform.release(),
                        rows=rows, scope="RAW x11rb submissions; ABI reply/check waits are diagnostic, not server execution time. Uninstrumented focus acknowledgement includes IPC CLI and observer queries; CPU ticks are quantized.")
        if args.out:
            args.out.write_text(json.dumps(evidence, indent=2) + "\n")
        if args.external:
            session = Session(directory / "external", helper, shim, args.wm, args.ctl, "float", 2, False)
            try:
                external_cases(session)
            finally:
                session.close()
            session = Session(directory / "sticky", helper, shim, args.wm, args.ctl, "sticky", 2, False)
            try:
                for view in (2, 1, 2, 1):
                    session.ctl("msg", f"view {view}")
                    session.ctl("query", "tree")
                    state = session.settle(2)
                    assert set(state["real"]) == set(session.windows)
                print("PASS: sticky clients preserve stacking across View changes", flush=True)
            finally:
                session.close()


if __name__ == "__main__":
    main()
