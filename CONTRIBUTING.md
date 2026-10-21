# Contributing to Maverick

Maverick is a Unix-style X11 tiling window manager: it arranges windows on an
X11 display, publishes the EWMH properties a desktop expects, and stops there.
It is not a desktop environment, and contributions are judged against that
scope. [README.md](README.md) describes the current tree,
[docs/architecture.md](docs/architecture.md) describes the module boundaries,
and [docs/sessions.md](docs/sessions.md) describes the session model and its
security boundary.

## What fits

- Bug fixes, including failures that only appear under a real X server, a
  particular client, or a particular configuration.
- EWMH or ICCCM behaviour Maverick should be getting right and is not.
- Layout, View and Carousel behaviour consistent with the documented model.
- `maverickctl` commands, exit codes and diagnostics.
- The installer in `installer/`, and the test harnesses in `tests/`.
- Documentation that is wrong about the current tree.

## What does not fit

- A desktop environment, desktop compositor, GPU path, wallpaper subsystem,
  notification daemon, system tray, or animation system. These are out of scope
  and are listed under *What Maverick is not* in the README.
- Wayland support.
- A second layout. Scroll is the only layout; `LayoutKind` has one variant.
- Configuration surface growth for its own sake. The in-tree TOML parser
  supports a subset of TOML, not the specification.
- Large mechanical rewrites, renamed subsystems, or reformatting of files a
  change does not otherwise touch.

Overview's temporary image presentation belongs in the existing X11 backend.
It may use Composite, Render and Damage without owning the desktop compositor
selection, redirecting the root, or adding a permanent rendering loop.

Unrelated refactoring should not be mixed into a feature or bug-fix change. A
reviewer has to be able to see whether the change is correct, and a rename
folded into a fix makes that impossible. Split it.

## How to make a change

- Keep it small and coherent. One intent per change.
- Preserve the minimal architecture. New code belongs behind an existing
  boundary — the core owns state, the X11 backend owns requests — rather than
  beside it.
- Avoid unnecessary dependencies. A new crate or a new `Cargo.toml` entry needs a
  concrete reason; `libc`, `rustix` and `x11rb` are the whole native and runtime
  dependency set today.
- Include tests appropriate to the change. Behaviour that can be decided without
  a display belongs in a state test; the X11 boundary is covered by the
  harnesses in `tests/`.
- A test must assert the property it claims, by parsing output or by calling the
  function, not by matching a substring that also survives the bug it is meant to
  catch.
- A test must not depend on the host: not on the console's display, not on an
  installed X server, and not on the state of `/tmp`. Tests that manufacture the
  state they need are deterministic everywhere; tests that assume it pass on one
  machine and fail on another.
- Update the README, the configuration sample or `CHANGELOG.md` when public
  behaviour, a default, a flag or a command changes.
- Do not change behaviour and add a workaround for it in the same change.

## Build and test

Rust 1.82 or newer is the minimum (`rust-version` in `Cargo.toml`). The X11
development libraries are required to link; the Debian/Ubuntu packages are
listed in the README under *Requirements*.

```bash
cargo build --release -p maverick -p maverickctl --locked
cargo check --workspace --all-targets --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --locked
```

`cargo fmt --all -- --check` is read-only; `cargo fmt --all` applies. Clippy is
run with default lints only. `--locked` is used on every command that resolves
the dependency graph, and CI fails on a stale `Cargo.lock`.

The installer is bash and has its own checks:

```bash
bash installer/lint.sh                  # bash -n, plus shellcheck when present
python3 installer/tests/partition.py    # installer behaviour suite
```

The harnesses in `tests/` need a real X server. `tests/xvfb-stacking.py` is the
automated regression and is what CI runs. `tests/xephyr-*.sh` are manual
integration scenarios; `tests/session-suite.sh` and `tests/session-security.sh`
cover sessions and their security boundary, and the session suite exits 77 with
a reason when it cannot run, so "could not run" is never reported as "passed".
Shell X11 harnesses use private runtime/configuration directories and clear
inherited instance selection. A requested fixed display must be unused; startup
verifies that the child server owns its lock before using it. Tests never remove
another server's lock or kill processes by name. The compatibility/browser
scenarios also use private Firefox profiles.

