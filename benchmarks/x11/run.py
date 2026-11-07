#!/usr/bin/env python3
"""Compare owned X11 window-manager processes with one native EWMH workload."""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import select
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
WMS = ("maverick", "i3", "bspwm", "openbox", "dwm")
PHASES = ("map", "focus", "fullscreen", "destroy")


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def read_line(process, timeout):
    """One outstanding request, one line; reject EOF and partial-line hangs."""
    data = bytearray()
    deadline = time.monotonic() + timeout
    while b"\n" not in data:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([process.stdout], [], [], remaining)[0]:
            raise TimeoutError(f"response deadline: pid={process.pid}")
        chunk = os.read(process.stdout.fileno(), 65536)
        if not chunk:
            raise RuntimeError(f"child closed response pipe: pid={process.pid}, exit={process.poll()}")
        data.extend(chunk)
        if len(data) > 1024 * 1024:
            raise ValueError("oversized response")
    line, _, tail = data.partition(b"\n")
    if tail:
        raise ValueError("unexpected unsolicited response")
    return line.decode("ascii")


def command(probe, text, expected, count):
    probe.stdin.write((text + "\n").encode())
    probe.stdin.flush()
    doc = json.loads(read_line(probe, 120))
    if expected is None:
        if doc != {"wm_ready": True}:
            raise ValueError("invalid WM readiness response")
    else:
        values = doc.get("samples_ms")
        if (doc.get("phase") != expected or not isinstance(values, list) or len(values) != count
                or any(type(x) not in (int, float) or not math.isfinite(x) or x < 0 for x in values)):
            raise ValueError("invalid or incomplete samples: " + expected)
    return doc


