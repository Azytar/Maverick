#!/usr/bin/env python3
"""Run an inventoried C probe on an owned Xvfb, retaining evidence on failure.

The default probe tests Wine-style focus offers. Select fullscreen-lifecycle-probe.c
and --complete fullscreen-lifecycle for lifecycle, publication, or property cases.
XInput2 and actual keyboard delivery remain covered by xvfb-input.py.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def line(pipe):
    if not select.select([pipe], [], [], 10)[0]:
        raise RuntimeError("X server startup timed out")
    value = pipe.readline().strip()
    if not value:
        raise RuntimeError("X server exited before reporting its display")
    return value


def wait_for(predicate, process, message):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(message + ": process exited")
        if predicate():
            return
        # Poll a condition; this interval is not an assumed settling delay.
        time.sleep(0.02)
    raise RuntimeError(message + ": deadline exceeded")


def inventory(text):
    names = json.loads(text)
    if (not isinstance(names, list) or not 1 <= len(names) <= 128
            or any(not isinstance(n, str) or not n or len(n) > 120 for n in names)
            or len(set(names)) != len(names)):
        raise ValueError("probe inventory must contain unique nonempty check names")
    return names


def validate_results(stdout, expected, complete):
    lines = stdout.splitlines()
    marker = "COMPLETE " + complete
    if not lines or lines[-1] != marker or lines.count(marker) != 1:
        raise ValueError("missing, repeated, or misplaced completion marker")
    records = []
    seen = set()
    for raw in lines:
        if not raw.startswith("RESULT "):
            continue
        detail = json.loads(raw[7:])
        if (not isinstance(detail, dict) or not isinstance(detail.get("name"), str)
                or type(detail.get("passed")) is not bool):
            raise ValueError("result requires a name and a JSON boolean passed field")
        name = detail["name"]
        if name not in expected or name in seen:
            raise ValueError("unexpected or repeated check: " + name)
        if name != expected[len(records)]:
            raise ValueError("out-of-order check: " + name)
        seen.add(name)
        records.append(detail)
    missing = set(expected) - seen
    if missing:
        raise ValueError("missing checks: " + ", ".join(sorted(missing)))
    return records


def file_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", type=Path, default=ROOT / "target/debug/maverick")
    parser.add_argument("--wm-revision")
    parser.add_argument("--probe", type=Path, default=ROOT / "tests/wine-focus-probe.c")
    parser.add_argument("--complete", default="wine-focus")
    parser.add_argument("--case", choices=("lifecycle", "publication", "properties"))
    parser.add_argument("--honor-initial-state", action="store_true")
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--config-via-xdg", action="store_true")
    parser.add_argument("--no-trace", action="store_true")
    parser.add_argument("--observer", action="store_true")
    parser.add_argument("--sanitize", action="store_true", help="compile the C probe with ASan and UBSan")
    args = parser.parse_args()
    started = time.monotonic()
    out = (args.evidence or Path(tempfile.mkdtemp(prefix="wine-focus-", dir="/tmp"))).resolve()
    out.mkdir(parents=True, exist_ok=True)
    print(f"EVIDENCE {out}", flush=True)
    failures, processes = [], []
    error = None
    expected = []
    display = None
    compiler = ["cc", "-Wall", "-Wextra", "-Werror", "-Wshadow", "-Wformat=2"]
    if args.sanitize:
        compiler += ["-fsanitize=address,undefined", "-fno-omit-frame-pointer", "-g"]
    helper = out / "probe"
    compiler += [str(args.probe.resolve()), "-o", str(helper), "-lX11"]
    probe_args = ["--case", args.case] if args.case else []
    env = {k: v for k, v in os.environ.items() if not k.startswith("MAVERICK_")
           and k not in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XAUTHORITY")}
    env["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=/nonexistent/maverick-test-bus"
    if not args.no_trace:
        env.update(MAVERICK_TRACE="1", MAVERICK_TRACE_PATH=str(out / "wm.trace"))
    env["MAVERICK_LOG"] = "warn"
    if args.sanitize:
        env["ASAN_OPTIONS"] = "detect_leaks=1:abort_on_error=1"
        env["UBSAN_OPTIONS"] = "halt_on_error=1:print_stacktrace=1"
    wm_hash = probe_hash = None
    try:
        wm_hash, probe_hash = file_hash(args.wm), file_hash(args.probe)
        if not args.wm.is_file() or not os.access(args.wm, os.X_OK):
            raise RuntimeError("WM binary is missing or not executable: " + str(args.wm))
        subprocess.run(compiler, check=True, timeout=60, capture_output=True, text=True)
        description = subprocess.run([str(helper), "--describe", *probe_args], env=env,
                                     capture_output=True, text=True, timeout=10, check=True)
        expected = inventory(description.stdout)
        # Runtime paths stay short even when the evidence path exceeds SUN_LEN.
        with tempfile.TemporaryDirectory(prefix="mprobe-", dir="/tmp") as runtime:
            for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
                path = Path(runtime) / key
                path.mkdir(mode=0o700)
                env[key] = str(path)
            config = out / "config.toml"
            config.write_text("[general]\nfocus_mouse = false\nwarp_cursor = false\n"
                              f"honor_initial_state = {str(args.honor_initial_state).lower()}\n"
                              "[autostart]\ncommands = []\n")
            if args.config_via_xdg:
                installed = Path(env["XDG_CONFIG_HOME"]) / "maverick/config.toml"
                installed.parent.mkdir()
                installed.write_text(config.read_text())
            try:
                with (out / "server.log").open("w") as server_log, (out / "wm.log").open("w") as logs, (out / "observer.log").open("w") as observation:
                    server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "1280x800x24",
                                               "-nolisten", "tcp", "-noreset"], stdout=subprocess.PIPE,
                                              stderr=server_log, text=True, env=env)
                    processes.append(server)
                    display = env["DISPLAY"] = ":" + line(server.stdout)
                    wm = subprocess.Popen([str(args.wm.resolve())] + ([] if args.config_via_xdg else ["--config", str(config)]),
                                          env=env, stdout=logs, stderr=logs)
                    processes.append(wm)

                    def ready():
                        r = subprocess.run(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env=env,
                                           capture_output=True, text=True, timeout=3)
                        return r.returncode == 0 and "window id #" in r.stdout

                    wait_for(ready, wm, "WM did not claim its private display")
                    if args.observer:
                        observer = subprocess.Popen([str(helper), "--observe", "60"], env=env,
                                                    stdout=observation, stderr=observation)
                        processes.append(observer)
                        wait_for(lambda: (out / "observer.log").read_text().startswith("READY observer\n"),
                                 observer, "observer did not acknowledge its subscription")
                    # Stream to files so timeout/crash preserves partial evidence.
                    with (out / "probe.stdout").open("w") as stdout, (out / "probe.stderr").open("w") as stderr:
                        run = subprocess.run([str(helper), *probe_args], env=env, stdout=stdout,
                                             stderr=stderr, text=True, timeout=60)
                    if wm.poll() is not None:
                        failures.append("wm-exited")
                    if server.poll() is not None:
                        failures.append("server-exited")
                    if args.observer and observer.poll() is not None:
                        failures.append("observer-exited")
                    if run.returncode != 0:
                        failures.append("probe-exit-status:" + str(run.returncode))
                    stdout = (out / "probe.stdout").read_text()
                    for detail in validate_results(stdout, expected, args.complete):
                        print(json.dumps(detail), flush=True)
                        if not detail["passed"]:
                            failures.append(detail["name"])
            finally:
                for process in reversed(processes):
                    if process.poll() is None:
                        process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
    except Exception as exc:
        error = str(exc)
        failures.append("harness-error")
        (out / "error.log").write_text(error + "\n")
        if isinstance(exc, subprocess.CalledProcessError):
            (out / "error.stdout").write_text(exc.stdout or "")
            (out / "error.stderr").write_text(exc.stderr or "")
        print("ERROR " + error, file=sys.stderr)
    finally:
        revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                                  capture_output=True, text=True).stdout.strip()
        (out / "manifest.json").write_text(json.dumps({
            "command": sys.argv, "harness_commit": revision, "wm_revision": args.wm_revision,
            "wm": str(args.wm.resolve()), "wm_sha256": wm_hash,
            "probe_sha256": probe_hash, "compiler": compiler,
            "expected_checks": expected, "case": args.case, "display": display,
            "publication_rounds": env.get("AUDIT_PUBLICATION_ROUNDS"),
            "publication_only": env.get("AUDIT_PUBLICATION_ONLY"),
            "trace_enabled": not args.no_trace, "sanitize": args.sanitize,
            "processes": [{"pid": p.pid, "returncode": p.returncode} for p in processes],
            "duration_seconds": time.monotonic() - started,
            "failures": failures, "exception": error or (str(sys.exc_info()[1]) if sys.exc_info()[1] else None),
        }, indent=2) + "\n")
    print(json.dumps({"failures": failures}), flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
