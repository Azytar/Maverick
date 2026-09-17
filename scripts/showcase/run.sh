#!/usr/bin/env bash
set -euo pipefail
exec python3 -B -u "$(dirname -- "${BASH_SOURCE[0]}")/showcase.py" "$@"
