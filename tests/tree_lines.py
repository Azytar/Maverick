#!/usr/bin/env python3
"""Read `maverick-msg query tree` JSON from stdin, print one line per managed
window: ID MON FS MX X Y W H TITLE.

Split out of tests/xephyr-2mon.sh because a `tree | python3 - <<EOF` pipeline
can NEVER work: the heredoc replaces the pipe as python's stdin, so the query
output goes nowhere (SIGPIPE) and json.load reads EOF. Every consumer of the
old shell function silently got empty input.
"""
import sys
import json


def walk(d):
    for m in d.get('monitors', []):
        for ws in m.get('workspaces', []):
            for col in ws.get('columns', []):
                for w in col.get('windows', []):
                    yield w
            for w in ws.get('floats', []):
                yield w


def main():
    try:
        d = json.load(sys.stdin)
    except Exception:
        return 0
    for w in walk(d):
        g = w.get('geom', [0, 0, 0, 0])
        print(w['id'], w.get('monitor', -1),
              int(bool(w.get('fullscreen'))), int(bool(w.get('maximized'))),
              g[0], g[1], g[2], g[3], w.get('title', ''))
    return 0


if __name__ == '__main__':
    sys.exit(main())
