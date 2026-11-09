# Arch validation and selective integration

Date: 2026-11-08. This campaign ran on Arch Linux, kernel
`7.2.10-x412dap-r2`, with Rust/Cargo 1.99.0. Xvfb and Xephyr results below
are automated nested-server results, not a physical-session PASS.

Machine-readable results and release hashes are in
[validation/stage18-results.json](validation/stage18-results.json). Full logs,
including failed attempts, are retained locally in
`/home/axiom/.cache/maverick-stage18-validation/logs/`.

## Repository verification

Public HTTPS fetches confirmed Maverick `engineering/optional-control` at
`1987f550cea14d54dc482c1fc51ec0dbc786299c`, and main at
`7276eb1165540552c372c9ff32d2ad9f1f71440e`.
The independent client was checked out at
`60ca498354643bac7b8a7b47b0e20e44692a5e39`; its later engineering head
`80b4f7a7f9ddc859a548d256b8a370ef06fa41e2` changes documentation.
The former vendored client matched the pinned checkout byte for byte before
retirement. A real fetch by that immutable SHA succeeded.

The desktop WM remained running: its supporting window stayed `0x400002`.
No desktop WM replacement, restart, installation into the user's prefix or
personal configuration replacement was performed. Installer runs used private
HOME/XDG directories and temporary prefixes.

## Changes and reproducible findings

- With cursor warping enabled, clicking another tile moved the pointer from
  `(82,300)` to `(400,300)`. The new regression failed on the original release
  and passed after separating pointer focus from keyboard warping. Pointer
  focus also cancels a deferred warp while preserving owed layout work.
- A control restart returned `no reply from the instance` even though re-exec
  occurred. Lifecycle/dispatch receipts now gate queue draining until the
  socket write completes. The WM uses a nonblocking lock attempt, and guard
  release re-arms its wake pipe. A unit test covers deferred delivery,
  nonblocking consumption and the replacement wakeup. The client pin and wire
  version remain unchanged.
- The two-monitor harness used the semantic `view` command where it intended
  raw dispatch. It now checks `msg view` failures explicitly.
- The pointer harness assumed both columns were visible, had warping disabled,
  and sent drag events to the inherited host DISPLAY. Its private configuration
  now exposes both columns with warping enabled; every drag targets Xephyr.
- That harness also retained an obsolete drop-to-tile expectation, explicitly
  identified as obsolete by existing main commit `177a68a`. It now asserts that
  dragging preserves floating membership, actually moves the float and preserves
  the neighbouring tile, then requires an explicit toggle to restore tiling.
  Right-button resizing is checked too.
- The Rust transient oracle compared real and published stacks during a focus
  transition. It retains exact equality assertions and additionally requires
  200 ms of matching, stable stacks. Reload key spacing is measured after the
  WM's observable reload, so the documented debounce is respected under load.
- An ephemeral X11 startup probe could trigger an Xvfb reset while the WM opened
  its connection. The float fixture now waits for a WM-thread query response.
  Its existing focus, geometry and stacking checks remain intact.

Initial builds exhausted the restricted `/tmp` quota. Build artifacts were
moved to the dedicated cache and the required gates rerun successfully.

## Executed results

| Check | Engineering | Integration candidate |
| --- | --- | --- |
| `cargo run -p maverick-xtask --locked -- check` | PASS | PASS |
| `cargo run -p maverick-xtask --locked -- release` | PASS | PASS |
| `cargo run -p maverick-xtask --locked -- x11` | PASS | PASS |
| Rust X11 campaign on Xvfb | 22/22 Minimal, 22/22 control | Existing main Python/C campaign retained |
| Final Rust X11 campaign on host-backed Xephyr | 22/22 Minimal, 22/22 control | 22/22 Minimal, 22/22 control, using the engineering runner |

Each explicit Rust X11 campaign reported zero ignored tests. Ordinary workspace
tests intentionally leave these ignored until the explicit campaign runs.
Integration Cargo commands used `CARGO_INCREMENTAL=0`; assertions and feature
coverage were unchanged.

