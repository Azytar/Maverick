# Arch validation before integration

CI proves Linux/Xvfb behavior; it does not establish a PASS for Axiom's physical
Arch session. Run these commands from the development checkout on Arch with the
existing README's Rust/X11/C test dependencies installed:

Prepare the independent checkout using the README build instructions first.
The `maverickctl/` directory must be a checkout at
`60ca498354643bac7b8a7b47b0e20e44692a5e39`, not the former vendored copy.

```sh
rustc --version
cargo --version
uname -a
cargo run -p maverick-xtask --locked -- check
cargo run -p maverick-xtask --locked -- release
cargo run -p maverick-xtask --locked -- x11
```

The Rust campaign must report 22 successful tests for Minimal and 21 for control,
without skipped tests in either run. It checks real X input focus/stacking,
keyboard reload/restart, and the absence of control runtime files/background
threads in Minimal. Default Cargo builds target only the Minimal WM. The
prepared independent client is built separately for the control campaign.

From an X11 desktop with Xephyr available, the retained nested-display checks
exercise the actual local input/monitor environment without replacing the WM
that owns the desktop:

```sh
MAVERICK_BIN="$PWD/target/release/maverick" \
MSG_BIN="$PWD/target/release/maverickctl" \
bash tests/xephyr-2mon.sh

MAVERICK_BIN="$PWD/target/release/maverick" \
MAVERICK_CTL="$PWD/target/release/maverickctl" \
MAVERICK_MSG="$PWD/target/release/maverickctl" \
bash tests/xephyr-pointer-float.sh
```

These legacy nested rigs use the control profile. The new Rust fixtures test
Minimal through X11/XTEST alone. To run the same 22 tests on an owned Xephyr
over the current X11 desktop, run each compiled profile explicitly:

```sh
MAVERICK_TEST_XSERVER=xephyr MAVERICK_TEST_PROFILE=minimal \
MAVERICK_TEST_WM="$PWD/target/minimal/release/maverick" \
cargo test -p maverick-testkit --test owned_xvfb --locked -- \
  --ignored --nocapture --test-threads=1

MAVERICK_TEST_XSERVER=xephyr MAVERICK_TEST_PROFILE=control \
MAVERICK_TEST_WM="$PWD/target/release/maverick" \
cargo test -p maverick-testkit --test owned_xvfb --locked -- \
  --ignored --nocapture --test-threads=1
```

Xephyr alone inherits the host DISPLAY/XAUTHORITY; the WM and probes use only
the allocated nested display and private HOME/XDG directories. The default
backend remains Xvfb; this option never attaches the WM to the host display.

Record command exit statuses and any observed
focus, fullscreen, pointer, restart or monitor failure. Do not infer real-client
compatibility or physical latency from Xvfb timings. Main integration remains
pending that evidence; the independent client is published and pinned at
`60ca498354643bac7b8a7b47b0e20e44692a5e39`.

## Axiom's physical observation

Automated Xvfb and Xephyr passes do not complete physical-session validation.
During a separately authorized physical session for each build, keep existing
applications open and check:

1. Navigate two tiles and one dialog with the physical keyboard and pointer;
   drag and resize the float, then fullscreen and restore a tile.
2. Change Views, enter/leave Overview and confirm no stuck input or lost focus.
3. Reload a harmless setting with Super+Control+R, then restart five times with
   Super+Shift+R; verify the same clients, selection and geometry survive.
4. Change a real monitor's resolution or connect/disconnect an available second
   monitor; confirm visible clients remain reachable and focus follows selection.

Record build SHA/profile, GPU, monitors, client names and observed defects.
Do not perform this checklist by replacing the desktop WM without permission.
