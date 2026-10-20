# Measuring Maverick

Measure the WM, the application and the external compositor separately. A low
WM CPU reading does not establish game frame rate, video smoothness or input
latency. Use a release build and keep the workload, configuration, keyboard
layout and compositor settings constant between comparisons.

For a running session, find its PID with `maverickctl list --json`, then use
ordinary process tools:

```bash
pidstat -u -r -w -p <wm-pid> 1
ps -p <wm-pid> -o pid,comm,etimes,pcpu,rss
```

`pidstat` is provided by `sysstat` on Arch Linux. It samples CPU, RSS, faults
and context switches by interval. `ps` reports CPU averaged over the process's
lifetime; it is not an instantaneous reading. Sample Picom and the application
under their own PIDs if they are involved. Maverick does not need a resident
metrics service.

## Reproducible isolated workload

The floating regression harness has an optional measurement mode. It starts
its own Xvfb, private configuration/runtime and probe clients; it never takes
over the caller's display or signals processes by name. It needs the same
tools as the regression: Xvfb, xprop, a C compiler and Xlib development files.

```bash
cargo build --release -p maverick -p maverickctl --locked
python3 tests/xvfb-floats.py --case profile --clients 10 --actions 100
python3 tests/xvfb-floats.py --case profile --clients 50 --actions 100
```

Each run has two floats and the remaining clients tiled. It samples an idle
interval after setup, then requests focus successively across all clients.
The JSON reports WM CPU time, RSS after the workload and CLI request round-trip
p50/p95/maximum. CPU is read from `/proc` at the kernel's clock-tick resolution:
zero means no measurable tick during that interval, not proof of zero work.
One-core CPU percentages use one core as 100%.

CLI round trips include starting `maverickctl`, instance resolution, the control
socket and its receipt. A dispatch receipt acknowledges queueing; it does not
prove the new window was displayed. The completion sentinel only establishes
that the final action was drained. These numbers are not keyboard-to-screen
latency. Repeat runs and compare distributions on the same machine. There is no
portable timing threshold in CI.

## Internal spans

The existing runtime trace needs no additional build feature. Enable it only
for a separate diagnostic run, because recording changes memory and timing:

```bash
mav_profile_trace="$(mktemp /tmp/maverick-profile.XXXXXX.trace)"
python3 tests/xvfb-floats.py --case profile --clients 10 --actions 100 \
  --trace-output "$mav_profile_trace"
```

The harness retains the raw TSV and reports durations for `state_action`,
`reconcile`, `x_flush` and `wait`. These summaries cover the whole session,
including setup and shutdown. `state_action` isolates core command execution;
`reconcile` covers deferred backend work. `wait` is blocking time, so a long
idle wait is expected. A complete `turn` includes that wait and must not be
interpreted as rendering time. Returning from an X11 flush does not prove
presentation by the server or compositor.

The trace header reports dropped and truncated records. A nonzero count means
the trace is incomplete. Trace storage grows to its fixed cap and is dumped at
shutdown, so use untraced runs for baseline CPU/RSS comparisons. The same
mechanism can be enabled when launching a named nested Xephyr session through
`maverickctl session create` with `MAVERICK_TRACE=1` and `MAVERICK_TRACE_PATH`;
close that explicit test session with `maverickctl session stop <test-name>`.

The core has a separate allocation regression:

```bash
cargo test -p maverick an_arrange_allocates_nothing --locked
```

It checks warmed-up layout projection and presentation with 1, 5, 10 and 50
windows. It does not cover allocations in X11 reconciliation, stacking, IPC,
Overview or client applications. Those paths require profiling; do not infer
that the entire WM allocates nothing from the core test.
