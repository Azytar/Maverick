#!/usr/bin/env python3
"""Compare two Maverick binaries using the unchanged external X11 WM harness."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import random
import shutil
import statistics
import subprocess
import sys


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, required=True,
                        help="directory containing the original run.py, probe.c and configs")
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=7)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 100:
        parser.error("repeats must be between 1 and 100")
    harness = args.harness.resolve()
    spec = importlib.util.spec_from_file_location("bench", harness / "run.py")
    bench = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bench)
    variants = {"base": args.base.resolve(), "candidate": args.candidate.resolve()}
    xvfb = shutil.which("Xvfb")
    if xvfb is None:
        parser.error("Xvfb is required")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    root = Path(__file__).resolve().parent.parent
    patch = subprocess.check_output([
        "git", "diff", "HEAD", "--", "src", "Cargo.toml", "Cargo.lock",
        "maverick-core", "maverick-sys", "maverick-x11", "maverick-toml",
    ], cwd=root)
    (out / "candidate.patch").write_bytes(patch)
    ticks = os.sysconf("SC_CLK_TCK")
    manifest = {
        "complete": False, "command": sys.argv,
        "binary_sha256": {name: digest(path) for name, path in variants.items()},
        "source_sha256": {
            str(path.relative_to(harness)): digest(path)
            for path in sorted(harness.rglob("*"))
            if path.is_file() and "__pycache__" not in str(path)
        },
        "source_revision": subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
        "candidate_patch_sha256": digest(out / "candidate.patch"),
        "runner_sha256": digest(__file__), "xvfb_sha256": digest(xvfb),
        "compiler_version": subprocess.check_output(
            ["cc", "--version"], text=True).splitlines()[0],
        "kernel": platform.platform(), "affinity": sorted(os.sched_getaffinity(0)),
        "clock_ticks": ticks, "library_path": os.environ.get("LD_LIBRARY_PATH"),
        "cpuinfo": Path("/proc/cpuinfo").read_text(),
        "cpu_max": Path("/sys/fs/cgroup/cpu.max").read_text(),
        "memory_max": Path("/sys/fs/cgroup/memory.max").read_text(),
        "parameters": {"clients": [10, 50], "focus": 500, "fullscreen": 40,
                       "idle_seconds": 2, "repeats": args.repeats,
                       "warmups": 1, "seed": 20261023},
    }
    save(out / "manifest.json", manifest)
    helper = out / "probe"
    subprocess.run([
        "cc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-Wshadow",
        "-Wformat=2", str(harness / "probe.c"), "-o", str(helper), "-lX11",
    ], check=True)
    workload = argparse.Namespace(xvfb=xvfb, focus=500, fullscreen=40, idle_seconds=2)
    binaries = {name: shutil.which(name) for name in bench.WMS}
    if binaries["bspwm"] is None:
        parser.error("the external harness requires bspwm in PATH to construct its environment")
    rows, raw = [], []
    rng = random.Random(20261023)
    # Shuffle the complete matrix each round; retain all warm-ups and failures.
    for repeat in range(-1, args.repeats):
        combinations = [(variant, n) for variant in variants for n in (10, 50)]
        rng.shuffle(combinations)
        for variant, clients in combinations:
            binaries["maverick"] = str(variants[variant])
            label = f"{repeat + 1:02d}-{variant}-{clients}"
            trial = bench.trial("maverick", clients, max(repeat, 0), repeat < 0,
                                workload, binaries, helper, out / label)
            trial["variant"] = variant
            raw.append(trial)
            save(out / "raw.json", raw)
            print(label, trial["success"], trial.get("error", ""), flush=True)
            if not trial["success"] or any(
                    process["returncode"] is None for process in trial["processes"]):
                raise SystemExit("trial failed: " + label)
            if repeat >= 0:
                rows.append({"variant": variant, "clients": clients, "repeat": repeat,
                             **bench.metrics(trial, ticks)})
    summary = []
    for clients in (10, 50):
        for metric in bench.metrics(raw[0], ticks):
            entry = {"clients": clients, "metric": metric}
            for variant in variants:
                values = [row[metric] for row in rows
                          if row["clients"] == clients and row["variant"] == variant]
                entry[variant] = None if any(v is None for v in values) else {
                    "median": statistics.median(values), "p25": bench.quantile(values, .25),
                    "p75": bench.quantile(values, .75), "runs": values,
                }
            summary.append(entry)
    save(out / "summary.json", summary)
    manifest.update(complete=True, raw_sha256=digest(out / "raw.json"))
    save(out / "manifest.json", manifest)


if __name__ == "__main__":
    main()
