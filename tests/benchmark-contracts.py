#!/usr/bin/env python3
"""Keep incomplete or malformed measurements out of the comparison chart."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parents[1] / "benchmarks/x11"
sys.path.insert(0, str(HERE))
import plot as benchmark_plot
import run as benchmark_run


class EvidenceContract(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="benchmark-contract-", dir="/tmp")
        self.addCleanup(self.scratch.cleanup)
        self.directory = Path(self.scratch.name)
        self.manifest = {"schema": 1, "complete": True, "clock_ticks_per_second": 100,
                         "parameters": {"clients": [2], "repeats": 1, "warmups": 0,
                                        "focus": 2, "fullscreen": 2}}
        # Synthetic fixtures test validation only; never used to draw published bars.
        self.runs = []
        for wm in benchmark_run.WMS:
            self.runs.append({"wm": wm, "clients": 2, "repeat": 0, "warmup": False,
                              "success": True, "startup_ms": 1,
                              "processes": [{"pid": 1, "returncode": 0}],
                              "resident": {"rss_bytes": 1024, "pss_bytes": 512},
                              "idle": {"cpu_ticks": 0, "seconds": 1},
                              "samples": {p: [1, 2] for p in benchmark_run.PHASES},
                              "phases": {p: {"cpu_ticks": 1, "seconds": 1} for p in benchmark_run.PHASES}})
        dwm = self.runs[-1]
        dwm["samples"]["focus"] = None
        dwm["phases"]["focus"] = None
        dwm["focus_supported"] = False
        dwm["focus_capability"] = {"urgent_only": True}

    def load(self, runs=None, manifest=None):
        runs = self.runs if runs is None else runs
        manifest = copy.deepcopy(self.manifest if manifest is None else manifest)
        raw = json.dumps(runs).encode()
        manifest["raw_sha256"] = hashlib.sha256(raw).hexdigest()
        (self.directory / "raw.json").write_bytes(raw)
        (self.directory / "manifest.json").write_text(json.dumps(manifest))
        return benchmark_plot.load_results(self.directory)

    def test_valid_matrix_preserves_missing_capability_as_null(self):
        manifest, runs = self.load()
        summary = benchmark_plot.summarize(manifest, runs)
        row = next(r for r in summary if r["wm"] == "dwm" and r["metric"] == "focus_p50_ms")
        self.assertIsNone(row["median"])
        row = next(r for r in summary if r["wm"] == "maverick" and r["metric"] == "map_p50_ms")
        self.assertEqual(row["median"], 1.5)

    def test_missing_duplicate_and_failed_trials_are_rejected(self):
        for mutation in (lambda r: r.pop(), lambda r: r.append(copy.deepcopy(r[0])),
                         lambda r: r[0].update(success=False)):
            runs = copy.deepcopy(self.runs)
            mutation(runs)
            with self.assertRaises(ValueError):
                self.load(runs)

    def test_bad_samples_cannot_become_bars(self):
        for values in ([], [1], [1, -1], [True, 1], [float("nan"), 1], [float("inf"), 1], None):
            runs = copy.deepcopy(self.runs)
            runs[0]["samples"]["map"] = values
            with self.subTest(values=values), self.assertRaises(ValueError):
                self.load(runs)

    def test_unexplained_dwm_focus_refusal_is_not_an_omission(self):
        runs = copy.deepcopy(self.runs)
        runs[-1]["focus_capability"]["urgent_only"] = False
        with self.assertRaises(ValueError):
            self.load(runs)

    def test_unreaped_children_are_rejected(self):
        runs = copy.deepcopy(self.runs)
        runs[0]["processes"][0]["returncode"] = None
        with self.assertRaises(ValueError):
            self.load(runs)

    def test_incomplete_manifest_and_changed_raw_data_are_rejected(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["complete"] = False
        with self.assertRaises(ValueError):
            self.load(manifest=manifest)
        self.load()
        (self.directory / "raw.json").write_text("[]")
        with self.assertRaisesRegex(ValueError, "checksum"):
            benchmark_plot.load_results(self.directory)

    def test_quantiles_interpolate_and_empty_data_fails(self):
        self.assertEqual(benchmark_run.quantile([4, 1, 3, 2], .5), 2.5)
        self.assertEqual(benchmark_run.quantile([4, 1, 3, 2], .25), 1.75)
        with self.assertRaises(ValueError):
            benchmark_run.quantile([], .5)


if __name__ == "__main__":
    unittest.main()
