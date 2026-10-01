#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  MAVERICK installer — static checks for the partitioned tree
#
#  Two passes, both cheap enough to run on every save:
#    1. bash -n        — every file parses. Always available.
#    2. shellcheck     — real analysis, when it is installed. Runs at error
#                        severity so a missing optional tool or a style
#                        opinion never blocks the build; report the rest.
#
#  The behavioural checks live in tests/partition.py.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
files=("$here/install.sh" "$here"/lib/*.sh)

rc=0
for f in "${files[@]}"; do
    if bash -n "$f"; then
        printf '  bash -n   ok   %s\n' "${f#"$here"/}"
    else
        printf '  bash -n FAIL   %s\n' "${f#"$here"/}" >&2
        rc=1
    fi
done

if command -v shellcheck >/dev/null 2>&1; then
    if shellcheck -x -S error "${files[@]}"; then
        printf '  shellcheck -S error   ok   (%d files)\n' "${#files[@]}"
    else
        printf '  shellcheck -S error FAIL\n' >&2
        rc=1
    fi
    # Everything below error severity is reported, never enforced: these are
    # the cases worth reading, and none of them are worth a red build.
    shellcheck -x -S warning "${files[@]}" || true
else
    printf '  shellcheck   not installed — install it for the deeper pass\n'
fi

exit "$rc"
