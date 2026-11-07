#!/usr/bin/env python3
"""Render only complete, validated measurements; no synthetic or fallback data."""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import statistics

from run import WMS, PHASES, metrics, quantile

LABELS = {"maverick": "Maverick", "i3": "i3", "bspwm": "bspwm", "openbox": "Openbox", "dwm": "dwm"}


def load_results(directory):
    manifest = json.loads((directory / "manifest.json").read_text())
    raw_bytes = (directory / "raw.json").read_bytes()
    if manifest.get("schema") != 1 or manifest.get("complete") is not True:
        raise ValueError("benchmark is incomplete or has an unknown schema")
    if hashlib.sha256(raw_bytes).hexdigest() != manifest.get("raw_sha256"):
        raise ValueError("raw data checksum mismatch")
    raw = json.loads(raw_bytes)
    parameters = manifest["parameters"]
    expected = {(wm, n, i, warmup) for wm in WMS for n in parameters["clients"]
                for warmup, repetitions in ((False, parameters["repeats"]), (True, parameters["warmups"]))
                for i in range(repetitions)}
    seen, measured = set(), []
    for run in raw:
        key = (run["wm"], run["clients"], run["repeat"], run["warmup"])
        if key not in expected or key in seen or run.get("success") is not True:
            raise ValueError("duplicate, unexpected, or failed trial")
        seen.add(key)
        if not run.get("processes") or any(p["returncode"] is None for p in run["processes"]):
            raise ValueError("unreaped process in trial")
        for phase in PHASES:
            values = run["samples"][phase]
            if values is None:
                if (run["wm"] != "dwm" or phase != "focus" or run.get("focus_supported") is not False
                        or run.get("focus_capability", {}).get("urgent_only") is not True):
                    raise ValueError("unexplained missing samples")
                continue
            count = run["clients"] if phase in ("map", "destroy") else parameters[phase]
            if len(values) != count or any(type(v) not in (int, float) or not math.isfinite(v) or v < 0 for v in values):
                raise ValueError("invalid or missing operation samples")
        if not run["warmup"]:
            measured.append(run)
    if seen != expected:
        raise ValueError("missing trials")
    return manifest, measured


def summarize(manifest, runs):
    groups = {}
    for run in runs:
        row = metrics(run, manifest["clock_ticks_per_second"])
        for metric, value in row.items():
            groups.setdefault((run["wm"], run["clients"], metric), []).append(value)
    summary = []
    for (wm, clients, metric), values in groups.items():
        valid = [v for v in values if v is not None]
        if valid and len(valid) != len(values):
            raise ValueError("inconsistent capability between repetitions")
        summary.append({"wm": wm, "clients": clients, "metric": metric, "runs": len(values),
                        "median": statistics.median(valid) if valid else None,
                        "p25": quantile(valid, .25) if valid else None,
                        "p75": quantile(valid, .75) if valid else None,
                        "minimum": min(valid) if valid else None,
                        "maximum": max(valid) if valid else None})
    return summary


