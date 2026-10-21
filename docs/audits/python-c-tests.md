# Python and C test costs

Measured on 2026-10-21, against release Maverick from `a8e7cec`, with the test
baseline at `f1c0046`. No production code changed. Each run owns a fresh Xvfb,
private runtime/configuration and child processes. All comparisons use the same
WM and control binaries. Times include interpreter startup, compilation where
needed, server/WM startup, assertions and cleanup. Three alternating baseline
and improved runs were followed by three native-client-only control runs.

| Configuration | Run 1 | Run 2 | Run 3 | Median |
| --- | ---: | ---: | ---: | ---: |
| Original Python/ctypes, 10 checks | 14.760 s | 14.756 s | 14.759 s | 14.759 s |
| Native client, original scenarios and waits | 13.664 s | 13.564 s | 13.662 s | 13.662 s |
| Native client and condition waits, 13 checks | 8.437 s | 8.336 s | 8.391 s | 8.391 s |

The final test is about 43% faster here. The native-client-only control improves
about 7%, including removal of ctypes bindings and event-draining threads; this
is not a CPU-language microbenchmark. Most of the improvement comes from
replacing intermediate settling delays with checked conditions. These numbers
are local measurements, not a CI performance threshold or a claim about game
latency. Re-run on the same binaries when comparing other machines.

## What moved, and why

`xvfb-input-hint.py` used manual ctypes definitions, shared Xlib state guarded by
a lock, two event-draining threads and external xprop/xdotool focus queries.
The original script has twenty 600ms settling sleeps; a profile recorded 55
external subprocess calls taking about 0.16s cumulatively. Rewriting the Python
control flow in C cannot remove the intentional observation time or server/WM
startup cost.

`input-hint-probe.c` now owns the two client windows and one Xlib connection in a
single process/thread. Python keeps process ownership, deadlines, diagnostics
and the WM's independent IPC focus/window-count assertions. The helper accepts
bounded commands and returns one JSON line per command. Malformed commands,
unknown windows, bad EWMH property shape and X errors fail explicitly. Absent
`_NET_ACTIVE_WINDOW` remains distinct from an invalid property; absence is
accepted only for the initial logically unselected state, with keyboard focus
on None, PointerRoot or the root window. Replies are diagnostic observations,
not atomic snapshots across the WM and X server.

There is also a concrete ABI reason for using C. The old hint writer serialized
`XWMHints` into bytes and passed `sizeof(XWMHints) / 4` as the element count to
`XChangeProperty(format=32)`. On this LP64 platform the structure is 56 bytes,
so it requested 14 elements rather than the nine ICCCM words. Xlib expects
native longs for format 32, which can also read beyond that byte buffer. Using
`XSetWMHints` and `XGetWMHints` delegates the native/wire representation to Xlib.

## Stronger assertions

The original ten scenarios remain. Eligible destinations now require logical,
published and real keyboard focus, observed repeatedly for 600ms after bounded
convergence. Refusal cases retain their observation interval and cannot pass
because a query failed. Intermediate property acknowledgements only fence the
client's X requests; they do not claim the WM processed a notification. Focus
preconditions are checked before changing hints or destroying a client.

Three added checks cover the previously promised but untested missing InputHint
bit and explicit focus preconditions. Compilation/run failures cannot pass the
suite. CI runs the regression with ASan/UBSan, including fatal UB recovery
settings. This still tests focus ownership, not actual key delivery, which is
covered separately by `xvfb-input.py`. Sampling every 20ms can miss shorter
focus excursions; the event-level Wine probe covers focus flaps separately.

`tree_lines.py` stays in Python. Invalid JSON formerly returned success and no
rows, indistinguishable from a valid empty desktop in the shell rig. It now
reports malformed input and validates all rows before printing any. Contract
cases cover valid empty trees, malformed documents, tiled/floating windows,
titles containing spaces and a broken tail after a valid window.

## Validation and migration decisions

- All nine timed runs passed; the final instrumented input-hint run passed all
  13 checks under ASan/UBSan.
- Four native-client contract tests passed on their own Xvfb without a WM,
  including input models, absent hints, malformed/oversized commands, destroyed
  clients and a wrongly typed active-window property.
- Sixteen display-free probe/tree contract tests passed. Strict C compilation
  with `-Wall -Wextra -Werror -Wshadow -Wformat=2 -fanalyzer` emitted no diagnostics.
- Two private source mutants independently falsifying real focus and published
  focus were rejected by the named eligibility-restoration assertion. Neither
  compiler failures nor broken startup count as a detected mutant.

Keep installer, JSON/result validation, session lifecycle and process cleanup in
Python: their primary work is external processes and structured data, and a C
rewrite adds parsing/ownership work without a demonstrated benefit. Keyboard,
stacking, cursor and input rigs already use C for their native observations.
Stacking/overview still contain observation and settling delays; optimize a
particular transition only after establishing its completion predicate. Do not
remove stability intervals or idle-publication regression conditions to improve
a benchmark.
