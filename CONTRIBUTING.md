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
The harnesses are not all isolated to the same standard — some helpers in
`tests/common.sh` kill processes by name or use fixed displays — so read a script
before running it and reserve the older ones for a disposable graphical session.

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
