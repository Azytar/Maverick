#!/usr/bin/env python3
"""Input-saturation measurement harness for Maverick (test-only, Agent B).

Brings up an isolated Xephyr display, the WM, and N managed clients; drives
synthetic XTEST input at a controlled rate; and samples the WM's CPU, RSS, FD
count and control-plane round-trip latency throughout, before, during and after
the burst.

Three independent instrumentation channels are combined, each with an
untraced control, because any one of them is confounded:

  ring   MAVERICK_TRACE=1 -- the in-WM trace ring buffer. The ONLY
         source of "which X11 events did the WM receive, and when did it
         process them", so it is the source of the amplification factor and of
         the queueing-delay trend. It is also the most perturbing (one 448-byte
         record memcpy per event), so it is never used for CPU/RSS/latency.
  wire   LD_PRELOAD=xwire.so -- captures the raw X11 request/reply/event stream
         off the WM's socket. Source of request / reply / flush counts. Adds one
         buffered 16-byte header per read/write, so it is far cheaper than the
         ring, but it still perturbs: every run that reports wire numbers is
         paired with a no-wire control.
  macro  cargo feature `input-trace,window-trace` -- eprintln! per focus() /
         arrange / reconcile. The only source of focus()/arrange/reconcile
         counts, and by far the most perturbing (a write(2) to stderr per line,
         synchronised against a pipe). Never used for latency or CPU.

The X server's event-time clock is calibrated against CLOCK_MONOTONIC once per
run with tools/xtimecal so the queueing delay is a real duration rather than a
constant-offset delta.

Usage:
  saturation.py --scenario NAME --mode MODE --rate R --duration S
                [--clients 6] [--out DIR] [--display :97]
                [--wm PATH] [--ctl PATH] [--ring on|off] [--wire on|off]
                [--macro on|off] [--baseline-s 3] [--recovery-s 30]

  saturation.py --suite --out DIR          # the full scenario matrix

Per scenario it writes DIR/NAME/:
  samples.csv        250 ms sample series (cpu, rss, fds, ctl latency, x rtt)
  summary.json       every measured number for the scenario
  ring.tsv           window-manager event-ring trace dump (if --ring on)
  wire.bin           raw X11 wire capture (if --wire on)
  wlog               WM stderr (macro trace + log)
  inject.txt         input injection log
  stress.txt         sender stdout
  nuke.log           post-run probe: does the WM still answer?
"""

import argparse
import csv
import json
import os
import re
import shutil
import signal
import struct
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
TOOLS = Path("/tmp/kilo")  # scratch tools: xtimecal, xwire.so, mgdwin

SCR_W, SCR_H = 1280, 720
FORBIDDEN_DISPLAYS = {":0", ":0.0", ""}

# X11 core event codes (bit 7 clear).
EV_NAME = {
    2: "KeyPress", 3: "KeyRelease", 4: "ButtonPress", 5: "ButtonRelease",
    6: "MotionNotify", 7: "EnterNotify", 8: "LeaveNotify", 9: "FocusIn",
    10: "FocusOut", 11: "KeymapNotify", 12: "Expose", 13: "GraphicsExpose",
    14: "NoExpose", 15: "VisibilityNotify", 16: "CreateNotify",
    17: "DestroyNotify", 18: "UnmapNotify", 19: "MapNotify", 20: "MapRequest",
    21: "ReparentNotify", 22: "ConfigureNotify", 23: "ConfigureRequest",
    24: "GravityNotify", 25: "ResizeRequest", 26: "CirculateNotify",
    27: "CirculateRequest", 28: "PropertyNotify", 29: "SelectionClear",
    30: "SelectionRequest", 31: "SelectionNotify", 32: "ColormapNotify",
    33: "ClientMessage", 34: "MappingNotify",
}
REQ_NAME = {
    1: "Create Window",
    2: "Change Window Attributes",
    3: "Get Window Attributes",
    4: "Destroy Window",
    5: "Destroy Subwindows",
    6: "Change Save Set",
    7: "Reparent Window",
    8: "Map Window",
    9: "Map Subwindows",
    10: "Unmap Window",
    11: "Unmap Subwindows",
    12: "Configure Window",
    13: "Circulate Window",
    14: "Get Geometry",
    15: "Query Tree",
    16: "Intern Atom",
    17: "Get Atom Name",
    18: "Change Property",
    19: "Delete Property",
    20: "Get Property",
    21: "List Properties",
    22: "Set Selection Owner",
    23: "Get Selection Owner",
    24: "Convert Selection",
    25: "Send Event",
    26: "Grab Pointer",
    27: "Ungrab Pointer",
    28: "Grab Button",
    29: "Ungrab Button",
    30: "Change Active Pointer Grab",
    31: "Grab Keyboard",
    32: "Ungrab Keyboard",
    33: "Grab Key",
    34: "Ungrab Key",
    35: "Allow Events",
    36: "Grab Server",
    37: "Ungrab Server",
    38: "Query Pointer",
    39: "Get Motion Events",
    40: "Translate Coordinates",
    41: "Warp Pointer",
    42: "Set Input Focus",
    43: "Get Input Focus",
    44: "Query Keymap",
    45: "Open Font",
    46: "Close Font",
    47: "Query Font",
    48: "Query Text Extents",
    49: "List Fonts",
    50: "List Fonts With Info",
    51: "Set Font Path",
    52: "Get Font Path",
    53: "Create Pixmap",
    54: "Free Pixmap",
    55: "Create Gc",
    56: "Change Gc",
    57: "Copy Gc",
    58: "Set Dashes",
    59: "Set Clip Rectangles",
    60: "Free Gc",
    61: "Clear Area",
    62: "Copy Area",
    63: "Copy Plane",
    64: "Poly Point",
    65: "Poly Line",
    66: "Poly Segment",
    67: "Poly Rectangle",
    68: "Poly Arc",
    69: "Fill Poly",
    70: "Poly Fill Rectangle",
    71: "Poly Fill Arc",
    72: "Put Image",
    73: "Get Image",
    74: "Poly_text8",
    75: "Poly_text16",
    76: "Image_text8",
    77: "Image_text16",
    78: "Create Colormap",
    79: "Free Colormap",
    80: "Copy Colormap And Free",
    81: "Install Colormap",
    82: "Uninstall Colormap",
    83: "List Installed Colormaps",
    84: "Alloc Color",
    85: "Alloc Named Color",
    86: "Alloc Color Cells",
    87: "Alloc Color Planes",
    88: "Free Colors",
    89: "Store Colors",
    90: "Store Named Color",
    91: "Query Colors",
    92: "Lookup Color",
    93: "Create Cursor",
    94: "Create Glyph Cursor",
    95: "Free Cursor",
    96: "Recolor Cursor",
    97: "Query Best Size",
    98: "Query Extension",
    99: "List Extensions",
    100: "Change Keyboard Mapping",
    101: "Get Keyboard Mapping",
    102: "Change Keyboard Control",
    103: "Get Keyboard Control",
    104: "Bell",
    105: "Change Pointer Control",
    106: "Get Pointer Control",
    107: "Set Screen Saver",
    108: "Get Screen Saver",
    109: "Change Hosts",
    110: "List Hosts",
    111: "Set Access Control",
    112: "Set Close Down Mode",
    113: "Kill Client",
    114: "Rotate Properties",
    115: "Force Screen Saver",
    116: "Set Pointer Mapping",
    117: "Get Pointer Mapping",
    118: "Set Modifier Mapping",
    119: "Get Modifier Mapping",
    127: "No Operation",
}