def render(directory, manifest, summary):
    os.environ.setdefault("MPLCONFIGDIR", str(directory / "matplotlib-cache"))
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.ticker import MaxNLocator

    small, large = min(manifest["parameters"]["clients"]), max(manifest["parameters"]["clients"])
    panels = [("Gestión de ventanas", "map_p50_ms", small, "ms · p50"),
              ("Gestión de ventanas", "map_p50_ms", large, "ms · p50"),
              ("Cola de gestión", "map_p95_ms", large, "ms · p95"),
              ("Cambio de foco", "focus_p50_ms", large, "ms · p50"),
              ("Cola de foco", "focus_p95_ms", large, "ms · p95"),
              ("Entrada y salida de fullscreen", "fullscreen_p50_ms", large, "ms · p50"),
              ("Retirada de ventanas", "destroy_p50_ms", large, "ms · p50"),
              ("Memoria residente del gestor", "rss_mib", large, "MiB · RSS"),
              ("CPU del gestor al gestionar", "map_cpu_ms_per_100_ops", large, "ms CPU / 100 ventanas")]
    lookup = {(x["wm"], x["clients"], x["metric"]): x for x in summary}
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
                         "axes.spines.top": False, "axes.spines.right": False,
                         "axes.spines.left": False, "svg.fonttype": "none"})
    fig, axes = plt.subplots(3, 3, figsize=(16, 12), facecolor="white")
    for ax, (title, metric, clients, unit) in zip(axes.flat, panels):
        rows = [lookup[(wm, clients, metric)] for wm in WMS]
        valid = [r for r in rows if r["median"] is not None]
        upper = max((r["p75"] for r in valid), default=1)
        ceiling = (upper or 1) * 1.35
        for i, (wm, row) in enumerate(zip(WMS, rows)):
            color = "#3570ff" if wm == "maverick" else "#dfe3e8"
            if row["median"] is None:
                ax.text(i, ceiling * .04, "No\ncompatible¹", ha="center", va="bottom", color="#737a86", fontsize=9)
                continue
            value = row["median"]
            ax.bar(i, value, width=.68, color=color, zorder=3)
            ax.errorbar(i, value, yerr=[[value - row["p25"]], [row["p75"] - value]],
                        color="#243247", capsize=3, lw=1, zorder=4)
            label = f"{value:.0f}" if value >= 100 else f"{value:.2f}" if value >= 1 else f"{value:.3f}"
            ax.text(i, row["p75"] + ceiling * .025, label, ha="center", va="bottom", fontsize=10,
                    color="#1f2b40", fontweight="bold" if wm == "maverick" else "normal")
        ax.set_title(f"{title}\n{clients} ventanas · {unit}", loc="left", fontsize=12, pad=16)
        ax.set_xticks(range(len(WMS)), [LABELS[wm] for wm in WMS], fontsize=10)
        ax.tick_params(axis="x", length=0, pad=8)
        ax.tick_params(axis="y", length=0, colors="#89909c", labelsize=8)
        ax.set_ylim(0, ceiling)
        ax.set_xlim(-.6, len(WMS) - .4)
        ax.yaxis.set_major_locator(MaxNLocator(4))
        ax.grid(axis="y", color="#edf0f4", zorder=0)
        ax.set_axisbelow(True)
        ax.spines["bottom"].set_color("#c6cbd4")
    n = manifest["parameters"]["repeats"]
    fig.suptitle("Maverick frente a otros gestores X11", x=.055, y=.985, ha="left",
                 fontsize=25, fontweight="bold", color="#142033")
    fig.text(.055, .944, f"Mediciones reales · menor es mejor · mediana de {n} ejecuciones · bigotes: P25–P75 entre ejecuciones",
             fontsize=12, color="#586579")
    tick_ms = 1000 / manifest["clock_ticks_per_second"]
    fig.text(.055, .033, f"Xvfb 1280×800 · sin compositor · mismo cliente C/EWMH · entorno virtualizado · CPU: resolución de {tick_ms:g} ms",
             fontsize=9, color="#657184")
    fig.text(.055, .015, "¹ dwm estándar marca urgencia ante _NET_ACTIVE_WINDOW; no cambia el foco. No se representa como cero. No mide FPS ni latencia a pantalla.",
             fontsize=9, color="#657184")
    fig.text(.945, .033, manifest["utc"][:10], ha="right", fontsize=9, color="#657184")
    fig.subplots_adjust(left=.055, right=.97, bottom=.09, top=.88, hspace=.62, wspace=.23)
    for extension in ("png", "svg", "pdf"):
        fig.savefig(directory / ("comparison." + extension), dpi=180, facecolor="white")
    plt.close(fig)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    manifest, runs = load_results(args.directory)
    summary = summarize(manifest, runs)
    (args.directory / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    with (args.directory / "summary.csv").open("w") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(summary[0]))
        writer.writeheader()
        writer.writerows(summary)
    render(args.directory, manifest, summary)
    print(args.directory / "comparison.png")


if __name__ == "__main__":
    main()
