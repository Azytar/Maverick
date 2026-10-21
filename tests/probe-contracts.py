#!/usr/bin/env python3
"""Check the C probe's failure contract without opening an X display."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("probe_harness", ROOT / "tests/xvfb-wine-focus.py")
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)


def record(name, passed=True):
    return "RESULT " + json.dumps({"name": name, "passed": passed})


class ResultContract(unittest.TestCase):
    def check_output(self, lines):
        return harness.validate_results("\n".join(lines), ["first", "second"], "test")

    def test_valid_results_and_diagnostics(self):
        self.assertEqual(len(self.check_output([
            "EVENT unrelated diagnostic", record("first"), record("second"), "COMPLETE test"])), 2)

    def test_false_result_stays_false(self):
        self.assertIs(self.check_output([record("first", False), record("second"), "COMPLETE test"])[0]["passed"], False)

    def test_mutated_transcripts_are_rejected(self):
        good = [record("first"), record("second"), "COMPLETE test"]
        mutants = {
            "missing-result": good[1:],
            "out-of-order": [good[1], good[0], good[-1]],
            "duplicate-result": [good[0], *good],
            "unknown-result": [*good[:-1], record("third"), good[-1]],
            "no-results": [good[-1]],
            "no-marker": good[:-1],
            "duplicate-marker": [*good, good[-1]],
            "marker-before-end": [*good, "EVENT too late"],
            "wrong-marker": [*good[:-1], "COMPLETE other"],
            "malformed-json": ["RESULT {", *good],
            "array-result": ["RESULT []", *good],
            "missing-name": ['RESULT {"passed":true}', *good],
            "missing-passed": ['RESULT {"name":"first"}', good[1], good[-1]],
        }
        for name, lines in mutants.items():
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.check_output(lines)

    def test_only_json_booleans_are_valid(self):
        for value in ("true", "false", 0, 1, None, [], {}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.check_output([record("first", value), record("second"), "COMPLETE test"])

    def test_inventory_rejects_empty_duplicate_or_untyped_names(self):
        for value in ([], {}, ["first", "first"], [""], [2], ["x"] * 129):
            with self.subTest(value=value), self.assertRaises(ValueError):
                harness.inventory(json.dumps(value))


class TreeLinesContract(unittest.TestCase):
    def run_tree(self, text):
        return subprocess.run(["python3", str(ROOT / "tests/tree_lines.py")],
                              input=text, capture_output=True, text=True, timeout=5)

    def test_empty_tree_is_valid(self):
        result = self.run_tree('{"monitors":[]}')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")

    def test_broken_input_cannot_claim_an_empty_tree(self):
        for text in ("", "{", "null", "[]", "{}", '{"monitors":{}}'):
            with self.subTest(text=text):
                result = self.run_tree(text)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertIn("invalid query tree", result.stderr)

    def test_tiled_and_floating_windows_keep_the_wire_format(self):
        window = {"id": 42, "monitor": 2, "fullscreen": True,
                  "geom": [1, 2, 640, 480], "title": "title with spaces"}
        doc = {"monitors": [{"workspaces": [{"columns": [{"windows": [window]}],
                                            "floats": [{"id": 43}]}]}]}
        result = self.run_tree(json.dumps(doc))
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "42 2 1 0 1 2 640 480 title with spaces\n43 -1 0 0 0 0 0 0 \n")
        doc["monitors"][0]["workspaces"][0]["floats"].append({"title": "missing id"})
        result = self.run_tree(json.dumps(doc))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")


class CContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix="c-contract-", dir="/tmp")
        cls.binary = Path(cls.scratch.name) / "probe"
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-Wshadow", "-Wformat=2",
                        str(ROOT / "tests/fullscreen-lifecycle-probe.c"), "-o", str(cls.binary), "-lX11"], check=True)
        cls.env = {k: v for k, v in os.environ.items() if k not in (
            "DISPLAY", "WAYLAND_DISPLAY", "AUDIT_PUBLICATION_ROUNDS", "AUDIT_PUBLICATION_ONLY")}

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def run_probe(self, *args, variables=None):
        return subprocess.run([str(self.binary), *args], env={**self.env, **(variables or {})},
                              capture_output=True, text=True, timeout=5)

    def test_decoder_boundary_cases(self):
        run = self.run_probe("--self-test")
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("SELFTEST property-decoder passed", run.stdout)

    def test_invalid_rounds_fail_before_x11(self):
        for text in ("", "0", "-1", "+1", " 1", "1x", "abc", "4097", "999999999999999999999999999999"):
            with self.subTest(text=text):
                run = self.run_probe("--describe", variables={"AUDIT_PUBLICATION_ROUNDS": text})
                self.assertEqual(run.returncode, 2, run.stdout + run.stderr)
                self.assertIn("AUDIT_PUBLICATION_ROUNDS", run.stderr)
                self.assertNotIn("DISPLAY", run.stderr)

    def test_round_bounds_have_a_nonempty_contract(self):
        for n in ("1", "4096"):
            run = self.run_probe("--describe", variables={"AUDIT_PUBLICATION_ROUNDS": n})
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertIn("publication-converges-without-new-events", harness.inventory(run.stdout))

    def test_invalid_duration_and_options_fail_before_x11(self):
        cases = [("--observe", v) for v in ("0", "-1", "nan", "inf", "oops", "3601")]
        cases += [("--case", "unknown"), ("--case",), ("--rounds", "0"), ("--unknown",)]
        for args in cases:
            with self.subTest(args=args):
                run = self.run_probe(*args)
                self.assertEqual(run.returncode, 2, run.stdout + run.stderr)
                self.assertNotIn("DISPLAY", run.stderr)

    def test_publication_only_flag_is_validated(self):
        for flag in ("", "yes", "2"):
            run = self.run_probe("--describe", variables={"AUDIT_PUBLICATION_ONLY": flag})
            self.assertEqual(run.returncode, 2, run.stderr)
        run = self.run_probe("--describe", variables={"AUDIT_PUBLICATION_ONLY": "1"})
        self.assertEqual(run.returncode, 2, run.stderr)

    def test_case_inventories_are_distinct(self):
        cases = {}
        for case in ("lifecycle", "publication", "properties"):
            run = self.run_probe("--describe", "--case", case)
            self.assertEqual(run.returncode, 0, run.stderr)
            cases[case] = harness.inventory(run.stdout)
        self.assertEqual(len(cases["properties"]), 7)
        self.assertEqual(len(cases["lifecycle"]), 27)
        self.assertEqual(len(cases["publication"]), 4)

    def test_startup_failures_preserve_a_failed_manifest_without_starting_x11(self):
        for case in ("missing-wm", "bad-compiler-input", "zero-rounds"):
            with self.subTest(case=case), tempfile.TemporaryDirectory(prefix="c-failure-", dir="/tmp") as scratch:
                directory=Path(scratch)
                source=ROOT / "tests/fullscreen-lifecycle-probe.c"
                if case=="bad-compiler-input":
                    source=directory / "bad.c"
                    source.write_text("this is not valid C;\n")
                wm=directory / "missing" if case=="missing-wm" else Path("/bin/true")
                env={**self.env, **({"AUDIT_PUBLICATION_ROUNDS":"0"} if case=="zero-rounds" else {})}
                evidence=directory / "evidence"
                run=subprocess.run(["python3",str(ROOT / "tests/xvfb-wine-focus.py"),"--wm",str(wm),
                                    "--probe",str(source),"--evidence",str(evidence)],
                                   env=env,capture_output=True,text=True,timeout=10)
                self.assertNotEqual(run.returncode,0,run.stdout)
                manifest=json.loads((evidence / "manifest.json").read_text())
                self.assertIn("harness-error",manifest["failures"])
                self.assertTrue(manifest["exception"])
                self.assertIsNone(manifest["display"])
                self.assertEqual(manifest["processes"],[])
                self.assertTrue((evidence / "error.log").is_file())

    def test_cli_rounds_override_valid_legacy_setting(self):
        run = self.run_probe("--describe", "--rounds", "8", variables={"AUDIT_PUBLICATION_ROUNDS": "128"})
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertEqual(len(harness.inventory(run.stdout)), 4)


if __name__ == "__main__":
    unittest.main(verbosity=2)
