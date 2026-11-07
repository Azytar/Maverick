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
| 9 — integration | complete | merged tree verified in CI; default-feature lint gap repaired and regression coverage extended |
| 10 — measured continuation | active | unique-placement reconcile avoids a redundant lookup; reproducible CPU workload added |

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
converted to passes; the updated branch passed all four jobs in [CI run 42393108260](https://github.com/Azytar/Maverick/actions/runs/42393108260)
before Stage 10 started: **1,005 tests passed per feature configuration**,
release, installer, MSRV and all 29 X11 steps, including the new alias reload.
Local Rust 1.85 all-target/all-feature checking also passed. The new borrowed
scratch regression and 46 release reconciliation tests passed locally; the
model/parser suite passed 93 tests with `PROPTEST_CASES=1024`, and release
configuration tests passed 54 tests (one manual workload ignored).
No physical-session, latency or performance result is claimed here.

## Stage 10: unique-placement reconciliation

The duplicate-coalescing map was built on every reconciliation and then looked
up again for **every** desired window, even when every placement was unique.
The map length already answers whether any duplicate exists. Reconciliation
now uses that answer to avoid the second lookup for unique snapshots; duplicate
snapshots retain last-write-wins, request ordering and the existing filter.
The map is still cleared and rebuilt on each pass. No persistent validity flag,
new cache, unsafe code, dependency or allocation policy was introduced.

A manual release workload was added **before** changing production. Baseline
commit: `7c67e20e6e84cfdaa25b36fb469f649ac644b573`. It exercises the real
borrowed DesiredState/reconcile path with 1, 5, 10, 50 and 200 windows, a duplicate
snapshot, and continuously changing geometry. Assertions check effect counts;
ordinary tests keep timing out of CI. Reproduce at the baseline and candidate:

```sh
cargo test --release -p maverick --bin maverick --locked reconcile_workload -- --ignored --nocapture
```

Measurements used Rust 1.99.1, release/default features, AMD EPYC 9V74 in this
container. Six balanced, shuffled AB/BA process pairs each ran nine batches of
20,000 calls per case after warm-up. The table is the median of the six process
medians; raw process summaries, order, environment and binary hashes are in
[`2026-11-07-reconcile.json`](../../benchmarks/results/2026-11-07-reconcile.json).

| Snapshot | Before, ns/call | After, ns/call | Reduction |
| --- | ---: | ---: | ---: |
| 1 unique window, settled | 40 | 35 | 12.5% |
| 5 unique windows, settled | 182.5 | 155.5 | 14.8% |
| 10 unique windows, settled | 366.5 | 317.5 | 13.4% |
| 50 unique windows, settled | 1754.5 | 1517.5 | 13.5% |
| 200 unique windows, settled | 7029 | 6121.5 | 12.9% |
| 50 windows plus a duplicate, settled | 1757.5 | 1806.5 | -2.8% |
| 50 unique windows, changing | 2008 | 1703.5 | 15.2% |

The unique-case ranges do not overlap in this experiment. Duplicate-case
ranges overlap (1743–1879 vs 1755–1829 ns), so no improvement is claimed for
that defensive path. The result measures CPU reconciliation with warm maps,
including real client lookups and effect allocation when geometry changes.
It does not measure X11 latency, scanout, input latency, physical desktop
performance or battery use. Both versions already allocate zero on the settled
path: the timing result is independent of that allocation invariant.

Local validation passes both strict Clippy configurations, 39 reconciler tests
with `PROPTEST_CASES=1024`, and both arrange/reconcile allocation guards.
The publication gate remains the same complete CI campaign, including native C
X11 clients; environment-dependent local failures remain documented above.

## Stage 10 continuation: focus requests and urgent appearance

Starting revision: `6fb8849`. The focus path was inspected before changing
production. `has_protocol` already caches `WM_PROTOCOLS`: its two reads inside
one `focus()` were duplicate cache lookups, not two server round trips.
The earlier saturation harness references scratch-only tools that are absent
from this checkout, so it was not presented as a runnable measurement.

The existing isolated float campaign now accepts `--request-output` and
`--focus-pattern cycle|repeat`. A test-only C preload counts x11rb's RAW XCB
submissions through both 64-bit submission APIs; nested forwarding is counted
once. Capture is scoped to the WM, and never enabled on clients or production
sessions. An independent Xlib client verifies real input focus and
`_NET_ACTIVE_WINDOW` after each action. Instrumented runs omit timing metrics:
these are queued request counts, not server completion, flush counts or latency.

The production-unchanged baseline (`122f331`) was measured in
[CI run 42395522831](https://github.com/Azytar/Maverick/actions/runs/42395522831).
Two clients and 20 actions per case produced:

| RAW XCB operation | Alternating focus | Repeated focus |
| --- | ---: | ---: |
| SetInputFocus | 20 | 20 |
| GetInputFocus | 0 | 0 |
| ChangeWindowAttributes | 40 | 20 |
| ConfigureWindow | 40 | 0 |
| QueryTree | 20 | 0 |
| ChangeProperty | 41 | 21 |
| GetProperty | 1 | 1 |

The measured interval includes a workload-completion urgency sentinel. Its
metadata work is included above. Reproduce the cycle and repeat cases with:

```sh
python3 tests/xvfb-floats.py --case profile --clients 2 --actions 20 --idle-seconds 0.05 --request-output /tmp/focus-cycle.jsonl
python3 tests/xvfb-floats.py --case profile --clients 2 --actions 20 --idle-seconds 0.05 --focus-pattern repeat --request-output /tmp/focus-repeat.jsonl
python3 tests/xvfb-floats.py --case urgency
```

The native urgency regression failed on that baseline: focus and the active
window agreed and the attention atom cleared, but the border retained the
urgent color. `focus()` now paints `col_focused` when it consumes urgency.
The protocol eligibility result is also reused within that call. No event is
dispatched between the reads, and later property notifications still invalidate
the existing cache. Explicit focus requests, server timestamps, child-focus
handling and Globally Active offers remain intact; no timing improvement or
request reduction is claimed from this correction.

The same CI run separately exposed a Proptest-1.11 float-sampler panic before
Maverick's presentation assertion ran. A standalone reproducer needs only a
PassThrough RNG with eight `0xff` bytes followed by zeroes and the range
`0.05f32..=1.0`. Root property fixtures now sample integer encodings for affected
positive float ranges, preserving every representable value and both endpoints
while changing the sampling distribution. The independent core test uses the
same encoding technique. No property/assertion is removed and no dependency is
changed. A regression runs the exact panic-triggering RNG stream; presentation
properties also passed 10,000 cases each locally with a sufficient reject budget.

The standard `xtask x11` campaign includes both request profiles and urgency,
so CI verifies them together with the existing native protocol clients. Local
strict Clippy in both feature modes, MSRV 1.85 and release builds pass. This
container still forbids X11/IPC sockets; final live validation runs in CI.

## Stage 11 — authoritative stacking observations

The production-unchanged baseline is `e0b7126`, measured in
[CI run 42398768369, attempt 2](https://github.com/Azytar/Maverick/actions/runs/42398768369/attempts/2).
The first attempt stopped at the cursor test's signal shutdown timeout. The
second completed all 60 measurement rows, then reproduced an external restack
leaving `_NET_CLIENT_LIST_STACKING` stale indefinitely on an idle server.

Explicit focus queues `SetInputFocus`, updates focus recency and arranges the
monitor. `stack_overlay` computes the policy order, raises each entry only when
that order differs, and sets `stacking_dirty`. At settled event-loop boundaries,
`flush_client_list` coalesces invalidations; `QueryTree.reply()` waits for the
actual root children, filters managed clients/docks, then publishes the property
with a checked write. The query also waits behind earlier requests on this
connection. Request submissions alone are not completion measurements.

Twelve cyclic floating focus actions produced this baseline on the CI Xvfb
runner (not Axiom's laptop):

| Clients | ConfigureWindow | QueryTree / successful query replies | Median instrumented query wait |
| --- | ---: | ---: | ---: |
| 2 | 24 | 12 / 12 | 88.9 us |
| 10 | 120 | 12 / 12 | 262.9 us |
| 50 | 600 | 12 / 12 | 1177.3 us |

Each row also made 12 checked property writes; their waits are recorded
separately. Repeated focus made zero queries in all 15 client/mode combinations.
Tiled cyclic focus made zero baseline queries; transient cases made 0/6/6.
Fullscreen and maximize each made 12. Query waits include preceding raises,
observer activity, scheduling and shim overhead; they are neither isolated
QueryTree server time nor evidence of physical latency. Uninstrumented rows
separately record native active-window + X focus acknowledgement (including IPC
CLI startup/observer queries), whole-campaign wall time and 10 ms CPU ticks.
Pure server execution time is unavailable. The logs retain all 60 rows.

Alternatives evaluated:

- **A, retain QueryTree:** chosen. Per-monitor policy order cannot establish the
  global effective order across external clients, reparenting and failed async
  requests. The measurements do not justify replacing the server oracle.
- **B, derive all order from events:** rejected. Partial notifications, synthetic
  events, unmanaged siblings and resynchronization add unnecessary state.
- **C, coalesce invalidations:** retain existing settled boundaries and cover
  older notification echoes with the QueryTree sequence watermark. New real
  structure observations invalidate publication, including external changes.
- **D, order cache:** rejected for this stage; a permanently stale second source
  of truth is worse than a query. No full stacking model is introduced.

The correction handles real root Configure/Map/Reparent/Circulate observations
for tracked clients/docks. Only sequences strictly older than the last snapshot
are covered; equality can describe another client's subsequent restack. x11rb's
extended sequence avoids 16-bit wrapping comparisons. Synthetic ICCCM
ConfigureNotify messages and child-targeted duplicate copies are excluded.
New geometry notifications can conservatively require a query even when the
relative stack did not change; correctness takes precedence over the counter.
No timer or periodic work is added.

The independent Xlib probe compares the published order with its own QueryTree,
checks real focus, tests redirected and override-redirect external attempts,
transients, rapid focus, docks, sticky View transitions and destruction. Unit
tests cover notification filtering and sequence equality/wrap boundaries.
Reproduce the complete campaign with:

```sh
cargo run -p maverick-xtask --locked -- x11
python3 tests/xvfb-stacking-campaign.py --out /tmp/maverick-stage11.json --external
```

CI preserves the measurement JSON alongside existing native probe evidence.
Local X11/IPC execution is blocked by this container's socket restrictions;
[post-correction CI run 42399569526](https://github.com/Azytar/Maverick/actions/runs/42399569526)
passed all four jobs and all 33 X11 steps. All 60 rows completed; ConfigureWindow
counts stayed identical. QueryTree counts stayed identical except tiled cyclic
focus, which conservatively changed from 0 to 12 per case; every repeated-focus
case still made zero queries. External restacks/docks, rapid focus and sticky
View transitions passed. No latency improvement is claimed.

### Stability follow-up — signal wake delivery

The baseline cursor shutdown timeout exposed a real pre-existing lost-wakeup
contract: signal handlers only set flags, and a process-directed signal may run
on the IPC thread rather than interrupt the WM's poll. Delivery just before
poll also loses the interrupt. A process-isolated Rust regression reproduced
both cases without X11: the flag was set but the WM wait timed out.

Handlers now additionally write one permanent, nonblocking CLOEXEC eventfd.
Only the WM wait consumes this descriptor; control-server waits cannot steal
its notification. The handler allocates/locks nothing and preserves errno.
Failure to create the descriptor is reported as unavailable signal handlers.
Four subprocess scenarios cover quit/regrab before/during the wait, verify
ordinary waits cannot consume the wake, and verify draining leaves no idle
spin. This correctness fix adds no timer or dependency; the necessary write
FFI remains confined to the existing system boundary.

## Stage 12 — floating stack ranks and scratch reuse

The unchanged production helper was measured first in `78f17fd`. The comparison
workload retains that algorithm as a test-only oracle, alternates two focus
recency snapshots, and measures nine interleaved batches per variant. Fixtures
have 2/10/50/200 floating clients, with or without parent/child pairs. Medians
below are ns/call on this container's AMD EPYC 9V74, Rust 1.99.1, release + LTO;
they measure CPU computation with Instant, not X11/IPC or laptop latency.

| Clients | Transients | Previous helper | Buffers + same stable sort | Precomputed ranks (chosen) |
| --- | --- | ---: | ---: | ---: |
| 2 | no | 45 | 27 | 32 |
| 2 | yes | 48 | 33 | 39 |
| 10 | no | 198 | 192 | 159 |
| 10 | yes | 241 | 243 | 207 |
| 50 | no | 2970 | 3039 | 1487 |
| 50 | yes | 3321 | 3442 | 2120 |
| 200 | no | 34614 | 33307 | 15111 |
| 200 | yes | 37175 | 36956 | 25617 |

Simple buffer reuse does not address repeated focus-stack position searches
inside the comparator. Switching to unstable sort with those searches worsened
50-client flat ordering to 4097 ns and 200-client ordering to 65921 ns, despite
zero warmed allocations. That variant is rejected. A reusable temporary map or
persistent order cache adds state without a demonstrated need here.

The chosen pass computes each rank once into one reusable `(rank, window)`
buffer, sorts the total key, then appends the existing parent-first order to the
caller's output. Ranks are rebuilt and the scratch is drained on every pass;
missing parents and cycles retain the previous deterministic behavior. The
surrounding layer assembly, desired-order cache, raises and authoritative
QueryTree publication are unchanged. No dependency, framework or unsafe is added.

Warm helper input/output allocation counts change from 2 to 0 in all eight
fixtures; this is not a zero-allocation claim for the whole `stack_overlay`
callback. A regression varies membership/focus with a pre-existing tile prefix.
Differential Proptest covers missing parents, cycles, partial focus histories,
shrinking and empty membership; it passed 10,000 cases locally. Existing
projection/reconciliation allocation guards remain in place. Reproduce with:

```sh
cargo test --release --bin maverick --locked float_stack_workload -- --ignored --nocapture
PROPTEST_CASES=10000 cargo test --bin maverick --locked floating_order
```

The signal follow-up also passed all four jobs in
[CI run 42400050048](https://github.com/Azytar/Maverick/actions/runs/42400050048),
including clean root-cursor shutdown and the full X11 campaign. Local full Cargo
tests hit the container's AF_UNIX EPERM restriction; the installer additionally
requires an unprivileged UID unavailable here. Neither test is weakened. Clean
release, strict Clippy in both feature modes and MSRV checks pass locally;
complete live validation uses CI.

## Next work

1. Keep Stage 3 focused on invariants that detect real state/effect regressions;
   do not duplicate the existing independent Carousel model.
2. Preserve Python/C harnesses whose measured role is process orchestration or
   an independent ABI/protocol oracle.
3. Extend measurements only when a new profile identifies another real cost;
   preserve the established zero-allocation and reconciliation contracts.
4. Preserve the authoritative QueryTree contract. Further reduction of raises
   needs evidence and external-order equivalence, not just a smaller counter.
5. Validate perceived focus behavior on Axiom's Arch/Xorg laptop before making
   physical latency or power claims; record battery versus mains power.