`tests/common.sh` registers child PIDs and kernel start times in files so
command substitutions cannot lose them. Its cleanup first uses
`maverickctl quit --session <test-id> --yes`, then falls back to signals for
registered processes only. Signal/crash tests deliberately signal their own
captured PIDs. Register new helpers with `mav_track "$!"`; propagate startup
failure before launching clients.

`python3 tests/harness-isolation.py` exercises successful and failed IPC cleanup
with a separate live WM that must survive and keep answering. It also checks
occupied displays, stale PID records and preservation of the caller's runtime
directory. CI runs it with Xvfb, so it needs no active desktop.

`python3 tests/session-teardown.py` creates a private Xvfb session and adopts
its orphaned children as a Linux subreaper. Shutdown must succeed even before
those exited children are reaped, while an unrelated process remains alive.
`--exec-client` also checks that a registered application and its child stop;
`--stale-group` verifies that a mismatched group identity grants no authority
over an unrelated process and makes incomplete shutdown explicit.

For an interactive sandbox, the existing session interface owns the whole
nested server: create a uniquely named session with
`maverickctl session create <test-name> --backend xephyr --binary <wm-path>`,
launch clients with `maverickctl exec <test-name> <program>`, and close it with
`maverickctl session stop <test-name>`. Instance `quit` and session `stop` have
different scopes; always name the test target explicitly.

CI (`.github/workflows/ci.yml`) runs three jobs: the workspace with strict
Clippy and both feature sets, the installer checks, and the Xvfb stacking smoke
test. All three must pass.

`python3 tests/xvfb-overview.py` checks displayed pixels and client-side
configure/map/unmap events across entry, navigation, repaint and exit. Its C
probe needs `libxcomposite-dev` in addition to `libx11-dev`; `--compositor`
checks Picom coexistence and `--without-composite` checks safe refusal.
The Xephyr wrapper uses the same pixel/event regression on its own display.

`python3 tests/xvfb-cursor.py` checks displayed cursor pixels before any client
opens, after opening and closing a client, and after shutdown. Its C probe
needs `libxfixes-dev` in addition to `libx11-dev`.

`python3 tests/xvfb-cursor-theme.py` measures cursor pixels using a generated
theme, covering session defaults, environment/resource precedence, DPI-derived sizes, View
switches, client cursor ownership and fallback without a usable theme or Render.
It uses the same probe and libraries; no installed cursor theme is required.

`python3 tests/xvfb-reload.py` checks focused tile visibility after changing
spacing, borders and accordion settings, including extreme gaps and seeded
combinations. It also checks that unchanged and malformed reloads preserve a
deliberate camera pan. It uses its own Xvfb and needs `libx11-dev`.

`python3 tests/xvfb-restart.py` checks layout, geometry, fullscreen transitions,
real X11 focus and opt-in autostart across repeated re-execs on its own Xvfb.
It needs `libx11-dev`; the clients stay open throughout the restart checks.
`--pointer-focus` enables hover focus and cursor warping before restarting;
the restored selection and camera must survive a stationary pointer.

`python3 tests/xvfb-restore.py` checks client fullscreen exit versus explicit
layout restoration, tiled/floating geometry, both maximized axes, EWMH flags,
compositor bypass and the default restoration shortcut. Repeated restoration
must produce no client configure/property events. It uses its own Xvfb and the
stacking probe, requiring `libx11-dev` and `xdotool`.
It also checks invalid EWMH state actions, attention transitions, repeated
maximize requests and workarea metadata after live View and dock changes.

`python3 tests/xvfb-floats.py --case focus` checks real X11 stacking when tiles
map, floats take focus and fullscreen changes its presented owner.
`--case transients` checks parent/dialog/sub-dialog ordering and focus on an
unrelated float. Both use their own Xvfb and the stacking probe (`libx11-dev`).
`--case metadata` compares the published stack with real X11 order, including
fullscreen covering a dock created afterwards.
`--case vertical` uses `xdotool` to exercise the default focus and row movement
shortcuts after merging columns, including the stale row cursor from collapse.

