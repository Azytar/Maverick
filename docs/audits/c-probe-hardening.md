# C probe failure-contract campaign

Base: `a8e7cec77ce9d9976265e1d73d85a76a8181bfa6`. Branch:
`test/c-probe-hardening`. This change touches test code, CI and contributor
instructions, not the window manager's production behavior or release version.
No Rust dependencies, test framework or release tags were added.

## Demonstrated false positive

The original probe accepted `AUDIT_PUBLICATION_ROUNDS=0`, performed no publication
cycles and emitted `publication-converges-without-new-events` with `passed:true`.
The original Python harness returned zero. The reproduction is retained in
`/workspace/work/maverick-c-tests/work/evidence/baseline-zero.log` and
`/tmp/mav-c-baseline-zero`. This is a test defect, not a newly observed WM defect.
The new probe rejects zero, empty, negative, malformed and overflowing counts
before opening DISPLAY; supported counts are 1..4096.

## Contracts made observable

- `--describe` declares the expected result sequence without opening X11. Both
  Wine-focus and fullscreen probes support it. The Python harness requires each
  name exactly once, in order, an actual JSON boolean and one final completion
  marker. Unknown, missing, duplicated, malformed and reordered results fail.
- A nonzero process exit fails even after a syntactically complete passing
  transcript. Timeout streams are written directly to evidence files. Compile,
  startup and argument errors produce a failed manifest, not an empty success.
- The format-32 reader respects Xlib's unsigned-long representation on LP64 and
  bounds replies to 4096 words. Missing, valid-empty, wrong-type, wrong-format and
  truncated properties are distinct. `_NET_WM_STATE` may legitimately be absent;
  an absent `_NET_CLIENT_LIST` never proves that withdrawal completed. Startup
  waits for its first typed publication instead of rejecting that transient gap.
- Focus transfers assert XGetInputFocus and preserve fullscreen over a bounded
  200 ms observation. Child, override-redirect and transient cases expect their
  target; unrelated-window/root cases expect repair to the selected main window.
  These are assertions of this WM policy, not a universal X11 focus rule.
- Initial map and every remap check IsViewable as well as client-list membership.
  The publication counter increments only after the remapped client is visible;
  it must equal the requested iteration count. Failed prerequisites stop the
  scenario before a later XSetInputFocus can turn the original failure into a
  secondary BadMatch.
- Runtime paths are independent of evidence paths, keeping the control socket
  below SUN_LEN. The observer acknowledges its subscription before the probe
  starts. Event draining is bounded so a stream cannot indefinitely postpone the
  observer's duration/stop checks. Every owned Xvfb/WM/observer is waited for, and
  its PID and return status are recorded. No kill-by-name operation is used.

The idle regression deliberately has **no client-message/IPC barrier** to wake
Maverick. Adding one would hide the defect it protects. Property/attribute reads
use the probe's own X connection. Diagnostics sample several server queries;
those values are not an atomic snapshot of all window state.

## Negative controls

The campaign compiles private copies of the C source with three controlled
mutations: removing the focus request, accepting the wrong property type, and
executing zero publication iterations. A mutation counts as detected only when
the intended named check emits `passed:false`. A compile or startup failure is
not a successful kill. This restriction caught an initial bad mutation that
made a parameter unused under `-Werror`; the mutation was corrected rather than
counting that compiler failure as evidence.

Separate canaries emit an otherwise passing complete transcript and then return
7, overflow a signed integer, or write beyond a heap allocation. The first must
be rejected by exit status. The latter two must produce the expected UBSan/ASan
runtime diagnostic and fail. `UBSAN_OPTIONS=halt_on_error=1` prevents recoverable
UBSan findings from being mistaken for success; ASan also checks leaks. These
faults exist only in the campaign evidence directory, never in production code.

Thirteen display-independent unittest methods also exercise malformed streams,
argument boundaries, decoder boundaries, and failed-startup manifests. They use
only Python's standard library and the system C compiler/Xlib headers.