The required X11 campaigns also passed the external keyboard/input ownership,
fullscreen/restoration, reload/restart, cursor/theme, Overview/pixel/compositor,
stacking and sanitizer/negative-control probes. Each C campaign reported
15 cases with no failures. No latency or performance improvement is claimed.

Additional Xephyr checks passed:

- Two logical RandR monitors: 18 checks, including per-monitor focus, ownership,
  fullscreen clipping, maximization, Views and pending focus.
- Pointer/float/resize scenarios, Overview pixels/events, configuration reuse
  across repeated restarts, and workarea changes caused by docks.
- Pinned-client queries, actions, real event subscriptions, a nested RandR
  resolution change, and 50 acknowledged restarts. Every restart had to produce
  another `maverick ready` record; a receipt alone did not count as completion.

The independent client passed fmt, strict Clippy and 285 tests. Its release
binary links libc/libgcc, without X11 or the WM engine. The full installer
suite passed, including no client fetch/build in Minimal, profile migration,
configuration preservation, foreign/modified/special client preservation,
and rejection of a mismatched fetched revision. The mismatch test uses Git
doubles; the successful SHA fetch was also exercised with real Git.

Real release artifacts were installed into private prefixes and migrated in
both directions. An unchanged owned client was retired on Minimal migration;
modified/foreign clients and personal configuration survived. These runs used
`--no-build`; compilation was independently exercised by the release gates.
`bash -n` passed; shellcheck was not installed.

One initial legacy fullscreen/pointer run missed fullscreen activation and
reported 6/8 checks. A repeat and final runs of both modalities passed 8/8.
Its optional input-trace assertions
were skipped on stock binaries. This isolated initial observation is retained
in the logs and is not represented as a diagnosed fullscreen defect.

## Separation and integration boundaries

Maverick no longer versions a copy of `maverickctl`. The optional ignored
checkout, installer development override `--controller-source DIR`, immutable
pin, independent Cargo workspace/lockfile and Minimal-without-client behavior
are preserved.

Maverick's integration branch starts from main and contains three selective
commits: `5351288`, `9eead19`, `8f98779`. Runtime/installer/CI dependencies from
`b330e38`, `f1b5034`, `f534536` and `1987f55` were selected together, followed by
the pointer fix and receipt-order correction. The engineering testkit commit
chain was not merged. This is not a fast-forward of the engineering branch.

The candidate adds only the runtime file `maverick-sys/src/protocol.rs`.
Deletions relative to main are confined to the intentionally extracted client.
Every existing main test remains; its Python/C campaign and necessary fixture
adaptations are retained. New testkit, validation scripts, reports and evidence
stay on engineering.

The client candidate `3f9e8ce` starts at its main `402abaa` and differs from the
tested pin only in `PROTOCOL.md`. Its runtime, lockfile, normal regression tests
and CI are identical to the verified pin. Neither main branch was modified.

The previous remote CI runs were independently checked: Maverick
[4/4 jobs](https://github.com/Azytar/Maverick/actions/runs/42443220880), client
[Rust 1.85/stable](https://github.com/Azytar/maverickctl/actions/runs/42443058350).
Those baseline runs do not establish CI success for newly prepared branches.
The initial SSH attempts failed with `Permission denied (publickey)` because
no identity was available. They published no branches and triggered no CI.
Axiom subsequently authorized publication and provided a loaded SSH agent;
SSH fetches then succeeded. The failed attempts remain historical evidence in
`initial_publication_attempt`. Publication uses only work branches over SSH,
without changing either main branch or substituting an HTTPS push.

Release binaries and the standalone X11 test runner are retained in the cache.
Generated debug caches were removed after verification to reclaim disk space.
The runner path and hash are recorded in the JSON evidence; rerunning it
requires explicit WM/profile environment variables.

## Remaining physical observation

No physical-session validation is claimed. Axiom still needs the short checklist
in [ARCH_VALIDATION.md](ARCH_VALIDATION.md) with real keyboard/pointer input,
actual applications, GPU presentation and real monitor mode changes/hotplug,
for each modality. Replacing the desktop WM requires separate authorization.
