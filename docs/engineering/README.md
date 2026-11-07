# Azytar Engineering Standard — Maverick

This directory records the current engineering baseline and the minimum context
needed to continue staged work without reconstructing it from chat history.

## Stage status

| Stage | Status | Notes |
| --- | --- | --- |
| 0 — audit | active | repository, CI, tests, architecture and language inventory inspected |
| 1 — Rust infrastructure | active | minimal dependency-free `maverick-xtask` added for Cargo gates and release builds |
| 2+ | not started | do not migrate test harnesses until the Stage 0 baseline is green |

## Stage 0 baseline

Baseline inspected: `main` at
`c45697b63455206834e53643aab12801c8462373`.

The previous successful CI baseline was
`807218896c05e3ffdaa565799ab523fca84e418e`. The current `main` build was
red after configuration hardening. The failures were traced to contract drift:

- unit tests still exercised exact configuration aliases that the parser had
  stopped accepting;
- X11 reload/restore tests still used those aliases;
- one unit test called the removed `theme_palette` helper;
- the reload test expected an out-of-range gap value to become live geometry
  even though validation correctly rejects it.

The compatibility decision is to preserve only aliases with exact semantics,
such as `n_tags -> view_count`, `gaps -> gaps_inner + gaps_outer`,
`auto_workspace_binds -> auto_view_binds`, the old color names and the
rule aliases. Old width/bias settings whose units or semantics changed remain
removed and diagnostic-only.

No production-performance improvement is claimed by this work.

## Language and test inventory

At the audited tree the repository contains substantial Rust production code,
plus Python/Bash orchestration and native C X11 probes. These are not all
migration targets.

- Rust owns production state, layout, X11 integration, system/session support,
  configuration parsing and most deterministic unit/property tests.
- Python owns several isolated X11 harnesses, process lifecycle campaigns,
  result validation and installer tests.
- Bash owns the installer and some compatibility rigs.
- C probes deliberately exercise Xlib/ICCCM/EWMH behavior from an independent
  native client and are valid interoperability oracles.

The existing measured audit in `docs/audits/python-c-tests.md` remains
authoritative: JSON/result validation and process orchestration should not be
rewritten merely to increase the Rust percentage. A migration must reduce
complexity, improve synchronization/oracles, or remove a demonstrated cost.

## Environment-dependent tests

The ordinary workspace suite can run without the owner's desktop, but several
integration suites require system interfaces:

- X11 suites use owned Xvfb displays and X11 utilities/libraries.
- Native probes require a C compiler and Xlib headers.
- Some input/overview checks require XTest, Composite, Render, Damage or a
  compositor.
- No current Maverick Stage 0 test requires the owner's physical GPU.

Tests must keep an owned display/runtime and must never signal processes by
global name.

## Stage 1 command surface

The dependency-free `maverick-xtask` is intentionally small. It delegates to
Cargo rather than creating a second build system.

```sh
cargo run -p maverick-xtask --locked -- check
cargo run -p maverick-xtask --locked -- release
```

`check` runs format verification, workspace Clippy with warnings denied,
all-target/all-feature tests, then the default-feature workspace tests. CI uses
the same command graph.

The CI workflow accepts pushes to `engineering/**`, supports manual dispatch,
uses read-only repository permissions and cancels superseded runs for the same
branch.

## Next work

1. Restore a fully green Stage 0 baseline.
2. Extend `xtask` only when a concrete suite needs a shared local/CI entry
   point; do not grow it into a generic framework.
3. Choose the first X11 Python migration by demonstrated benefit. Prefer a
   harness where Rust can replace arbitrary sleeps with observable X11/process
   conditions.
4. Preserve C probes that provide an independent ABI/protocol oracle.
5. Begin performance work only after correctness gates are stable and retain
   before/after evidence for every claim.
