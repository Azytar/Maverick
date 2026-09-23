#!/usr/bin/env python3
"""A small terminal monitor for the Maverick showcase.

It is a real X11 client: it polls Maverick's public control socket and renders
the returned window tree in a terminal. It does not draw fake windows or read
private WM state.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import time


def query(binary: Path, name: str, topic: str) -> dict:
    result = subprocess.run(
        [str(binary), "query", topic, "--name", name],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=2,
        check=False,
        env=os.environ.copy(),
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or f"maverickctl exited {result.returncode}")
    return json.loads(result.stdout)


def rows(tree: dict, active_workspace: int) -> tuple[list[tuple[str, ...]], int, int, int]:
    result: list[tuple[str, ...]] = []
    columns = 0
    floats = 0
    camera = 0
    for monitor in tree.get("monitors", []):
        for workspace in monitor.get("workspaces", []):
            if int(workspace.get("index", -1)) != active_workspace:
                continue
            camera = int(workspace.get("scroll", 0))
            for column in workspace.get("columns", []):
                columns += 1
                for window in column.get("windows", []):
                    result.append(row(window, "tiled", monitor, workspace))
            floats += len(workspace.get("floats", []))
            for window in workspace.get("floats", []):
                result.append(row(window, "float", monitor, workspace))
    return result, columns, floats, camera


def row(window: dict, mode: str, monitor: dict, workspace: dict) -> tuple[str, ...]:
    geometry = window.get("geom") or [0, 0, 0, 0]
    app = window.get("instance") or window.get("class") or window.get("title") or "unnamed"
    title = window.get("title") or ""
    label = f"{app} — {title}" if title and title != app else app
    return (
        str(window.get("id", "")),
        mode,
        label[:34],
        f"{monitor.get('index', 0)}:{workspace.get('index', 0)}",
        " ".join(str(value) for value in geometry),
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Maverick live terminal monitor")
    parser.add_argument("--maverick-bin", type=Path, required=True)
    parser.add_argument("--name", default="showcase")
    parser.add_argument("--interval", type=float, default=0.8)
    args = parser.parse_args()
    while True:
        try:
            tree = query(args.maverick_bin, args.name, "tree")
            state = query(args.maverick_bin, args.name, "state")
            active_workspace = int(state.get("monitors", [{}])[0].get("active_ws", 0))
            windows, columns, floats, camera = rows(tree, active_workspace)
            focused = state.get("monitors", [{}])[0].get("focused_title") or "—"
            print("\033[2J\033[H", end="")
            print("MAVERICK / LIVE WINDOW TREE", flush=True)
            print(f"layout column   columns {columns}   floats {floats}   camera {camera}px", flush=True)
            print(f"focus {focused[:70]}", flush=True)
            print("-" * 92, flush=True)
            print(f"{'XID':<10} {'MODE':<6} {'APPLICATION':<34} {'M/W':<5} GEOMETRY", flush=True)
            for window in windows:
                print(
                    f"{window[0]:<10} {window[1]:<6} {window[2]:<34} "
                    f"{window[3]:<5} {window[4]}",
                    flush=True,
                )
            print("-" * 92, flush=True)
            print("query tree · public IPC · no synthetic geometry", flush=True)
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            print("\033[2J\033[H", end="")
            print(f"MAVERICK / IPC UNAVAILABLE\n{error}", flush=True)
        time.sleep(max(0.2, args.interval))


if __name__ == "__main__":
    raise SystemExit(main())
