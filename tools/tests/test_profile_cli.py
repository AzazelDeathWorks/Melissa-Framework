import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "profile_cli.py"
SPEC = importlib.util.spec_from_file_location("profile_cli", SCRIPT)
PROFILE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROFILE)


@unittest.skipUnless(sys.platform in ("linux", "darwin"), "POSIX wait4 profiler")
class ProfileTests(unittest.TestCase):
    def test_wait4_reports_each_child_and_only_fresh_state_footprint(self):
        with tempfile.TemporaryDirectory() as root:
            home = Path(root) / "state"
            empty = PROFILE.measure([sys.executable, "-c", "pass"], home)
            self.assertGreater(empty["process_lifetime_peak_rss_bytes"], 0)
            self.assertGreaterEqual(empty["cpu_time_us"], 0)
            self.assertEqual(empty["state_logical_bytes"], 0)
            home.mkdir()
            (home / "data").write_bytes(b"example")
            os.symlink(home / "data", home / "link")
            self.assertEqual(PROFILE.footprint(home)["state_logical_bytes"], 7)
            self.assertGreater(PROFILE.footprint(home)["state_allocated_bytes"], 0)

    def test_nonzero_or_timeout_never_produces_successful_sample(self):
        with tempfile.TemporaryDirectory() as root:
            for code, timeout in [("raise SystemExit(2)", 30), ("import time; time.sleep(30)", 0.1)]:
                with self.assertRaises(RuntimeError):
                    PROFILE.measure([sys.executable, "-c", code], Path(root), timeout=timeout)

    def test_repeat_count_is_bounded(self):
        for count in (0, -1, 101):
            with self.assertRaises(ValueError):
                PROFILE.profile(Path(sys.executable), count)

    def test_profile_hashes_binary_and_preserves_unsupported_self_counters(self):
        with tempfile.TemporaryDirectory() as root:
            binary = Path(root) / "fixture"
            binary.write_text(f"#!{sys.executable}\nimport json\nprint(json.dumps({{'sample':{{'working_set_bytes':123,'post_exec_peak_working_set_bytes':None}}}}))\n")
            binary.chmod(0o700)
            report = PROFILE.profile(binary, 1)
            self.assertEqual(len(report["binary_sha256"]), 64)
            self.assertEqual(len(report["cases"]), 4)
            resources = report["cases"]["resources"]["median"]
            self.assertEqual(resources["sampled_current_rss_bytes"], 123)
            self.assertIsNone(resources["sampled_post_exec_peak_rss_bytes"])
