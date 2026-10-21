#!/usr/bin/env python3
"""Test stacking harness startup and diagnostics without a real X server."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("stacking", ROOT / "tests/xvfb-stacking.py")
stacking = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stacking)


class StartupContract(unittest.TestCase):
    def server(self, source):
        process = subprocess.Popen([sys.executable, "-c", source], stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE)
        self.addCleanup(process.stdout.close)
        self.addCleanup(process.stderr.close)

        def stop():
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
        self.addCleanup(stop)
        return process

    def test_cold_start_can_exceed_the_probe_response_budget(self):
        process = self.server("import os,time; time.sleep(5.2); os.write(1,b'123\\n'); time.sleep(30)")
        self.assertEqual(stacking.wait_display(process), ":123")

    def test_split_record_is_assembled_before_it_is_accepted(self):
        process = self.server("import os,time; os.write(1,b'12'); time.sleep(.05); os.write(1,b'3\\n'); time.sleep(30)")
        self.assertEqual(stacking.wait_display(process, timeout=3), ":123")

    def test_missing_or_unterminated_readiness_has_a_deadline(self):
        for prefix in ("", "os.write(1,b'123');"):
            with self.subTest(prefix=prefix):
                process = self.server("import os,time; " + prefix + "time.sleep(30)")
                with self.assertRaisesRegex(RuntimeError, "within 0.2s"):
                    stacking.wait_display(process, timeout=.2)

    def test_invalid_or_oversized_display_is_rejected(self):
        for record in (b"foreign:0\n", b"\n", b"123\nextra", b"x" * 40):
            with self.subTest(record=record):
                process = self.server(f"import os,time; os.write(1,{record!r}); time.sleep(30)")
                with self.assertRaisesRegex(RuntimeError, "invalid|oversized"):
                    stacking.wait_display(process, timeout=3)

    def test_early_exit_and_closed_pipe_are_not_timeouts(self):
        process = self.server("import sys; sys.exit(17)")
        process.wait(timeout=3)
        with self.assertRaisesRegex(RuntimeError, "status 17"):
            stacking.wait_display(process, timeout=3)
        process = self.server("import os,time; os.close(1); time.sleep(30)")
        with self.assertRaisesRegex(RuntimeError, "closed its readiness pipe"):
            stacking.wait_display(process, timeout=3)

    def test_diagnostics_never_query_an_unclaimed_display(self):
        output = io.StringIO()
        with patch.object(stacking.subprocess, "run") as run, contextlib.redirect_stdout(output):
            stacking.failure_diagnostics(io.StringIO("startup sentinel\n"), {"DISPLAY": ":foreign"}, None)
        run.assert_not_called()
        self.assertIn("startup sentinel", output.getvalue())

    def test_diagnostic_failure_preserves_the_original_exception(self):
        for error in (FileNotFoundError("xwininfo"), subprocess.TimeoutExpired("xwininfo", 5)):
            with self.subTest(error=error), patch.object(stacking.subprocess, "run", side_effect=error):
                with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, "original"):
                    try:
                        raise RuntimeError("original")
                    except RuntimeError:
                        stacking.failure_diagnostics(io.StringIO("sentinel"), {"DISPLAY": ":123"}, ":123")
                        raise

    def test_startup_failure_prints_stderr_and_reaps_only_its_server(self):
        processes, environments = [], []
        real_popen = subprocess.Popen
        def owned(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            processes.append(process)
            environments.append(kwargs.get("env", {}))
            return process
        with tempfile.TemporaryDirectory(prefix="stacking-contract-", dir="/tmp") as tmp:
            xvfb = Path(tmp) / "Xvfb"
            xvfb.write_text(f"#!{sys.executable}\nimport sys\nprint('startup sentinel',file=sys.stderr)\nsys.exit(17)\n")
            xvfb.chmod(0o755)
            output = io.StringIO()
            env = {"PATH": tmp + os.pathsep + os.environ.get("PATH", ""), "DISPLAY": ":foreign"}
            with patch.dict(os.environ, env), patch.object(stacking, "require_wm_binary"), \
                    patch.object(stacking.subprocess, "Popen", side_effect=owned), \
                    patch.object(stacking.subprocess, "run") as run, contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(RuntimeError, "Xvfb"):
                    stacking.main()
            self.assertIn("startup sentinel", output.getvalue())
            self.assertEqual(len(processes), 1)
            self.assertEqual(processes[0].returncode, 17)
            self.assertEqual(run.call_count, 1)  # Compiler only; no diagnostic X client.
            self.assertNotIn("DISPLAY", environments[0])
            processes[0].stdout.close()


if __name__ == "__main__":
    unittest.main()