def proc_stats(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    ticks = int(fields[11]) + int(fields[12])
    result = {"cpu_ticks": ticks, "rss_bytes": int(fields[21]) * os.sysconf("SC_PAGE_SIZE"),
              "starttime_ticks": int(fields[19])}
    try:
        smaps = Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()
        result["pss_bytes"] = int(next(line for line in smaps if line.startswith("Pss:")).split()[1]) * 1024
    except (OSError, StopIteration):
        result["pss_bytes"] = None
    return result


def stop(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
    for pipe in (process.stdin, process.stdout):
        if pipe:
            pipe.close()


def launch(name, binaries, config):
    if name == "maverick":
        return [binaries[name], "--config", str(config / "maverick.toml")]
    if name == "i3":
        return [binaries[name], "-c", str(config / "i3.conf")]
    if name == "bspwm":
        return [binaries[name], "-c", "/bin/true"]
    if name == "dwm":
        return [binaries[name]]
    return [binaries[name], "--config-file", str(config / "openbox.xml"), "--sm-disable"]


def trial(name, clients, repeat, warmup, args, binaries, helper, directory):
    directory.mkdir()
    started = time.monotonic()
    result = {"wm": name, "clients": clients, "repeat": repeat, "warmup": warmup,
              "success": False, "samples": {}, "phases": {}, "load_start": os.getloadavg(),
              "processes": []}
    processes = []
    try:
        with tempfile.TemporaryDirectory(prefix="wmbench-", dir="/tmp") as runtime:
            runtime = Path(runtime)
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(("MAVERICK_", "I3SOCK", "BSPWM_"))
                   and k not in ("DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS")}
            for key in ("HOME", "XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"):
                path = runtime / key
                path.mkdir(mode=0o700)
                env[key] = str(path)
            env.update(DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent/wmbench-bus",
                       BSPWM_SOCKET=str(runtime / "bspwm.sock"),
                       BENCH_CONFIG_READY=str(runtime / "config-ready"))
            config = directory / "configs"
            shutil.copytree(HERE / "configs", config)
            # Keep the user's supplied binaries together, even outside PATH.
            env["PATH"] = str(Path(binaries["bspwm"]).parent) + os.pathsep + env.get("PATH", "")
            with (directory / "server.log").open("w") as server_log, \
                    (directory / "wm.log").open("w") as wm_log, \
                    (directory / "probe.log").open("w") as probe_log:
                try:
                    server = subprocess.Popen([args.xvfb, "-displayfd", "1", "-screen", "0", "1280x800x24",
                                               "-nolisten", "tcp", "-noreset"], env=env,
                                              stdout=subprocess.PIPE, stderr=server_log)
                    processes.append(server)
                    display = read_line(server, 30)
                    if not display.isascii() or not display.isdecimal():
                        raise ValueError("invalid Xvfb display number")
                    env["DISPLAY"] = ":" + display
                    result["display"] = env["DISPLAY"]
                    probe = subprocess.Popen([str(helper)], env=env, stdin=subprocess.PIPE,
                                             stdout=subprocess.PIPE, stderr=probe_log)
                    processes.append(probe)
                    if json.loads(read_line(probe, 10)) != {"probe_ready": True}:
                        raise ValueError("probe did not acknowledge subscription")
                    argv = launch(name, binaries, config)
                    result["wm_command"] = argv
                    t0 = time.monotonic()
                    wm = subprocess.Popen(argv, env=env, stdout=wm_log, stderr=wm_log)
                    processes.append(wm)
                    command(probe, "ready", None, 0)
                    result["startup_ms"] = (time.monotonic() - t0) * 1000
                    probe.stdin.write(b"prime\n")
                    probe.stdin.flush()
                    if json.loads(read_line(probe, 35)) != {"primed": True}:
                        raise ValueError("missing usable-loop barrier")
                    if name == "bspwm":
                        bspc = str(Path(binaries[name]).with_name("bspc"))
                        settings = json.loads((config / "bspwm.json").read_text())
                        for words in settings["commands"]:
                            if words[0] == "config":
                                current = subprocess.run([bspc, *words[:2]], env=env, capture_output=True,
                                                         text=True, check=True, timeout=5).stdout.strip()
                                if current == words[2]:
                                    continue
                            subprocess.run([bspc, *words], env=env, stdout=wm_log, stderr=wm_log,
                                           check=True, timeout=5)
                    result["focus_supported"] = name != "dwm"
                    # Allow startup/configuration housekeeping to settle equally.
                    time.sleep(.25)
                    for phase, operations in (("map", clients), ("focus", args.focus),
                                              ("fullscreen", args.fullscreen), ("destroy", clients)):
                        if phase == "focus":
                            if name == "dwm":
                                probe.stdin.write(b"focus-capability\n")
                                probe.stdin.flush()
                                capability = json.loads(read_line(probe, 3))
                                result["focus_capability"] = capability
                                result["focus_supported"] = capability.get("focus_supported") is True
                                if not result["focus_supported"] and capability.get("urgent_only") is not True:
                                    raise RuntimeError("unexplained EWMH focus refusal")
                            before = proc_stats(wm.pid)
                            idle_start = time.monotonic()
                            time.sleep(args.idle_seconds)
                            idle_end = time.monotonic()
                            after = proc_stats(wm.pid)
                            result["resident"] = after
                            result["idle"] = {"seconds": idle_end - idle_start,
                                              "cpu_ticks": after["cpu_ticks"] - before["cpu_ticks"]}
                        if phase == "focus" and not result["focus_supported"]:
                            result["samples"]["focus"] = None
                            result["phases"]["focus"] = None
                            continue
                        before = proc_stats(wm.pid)
                        phase_start = time.monotonic()
                        text = "destroy" if phase == "destroy" else f"{phase} {operations}"
                        doc = command(probe, text, phase, operations)
                        phase_end = time.monotonic()
                        after = proc_stats(wm.pid)
                        if before["starttime_ticks"] != after["starttime_ticks"]:
                            raise RuntimeError("process identity changed")
                        result["samples"][phase] = doc["samples_ms"]
                        result["phases"][phase] = {"seconds": phase_end - phase_start,
                                                   "cpu_ticks": after["cpu_ticks"] - before["cpu_ticks"]}
                    if any(p.poll() is not None for p in processes):
                        raise RuntimeError("a child exited before workload completion")
                    probe.stdin.write(b"quit\n")
                    probe.stdin.flush()
                    if probe.wait(timeout=5) != 0:
                        raise RuntimeError("probe exited unsuccessfully")
                    result["success"] = True
                except Exception:
                    for tool in (["xwininfo", "-root", "-tree"], ["xprop", "-root"]):
                        try:
                            diagnostic = subprocess.run(tool, env=env, capture_output=True, text=True, timeout=3)
                            (directory / (tool[0] + ".log")).write_text(diagnostic.stdout + diagnostic.stderr)
                        except (OSError, subprocess.TimeoutExpired):
                            pass
                    raise
                finally:
                    # Observer goes first, WM next, X server last; signal only owned PIDs.
                    for process in ([processes[1]] if len(processes) > 1 else []) + processes[2:][::-1] + processes[:1]:
                        stop(process)
                    result["processes"] = [{"pid": p.pid, "returncode": p.returncode} for p in processes]
    except Exception as error:
        result["error"] = f"{type(error).__name__}: {error}"
    result["duration_seconds"] = time.monotonic() - started
    result["load_end"] = os.getloadavg()
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def quantile(values, fraction):
    if not values:
        raise ValueError("empty observations")
    ordered = sorted(values)
    position = (len(ordered) - 1) * fraction
    low = math.floor(position)
    high = math.ceil(position)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def metrics(run, ticks):
    row = {"startup_ms": run["startup_ms"], "rss_mib": run["resident"]["rss_bytes"] / 2**20,
           "pss_mib": None,
           "idle_cpu_percent": 100 * run["idle"]["cpu_ticks"] / ticks / run["idle"]["seconds"]}
    if run["resident"]["pss_bytes"] is not None:
        row["pss_mib"] = run["resident"]["pss_bytes"] / 2**20
    for phase in PHASES:
        values = run["samples"][phase]
        if values is None:
            row[phase + "_p50_ms"] = None
            row[phase + "_p95_ms"] = None
            row[phase + "_cpu_ms_per_100_ops"] = None
            continue
        row[phase + "_p50_ms"] = quantile(values, .5)
        row[phase + "_p95_ms"] = quantile(values, .95)
        row[phase + "_cpu_ms_per_100_ops"] = 100000 * run["phases"][phase]["cpu_ticks"] / ticks / len(values)
    return row


def optional_text(path):
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--maverick", default=str(ROOT / "target/release/maverick"))
    for name in WMS[1:]:
        parser.add_argument("--" + name, default=name)
    parser.add_argument("--xvfb", default="Xvfb")
    parser.add_argument("--clients", type=int, nargs="+", default=[10, 50])
    parser.add_argument("--repeats", type=int, default=7)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--focus", type=int, default=500)
    parser.add_argument("--fullscreen", type=int, default=40)
    parser.add_argument("--idle-seconds", type=float, default=2)
    parser.add_argument("--seed", type=int, default=20261022)
    args = parser.parse_args()
    if (any(n < 2 or n > 256 for n in args.clients) or len(set(args.clients)) != len(args.clients)
            or not 1 <= args.repeats <= 100 or not 0 <= args.warmups <= 10
            or not 1 <= args.focus <= 10000 or not 2 <= args.fullscreen <= 10000
            or args.fullscreen % 2 or not math.isfinite(args.idle_seconds) or args.idle_seconds < .1):
        parser.error("invalid workload bounds (fullscreen count must be even)")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    binaries = {}
    for name in WMS:
        binary = shutil.which(getattr(args, name))
        if not binary:
            parser.error("missing executable: " + getattr(args, name))
        binaries[name] = str(Path(binary).resolve())
    xvfb = shutil.which(args.xvfb)
    if not xvfb:
        parser.error("missing Xvfb")
    args.xvfb = str(Path(xvfb).resolve())
    compiler = ["cc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-Wshadow", "-Wformat=2",
                str(HERE / "probe.c"), "-o", str(out / "probe"), "-lX11"]
    subprocess.run(compiler, check=True, timeout=60)
    helper = out / "probe"
    versions = {}
    with tempfile.TemporaryDirectory(prefix="bench-version-", dir="/tmp") as tmp:
        env = dict(os.environ, HOME=tmp, XDG_CACHE_HOME=tmp)
        for name, binary in binaries.items():
            run = subprocess.run([binary, "-v" if name in ("bspwm", "dwm") else "--version"],
                                 env=env, capture_output=True, text=True, timeout=5)
            if run.returncode != 0 and not (name == "dwm" and run.returncode == 1):
                raise RuntimeError("version query failed: " + binary)
            versions[name] = (run.stdout + run.stderr).strip()
    ticks = os.sysconf("SC_CLK_TCK")
    manifest = {"schema": 1, "utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "command": sys.argv, "parameters": vars(args) | {"out": str(out)},
                "binaries": {name: {"path": path, "sha256": digest(path), "version": versions[name]}
                             for name, path in binaries.items()},
                "xvfb": {"path": args.xvfb, "sha256": digest(args.xvfb)},
                "source_sha256": {str(path.relative_to(ROOT)): digest(path)
                                  for path in sorted(HERE.rglob("*")) if path.is_file() and "__pycache__" not in str(path)},
                "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                "tracked_changes": subprocess.check_output(["git", "diff", "HEAD", "--stat"], cwd=ROOT, text=True),
                "compiler": compiler, "compiler_version": subprocess.check_output(["cc", "--version"], text=True).splitlines()[0],
                "kernel": platform.platform(), "python": platform.python_version(),
                "os_release": optional_text("/etc/os-release"), "cpuinfo": optional_text("/proc/cpuinfo"),
                "cpu_affinity": sorted(os.sched_getaffinity(0)), "clock_ticks_per_second": ticks,
                "cgroup_cpu_max": optional_text("/sys/fs/cgroup/cpu.max"),
                "cgroup_memory_max": optional_text("/sys/fs/cgroup/memory.max"),
                "runtime_library_path": os.environ.get("LD_LIBRARY_PATH"),
                "x_data_dirs": os.environ.get("XDG_DATA_DIRS"),
                "screen": "1280x800x24", "compositor": None, "complete": False}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    rng = random.Random(args.seed)
    runs = []
    blocks = [(True, i) for i in range(args.warmups)] + [(False, i) for i in range(args.repeats)]
    for warmup, repeat in blocks:
        block = [(name, n) for name in WMS for n in args.clients]
        rng.shuffle(block)
        for name, n in block:
            label = f'{"warmup" if warmup else "run"}-{repeat:02d}-{name}-{n}'
            run = trial(name, n, repeat, warmup, args, binaries, helper, out / label)
            runs.append(run)
            print(json.dumps({"trial": label, "success": run["success"],
                              "seconds": round(run["duration_seconds"], 3), "error": run.get("error")}), flush=True)
            (out / "raw.json").write_text(json.dumps(runs, indent=2) + "\n")
            if not run["success"]:
                raise SystemExit("trial failed; retained evidence in " + str(out / label))
    rows = []
    for run in runs:
        if not run["warmup"]:
            rows.append({"wm": run["wm"], "clients": run["clients"], "repeat": run["repeat"], **metrics(run, ticks)})
    with (out / "runs.csv").open("w") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    manifest["complete"] = True
    manifest["raw_sha256"] = digest(out / "raw.json")
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("COMPLETE " + str(out), flush=True)


if __name__ == "__main__":
    main()
