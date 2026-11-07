# Regression entry points

Run from a checkout with Rust, a C compiler, Xvfb, xdotool, x11-utils, Picom,
and the X11 development libraries installed:

```sh
cargo run -p maverick-xtask --locked -- check
cargo run -p maverick-xtask --locked -- release
cargo run -p maverick-xtask --locked -- x11
```

`check` runs default-feature and all-feature Rust tests. `x11` explicitly runs
ignored `maverick-testkit` tests against both Minimal and control release WMs, with two test threads,
then the retained external-client campaign against control. There are 21 Rust X11 tests per profile. A missing dependency or release
binary is an error. Ordinary `cargo test` does not execute ignored X11 tests.

## Rust testkit contracts

`owned_xvfb` covers server/EWMH stacking equivalence, float focus, nested
transients (both parent-first and child-first destruction), fullscreen entry,
exit, float geometry restoration, docks, initial ribbon fullscreen, owned
popups, denied client fullscreen, withdrawal/remap of the same XID, stale focus,
invalid state actions, malformed hints, unexpected connection loss, and owned
process cleanup. XTEST drives real key grabs for column collapse, vertical
navigation/movement, Views/carousel, Overview and exec/restart. WM_HINTS tests
observe both real and published focus for 600 ms per eligibility transition,
including refusals, flagless/missing hints, remap and focus fallback.

The fixture owns each child through `Child`; it never selects a host DISPLAY
or signals a discovered PID. Xvfb assigns the display through `-displayfd`,
with TCP disabled. HOME/XDG paths are private, runtime directories use mode
0700, and desktop/session/LD_PRELOAD variables are removed from child commands.
No client or assertion needs a control socket or maverickctl. Protocol helpers
live in the integration test support module, not in the shipped WM.

Xvfb startup has a 30 s deadline, condition waits have a 12 s deadline, and each
protocol connection has a 60 s total deadline. Its cancellable watchdog owns a
duplicate socket and **drops with that connection**, so dropping a client still
causes the server to destroy its windows. Timeout shuts down only that socket
and wakes synchronous replies. Process guards kill/wait only their own children;
temporary files drop afterward. Failure messages include bounded log tails.

## Inventory and retention decisions

Paths below are relative to `tests/` unless qualified. Retained external
oracles verify contracts beyond the smaller Rust fixtures; their presence does
not authorize a desktop or privileged test in the automatic Xvfb campaign.

