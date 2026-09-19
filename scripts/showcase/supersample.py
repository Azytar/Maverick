#!/usr/bin/env python3
"""Phase-1 supersampling experiment driver (offline, read-only vs showcase.py)."""
import argparse
import hashlib
import importlib.util
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
_SPEC = importlib.util.spec_from_file_location(
    "showcase_baseline", ROOT / "scripts/showcase/showcase.py")
showcase = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(showcase)

FINAL = (1440, 900)
VARIANTS = {
    "base": (1440, 900, 11, None, "baseline control 1440x900"),
    "v2":   (2880, 1800, 22, None, "2x supersampling"),
    "v25":  (3600, 2250, 28, None, "2.5x (11*2.5=27.5->28)"),
    "v4k":  (3840, 2400, 29, None, "4K-class (11*2.6667->29)"),
    "v2d144": (2880, 1800, 11, 144, "2x + explicit -dpi 144 (fs 11.5->11)"),
    "v2d192": (2880, 1800, 9, 192, "2x + explicit -dpi 192 (fs 8.6->9)"),
}
SCENES = showcase.SCENES


class XSession(showcase.Session):
    def __init__(self, scene, binaries, output, internal, font_px, dpi):
        super().__init__(scene, binaries, output)
        self.internal = tuple(internal)
        self.font_px = int(font_px)
        self.dpi = dpi
        self.scale = internal[0] / FINAL[0]

    def start(self):
        import os
        read_fd, write_fd = os.pipe()
        try:
            args = ["Xephyr", "-displayfd", str(write_fd), "-screen",
                    f"{self.internal[0]}x{self.internal[1]}",
                    "-nolisten", "tcp", "-ac",
                    "+extension", "GLX", "+extension", "Composite",
                    "+extension", "DAMAGE"]
            if self.dpi:
                args += ["-dpi", str(self.dpi)]
            self.xephyr = self.spawn(args, "xephyr", pass_fds=(write_fd,))
            os.close(write_fd)
            write_fd = None
            os.set_blocking(read_fd, False)
            display = showcase.wait_for(
                "Xephyr display allocation",
                lambda: os.read(read_fd, 64).strip())
        finally:
            os.close(read_fd)
            if write_fd is not None:
                os.close(write_fd)
        self.env["DISPLAY"] = ":" + display.decode()
        showcase.wait_for(
            "X11 readiness",
            lambda: showcase.run(["xdpyinfo"], self.env, False).returncode == 0)
        s = self.scale
        rr = showcase.RADIUS
        ro = ("compositor", "rounded", "tiled-spacing", "floating-scroll",
              "fullscreen-decoration", *showcase.FOCUS_SCENES,
              *showcase.FULLSCREEN_SCENES, *showcase.TRANSITION_SCENES)
        gi = 6 if self.scene == "tiled-spacing" else 4
        go = 10 if self.scene == "tiled-spacing" else 8
        config = self.path / "config.toml"
        config.write_text(
            "[general]\n"
            f"border_width = {max(1, round(1 * s))}\n"
            f"corner_radius = {round(rr * s) if self.scene in ro else 0}\n"
            f"gaps_inner = {round(gi * s)}\n"
            f"gaps_outer = {round(go * s)}\n"
            "column_width = 0.31\n"
            "n_tags = 3\n"
            "focus_mouse = false\n"
            "warp_cursor = false\n"
            "[colors]\n"
            "normal = 0x45475a\n"
            "focused = 0x89b4fa\n"
            "[animations]\n"
            f"enabled = {str(self.scene == 'fullscreen-transition-gl').lower()}\n"
            "[compositor]\n"
            f"enabled = {str(self.scene in showcase.GL_SCENES).lower()}\n"
            'backend = "opengl"\n'
            "fullscreen_bypass = false\n"
            "[autostart]\n"
            'commands = [["/usr/bin/true"]]\n'
            "[[rules]]\n"
            'instance = "showcase3"\n'
            f"opacity = {0.78 if self.scene == 'compositor' else 1.0}\n")
        if self.scene in (*showcase.FOCUS_SCENES, *showcase.FULLSCREEN_SCENES):
            fh = config.open("a")
            fh.write('\n[[rules]]\ninstance = "showcase6"\nfloat = true\n')
            fh.close()
        showcase.run([str(self.binaries / "maverick"), "--check-config",
                      str(config)], self.env)
        self.wm = self.spawn([str(self.binaries / "maverick"), "--config",
                              str(config), "--name", "showcase"], "wm")
        showcase.wait_for("Maverick IPC startup",
                          lambda: self.state().get("monitors"))
        showcase.run(["xsetroot", "-solid", "#11111b"], self.env)
        print(f"{self.scene}: WM on {self.env['DISPLAY']} pid {self.wm.pid} "
              f"internal={self.internal[0]}x{self.internal[1]} "
              f"font={self.font_px} dpi={self.dpi or 'default'}", flush=True)

    def terminal(self, number, title, source):
        name = f"showcase{number}"
        if self.scene in showcase.FULLSCREEN_SCENES:
            background = {2: "#542638", 3: "#245447"}.get(number, "#1e1e2e")
        else:
            background = "#1e1e2e"
        self.spawn(["xterm", "-name", name, "-class", "Showcase",
                    "-title", title, "-fa", "DejaVu Sans Mono",
                    "-fs", str(self.font_px), "-bg", background,
                    "-fg", "#cdd6f4", "-cr", background, "+sb", "-b",
                    str(max(1, round(18 * self.scale))),
                    "-geometry", "72x36", "-e", sys.executable,
                    str(Path(showcase.__file__).resolve()),
                    "--client", title, source], name)
        return showcase.wait_for(
            f"client {number}",
            lambda: showcase.run(
                ["xdotool", "search", "--onlyvisible", "--classname",
                 "^" + name + "$"], self.env).stdout.strip().splitlines())[0]

    def capture_hires(self, windows):
        geometry = self.stable(windows)
        path = self.output / f"{self.scene}-hires.png"
        showcase.run(["import", "-display", self.env["DISPLAY"],
                      "-window", "root", str(path)], self.env)
        dimensions = showcase.run(
            ["identify", "-format", "%wx%h", str(path)]).stdout
        expect = f"{self.internal[0]}x{self.internal[1]}"
        if dimensions != expect:
            raise RuntimeError(
                f"Unexpected hires dimensions: {dimensions} vs {expect}")
        print(f"  captured hires {path} ({dimensions})", flush=True)
        return geometry


