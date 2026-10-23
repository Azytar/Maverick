# Reproducible X11 WM comparison

Compare Maverick, i3, bspwm, Openbox and dwm with the same passive X11 clients.
The harness owns every Xvfb and child PID, ignores the caller's DISPLAY and never
starts a compositor or signals processes by name. No combined score is produced.

## Reproduce

On Debian 13, install the dependencies on your benchmark machine:

```sh
apt-get install --no-install-recommends build-essential libx11-dev libx11-xcb-dev libxcb1-dev xvfb i3-wm bspwm openbox dwm python3-matplotlib fonts-dejavu-core
cargo build --release -p maverick --locked
python3 benchmarks/x11/run.py --out /tmp/wm-comparison \
  --maverick target/release/maverick --dwm /usr/bin/dwm.default \
  --clients 10 50 --repeats 7 --warmups 1 --focus 500 --fullscreen 40 \
  --idle-seconds 2 --seed 20261022
python3 benchmarks/x11/plot.py /tmp/wm-comparison
python3 tests/benchmark-contracts.py
```

Use default Maverick features and the release profile. On systems without
Debian's dwm variants, pass the path to stock dwm. The `--i3`, `--bspwm`,
`--openbox`, `--dwm` and `--xvfb` arguments accept executable paths; bspc must be
beside bspwm. The output directory must not exist, preventing accidental mixing
of failed and new runs. Run on an otherwise idle machine. Repeat the complete
experiment when changing hardware, libraries or configuration.

## Endpoints and configuration

Every trial uses a fresh Xvfb at 1280x800x24. Optional launchers, pointer-following
focus and cursor warps are disabled. No compositor or status process is started.
Maverick keeps its ordinary layout/gaps; i3 and bspwm tile; Openbox stacks
undecorated floating clients; stock dwm.default retains its compiled layout,
borders and bar. These are common protocol operations, not feature-equivalent
layout engines. The configurations are copied into each trial's evidence.

The observer subscribes before WM launch. Advertised EWMH readiness is recorded,
but not ranked in the chart: some WMs publish it before their event loop is ready.
An untimed, retried sentinel window checks a usable loop. bspwm configuration
runs after that barrier. Already-equal settings are read before writing because
bspwm rejects a redundant focus-follows-pointer write. A common 250ms settling
interval follows. Timed operations are never retried to hide a failure.

Each workload then performs these phases:

- **Map:** serially map 10 or 50 clients. Creation/property setup precedes the
  per-operation timer. Completion requires viewability, membership in
  `_NET_CLIENT_LIST`, and normal `WM_STATE`. This measures individual mapping
  into a growing population, not a simultaneous burst or application startup.
- **Idle/memory:** keep the clients alive for two seconds, sampling WM CPU ticks,
  RSS and PSS. RSS includes shared resident pages; PSS is also retained. Neither
  is whole-session memory; Xvfb and the observer are separate processes.
- **Focus:** 500 round-robin `_NET_ACTIVE_WINDOW` pager requests. Completion
  requires both the published active window and real X input focus to equal the
  requested passive client. No CLI process is started in the timed operation.
- **Fullscreen:** 40 alternating ADD/REMOVE requests, or 20 pairs, on the last
  client. Entry requires the state atom, viewability, screen-sized geometry and
  zero border; exit requires clearing the atom and restoring the previously
  observed normal geometry. The chart combines individual entry/exit timings,
  not the total duration of a pair.
- **Destroy:** destroy newest clients first and wait for removal from the WM's
  client list. This measures bookkeeping, not application shutdown time.

The native wait loop checks state after X events without a fixed per-operation
sleep. Timings include query/reply and scheduling costs; they are not isolated
WM execution time. Unexpected X errors, wrong property shapes, incomplete sample
records, timeout and premature child exits fail the trial and retain evidence.
The harness stops on the first failed trial. It reaps only its own processes.

### dwm focus policy

Stock dwm 6.5 handles `_NET_ACTIVE_WINDOW` by marking an unselected client urgent,
rather than selecting it. The benchmark verifies this outside timing and records
`focus_supported: false` and `urgent_only: true`. The focus panels show **No
compatible**, never zero, infinity or a substitute shortcut measurement. Other
phases still run. A patched build honoring the same request can run the focus
phase if the capability check succeeds.

The upstream `clientmessage()` implementation is in
<https://dl.suckless.org/dwm/dwm-6.5.tar.gz>. The recorded run uses Debian's
`dwm.default` from `dwm 6.5-1`, identified by version and binary hash.

## Statistics

Seven measured repetitions plus one discarded warm-up cover each of five WMs
at two client counts: 80 trials, of which 70 contribute to the chart. Each round
shuffles the full combination list with the recorded seed. Warm-ups and all
outliers remain in raw data; only explicitly marked warm-ups are excluded.

A bar is the median of seven **run-level** statistics. Per-run p50/p95 use linear
interpolation at `(N-1)*q`. Error bars show P25-P75 across those run statistics,
not confidence intervals or pooled samples. The map p95 at 50 windows therefore
represents the tail within each 50-map trial. No portable timing threshold is
used in CI. Small overlapping distributions do not establish a useful winner.

CPU is quantized by `/proc/PID/stat` ticks. Zero means no tick observed, not no
work. The CPU panel normalizes map-phase WM CPU time to 100 windows; it includes
WM processing of setup properties and map requests. It excludes observer and
X-server CPU, although their scheduling/round trips affect operation latency.

## Evidence and graph

- `manifest.json`: exact commands, workload/seed, versions, binary/source SHA-256,
  production revision, compiler, kernel/CPU, affinity, cgroup limits, library/data
  paths, resolution and clock frequency. It stays incomplete until all trials pass.
- `raw.json`: every operation sample, warm-up flag, CPU ticks, RSS/PSS, child
  return codes and host load before/after each trial.
- Trial directories: copied configs, WM/server/probe logs and standalone results.
- `runs.csv`, `summary.csv`, `summary.json`: run-level numbers and chart aggregates.
- `comparison.png`, `.svg`, `.pdf`: Matplotlib exports from the complete, validated
  data. No image-generation model or illustrative values are used.

The plotter rejects incomplete matrices, changed raw checksums, failed/unreaped
trials, short/nonfinite sample lists and unexplained missing values. The seven
contract tests exercise these failure cases with explicit synthetic fixtures;
those fixtures are never used for the published chart.

Xvfb has no physical display scanout and the clients draw no application UI.
These results do not measure FPS, compositor performance, keyboard-to-screen
latency or real-game capture. They describe this machine and workload; there is
no universal WM ranking. The supplied run is virtualized and CPU-quota limited.
