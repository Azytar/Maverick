#!/usr/bin/env python3
"""Reproducible Maverick showcase entry point."""
from __future__ import annotations

import argparse
from pathlib import Path
import sys

from harness import DEFAULT_EVIDENCE, DEFAULT_SIZE, ROOT, Session, ShowcaseError, parse_size
from scenes import SCENES, Showcase


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run the isolated Maverick column, scroll, real-client and float showcase."
    )
    parser.add_argument(
        "scene",
        nargs="?",
        default="all",
        choices=(*SCENES, "all"),
        help="capture one scene (its prerequisites are run first) or the complete story",
    )
    parser.add_argument(
        "--size",
        default=f"{DEFAULT_SIZE[0]}x{DEFAULT_SIZE[1]}",
        help="nested Xephyr size, for example 1920x1080 (default: %(default)s)",
    )
    parser.add_argument(
        "--bin-dir",
        type=Path,
        default=ROOT / "target/debug",
        help="directory containing maverick and maverickctl (default: %(default)s)",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "docs/screenshots",
        help="directory for the authentic root-window captures (default: %(default)s)",
    )
    parser.add_argument(
        "--evidence",
        type=Path,
        default=DEFAULT_EVIDENCE,
        help="directory for JSON state/tree evidence (default: %(default)s)",
    )
    parser.add_argument("--list", action="store_true", help="list scenes and exit")
    args = parser.parse_args()
    if args.list:
        print("scenes: " + ", ".join(SCENES))
        return 0
    try:
        size = parse_size(args.size)
        binaries = args.bin_dir.resolve()
        session = Session(binaries, args.output.resolve(), args.evidence.resolve(), size)
        try:
            session.start()
            target = "hero" if args.scene == "all" else args.scene
            Showcase(session).run_until(target)
        finally:
            session.close()
    except (OSError, RuntimeError, ValueError, ShowcaseError) as error:
        print(f"showcase: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
