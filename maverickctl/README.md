# maverickctl

Independent Unix control client and session manager for the Maverick X11 window
manager. The client builds without cloning or linking the window manager.
Requires Linux and Rust 1.85 or newer.

```sh
cargo build --release --locked
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
./target/release/maverickctl --help
./target/release/maverickctl session --help
```

The default build includes CLI commands, discovery, JSON queries, subscriptions,
window/View actions, reload/restart/quit, and Xvfb/Xephyr session lifecycle tools.
A running WM must explicitly enable `control-ipc`; Maverick Minimal offers no
control socket. The client reports incompatible control protocol versions before
sending control commands. Ping and identify remain available for discovery of
legacy instances without treating them as controllable.

`control/` contains the versioned framing contract, identity/path helpers and
JSON codec needed by the client. Its `test-server` feature compiles the extracted
reference listener/hub and existing fd-poll boundary for regression tests only;
normal release builds include no listener, hub, WM engine, X11 library, signal
handlers or terminal-detachment code. The original client tests and protocol
property tests are retained. Both Rust 1.85 and stable are checked in CI.

The protocol and test fixture originate in Azytar/Maverick, with GPL-3.0 code
and authorship preserved. This repository does not declare a Git dependency on
the WM. See [PROTOCOL.md](PROTOCOL.md) for compatibility and bounds.
