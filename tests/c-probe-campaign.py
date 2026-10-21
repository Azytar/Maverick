#!/usr/bin/env python3
"""Verify both passing probes and deliberate failures on private X servers.

Mutants live only in the evidence directory; production sources are never edited.
A compiler/startup failure does not count as detecting a semantic mutant.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wm", type=Path, default=ROOT / "target/release/maverick")
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--extended", action="store_true", help="also run 1024 publication cycles")
    parser.add_argument("--reference-wm", type=Path, help="known faulty WM: publication must fail")
    args = parser.parse_args()
    out = (args.evidence or Path(tempfile.mkdtemp(prefix="c-probes-", dir="/tmp"))).resolve()
    out.mkdir(parents=True, exist_ok=True)
    print("EVIDENCE " + str(out), flush=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith("AUDIT_PUBLICATION_")}
    source = ROOT / "tests/fullscreen-lifecycle-probe.c"
    results = []

    def record(name, command, code, seconds, passed, detail=None):
        result = dict(name=name, command=command, exit_code=code, seconds=seconds,
                      passed=passed, detail=detail)
        results.append(result)
        (out / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(json.dumps(result), flush=True)

    started = time.monotonic()
    contract_command = [sys.executable, str(ROOT / "tests/probe-contracts.py")]
    with (out / "contracts.log").open("w") as log:
        run = subprocess.run(contract_command, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=30)
    record("contracts", contract_command, run.returncode, time.monotonic() - started, run.returncode == 0)

    def case(name, *, probe=source, mode="lifecycle", variables=None, extra=(),
             wm=None, failed_check=None, failure=None, long_path=False, complete="fullscreen-lifecycle", diagnostic=None):
        evidence = out / name
        if long_path:
            evidence = evidence / ("long-evidence-" + "x" * 150)
        command = [sys.executable, str(ROOT / "tests/xvfb-wine-focus.py"),
                   "--wm", str((wm or args.wm).resolve()), "--probe", str(probe),
                   "--complete", complete,
                   "--evidence", str(evidence), *extra]
        if mode is not None:
            command += ["--case", mode]
        started = time.monotonic()
        with (out / (name + ".log")).open("w") as log:
            run = subprocess.run(command, env={**env, **(variables or {})}, stdout=log,
                                 stderr=subprocess.STDOUT, timeout=300)
        manifest = json.loads((evidence / "manifest.json").read_text())
        reaped = all(p["returncode"] is not None for p in manifest["processes"])
        passed = run.returncode == 0 and not manifest["failures"] and reaped
        if failed_check:
            records = []
            stdout = evidence / "probe.stdout"
            if stdout.exists():
                records = [json.loads(x[7:]) for x in stdout.read_text().splitlines() if x.startswith("RESULT ")]
            passed = run.returncode != 0 and reaped and any(
                r["name"] == failed_check and r["passed"] is False for r in records)
        elif failure:
            passed = run.returncode != 0 and reaped and any(failure in f for f in manifest["failures"])
        if diagnostic:
            stderr=evidence / "probe.stderr"
            passed=passed and stderr.exists() and diagnostic in stderr.read_text()
        record(name, command, run.returncode, time.monotonic() - started, passed,
               dict(evidence=str(evidence), expected_failed_check=failed_check,
                    expected_failure=failure, processes_reaped=reaped))
        return evidence

    case("wine-focus", probe=ROOT / "tests/wine-focus-probe.c", mode=None, complete="wine-focus")
    case("lifecycle", extra=("--no-trace",))
    case("lifecycle-honor", extra=("--honor-initial-state",))
    case("properties-asan-ubsan", mode="properties", extra=("--sanitize",))
    case("publication", mode="publication", variables={"AUDIT_PUBLICATION_ROUNDS": "128"}, extra=("--no-trace", "--honor-initial-state"))
    case("publication-only", mode="publication", variables={"AUDIT_PUBLICATION_ROUNDS": "32", "AUDIT_PUBLICATION_ONLY": "1"})
    case("observer-long-evidence", extra=("--observer",), long_path=True)
    if args.extended:
        case("publication-1024", mode="publication", variables={"AUDIT_PUBLICATION_ROUNDS": "1024"}, extra=("--no-trace",))

    mutants = out / "mutants"
    mutants.mkdir(exist_ok=True)
    text = source.read_text()
    mutations = [
        ("missing-focus", "XSetInputFocus(d, to, RevertToParent, CurrentTime); XFlush(d);", "(void)to; XFlush(d);",
         "lifecycle", "child-preserves-fullscreen"),
        ("wrong-property-type", "actual != expected || format != 32", "(actual != expected && expected == None) || format != 32",
         "properties", "property-wrong-type"),
        ("zero-iterations", "for(long i=0;i<rounds;i++)", "for(long i=0;i<0;i++)",
         "publication", "publication-converges-without-new-events"),
    ]
    for name, before, after, mode, check in mutations:
        if text.count(before) != 1:
            raise RuntimeError("mutation site changed: " + name)
        mutant = mutants / (name + ".c")
        mutant.write_text(text.replace(before, after))
        case("reject-" + name, probe=mutant, mode=mode,
             variables={"AUDIT_PUBLICATION_ROUNDS": "8"} if mode == "publication" else None,
             failed_check=check)

    # A valid transcript must not mask an unsuccessful process exit.
    crash = mutants / "nonzero-exit.c"
    crash.write_text('#include <stdio.h>\n#include <string.h>\n'
                     'int main(int argc,char **argv){\n'
                     'if(argc==2 && !strcmp(argv[1],"--describe")){puts("[\\"partial\\"]");return 0;}\n'
                     'puts("RESULT {\\"name\\":\\"partial\\",\\"passed\\":true}");\n'
                     'puts("COMPLETE fullscreen-lifecycle");return 7;}\n')
    case("reject-nonzero-exit", probe=crash, mode=None, failure="probe-exit-status:7")
    sanitizer_header = ('#include <stdio.h>\n#include <string.h>\n#include <stdlib.h>\n#include <limits.h>\n'
                        'int main(int argc,char **argv){\n'
                        'if(argc==2 && !strcmp(argv[1],"--describe")){puts("[\\"partial\\"]");return 0;}\n'
                        'puts("RESULT {\\"name\\":\\"partial\\",\\"passed\\":true}");\n'
                        'puts("COMPLETE fullscreen-lifecycle");fflush(stdout);\n')
    faults = [
        ("asan", "volatile size_t i=8;char *p=malloc(1);if(!p)return 2;p[i]=1;free(p);return 0;}\n",
         "AddressSanitizer: heap-buffer-overflow"),
        ("ubsan", "volatile int high=INT_MAX;volatile int one=1;int overflow=high+one;return overflow==0;}\n",
         "runtime error: signed integer overflow"),
    ]
    for name, body, diagnostic in faults:
        fault=mutants / (name + ".c")
        fault.write_text(sanitizer_header + body)
        case("reject-" + name, probe=fault, mode=None, extra=("--sanitize",),
             failure="probe-exit-status:", diagnostic=diagnostic)
    case("reject-zero-rounds", variables={"AUDIT_PUBLICATION_ROUNDS": "0"}, mode="publication", failure="harness-error")
    if args.reference_wm:
        case("reference-regression", wm=args.reference_wm, mode="publication",
             variables={"AUDIT_PUBLICATION_ROUNDS": "128"}, extra=("--no-trace",),
             failed_check="publication-converges-without-new-events")
    (out / "sources.json").write_text(json.dumps({
        "base": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "wm_sha256": hashlib.sha256(args.wm.read_bytes()).hexdigest(),
        "probe_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
    }, indent=2) + "\n")
    bad = [r["name"] for r in results if not r["passed"]]
    print(json.dumps({"cases": len(results), "failures": bad}), flush=True)
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
