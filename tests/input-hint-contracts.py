#!/usr/bin/env python3
"""Exercise the native WM_HINTS client on an owned server, without a WM."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("input_hint", ROOT / "tests/xvfb-input-hint.py")
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)


def stop(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


class NativeClientContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix="hint-contract-", dir="/tmp")
        cls.addClassCleanup(cls.scratch.cleanup)
        cls.helper = Path(cls.scratch.name) / "probe"
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-Wshadow",
                        "-Wformat=2", str(ROOT / "tests/input-hint-probe.c"),
                        "-o", str(cls.helper), "-lX11"], check=True, timeout=30)
        cls.env = {k: v for k, v in os.environ.items()
                   if k not in ("DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY")}
        cls.server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0",
                                       "800x600x24", "-nolisten", "tcp", "-noreset"],
                                      env=cls.env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                      text=True)
        cls.addClassCleanup(stop, cls.server)
        cls.addClassCleanup(cls.server.stdout.close)
        cls.env["DISPLAY"] = ":" + harness.line(cls.server.stdout)

    def start(self):
        process = subprocess.Popen([str(self.helper)], env=self.env, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1)
        self.addCleanup(process.stdin.close)
        self.addCleanup(process.stdout.close)
        self.addCleanup(process.stderr.close)
        self.addCleanup(stop, process)
        return process, harness.Probe(process)

    def test_native_hints_roundtrip(self):
        process, probe = self.start()
        client = harness.Client(probe, 0)
        self.assertEqual(client.server_input(), (True, False))
        client.declare_input(True)
        self.assertEqual(client.server_input(), (True, True))
        probe.command("input 0 2")
        self.assertEqual(client.server_input(), (False, False))
        client.withdraw_hints()
        self.assertIsNone(client.server_input())
        # A missing EWMH property stays explicit; no WM owns this server.
        self.assertIsNone(client.snapshot()["active"])
        client.withdraw()
        client.reoffer()
        client.destroy()
        process.stdin.close()
        self.assertEqual(process.wait(timeout=5), 0)

    def test_bad_commands_fail_instead_of_acknowledging(self):
        for command in ("create 2", "create -1", "input 0 3", "create 0 extra",
                        "create 0 1", "create 4294967296", "input 0 1junk",
                        "create 0" + " " * 130, "snapshot 1", "unknown 0"):
            with self.subTest(command=command):
                process, probe = self.start()
                if command.startswith("input") or command.startswith("unknown"):
                    harness.Client(probe, 0)
                process.stdin.write(command + "\n")
                process.stdin.flush()
                self.assertEqual(process.wait(timeout=5), 2)
                self.assertEqual(process.stdout.read(), "")
                self.assertIn("input-hint-probe:", process.stderr.read())

    def test_destroyed_window_cannot_supply_a_snapshot(self):
        process, probe = self.start()
        client = harness.Client(probe, 0)
        client.destroy()
        process.stdin.write("snapshot 0\n")
        process.stdin.flush()
        self.assertEqual(process.wait(timeout=5), 2)
        self.assertIn("window does not exist", process.stderr.read())

    def test_wrong_property_type_is_not_absent_selection(self):
        process, probe = self.start()
        client = harness.Client(probe, 0)
        subprocess.run(["xprop", "-root", "-f", "_NET_ACTIVE_WINDOW", "8s", "-set",
                        "_NET_ACTIVE_WINDOW", "invalid"], env=self.env, check=True, timeout=5)
        try:
            process.stdin.write("snapshot 0\n")
            process.stdin.flush()
            self.assertEqual(process.wait(timeout=5), 2)
            self.assertIn("invalid _NET_ACTIVE_WINDOW", process.stderr.read())
        finally:
            subprocess.run(["xprop", "-root", "-remove", "_NET_ACTIVE_WINDOW"],
                           env=self.env, check=True, timeout=5)


if __name__ == "__main__":
    unittest.main()