## Measured execution

Environment: Debian 13, GCC 14, private Xvfb 1280x800x24. The tested release WM
is the existing corrected build; its SHA-256 is recorded in every run manifest.
The full command, case-specific expected failures, elapsed seconds, source hash,
compiler flags, process statuses and partial streams live under
`/tmp/mav-c-campaign-final`. Expected negative-control exit codes are nonzero;
`passed` below means that the control detected its intended failure.

```bash
. /workspace/.maverick-tools/env.sh
python3 tests/c-probe-campaign.py \
  --wm /workspace/Maverick/target/release/maverick --extended \
  --reference-wm /workspace/work/maverick-audit/work/evidence/vertical-uninstrumented \
  --evidence /tmp/mav-c-campaign-final
```

| Case | Expected outcome | Verified | Seconds |
|---|---|---|---|
| contracts | success | yes | 1.406 |
| wine-focus | success | yes | 12.403 |
| lifecycle | success | yes | 3.293 |
| lifecycle-honor | success | yes | 3.405 |
| properties-asan-ubsan | success | yes | 1.001 |
| publication | success | yes | 3.502 |
| publication-only | success | yes | 0.842 |
| observer-long-evidence | success | yes | 3.006 |
| publication-1024 | success | yes | 8.041 |
| reject-missing-focus | specific failure detected | yes | 5.679 |
| reject-wrong-property-type | specific failure detected | yes | 0.214 |
| reject-zero-iterations | specific failure detected | yes | 0.710 |
| reject-nonzero-exit | specific failure detected | yes | 0.498 |
| reject-asan | specific failure detected | yes | 0.707 |
| reject-ubsan | specific failure detected | yes | 0.689 |
| reject-zero-rounds | specific failure detected | yes | 0.559 |
| reference-regression | specific failure detected | yes | 6.038 |

All 17 campaign cases passed their contracts. The 1024-cycle run completed all
1024 cycles. The optional reference binary still fails the named publication
check, confirming that stronger assertions did not accidentally wake the WM and
mask that older regression. The reference is an explicitly supplied local
historical artifact, not a second build claimed to be current main.

The C probe also compiled without diagnostics under:

```bash
cc -std=c11 -Wall -Wextra -Werror -Wshadow -Wformat=2 -fanalyzer \
  tests/fullscreen-lifecycle-probe.c -o work/probe -lX11
```

| Workspace validation | Exit status | Seconds |
|---|---|---|
| `cargo fmt --all -- --check` | 0 | 0.561 |
| `cargo check --workspace --all-targets --all-features --locked --offline` | 0 | 7.190 |
| `cargo clippy --workspace --all-targets --all-features --locked --offline -- -D warnings` | 0 | 8.248 |
| `cargo test --workspace --all-targets --all-features --locked --offline` | 0 | 113.444 |
| `cargo test --workspace --all-targets --locked --offline` | 0 | 59.169 |

The Rust test runs use `PROPTEST_RNG_SEED=20261020` and an owned short writable
XDG_RUNTIME_DIR. The input-probe protocol, ownership, child and capture cases
were checked separately against the release binary; their commands/results are
in `work/evidence/input-validation.jsonl`. Keyboard delivery and XInput2 remain
those probes' responsibility: XGetInputFocus alone is not a delivery test.

CI runs the ordinary campaign (128 publication cycles), sanitizers and negative
controls, and uploads the evidence even on failure. `--extended` and
`--reference-wm` are optional local investigation controls, not requirements on
CI to fetch/build arbitrary historical commits.

## Limits

These are finite observations on isolated X servers. They do not prove focus
stability forever, all possible protocol interleavings, or Wine/Blood Strike's
internal state. The earlier game's unresolved behavior is unchanged by this
campaign. No live desktop was manipulated. No package version was bumped because
no production API/behavior or release was changed.
