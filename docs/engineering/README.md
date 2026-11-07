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
| 4 — performance | active | steady-state reconcile reduced from two allocations to zero with full X11 equivalence |
| 7 — security | active | dependency advisories/sources gate and explicit MSRV enforcement added |

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

`check` runs format verification, an explicit all-target workspace check,
Clippy with warnings denied for both all-features and default features, then
both workspace test configurations. `x11`
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

## Stage 4 second vertical result

The remaining steady-state reconciliation allocation came from the defensive
last-write-wins map used to coalesce duplicate window placements. That map is
scratch only: it is rebuilt from Desired every pass and is not a second source
of window-manager state.

`AppliedState` now retains that map's capacity between reconciliations. Each
pass clears and rebuilds the scratch contents, preserving the duplicate policy
without allocating after warm-up. The allocation regression now requires
**zero heap allocations per steady-state reconcile** for 1, 5, 10 and 50
windows.

The branch passes the full Rust gates, installer suite and isolated X11
campaign. This establishes the steady-state allocation invariant and functional
equivalence. It still does **not** establish a wall-clock or presentation
latency improvement; non-empty reconciliations may legitimately grow the
returned effects vector because emitting real X11 work is not the no-op case
measured here.

## Stage 7 dependency and MSRV result

The first dependency-policy run exposed a real consistency defect before it
found a vulnerability: Maverick declared Rust 1.82, but the locked development
graph contained `wit-bindgen 0.57.1` through
`proptest -> rand -> getrandom -> wasip2`. Cargo 1.82 cannot parse that
Edition-2024 package, so the declared MSRV was no longer a property of the
checked-in lockfile.

Maverick is an application and no repository documentation promised Rust 1.82
as an external compatibility contract. The workspace MSRV is therefore now
**Rust 1.85**, the first stable release with Edition 2024 support. CI runs a
separate Rust-1.85 workspace check over all targets/features so this value
cannot silently drift again.

`cargo-deny` runs through the official action pinned to an immutable commit.
The initial policy intentionally covers only two high-confidence classes:

- RustSec advisories, with no ignored advisory IDs;
- dependency sources, denying unknown registries and Git dependencies while
  allowing crates.io and local/path workspace crates.

The current lockfile contains no Git dependencies and the dependency-policy
workflow passes on Rust 1.85. License and duplicate/bans policy is deliberately
deferred until an explicit license/version inventory is reviewed; Stage 7 does
not create broad exception lists merely to make a stricter template pass.

## Stage 8 local consolidation

`engineering/next` joins the Stage 7 Rust infrastructure, allocation and
dependency work with `config/toml-hardening` (parser validation, bounded typo
suggestions, and runtime work reduction). Both original histories are kept by
merge commits. Three divergent fixes were carried forward separately: two X11
cursor visibility probe changes and a `ViewId` exhaustion guard.

The configuration merge deliberately keeps exact-semantic legacy key aliases
from the repaired Stage 7 baseline while retaining stricter TOML validation.
The hardening branch's removed-alias test was narrowed to non-equivalent old
settings; both the reload contract and new line-number diagnostics remain.

Static workflow validation, Bash syntax checking, Python compilation, the
benchmark-contract suite (7 tests), and stacking-audit unit tests (14 tests)
passed locally. Rust compilation and X11 integration are **not yet verified**:
the integration environment has no `cargo`/Rust toolchain or X11 development
headers, and the installer suite requires a non-root user that this environment
cannot switch to. No performance numbers for this merged tree are claimed.
This was the local verification state at consolidation. The branch was later
published at `6831720`; see Stage 9 for the subsequent integration results.

## Stage 9 integration validation

Starting revision: `6831720f9b1160284c49191e79b9c8b34e0e3ec1`, clean
`engineering/next`; local history contains 115 commits beyond the inspected
`origin/main`. The merge boundaries, configuration aliases/diagnostics,
borrowed DesiredState, reconciler scratch, View identity, event wait and CI
were reviewed before editing production.

The initial revision passed [CI run 42391563106](https://github.com/Azytar/Maverick/actions/runs/42391563106):
Rust 1.85 all-target/all-feature check; strict all-feature Clippy; **1,004
passing tests per feature configuration**, with two manual timing workloads
ignored in each; release build; installer suite; and all 29 X11 campaign
steps, including native C probes. Dependency policy also passed. These are
GitHub runner results, not local or physical-hardware measurements.

Local validation exposed a coverage gap: `desired_len` in the borrowed render
path was used only by `window-trace` but declared unconditionally. Default
Clippy with `-D warnings` failed even though all-feature Clippy passed. The
binding now shares the trace feature guard. `xtask check` and `xtask clippy`
now lint both feature configurations; `check` also performs the explicit
workspace check, so CI inherits the same gates without a separate command list.
No lint suppression, dependency or lockfile change was needed.

Additional regressions exercise duplicate-coalescing scratch across borrowed,
empty and shortened snapshots, and exact legacy aliases through live X11
reload after canonical configuration. Existing per-field semantic fallback
policy is preserved: syntactically invalid, missing or unreadable files leave
the active configuration untouched; individually invalid fields in a parsed
file use compiled defaults, as the reload test already specifies.

Local Rust 1.99.1 checks and strict Clippy pass for both feature configurations;
release builds and the new scratch regression pass. The benchmark-contract
suite (7 tests), stacking-audit unit tests (26 tests), Bash syntax and Python
compilation pass. This runtime denies Unix socket `bind()` with EPERM:
workspace IPC tests fail and Xvfb cannot open a listener. The installer suite
requires a non-root user. Those environment failures are not skipped or
converted to passes; the updated branch must pass CI before Stage 10 starts.
No physical-session, latency or performance result is claimed here.

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