def downsample(hires, final):
    t0 = time.monotonic()
    showcase.run(["magick", str(hires), "-filter", "Lanczos",
                  "-resize", f"{FINAL[0]}x{FINAL[1]}!",
                  "-colorspace", "sRGB", "-strip", str(final)])
    return time.monotonic() - t0


def font_record(face, px):
    try:
        out = showcase.run(["fc-match", "-v", f"{face}:size={px}"]).stdout
    except RuntimeError as error:
        return f"fc-match failed: {error}"
    keep = [ln for ln in out.splitlines()
            if any(k in ln for k in ("family:", "fullname:", "file:",
                                     "pixelsize", "antialias", "hint",
                                     "dpi", "foundry", "style:"))]
    return "\n".join(keep[:16])


def readme_sizes(final_png, outdir, scene):
    thumbs = {}
    for width in (720, 800, 850, 900):
        dest = outdir / f"{scene}-w{width}.png"
        showcase.run(["magick", str(final_png), "-filter", "Lanczos",
                      "-resize", f"{width}x", "-colorspace", "sRGB",
                      "-strip", str(dest)])
        thumbs[width] = dest.stat().st_size
    return thumbs


def run_scene(variant, scene, binaries, outdir, evidence):
    iw, ih, font_px, dpi, label = VARIANTS[variant]
    session = XSession(scene, binaries, outdir, (iw, ih), font_px, dpi)
    hires_time = down_time = 0.0
    try:
        session.start()
        windows = [session.terminal(1, "01 / Configuration", "config/config.toml")]
        pixel_checks = None
        trans_checks = None
        s = session.scale
        if scene in showcase.TRANSITION_SCENES:
            windows.append(session.terminal(2, "B / Fullscreen presentation", "Cargo.toml"))
        elif scene not in showcase.FULLSCREEN_SCENES:
            windows.extend([session.terminal(2, "02 / Workspace", "Cargo.toml"),
                            session.terminal(3, "03 / X11 client", "tests/realwin.c")])
            session.action("focus:left")
            session.action("focus:left")
            session.action(f"grow_col:{round(-994 * s)}")
            session.action("focus:right")
            session.action("focus:right")
            session.stable(windows)
        if scene in showcase.TRANSITION_SCENES:
            trans_checks = showcase.fullscreen_transition(session, windows, evidence)
        elif scene in showcase.FULLSCREEN_SCENES:
            pixel_checks = showcase.fullscreen_new_window(session, windows, evidence)
        elif scene in showcase.FOCUS_SCENES:
            pixel_checks = showcase.rounded_focus(session, windows, evidence)
        elif scene == "navigation":
            windows.extend([session.terminal(4, "04 / Rendering", "maverick-gl/Cargo.toml"),
                            session.terminal(5, "05 / IPC", "maverick-sys/Cargo.toml")])
            session.action("focus:left")
            session.action("focus:left")
        elif scene in ("floating", "compositor"):
            session.action("toggle_float")
            session.stable(windows)
            showcase.run(["xdotool", "windowsize", windows[-1],
                          str(round(680 * s)), str(round(510 * s))], session.env)
            session.stable(windows)
            showcase.run(["xdotool", "windowmove", windows[-1],
                          str(round(590 * s)), str(round(290 * s))], session.env)
        elif scene == "fullscreen":
            session.action("toggle_fullscreen")
        elif scene == "floating-scroll":
            session.action("toggle_float")
            session.stable(windows)
            showcase.run(["xdotool", "windowsize", windows[-1],
                          str(round(480 * s)), str(round(360 * s))], session.env)
            session.stable(windows)
            showcase.run(["xdotool", "windowmove", windows[-1],
                          str(round(480 * s)), str(round(270 * s))], session.env)
            before = showcase.float_geometry(session, windows[-1])
            session.action("focus:left")
            session.action("focus:left")
            session.stable(windows)
            after = showcase.float_geometry(session, windows[-1])
            if before is None or before != after:
                raise RuntimeError(f"float moved: {before} -> {after}")
            print("  floating isolation verified during scroll", flush=True)
        elif scene == "fullscreen-decoration":
            session.action("toggle_fullscreen")
            session.stable(windows)
            session.action("toggle_fullscreen")
            session.stable(windows)
        else:
            session.action("focus:left")
        if scene in showcase.GL_SCENES:
            showcase.wait_for("actual GL renderer", session.gl_active)
        t0 = time.monotonic()
        geometry = session.capture_hires(windows)
        hires_time = time.monotonic() - t0
        final = outdir / f"{scene}.png"
        down_time = downsample(outdir / f"{scene}-hires.png", final)
        dimensions = showcase.run(
            ["identify", "-format", "%wx%h", str(final)]).stdout
        assert dimensions == f"{FINAL[0]}x{FINAL[1]}", dimensions
        thumbs = readme_sizes(final, outdir, scene)
        record = {"scene": scene, "variant": variant, "label": label,
                  "internal": f"{iw}x{ih}",
                  "dpi": dpi or "default", "terminal_font_px": font_px,
                  "final": dimensions,
                  "capture_s": round(hires_time, 3),
                  "downsample_s": round(down_time, 3),
                  "hires_bytes": (outdir / f"{scene}-hires.png").stat().st_size,
                  "final_bytes": final.stat().st_size,
                  "final_sha256": hashlib.sha256(final.read_bytes()).hexdigest(),
                  "readme_thumbs_bytes": thumbs,
                  "geometry": geometry,
                  "state": session.state(), "tree": session.tree(),
                  "gl_active": session.gl_active()}
        if trans_checks is not None:
            record["transition_checks"] = trans_checks
        if pixel_checks is not None:
            record["pixel_checks"] = pixel_checks
        (evidence / f"{variant}-{scene}.json").write_text(
            json.dumps(record, indent=2) + "\n")
        (outdir / f"font-{scene}.txt").write_text(
            f"face=DejaVu Sans Mono px={font_px} dpi={dpi or 'default'}\n"
            + font_record("DejaVu Sans Mono", font_px) + "\n")
        print(f"  final {final} ({dimensions}, {record['final_bytes']}B)", flush=True)
        return record
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser(description="Phase-1 supersample driver")
    parser.add_argument("--variant", choices=(*VARIANTS, "all"), default="v2")
    parser.add_argument("--scene", choices=(*SCENES, "all"), default="tiling")
    parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
    parser.add_argument("--out-root", type=Path, default=Path("/tmp/mav-phase1"))
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args()
    if args.list:
        for name, spec in VARIANTS.items():
            print(f"{name:8} {spec[0]}x{spec[1]} font={spec[2]} "
                  f"dpi={spec[3] or 'default'}  # {spec[4]}")
        return
    variants = list(VARIANTS) if args.variant == "all" else [args.variant]
    scenes = list(SCENES) if args.scene == "all" else [args.scene]
    binaries = args.bin_dir.resolve()
    for binary in ("maverick", "maverickctl"):
        if not (binaries / binary).exists():
            parser.error(f"Missing {binaries / binary}")
    import os
    if not os.environ.get("DISPLAY"):
        parser.error("DISPLAY needed for Xephyr")
    rows = []
    for variant in variants:
        outdir = args.out_root / variant
        outdir.mkdir(parents=True, exist_ok=True)
        for scene in scenes:
            t0 = time.monotonic()
            record = run_scene(variant, scene, binaries, outdir, outdir)
            record["wall_s"] = round(time.monotonic() - t0, 2)
            rows.append(record)
    summary = {"rows": [
        {k: r.get(k) for k in ("scene", "variant", "internal", "dpi",
                               "terminal_font_px", "final", "wall_s",
                               "hires_bytes", "final_bytes", "final_sha256")}
        for r in rows]}
    for variant in variants:
        (args.out_root / variant / "report.json").write_text(
            json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