`--case profile` is an optional CPU/RSS and control round-trip workload, with
`--clients`, `--actions`, `--idle-seconds` and optional `--trace-output`.
It does not impose timing thresholds in CI; [docs/performance.md](docs/performance.md)
describes its scope and how to interpret the measurements.

`python3 tests/xvfb-input.py --case protocol` validates ICCCM timestamps;
`--case ownership` checks transfers and activation protections; `--case child`
checks cached-time desynchronization; `--case capture` checks core/XI2 keys,
InputOnly confinement and mode changes. `--exclusive` selects true fullscreen;
`--server xephyr` runs the same checks in a private nested Xephyr. The probe
needs `libxi-dev` and `libxtst-dev` in addition to `libx11-dev`. CI runs every
Xvfb case and `tests/xvfb-wine-focus.py` against the release binary.
See [docs/x11-input.md](docs/x11-input.md) for real-session diagnostics and
the distinction between protocol regressions and testing actual games.

`python3 tests/overview-isolation.py` checks the Overview rig's startup and
cleanup with tool doubles, without needing an X server. It verifies that the
rig uses only the display claimed by its child, reaps that child on failure,
and never requests global process kills. CI runs it alongside the Xvfb smoke.

## Reporting bugs and proposing features

Use the issue templates. A bug report is reproducible from the description
alone: which Maverick version, the X11 environment, the application involved,
the steps, and what was expected instead. A feature request describes the problem
first and then the proposed behaviour, with the alternatives already considered.

For a change in public behaviour, add an entry under `[Unreleased]` in
[CHANGELOG.md](CHANGELOG.md), which follows Keep a Changelog.

## Security

Do not report a vulnerability through a public issue, a pull request, or
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Follow [SECURITY.md](SECURITY.md).

## Licensing

Maverick is licensed under the GNU General Public License, version 3; see
[LICENSE](LICENSE). Contributions are accepted under the same terms.

### C probe contracts and negative controls

`python3 tests/c-probe-campaign.py --wm target/release/maverick` runs the owned
Xvfb probes, ASan/UBSan property checks and deliberate faulty probes. A negative
control passes only when its named check fails (or its expected runtime sanitizer
report appears); a compile/startup failure does not count as detecting a mutant.
Add `--extended` for 1024 unmap/remap cycles. `--evidence DIR` retains the per-case
commands, binary/source hashes, transcripts, process exit statuses and timing.

`python3 tests/probe-contracts.py` needs a C compiler and Xlib headers, but no
DISPLAY. It checks malformed result streams, argument bounds and decoder errors.
Both C probes support `--describe` without opening X11; the harness requires every
inventoried check exactly once, in order, with JSON booleans and a final completion
marker. Nonzero exit status still fails even if every emitted check says true.

To run only the fullscreen probe's server property checks:

```bash
python3 tests/xvfb-wine-focus.py --wm target/release/maverick \
  --probe tests/fullscreen-lifecycle-probe.c --complete fullscreen-lifecycle \
  --case properties --sanitize
```

The other cases are `lifecycle` and `publication`. Existing
`AUDIT_PUBLICATION_ROUNDS=128` invocations remain supported; empty, zero, negative,
malformed or greater-than-4096 counts fail before connecting to X11. A missing
`_NET_WM_STATE` can describe a normal window, but a missing client list does not
prove withdrawal: publication checks wait for a typed list. The idle regression
uses property queries only, without injecting a client event or IPC barrier to
wake the WM. Keep that constraint when extending the test.

The tests distinguish selection, real X input focus, map state and geometry.
Keyboard delivery and XInput2 capture are still tested by `xvfb-input.py`; a focus
query alone does not prove that the destination receives keys. No test here
establishes Wine's internal focus or the behavior of a real game.