| Tests/tools | Responsibility and decision |
| --- | --- |
| Workspace `#[test]` / proptest, `config_check.rs`, `child_lifecycle.rs`, `source_constraints.rs`, `no_wait_in_wm.rs` | Keep Rust for model, configuration, child ownership and source contracts. Core tests already cover layout, carousel, Views, restoration and reconciliation. |
| `maverick-testkit/tests/owned_xvfb.rs`, `support/` | Rust protocol fixtures, orchestration, bounded waits, negative cases and XTEST. Preferred home for new independent X11 regressions. |
| `xvfb-floats.py` | Keep explicit user/IPC focus, float conversion, fullscreen peek, urgency border pixels and raw XCB request profiles. Transient/vertical cases are replaced by Rust. CI runs the metadata case, which includes the focus case and dock checks, once. |
| `xvfb-stacking.py`, `stacking-startup.py` | Keep cross-View fullscreen mode matrix, dock pixels and keyboard navigation; startup contract tests still protect this active harness. Basic QueryTree checks alone do not replace it. |
| `xvfb-reload.py` | Keep live control-protocol reload, malformed/missing configs, camera and border checks. Rust config validation is complementary. |
| `xvfb-restart.py`, `xvfb-restore.py` | Keep full state serialization, multiple Views, pointer/autostart preservation, Overview restart and presentation restoration. The Rust restart test checks the independent keyboard path and observable X11 state. |
| `xvfb-cursor.py`, `xvfb-cursor-theme.py`, `cursor-probe.c` | Keep external Xlib/XFixes cursor ownership, image/theme/resource precedence and shutdown restoration. No Rust cursor-equivalence claim. |
| `xvfb-input-hint.py`, `input-hint-contracts.py`, `input-hint-probe.c` | Keep native client parser failures, sanitizers, direct control commands and logical-focus observations. Rust independently covers EWMH/keyboard eligibility without IPC. |
| `xvfb-input.py`, `input-probe.c` | Keep native XI2 ownership, actual keyboard delivery, child focus and exclusive/nonexclusive input capture. XTEST key-grab fixtures are not equivalent. |
| `c-probe-campaign.py`, `xvfb-wine-focus.py`, `probe-contracts.py`, `wine-focus-probe.c`, `fullscreen-lifecycle-probe.c` | Keep independent C clients, ASan/UBSan, protocol/property checks and semantic mutants. Compiler/startup errors must not count as mutant detection. Python handles their evidence manifests and process orchestration. |
| `xvfb-overview.py`, `overview-probe.c` | Keep rendered pixels, damage, client event counts, Picom ownership and absent Composite. Rust verifies geometry/mapping and keyboard toggles separately. |
| `harness-isolation.py`, `overview-isolation.py` | Keep negative tests of retained shell harnesses, including unrelated processes and startup failure. |
| `session-teardown.py` | Keep deterministic process identity, exec-client and stale-group teardown contracts. These differ from dropping testkit children. |
| `xvfb-stacking-campaign.py`, `saturation.py`, `xcb-request-profile.c` | Keep optional measurement/trace analysis in Python/C. Performance observers and raw-wire interceptors stay outside production and do not replace functional assertions. |
| `xvfb-keyboard.py` | Keep optional exhaustive modifier/key mapping and event-barrier diagnostics; Rust covers key grabs used in normal regressions. |
| `install-smoke.py`, `installer/tests/partition.py`, `installer/*.sh` | Installer is a separate Bash integration boundary with filesystem/PTY/tool doubles. Retain its dedicated CI job. |
| `common.sh`, `private-env.sh`, `tree_lines.py` | Keep glue/helpers while their shell consumers remain. |
| `session-suite.sh`, `session-isolation.sh`, `session-security.sh`, `xephyr-ipc-edge.sh` | Keep opt-in nested-session/control/security verification. A second-user security case requires an explicitly provisioned account. |
| `xephyr-suite.sh`, `compat-matrix.sh`, `xephyr-stress.sh`, `xephyr-client-death.sh` | Keep opt-in real applications, hostile C clients and stress scenarios. Xvfb fixtures do not establish Firefox/Wine/GL compatibility. |
| `xephyr-2mon.sh`, `xephyr-pointer-float.sh`, `xephyr-fs-pointer.sh`, `xephyr-fs-stale.sh`, `xephyr-fs-stale-deterministic.sh` | Keep opt-in RandR/pointer/nested-desktop scenarios. Not a hardware PASS from CI. |
| `xephyr-restart.sh`, `xephyr-restart-config.sh`, `xephyr-restart-stress.sh`, `xephyr-shutdown.sh`, `xephyr-disconnect.sh` | Keep opt-in real-client restart/config/shutdown/session-record contracts pending complete equivalence. |
| `xephyr-overview.sh`, `xephyr-real-fullscreen.sh` | Keep nested display wrapper and manual visual diagnostic; the latter records evidence and is not a regression gate. |

## Equivalence boundary

CI run [42420744760](https://github.com/Azytar/Maverick/actions/runs/42420744760)
ran all 19 Rust X11 tests and the original Python transient/vertical cases
successfully in the same campaign. The redundant Python cases and their xtask
entries were then removed. The Rust vertical case uses the actual default
Super+Control+J, Super+K/J and Super+Shift+K/J bindings and verifies server
geometry plus real/EWMH focus. The transient case additionally destroys each
generation in both orders. The metadata case subsumes the separate Python
focus invocation, so CI keeps that stricter native-client check only once.

## Environment limitations

The Rust fixtures require local Unix sockets. A sandbox denying AF_UNIX socket
creation cannot run Xvfb or socket-backed session tests: report that command as
failed/blocked, never skipped-success. Do not enable a TCP listener or reuse a
physical display to work around it. CI runs the full Xvfb campaign on Linux.
Physical multi-monitor, pointer and real-application checks remain opt-in; use
the corresponding scripts on a prepared Arch workstation with a nested server.
