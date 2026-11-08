# Arch validation before integration

CI proves Linux/Xvfb behavior; it does not establish a PASS for Axiom's physical
Arch session. Run these commands from the development checkout on Arch with the
existing README's Rust/X11/C test dependencies installed:

```sh
rustc --version
cargo --version
uname -a
cargo run -p maverick-xtask --locked -- check
cargo run -p maverick-xtask --locked -- release
cargo run -p maverick-xtask --locked -- x11
```

The Rust campaign must report 21 successful tests for Minimal and 21 for control,
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
Minimal through X11/XTEST alone. Record command exit statuses and any observed
focus, fullscreen, pointer, restart or monitor failure. Do not infer real-client
compatibility or physical latency from Xvfb timings. Main integration remains
pending that evidence; the independent client is published and pinned at
`60ca498354643bac7b8a7b47b0e20e44692a5e39`.