def name_for(mapping, code):
    return mapping.get(code, f"code{code}")


class Run:
    """One scenario's lifetime: Xephyr + WM + clients + probes, torn down hard."""

    def __init__(self, args):
        self.args = args
        self.display = args.display
        if self.display in FORBIDDEN_DISPLAYS:
            raise SystemExit(f"refusing to touch display {self.display!r}")
        self.outdir = Path(args.out) / (args.scenario or "suite")
        if self.outdir.exists():
            shutil.rmtree(self.outdir)
        self.outdir.mkdir(parents=True)
        # Hermetic runtime dir, as mandated: a fresh mktemp -d per run.
        self.xdg = Path(subprocess.run(
            ["mktemp", "-d", "/tmp/kilo/xdg-XXXXXXXX"],
            capture_output=True, text=True, check=True).stdout.strip())
        self.procs = []
        self.rows = []
        self.cal = None

    def env(self, **extra):
        e = os.environ.copy()
        e["DISPLAY"] = self.display
        e["XDG_RUNTIME_DIR"] = str(self.xdg)
        e["XDG_CONFIG_HOME"] = str(self.outdir / "config")
        e["XDG_STATE_HOME"] = str(self.outdir / "state")
        e["XDG_CACHE_HOME"] = str(self.outdir / "cache")
        e.pop("WAYLAND_DISPLAY", None)
        e["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=" + str(self.outdir / "no-bus")
        e.update(extra)
        return e

    def spawn(self, cmd, **kw):
        p = subprocess.Popen(cmd, **kw)
        self.procs.append(p)
        return p

    def popen(self, cmd, **kw):
        return subprocess.run(cmd, capture_output=True, text=True, **kw)

    def sh(self, cmd, **extra):
        return subprocess.run(cmd, shell=True, env=self.env(**extra),
                              capture_output=True, text=True, **kw_timeouts)

    # ── bring-up ─────────────────────────────────────────────────────────
    def start_xephyr(self):
        # Xephyr is itself a server: it needs a *host* X server to nest its
        # window into, and that host is the live session. Only Xephyr ever
        # talks to it -- the WM, the clients, the sender and every probe run
        # against the nested :97, which has its own server, its own event
        # queue and no relation to the live session's WM.
        host = os.environ.get("MAV_HOST_DISPLAY", ":0")
        log = open(self.outdir / "xephyr.log", "w")
        self.xephyr = self.spawn(
            ["Xephyr", self.display, "-screen", f"{SCR_W}x{SCR_H}", "-ac",
             "-resizeable", "-nolisten", "tcp",
             "+extension", "RANDR", "+extension", "Composite",
             "+extension", "XTEST", "+extension", "MIT-SHM"],
            env=self.env(DISPLAY=host, MAV_HOST_DISPLAY=host), stdout=log, stderr=log)
        for _ in range(200):
            if self.xephyr.poll() is not None:
                raise RuntimeError(
                    f"Xephyr exited rc={self.xephyr.returncode}: "
                    + (self.outdir / "xephyr.log").read_text()[-2000:])
            r = self.popen(["xprop", "-root"], env=self.env())
            if r.returncode == 0:
                return
            time.sleep(0.05)
        raise RuntimeError("Xephyr did not accept connections")

    def calibrate(self):
        r = self.popen([str(TOOLS / "xtimecal")], env=self.env())
        m = re.search(r"offset_ns=(-?\d+)", r.stdout)
        if not m:
            raise RuntimeError(f"xtimecal failed: {r.stdout!r} {r.stderr!r}")
        self.cal = int(m.group(1))
        return self.cal

    def start_wm(self):
        env = self.env(MAVERICK_LOG="debug")
        if self.args.ring == "on":
            env["MAVERICK_TRACE"] = "1"
            env["MAVERICK_TRACE_PATH"] = str(self.outdir / "ring.tsv")
        if self.args.wire == "on":
            env["LD_PRELOAD"] = str(TOOLS / "xwire.so")
            env["XWIRE_OUT"] = str(self.outdir / "wire.bin")
            env["XWIRE_MAX_BYTES"] = str(self.args.wire_max)
            # LD_PRELOAD is inherited by the autostart children the WM spawns;
            # without this they would each open the same capture file.
            env["XWIRE_ONLY"] = "maverick"
        wlog = open(self.outdir / "wlog", "w")
        self.wm = self.spawn([self.args.wm], env=env, stdout=wlog, stderr=wlog)
        # Readiness probe: `query tree` forces a round trip through the WM event
        # loop. (`state` answers from a cached snapshot and returns {} on an idle
        # WM, so it cannot be used to detect readiness.)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            r = self.popen([self.args.ctl, "query", "tree"], env=self.env())
            if r.returncode == 0 and r.stdout.strip():
                return
            if self.wm.poll() is not None:
                raise RuntimeError(
                    f"WM exited rc={self.wm.returncode}\n"
                    + (self.outdir / "wlog").read_text()[-2000:])
            time.sleep(0.1)
        raise RuntimeError("WM never answered `query tree`")

    def start_clients(self, n):
        pids = []
        for i in range(n):
            p = self.spawn([str(TOOLS / "mgdwin")], env=self.env(
                MGDTITLE=f"mgd{i}", MGDCLASS=f"mgd{i}"),
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            pids.append(p)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            r = self.popen([self.args.ctl, "query", "tree"], env=self.env())
            if r.returncode == 0:
                try:
                    tree = json.loads(r.stdout)
                except json.JSONDecodeError:
                    tree = None
                n_managed = count_tree(tree)
                if n_managed >= n:
                    return pids, n_managed
            time.sleep(0.2)
        return pids, n_managed

    # ── probes ───────────────────────────────────────────────────────────
    def proc_stat(self, pid):
        try:
            raw = open(f"/proc/{pid}/stat").read()
            parts = raw.rsplit(") ", 1)[1].split()
            return (int(parts[11]) + int(parts[12])) / os.sysconf("SC_CLK_TCK")
        except Exception:
            return None

    def proc_rss(self, pid):
        try:
            for line in open(f"/proc/{pid}/status"):
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
        except Exception:
            return None
        return None

    def proc_fds(self, pid):
        try:
            return len(os.listdir(f"/proc/{pid}/fd"))
        except Exception:
            return None

    def probe_ctl(self, timeout=30):
        t0 = time.monotonic()
        try:
            r = subprocess.run([self.args.ctl, "query", "tree"], env=self.env(),
                               capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            return None, "timeout"
        dt = time.monotonic() - t0
        return (dt, "ok") if r.returncode == 0 else (dt, f"rc{r.returncode}")

    def audit(self):
        """Independent liveness check, deliberately not going through the WM.

        `xwininfo -root -children` counts the server's own toplevel windows and
        `xprop _NET_CLIENT_LIST` the WM's own EWMH claim, so a disagreement
        between them, the WM and the control plane localises the failure.
        """
        r = self.popen(["xwininfo", "-root", "-children"], env=self.env())
        n_top = 0
        for m in re.finditer(r"^\s+0x[0-9a-f]+ ", r.stdout, re.M):
            n_top += 1
        rp = self.popen(["xprop", "-root", "_NET_CLIENT_LIST"], env=self.env())
        clist = re.search(r"_NET_CLIENT_LIST\(([^)]*)\)", rp.stdout)
        n_ewmh = len(re.findall(r"window id # 0x", clist.group(1))) if clist else 0
        rq = self.popen([self.args.ctl, "query", "tree"], env=self.env())
        n_ctl = -1
        if rq.returncode == 0:
            try:
                n_ctl = count_tree(json.loads(rq.stdout))
            except json.JSONDecodeError:
                n_ctl = -2
        return {"toplevel_xwininfo": n_top, "net_client_list": n_ewmh,
                "ctl_tree": n_ctl, "wm_alive": self.wm.poll() is None,
                "clients_alive": sum(1 for c in self.clients if c.poll() is None)
                if hasattr(self, "clients") else None}

    def probe_x(self):
        t0 = time.monotonic()
        r = self.popen(["xprop", "-root", "_NET_SUPPORTING_WM_CHECK"])
        return time.monotonic() - t0 if r.returncode == 0 else None

    def sample(self, tag, t_rel, note=""):
        ctl, ctl_status = self.probe_ctl()
        x = self.probe_x()
        row = {
            "t": round(t_rel, 3), "tag": tag, "note": note,
            "cpu_s": f"{v:.3f}" if (v := self.proc_stat(self.wm.pid)) is not None else "",
            "rss_kb": self.proc_rss(self.wm.pid) or "",
            "fds": self.proc_fds(self.wm.pid) or "",
            "ctl_ms": f"{ctl * 1000:.2f}" if ctl is not None else "",
            "ctl_status": ctl_status,
            "x_ms": f"{x * 1000:.2f}" if x is not None else "",
        }
        self.rows.append(row)
        return ctl

    # ── teardown ─────────────────────────────────────────────────────────
    def teardown(self):
        for p in reversed(self.procs):
            if p.poll() is None:
                p.terminate()
        for p in reversed(self.procs):
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
                try:
                    p.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    pass
        shutil.rmtree(self.xdg, ignore_errors=True)


def kw_timeouts(**kw):
    kw.setdefault("timeout", 60)
    return kw


def count_tree(tree):
    """Number of managed windows in a `query tree` reply.

    The reply is `{"sel_mon":..,"monitors":[{"workspaces":[{"columns":
    [{"windows":[W,...],"floats":[W,...]}]}]}]}`; every leaf here is one
    client the WM is actually tracking (src/core/ipc.rs `tree_json`).
    """
    if not isinstance(tree, dict):
        return 0
    n = 0
    for mon in tree.get("monitors") or []:
        for ws in mon.get("workspaces") or []:
            for col in ws.get("columns") or []:
                n += len(col.get("windows") or [])
            n += len(ws.get("floats") or [])
    return n


# ── ring trace analysis ──────────────────────────────────────────────────
def parse_ring(path, offset_ns):
    """Parse the window-manager event-ring trace.

    Returns a dict with:
      records        list of (ns, turn, event, fields)
      header         the two # lines
      receipts       [(ns, turn, kind, detail, x_time_ms, win)]
      opcodes        Counter of `event_receipt opcode=N` (the events the WM
                     received that carry no pointer/keyboard detail)
      spans          {name: [(ns, duration_ns)]} for `boundary=end` records
      input_receipts by kind
    """
    recs = []
    header = []
    try:
        with open(path, errors="replace") as f:
            for line in f:
                if line.startswith("#"):
                    header.append(line.strip())
                    continue
                p = line.rstrip("\n").split("\t")
                if len(p) < 4:
                    continue
                try:
                    recs.append((int(p[0]), int(p[1]), p[2], p[3]))
                except ValueError:
                    continue
    except FileNotFoundError:
        return None

    receipts, opcodes, spans, counts = [], Counter(), {}, Counter()
    open_spans = {}
    for ns, turn, ev, fields in recs:
        counts[ev] += 1
        if ev == "input_receipt":
            d = dict(re.findall(r"(\w+)=(\S+)", fields))
            receipts.append((ns, turn, d.get("kind", "?"), int(d.get("detail", 0) or 0),
                             int(d.get("x_time_ms", 0) or 0), int(d.get("win", 0) or 0),
                             int(d.get("state", 0) or 0)))
        elif ev == "event_receipt":
            m = re.search(r"opcode=(\d+)", fields)
            if m:
                opcodes[int(m.group(1))] += 1
        elif ev.endswith("boundary=end") or "boundary=end" in fields:
            m = re.match(r"(.*?)boundary=end", fields)
            name = m.group(1).strip() if m else ev
            dur = re.search(r"duration_ns=(\d+)", fields)
            spans.setdefault(name or ev, []).append((ns, int(dur.group(1)) if dur else 0))
    return {"records": recs, "header": header, "receipts": receipts,
            "opcodes": opcodes, "spans": spans, "counts": counts,
            "offset_ns": offset_ns, "anchor_ns": ring_mono_anchor_local(recs)}


def ring_mono_anchor_local(recs):
    """CLOCK_MONOTONIC (ns) of the trace's t=0, from its clock_anchor record."""
    for ns, _, ev, fields in recs:
        if ev == "clock_anchor_after" and "monotonic_ns=" in fields:
            m = re.search(r"monotonic_ns=(\d+)", fields)
            if m:
                return int(m.group(1)) - ns
    return None


def event_totals(ring):
    """Total events the WM received, by category."""
    if not ring:
        return {}
    out = {}
    for code, n in ring["opcodes"].items():
        out[name_for(EV_NAME, code)] = out.get(name_for(EV_NAME, code), 0) + n
    kinds = Counter()
    for _, _, kind, detail, _, _, _ in ring["receipts"]:
        kinds[f"{kind}:{detail}"] += 1
    for k, n in kinds.items():
        out[k] = n
    return out


def queue_delay(ring, window=None):
    """Queueing delay = WM processing time - server event time, in ms.

    `x_time_ms` is the X server's own event timestamp (ms, constant offset from
    CLOCK_MONOTONIC, calibrated per run), `ns` is the WM's CLOCK_MONOTONIC
    receipt time. The difference is how long the event sat in the X queue (plus
    the poll loop's blocking wait) before the WM got to it.

    `window` = (t0_ns, t1_ns) restricts the series to the burst.
    """
    if not ring or not ring["receipts"]:
        return {}
    off = ring["offset_ns"]
    anchor = ring.get("anchor_ns") or 0
    pts = []
    for ns, turn, kind, detail, xt, win, state in ring["receipts"]:
        # `ns` is elapsed since the trace buffer was created; the buffer records
        # its own CLOCK_MONOTONIC anchor, so the receipt's absolute monotonic
        # time is anchor + ns. `xt` is the X server clock in ms.
        pts.append((ns, anchor + ns - (xt * 1_000_000 + off)))
    if window:
        pts = [p for p in pts if window[0] <= p[0] <= window[1]]
    if len(pts) < 4:
        return {}
    n = len(pts)
    delays = [p[1] / 1e6 for p in pts]
    t0 = pts[0][0]
    xs = [(p[0] - t0) / 1e9 for p in pts]
    mx, my = sum(xs) / n, sum(delays) / n
    den = sum((x - mx) ** 2 for x in xs)
    slope = (sum((x - mx) * (d - my) for x, d in zip(xs, delays)) / den) if den else 0.0
    # Least-squares R^2 of delay vs time: 1.0 = perfectly monotonic ramp.
    sst = sum((d - my) ** 2 for d in delays)
    sse = sum((d - (my + slope * (x - mx))) ** 2 for x, d in zip(xs, delays))
    r2 = 1 - sse / sst if sst > 1e-9 else 0.0

    def at(frac):
        return round(delays[min(n - 1, int(frac * n))], 1)

    return {
        "n": n,
        "t0_ns": t0, "t1_ns": pts[-1][0],
        "span_s": round(xs[-1], 3),
        "delay_first_ms": round(delays[0], 1),
        "delay_p10_ms": at(0.10), "delay_p25_ms": at(0.25), "delay_p50_ms": at(0.50),
        "delay_p75_ms": at(0.75), "delay_p90_ms": at(0.90),
        "delay_last_ms": round(delays[-1], 1),
        "delay_max_ms": round(max(delays), 1),
        "delay_min_ms": round(min(delays), 1),
        "delay_growth_ms": round(delays[-1] - delays[0], 1),
        "delay_slope_ms_per_s": round(slope, 1),
        "delay_r2": round(r2, 3),
        "verdict": ("UNBOUNDED (delay ramps monotonically)"
                    if r2 > 0.9 and slope > 50 else
                    "BOUNDED (delay flat)" if r2 > 0.9 else
                    "MIXED"),
    }


def queue_delay_series(ring, window, buckets=20):
    """Median queueing delay per equal-time bucket across the burst.

    The decisive signal is the *shape* of this series: flat = the WM keeps up
    with the arrival rate; a monotone ramp = the backlog grows without bound.
    """
    if not ring or not window or not ring["receipts"]:
        return []
    off = ring["offset_ns"]
    anchor = ring.get("anchor_ns") or 0
    pts = []
    for ns, turn, kind, detail, xt, win, state in ring["receipts"]:
        if not (window[0] <= ns <= window[1]):
            continue
        pts.append((ns, anchor + ns - (xt * 1_000_000 + off)))
    if len(pts) < buckets:
        return []
    t0, t1 = pts[0][0], pts[-1][0]
    width = (t1 - t0) / buckets
    out = []
    for b in range(buckets):
        lo, hi = t0 + b * width, t0 + (b + 1) * width
        sel = sorted(d / 1e6 for t, d in pts if lo <= t < hi or (b == buckets - 1 and t == t1))
        if not sel:
            continue
        out.append({"bucket": b, "t_s": round((b + 0.5) * width / 1e9, 3),
                    "n": len(sel), "median_ms": round(sel[len(sel) // 2], 1),
                    "p90_ms": round(sel[min(len(sel) - 1, int(0.9 * len(sel)))], 1),
                    "max_ms": round(sel[-1], 1)})
    return out


def dispatch_cost(ring):
    """Per-event service cost, straight from the event_dispatch spans."""
    if not ring:
        return {}
    sp = ring["spans"].get("event_dispatch", [])
    if not sp:
        return {}
    d = sorted(x[1] for x in sp)
    n = len(d)

    def pct(p):
        return d[min(n - 1, int(p * n))]

    mean_ns = sum(d) / n
    return {
        "n": n, "mean_us": round(mean_ns / 1000, 1),
        "p50_us": round(pct(0.5) / 1000, 1), "p90_us": round(pct(0.9) / 1000, 1),
        "p99_us": round(pct(0.99) / 1000, 1), "max_us": round(d[-1] / 1000, 1),
        "service_rate_per_s": round(1e9 / mean_ns, 1) if mean_ns > 0 else 0,
    }


def turn_cost(ring):
    """Turn structure: how many events per loop turn, and what a turn costs."""
    if not ring:
        return {}
    turn_spans = ring["spans"].get("turn", [])
    dispatch = ring["spans"].get("event_dispatch", [])
    # Events per turn: count turn_begin records and receipts per turn number.
    per_turn = Counter()
    for _, turn, ev, _ in ring["records"]:
        if ev == "event_receipt":
            per_turn[turn] += 1
        elif ev == "input_receipt":
            per_turn[turn] += 1
    out = {"turns_spanned": len(turn_spans),
           "events_per_turn_max": max(per_turn.values()) if per_turn else 0,
           "events_per_turn_mean": round(sum(per_turn.values()) / len(per_turn), 2)
           if per_turn else 0}
    if turn_spans:
        d = sorted(x[1] for x in turn_spans)
        out["turn_mean_us"] = round(sum(d) / len(d) / 1000, 1)
        out["turn_p99_us"] = round(d[min(len(d) - 1, int(0.99 * len(d)))] / 1000, 1)
        out["turn_max_us"] = round(d[-1] / 1000, 1)
    if dispatch:
        dt = sorted(x[1] for x in dispatch)
        out["dispatch_p50_us"] = round(dt[len(dt) // 2] / 1000, 1)
    for name in ("wait", "control", "x_flush", "action", "state_action"):
        s = ring["spans"].get(name)
        if s:
            dd = sorted(x[1] for x in s)
            out[f"{name}_n"] = len(dd)
            out[f"{name}_mean_us"] = round(sum(dd) / len(dd) / 1000, 1)
            out[f"{name}_p99_us"] = round(dd[int(0.99 * (len(dd) - 1))] / 1000, 1)
    return out


# ── wire capture analysis ───────────────────────────────────────────────
def parse_wire(path, window=None):
    """Demultiplex the captured X11 byte streams into requests, replies, events.

    A write() on the X fd is one libxcb flush (or a mid-buffer drain), so the
    number of write records is the number of flushes. Both directions are
    parsed *incrementally* over a persistent buffer: a read() boundary can fall
    anywhere, including inside a reply or an event, so each record's payload is
    appended to whatever is left over from the previous one.

    Outbound framing: the connection-setup request is a fixed 12-byte header
    (byte-order, unused, major, minor, name-len, data-len, unused) and is NOT a
    normal request -- parsing it as one desynchronises the whole stream. After
    it, every request is `opcode + 2-byte length in 4-byte units`, with
    BIG-REQUESTS (length 0) carrying a 4-byte byte count at offset 4.

    Inbound framing: 32-byte events, plus replies (first byte 1, with a
    4-byte extra-length at offset 4) and errors (first byte 0, always 32).

    `window` restricts the accounting to a CLOCK_MONOTONIC [lo, hi] range --
    the injection burst -- so the request/event counts are per-input-event
    rather than diluted by start-up and shutdown.
    """
    # The shim writes one file per recording process: <path>.<pid>.
    p = Path(path)
    files = sorted(p.parent.glob(p.name + ".*")) or ([p] if p.exists() else [])
    if not files:
        return None
    blob = b"".join(f.read_bytes() for f in files)
    recs = []
    i, n = 0, len(blob)
    while i + 16 <= n:
        tag, kind, t, ln = struct.unpack_from("<BB2xQI", blob, i)
        i += 16
        payload = blob[i:i + ln]
        i += ln
        if len(payload) < ln:
            break
        # NOTE: the window is applied when *counting*, not here. Dropping
        # whole records would start the demultiplexer on an arbitrary byte
        # boundary and desynchronise it for the rest of the stream, which is
        # exactly the failure that silently zeroes the event counts.
        recs.append((tag, kind, t, payload))

    reqs, flushes, setup_done = Counter(), 0, False
    setup_in = False
    replies, errors, events, generic = 0, 0, Counter(), Counter()
    req_bytes = reply_bytes = ev_bytes = 0
    outbuf, inbuf = bytearray(), bytearray()
    wtimes, rtimes = [], []

    def inwin(t):
        return window is None or window[0] <= t <= window[1]

    for tag, kind, t, payload in recs:
        if tag == ord("W"):
            if inwin(t):
                flushes += 1
            wtimes.append(t)
            if not setup_done:
                if payload and payload[0] in (0x6C, 0x42):
                    nlen, dlen = struct.unpack_from("<HH", payload, 6)
                    tot = (12 + nlen + dlen + 3) & ~3
                    outbuf += payload[tot:]
                    setup_done = True
                    continue
            outbuf += payload
            j = 0
            while j + 4 <= len(outbuf):
                op = outbuf[j]
                ln = struct.unpack_from("<H", outbuf, j + 2)[0]
                if ln == 0:
                    if j + 8 > len(outbuf):
                        break
                    tot = 4 + struct.unpack_from("<I", outbuf, j + 4)[0]
                else:
                    tot = ln * 4
                if tot < 4 or j + tot > len(outbuf):
                    break
                if inwin(t):
                    if op >= 128:
                        reqs[f"ext{op}.{outbuf[j + 1]}"] += 1
                    else:
                        reqs[name_for(REQ_NAME, op)] += 1
                    req_bytes += tot
                j += tot
            del outbuf[:j]
        else:
            rtimes.append(t)
            inbuf += payload
            j = 0
            while j < len(inbuf):
                b0 = inbuf[j]
                if not setup_in:
                    # The connection-setup reply is NOT a normal reply: an
                    # 8-byte header whose length at offset 6 is in 4-byte units
                    # and counts everything after the header.
                    if j + 8 > len(inbuf):
                        break
                    tot = 8 + struct.unpack_from("<H", inbuf, j + 6)[0] * 4
                    if j + tot > len(inbuf):
                        break
                    setup_in = True
                    j += tot
                    continue
                if b0 in (0, 1):
                    if j + 8 > len(inbuf):
                        break
                    # A reply carries extra data in 4-byte units at offset 4; an
                    # error is always exactly 32 bytes and has no such field, so
                    # reading one as a length desynchronises the stream.
                    if b0 == 0:
                        tot = 32
                        if inwin(t):
                            errors += 1
                    else:
                        tot = 32 + struct.unpack_from("<I", inbuf, j + 4)[0] * 4
                        if inwin(t):
                            replies += 1
                            reply_bytes += tot
                    if j + tot > len(inbuf):
                        break
                    j += tot
                else:
                    if j + 32 > len(inbuf):
                        break
                    if inwin(t):
                        if b0 & 0x80:
                            # GenericEvent: the low 7 bits repeat the core
                            # event code (0x80|22 == a ConfigureNotify carried
                            # as a generic event), byte1 is the extension
                            # opcode and byte2 the extension's own event code.
                            # x11rb delivers these as Event::Unknown when it
                            # has no variant, which is why the WM's own trace
                            # reports them by raw opcode.
                            code = b0 & 0x7F
                            generic[f"{code}.ext{inbuf[j + 1]}.{inbuf[j + 2]}"] += 1
                            events["generic:" + name_for(EV_NAME, code)] += 1
                        else:
                            events[name_for(EV_NAME, b0)] += 1
                        ev_bytes += 32
                    j += 32
            del inbuf[:j]
    return {"flushes": flushes, "requests": sum(reqs.values()),
            "request_bytes": req_bytes, "requests_by_name": dict(reqs.most_common()),
            "replies": replies, "reply_bytes": reply_bytes, "errors": errors,
            "events_received": sum(events.values()),
            "events_by_name": dict(events.most_common()),
            "generic_by_name": dict(generic.most_common()),
            "stream_residue": {"out": len(outbuf), "in": len(inbuf)},
            "bytes_in": sum(len(p) for tg, _, _, p in recs if tg == ord("R")),
            "bytes_out": sum(len(p) for tg, _, _, p in recs if tg == ord("W"))}


# ── macro trace (stderr) analysis ───────────────────────────────────────
def parse_wlog(path):
    c = Counter()
    kinds = Counter()
    try:
        text = Path(path).read_text(errors="replace")
    except FileNotFoundError:
        return {}
    for line in text.splitlines():
        if "[INPUT-TRACE]" in line:
            c["input_trace_lines"] += 1
        if "[WINDOW-TRACE]" in line:
            c["window_trace_lines"] += 1
        if "focus() called" in line:
            c["focus_calls"] += 1
        if "focus() SET" in line:
            c["focus_set"] += 1
        if "reconcile_focus" in line and "[INPUT-TRACE]" in line:
            c["reconcile_focus"] += 1
        if line.startswith("[WINDOW-TRACE] reconcile "):
            c["reconcile"] += 1
        if line.startswith("[WINDOW-TRACE] arrange "):
            c["arrange"] += 1
        if "on_button_press" in line:
            c["on_button_press"] += 1
        if "BR-enter" in line:
            c["button_release"] += 1
        if "motion" in line.lower() and "on_motion" in line:
            c["on_motion"] += 1
        m = re.search(r"action=(\w+)", line)
        if m:
            kinds["action_" + m.group(1)] += 1
    # The ring buffer fills at 262144 records; if the dump header says so, the
    # counts are a lower bound.
    return dict(c) | {"trace_actions": dict(kinds)}


# ── driver ──────────────────────────────────────────────────────────────
def run_scenario(args, spec):
    name = spec["name"]
    a = argparse.Namespace(**{**vars(args), **spec})
    a.scenario = name
    # A run with the macro features off is only a true control if the binary was
    # also built without them: that stderr tracing is compiled in, not switched
    # at runtime, so the traced binary keeps paying for it.
    if a.macro == "off" and "wm" not in spec:
        a.wm = a.wm.replace("-traced", "-plain")
        a.ctl = a.ctl.replace("-traced", "-plain")
    run = Run(a)
    started = time.time()
    try:
        run.start_xephyr()
        cal = run.calibrate()
        run.start_wm()
        clients, managed = run.start_clients(a.clients)
        run.clients = clients
        print(f"[{name}] up: cal_offset={cal} clients={len(clients)} managed={managed}", flush=True)

        t0 = time.monotonic()
        base = []
        while time.monotonic() - t0 < a.baseline_s:
            c = run.sample("baseline", time.monotonic() - t0)
            if c is not None:
                base.append(c)
            time.sleep(0.25)
        base_lat = sorted(base)[len(base) // 2] if base else 0.05
        print(f"[{name}] idle ctl median = {base_lat*1000:.1f} ms", flush=True)

        # ── burst ───────────────────────────────────────────────────────
        inject = run.outdir / "inject.txt"
        env = run.env(XTEST_STRESS_INJECT=str(inject))
        t_stress0 = time.monotonic()
        sender = subprocess.Popen(
            [str(HERE / "xtest-stress"), a.mode, str(a.rate), str(a.duration),
             str(a.px), str(a.py)],
            env=env, stdout=open(run.outdir / "stress.txt", "w"),
            stderr=subprocess.DEVNULL)
        run.procs.append(sender)
        while sender.poll() is None:
            run.sample("stress", time.monotonic() - t_stress0)
            time.sleep(0.25)
        sender.wait()
        t_stress1 = time.monotonic()
        print(f"[{name}] burst {a.duration}s took {t_stress1-t_stress0:.1f}s wall", flush=True)
        audits = [{"phase": "t+0.0s", **run.audit()}]

        # ── recovery ────────────────────────────────────────────────────
        t_rec0 = time.monotonic()
        recovery_s, quiet, ctl_series = None, 0, []
        while time.monotonic() - t_rec0 < a.recovery_s:
            c = run.sample("recovery", time.monotonic() - t_stress1)
            ctl_series.append((round(time.monotonic() - t_stress1, 3),
                               round(c * 1000, 1) if c is not None else None))
            if c is not None and c < base_lat * 1.5:
                quiet += 1
                if quiet >= 3:
                    recovery_s = time.monotonic() - t_rec0
                    break
            else:
                quiet = 0
            time.sleep(0.25)
            for mark_s, ph in ((2.0, "t+2s"), (5.0, "t+5s"), (15.0, "t+15s")):
                if (time.monotonic() - t_stress1) >= mark_s and ph not in {x["phase"] for x in audits}:
                    audits.append({"phase": ph, **run.audit()})
        if recovery_s is None:
            recovery_s = time.monotonic() - t_rec0
        audits.append({"phase": "end", **run.audit()})
        print(f"[{name}] recovery {recovery_s:.2f}s", flush=True)

        # ── post-burst liveness probe: still managing? still focused? ───
        r = run.popen([a.ctl, "query", "tree"], env=run.env())
        post_tree_ok = r.returncode == 0
        post_managed = count_tree(json.loads(r.stdout)) if post_tree_ok else -1

        # ── shutdown (dumps the ring buffer) ────────────────────────────
        wm_exited = run.wm.poll() is not None
        run.wm.terminate()
        try:
            run.wm.wait(timeout=20)
        except subprocess.TimeoutExpired:
            run.wm.kill()
        run.procs.remove(run.wm)
    finally:
        run.teardown()

    # ── analysis ─────────────────────────────────────────────────────────
    stress = (run.outdir / "stress.txt").read_text().strip()
    m = re.search(r"STRESS_RESULT (.*)", stress)
    sent_fields = dict(kv.split("=", 1) for kv in m.group(1).split()) if m else {}
    ring = parse_ring(run.outdir / "ring.tsv", cal)
    wire_all = parse_wire(run.outdir / "wire.bin") if a.wire == "on" else None
    wlog = parse_wlog(run.outdir / "wlog")

    # Burst window in ring-clock terms: the sender's own injection log is
    # CLOCK_MONOTONIC, and so is the trace's `ns`, so the two are directly
    # comparable once the trace's start instant is recovered from its clock
    # anchor record.
    anchor = ring_mono_anchor(ring)
    burst = injection_window(run.outdir / "inject.txt", anchor, ring)
    # The wire stamps absolute CLOCK_MONOTONIC and the burst window is in
    # trace-relative ns, so the anchor goes back on.
    wire_win = ((burst[0] + anchor, burst[1] + anchor)
                if burst and anchor is not None else None)
    wire = parse_wire(run.outdir / "wire.bin", window=wire_win) if a.wire == "on" else None
    amp_early = amplification(ring, burst)

    # Per-input-event amplification: events the WM received divided by input
    # events it received, restricted to the burst window.
    amp = amplification(ring, burst)

    base_rows = [r for r in run.rows if r["tag"] == "baseline" and r["ctl_ms"]]
    str_rows = [r for r in run.rows if r["tag"] == "stress" and r["cpu_s"]]
    rec_rows = [r for r in run.rows if r["tag"] == "recovery"]

    def f(x, k=3):
        return round(float(x), k) if x not in (None, "") else None

    cpu_stress = f(float(str_rows[-1]["cpu_s"]) - float(str_rows[0]["cpu_s"])) if len(str_rows) >= 2 else None
    cpu_total = f(float(run.rows[-1]["cpu_s"]) - float(run.rows[0]["cpu_s"])) if run.rows[-1]["cpu_s"] and run.rows[0]["cpu_s"] else None
    rss = [int(r["rss_kb"]) for r in run.rows if r["rss_kb"]]
    fds = [int(r["fds"]) for r in run.rows if r["fds"]]
    ctl = [float(r["ctl_ms"]) for r in run.rows if r["ctl_ms"]]

    summary = {
        "scenario": name, "mode": a.mode, "rate": a.rate, "duration_s": a.duration,
        "clients": a.clients, "managed_seen": managed,
        "ring": a.ring, "wire": a.wire, "macro": a.macro,
        "wm_binary": a.wm, "wall_clock_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(started)),
        "x_time_offset_ns": cal,
        "sent": sent_fields,
        "input_rate_per_s": f(float(sent_fields.get("rate_sent", 0)), 1),
        "input_actual_rate_per_s": f(
            float(sent_fields.get("sent", 0)) / max(float(sent_fields.get("elapsed", 1)), 1e-9), 1),
        "idle_ctl_ms": f(base_lat * 1000, 1),
        "ctl_ms_min": min(ctl) if ctl else None, "ctl_ms_max": max(ctl) if ctl else None,
        "ctl_ms_median": f(sorted(ctl)[len(ctl) // 2], 1) if ctl else None,
        "ctl_timeout_samples": sum(1 for r in run.rows if r["ctl_status"] != "ok"),
        "x_rtt_ms_max": f(max(float(r["x_ms"]) for r in run.rows if r["x_ms"]), 1) if any(r["x_ms"] for r in run.rows) else None,
        "cpu_s_stress": cpu_stress, "cpu_s_total": cpu_total,
        "rss_kb_start": rss[0] if rss else None, "rss_kb_max": max(rss) if rss else None,
        "rss_kb_end": rss[-1] if rss else None,
        "fds_start": fds[0] if fds else None, "fds_max": max(fds) if fds else None,
        "fds_end": fds[-1] if fds else None,
        "recovery_s": f(recovery_s, 2),
        "post_burst_ctl_ok": post_tree_ok, "post_burst_managed": post_managed,
        "wm_exited_before_shutdown": wm_exited,
        "audits": audits,
        "burst_window_ns": burst,
        "burst_window_mono_ns": wire_win,
        "amplification": amp,
        "queue_delay": queue_delay(ring, burst),
        "queue_delay_series": queue_delay_series(ring, burst),
        "dispatch_cost": dispatch_cost(ring),
        "turns": turn_cost(ring),
        "ring_events_total": event_totals(ring),
        "ring_header": (ring or {}).get("header", []),
        "ring_record_cap_reached": cap_reached(ring),
        "wire": wire,
        "wire_whole_run": wire_all,
        "per_input_x11": per_input_x11(wire, amp_early),
        "macro": wlog,
        "ctl_series": ctl_series,
    }
    (run.outdir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    with open(run.outdir / "samples.csv", "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=["t", "tag", "note", "cpu_s", "rss_kb",
                                           "fds", "ctl_ms", "ctl_status", "x_ms"])
        w.writeheader()
        w.writerows(run.rows)
    print(f"[{name}] amplification={json.dumps(amp)}", flush=True)
    print(f"[{name}] queue_delay={json.dumps(summary['queue_delay'])}", flush=True)
    return summary


def per_input_x11(wire, amp):
    """X11 requests / replies / flushes / errors per *injected* input event."""
    if not wire or not amp or not amp.get("input_events_received"):
        return {}
    n = amp["input_events_received"]
    return {"input_events": n,
            "requests_per_input": round(wire["requests"] / n, 2),
            "replies_per_input": round(wire["replies"] / n, 2),
            "flushes_per_input": round(wire["flushes"] / n, 2),
            "errors_per_input": round(wire["errors"] / n, 3),
            "request_bytes_per_input": round(wire["request_bytes"] / n, 1),
            "events_per_input_wire": round(wire["events_received"] / n, 2),
            "top_requests_per_input": {k: round(v / n, 2)
                                       for k, v in list(wire["requests_by_name"].items())[:12]}}


def cap_reached(ring):
    if not ring:
        return None
    for h in ring["header"]:
        m = re.search(r"dropped=(\d+)", h)
        if m:
            return int(m.group(1))
    return None


def ring_mono_anchor(ring):
    """CLOCK_MONOTONIC (ns) of the trace's t=0, from its clock_anchor record."""
    if not ring:
        return None
    for ns, _, ev, fields in ring["records"]:
        if ev == "clock_anchor_after" and "monotonic_ns=" in fields:
            m = re.search(r"monotonic_ns=(\d+)", fields)
            if m:
                return int(m.group(1)) - ns
    return None


def injection_window(inject_path, anchor, ring):
    """(t0_ns, t1_ns) in trace-clock terms covering the injected burst."""
    if not ring or anchor is None:
        return None
    lo = hi = None
    try:
        for line in Path(inject_path).read_text().splitlines():
            parts = line.split()
            if len(parts) < 3:
                continue
            t = int(parts[1]) - anchor  # CLOCK_MONOTONIC -> trace ns
            lo = t if lo is None else min(lo, t)
            hi = t if hi is None else max(hi, t)
    except FileNotFoundError:
        return None
    if lo is None:
        return None
    return (lo, hi)


def amplification(ring, window):
    """X11 events the WM received per *injected* input event, by category.

    Denominator: the input events (button/key/motion) the WM actually received
    inside the burst window -- every injected event is delivered, so this is the
    injected count. Numerator: every event the WM received, including the ones
    the WM's own actions generated (ConfigureNotify from its own ConfigureWindow,
    FocusIn/FocusOut from its own SetInputFocus, PropertyNotify from its own
    _NET_* writes, Enter/Leave from pointer motion across windows).
    """
    if not ring or not window:
        return {}
    lo, hi = window
    inb = Counter()
    total = 0
    for ns, _, kind, detail, xt, win, state in ring["receipts"]:
        if not (lo <= ns <= hi):
            continue
        if kind in ("button_press", "button_release"):
            inb[f"button{detail}_"
                + ("press" if kind == "button_press" else "release")] += 1
        elif kind == "motion":
            inb["motion"] += 1
        elif kind == "key_press":
            inb["key_press"] += 1
        elif kind == "key_release":
            inb["key_release"] += 1
        elif kind == "enter":
            inb["enter"] += 1
        elif kind == "leave":
            inb["leave"] += 1
        total += 1
    for ns, turn, ev, fields in ring["records"]:
        if ev != "event_receipt" or not (lo <= ns <= hi):
            continue
        m = re.search(r"opcode=(\d+)", fields)
        if m:
            inb[name_for(EV_NAME, int(m.group(1)))] += 1
            total += 1
    injected = sum(v for k, v in inb.items()
                   if k.startswith("button") or k in ("motion", "key_press", "key_release"))
    buttons = {k: v for k, v in inb.items() if k.startswith("button")}
    generated = {k: v for k, v in inb.items() if not k.startswith("button")
                 and k not in ("motion", "key_press", "key_release")}
    notches = buttons.get("button5_press", 0) + buttons.get("button4_press", 0)
    clicks = buttons.get("button1_press", 0) + buttons.get("button2_press", 0) + \
        buttons.get("button3_press", 0)
    return {
        "input_events_received": injected,
        "all_events_received": total,
        "events_per_input": round(total / injected, 2) if injected else None,
        "generated_events": sum(generated.values()),
        "generated_per_input": round(sum(generated.values()) / injected, 2) if injected else None,
        "per_input": round(total / (notches + clicks) if (notches + clicks) else 0, 2),
        "notches": notches, "clicks": clicks,
        "motions": inb.get("motion", 0),
        "breakdown": dict(sorted(inb.items(), key=lambda kv: -kv[1])),
    }


# ── the matrix ──────────────────────────────────────────────────────────
SUITE = [
    # --- traced: amplification + queueing-delay trend (ring buffer on) -------
    # The ring holds 262144 records, so the burst length is chosen to stay
    # inside it at the rate in question; `dropped` in the dump header proves
    # whether it did.
    dict(name="scroll-50",       mode="scroll",    rate=50,  duration=12),
    dict(name="scroll-200",      mode="scroll",    rate=200, duration=12),
    dict(name="click-50",        mode="click",     rate=50,  duration=12),
    dict(name="click-500",       mode="click",     rate=500, duration=12),
    dict(name="motion-50",       mode="motion",    rate=50,  duration=12),
    dict(name="combined-50",     mode="combined",  rate=50,  duration=12),
    dict(name="maxscroll",       mode="maxscroll", rate=0,   duration=8),
    dict(name="maxclick",        mode="maxclick",  rate=0,   duration=8),
    # --- knee search: 4 s traced bursts around the suspected saturation ----
    dict(name="scroll-100",  mode="scroll", rate=100,  duration=4, recovery_s=15),
    dict(name="scroll-150",  mode="scroll", rate=150,  duration=4, recovery_s=15),
    dict(name="scroll-300",  mode="scroll", rate=300,  duration=4, recovery_s=15),
    dict(name="click-200",   mode="click",  rate=200,  duration=4, recovery_s=15),
    dict(name="click-1000",  mode="click",  rate=1000, duration=4, recovery_s=15),
    dict(name="motion-200",  mode="motion", rate=200,  duration=4, recovery_s=15),
    # --- untraced controls: ring off, macro off ---------------------------
    dict(name="scroll-50-nt",   mode="scroll",    rate=50,  duration=12,
         ring="off", macro="off"),
    dict(name="scroll-200-nt",  mode="scroll",    rate=200, duration=12,
         ring="off", macro="off"),
    dict(name="click-50-nt",    mode="click",     rate=50,  duration=12,
         ring="off", macro="off"),
    dict(name="click-500-nt",   mode="click",     rate=500, duration=12,
         ring="off", macro="off"),
    dict(name="motion-50-nt",   mode="motion",    rate=50,  duration=12,
         ring="off", macro="off"),
    dict(name="combined-50-nt", mode="combined",  rate=50,  duration=12,
         ring="off", macro="off"),
    dict(name="maxscroll-nt",   mode="maxscroll", rate=0,   duration=8,
         ring="off", macro="off"),
    dict(name="maxclick-nt",    mode="maxclick",  rate=0,   duration=8,
         ring="off", macro="off"),
    # --- fully clean: no ring, no macro, no wire shim ---------------------
    # The only runs whose CPU / RSS / control-latency numbers are used as
    # intrinsic behaviour rather than as instrumented behaviour.
    dict(name="idle-clean",     mode="scroll", rate=1, duration=1,
         ring="off", macro="off", wire="off", baseline_s=6),
    dict(name="scroll-50-clean",  mode="scroll", rate=50,  duration=12,
         ring="off", macro="off", wire="off"),
    dict(name="scroll-200-clean", mode="scroll", rate=200, duration=12,
         ring="off", macro="off", wire="off"),
    dict(name="click-50-clean",   mode="click",  rate=50,  duration=12,
         ring="off", macro="off", wire="off"),
    dict(name="click-500-clean",  mode="click",  rate=500, duration=12,
         ring="off", macro="off", wire="off"),
    dict(name="motion-50-clean",  mode="motion", rate=50,  duration=12,
         ring="off", macro="off", wire="off"),
    dict(name="combined-50-clean", mode="combined", rate=50, duration=12,
         ring="off", macro="off", wire="off"),
    dict(name="maxscroll-clean",  mode="maxscroll", rate=0, duration=8,
         ring="off", macro="off", wire="off"),
    dict(name="maxclick-clean",   mode="maxclick", rate=0, duration=8,
         ring="off", macro="off", wire="off"),
    # --- long soak: does the backlog ever stop draining? ------------------
    dict(name="soak-scroll-150", mode="scroll", rate=150, duration=60,
         ring="off", macro="off", baseline_s=5, recovery_s=60),
    dict(name="soak-maxscroll",   mode="maxscroll", rate=0, duration=30,
         ring="off", macro="off", baseline_s=5, recovery_s=90),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--recovery-s", type=float, default=30.0)
    ap.add_argument("--scenario")
    ap.add_argument("--mode")
    ap.add_argument("--rate", type=float, default=50)
    ap.add_argument("--duration", type=float, default=10)
    ap.add_argument("--clients", type=int, default=6)
    ap.add_argument("--out", default="/tmp/kilo/sat")
    ap.add_argument("--display", default=":97")
    ap.add_argument("--wm", default="/tmp/kilo/bin/maverick-traced")
    ap.add_argument("--ctl", default="/tmp/kilo/bin/maverickctl-traced")
    ap.add_argument("--ring", choices=["on", "off"], default="on")
    ap.add_argument("--wire", choices=["on", "off"], default="on")
    ap.add_argument("--macro", choices=["on", "off"], default="on")
    ap.add_argument("--wire-max", type=int, default=512 * 1024 * 1024)
    ap.add_argument("--baseline-s", type=float, default=3.0)
    ap.add_argument("--px", type=int, default=640)
    ap.add_argument("--py", type=int, default=360)
    ap.add_argument("--suite", action="store_true")
    ap.add_argument("--only", default="")
    args = ap.parse_args()

    if args.macro == "on" and "traced" not in args.wm:
        print("WARNING: --macro on but the WM binary was built without the "
              "input-trace/window-trace features; the macro counts will be 0.",
              file=sys.stderr)

    if args.suite:
        specs = [s for s in SUITE if not args.only or s["name"] in args.only.split(",")]
        out = []
        for spec in specs:
            print(f"\n===== {spec['name']} =====", flush=True)
            # Each scenario owns its own directory; without this they would all
            # share `out/suite` and wipe each other.
            args.scenario = spec["name"]
            try:
                out.append(run_scenario(args, spec))
            except Exception as e:  # keep the suite going
                print(f"[{spec['name']}] FAILED: {e}", flush=True)
                out.append({"scenario": spec["name"], "error": str(e)})
            (Path(args.out) / "suite.json").write_text(
                json.dumps(out, indent=2) + "\n")
        print(f"\nwrote {args.out}/suite.json ({len(out)} scenarios)")
        return

    if not args.scenario or not args.mode:
        ap.error("--scenario and --mode are required unless --suite")
    s = run_scenario(args, dict(name=args.scenario, mode=args.mode,
                                rate=args.rate, duration=args.duration,
                                ring=args.ring, wire=args.wire, macro=args.macro))
    print(json.dumps(s, indent=2))


if __name__ == "__main__":
    main()
