# Phase 0 — Baseline

## HEAD
`0d83275` — docs(config): retire the compositor keys from the sample configuration

## Working-tree state
Pre-existing modifications, unrelated to this campaign (docs/install only):
```
 M CHANGELOG.md
 M README.es.md
 M README.md
 M install.sh
 M tests/install-smoke.py
?? installer/
```
These are left untouched. No source files are modified at baseline.

## Test count
`cargo test` (root package): **470 passed, 0 failed**
- `maverick` lib/bin: 461 passed
- `tests/child_lifecycle.rs`: 5 passed
- `tests/no_wait_in_wm.rs`: 2 passed
- `tests/source_constraints.rs`: 2 passed

## Build status
`cargo check` — OK (dev profile, no warnings)

## Clippy status
`cargo clippy --all-targets --all-features -- -D warnings` — OK (no warnings)

## Environment
- Xephyr, Xvfb, xvfb-run, xprop, xwininfo, xev, xrandr available
- No xdotool / xte / wmctrl (stress input via XTEST needs a custom sender or xdotool alternative)
