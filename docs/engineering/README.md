# Azytar Engineering Standard — Maverick

This directory records the current engineering baseline and the minimum context
needed to continue staged work without reconstructing it from chat history.

## Stage status

| Stage | Status | Notes |
| --- | --- | --- |
| 0 — audit | complete | repository, CI, tests, architecture and language inventory inspected; baseline restored |
| 1 — Rust infrastructure | complete | dependency-free `maverick-xtask` owns Cargo/release/X11 orchestration; branch CI is green |
| 2 — selective migration | active | keep measured Python/C oracles unless a Rust migration demonstrates a concrete benefit |
| 3 — reliability | active | View reconciliation fixed-point property and minimal `RemoveView` contracts added |
| 4 — performance | active | desired/reconcile allocation baseline measured; first production copy removed |

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
cargo run -p maverick-xtask --locked -- x11
```

`check` runs format verification, workspace Clippy with warnings denied,
all-target/all-feature tests, then the default-feature workspace tests. `x11`
executes the existing isolated Python/C X11 campaign through the same local/CI
entry point without replacing its independent behavior oracles.

The CI workflow accepts pushes to `engineering/**`, supports manual dispatch,
uses read-only repository permissions and cancels superseded runs for the same
branch.

## Stage 4 first vertical result

The first measured target was the steady-state
`Placements -> DesiredState -> reconcile` path. The pre-change allocation
guard measured **two heap allocations per reconcile** after warm-up: one for the
copied desired-window vector and one for duplicate-coalescing state inside the
reconciler.

Production now borrows the already-owned `Placements` buffer through
`DesiredState`; tests retain an explicit owned fixture representation instead
of forcing ownership back into the live path. The deterministic regression guard
requires **one allocation per steady-state reconcile** for 1, 5, 10 and 50
windows. The remaining allocation belongs to reconciliation scratch state.

The branch passed the full Rust gates, installer suite and isolated X11 campaign.
This establishes correctness and the allocation reduction. It does **not**
establish a wall-clock, input-latency or presentation improvement. The next
performance check must use the existing Xvfb profile workload and then, only if
the effect survives there, controlled physical-hardware measurements.

## Next work

1. Keep Stage 3 focused on invariants that detect real state/effect regressions;
   do not duplicate the existing independent Carousel model.
2. Preserve Python/C harnesses whose measured role is process orchestration or
   an independent ABI/protocol oracle.
3. Begin Stage 4 with a deterministic allocation baseline for the
   `Placements -> DesiredState -> reconcile` path before changing it.
4. Compare any Stage 4 change against the existing Xvfb profile workload and
   retain before/after evidence; no performance claim follows from allocation
   counts alone.
