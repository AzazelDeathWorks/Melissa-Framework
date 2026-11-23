import importlib.util
import asyncio
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "profile_task.py"
SPEC = importlib.util.spec_from_file_location("profile_task", SCRIPT)
PROFILE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROFILE)
from melissa_resources import CgroupMeter, ResourceRecorder, delta, fields, uint
import melissa_resources


class CounterTests(unittest.TestCase):
    def test_remote_pid_collision_cannot_attribute_a_local_cgroup_to_the_worker(self):
        with patch.object(melissa_resources, "read", return_value="0::/system.slice/sshd.service\n"):
            with self.assertRaises(ValueError): CgroupMeter.for_pid(123, "a" * 64)
        with patch.object(melissa_resources, "read", return_value="0::/\n"):
            with self.assertRaises(ValueError): CgroupMeter.for_pid(123, "a" * 64)

    def test_missing_and_reset_counters_are_not_zero(self):
        self.assertIsNone(delta(None, 10))
        self.assertIsNone(delta(10, 9))
        self.assertEqual(delta(10, 10), 0)
        for invalid in ("-1", "1.5", "x", "18446744073709551616", "١"):
            self.assertIsNone(uint(invalid))
        self.assertEqual(fields("usage_usec 1\nusage_usec 2"), {})

    def test_cgroup_samples_are_scoped_and_not_physical_disk(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)
            (path / "cpu.stat").write_text("usage_usec 100\n")
            (path / "memory.current").write_text("1024\n")
            (path / "memory.peak").write_text("9000\n")
            (path / "io.stat").write_text("8:0 rbytes=50 wbytes=60\n")
            meter = CgroupMeter(path)
            (path / "cpu.stat").write_text("usage_usec 250\n")
            (path / "memory.current").write_text("4096\n")
            (path / "io.stat").write_text("8:0 rbytes=150 wbytes=460\n")
            result = meter.finish()
            self.assertEqual(result["cpu_time_delta_us"], 150)
            self.assertEqual(result["read_block_bytes_delta"], 100)
            self.assertEqual(result["write_block_bytes_delta"], 400)
            self.assertEqual(result["sampled_peak_memory_bytes"], 4096)
            self.assertEqual(result["lifetime_peak_memory_bytes"], 9000)
            self.assertIn("not physical disk", result["scope"])
            (path / "io.stat").unlink()
            self.assertIsNone(meter.finish()["read_block_bytes_delta"])
            self.assertIsNone(meter.finish(False)["sampled_peak_memory_bytes"])

    def test_recreated_cgroup_and_malformed_io_are_unavailable(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "group"
            path.mkdir()
            (path / "io.stat").write_text("8:0 rbytes=1 wbytes=2\n8:0 rbytes=3 wbytes=4")
            meter = CgroupMeter(path)
            self.assertIsNone(meter.baseline["read_block_bytes"])
            shutil.rmtree(path)
            result = meter.finish()
            self.assertFalse(result["identity_stable"])
            self.assertIsNone(result["cpu_time_delta_us"])


@unittest.skipUnless(sys.platform in ("linux", "darwin"), "wait4 requires POSIX")
class HostTests(unittest.TestCase):
    def test_real_command_usage_has_no_output_arguments_or_environment(self):
        result = PROFILE.profile([sys.executable, "-c", "print('SECRET_SENTINEL')"], timeout=5)
        self.assertEqual(result["exit_code"], 0)
        self.assertGreater(result["host_command"]["cpu_time_us"], 0)
        self.assertEqual(len(result["executable_sha256"]), 64)
        self.assertNotIn("SECRET_SENTINEL", json.dumps(result))
        self.assertIsNone(result["worker"]["cpu_time_delta_us"])

    def test_failed_and_timed_out_commands_remain_in_the_report(self):
        failed = PROFILE.profile([sys.executable, "-c", "raise SystemExit(7)"])
        self.assertEqual(failed["exit_code"], 7)
        timed = PROFILE.profile([sys.executable, "-c", "import time;time.sleep(20)"], timeout=0.05)
        self.assertEqual(timed["stop_reason"], "host_timeout")
        self.assertNotEqual(timed["exit_code"], 0)

    def test_bounds(self):
        for timeout in (0, -1, float("inf"), float("nan"), 86401):
            with self.assertRaises(ValueError):
                PROFILE.profile([sys.executable], timeout=timeout)
        with self.assertRaises(ValueError):
            PROFILE.profile([])

    def test_inaccessible_worker_is_not_measured_as_zero(self):
        with patch.object(PROFILE, "container_identity", return_value=("a" * 64, 123, "date")), patch.object(PROFILE.shutil, "which", side_effect=lambda name: sys.executable), patch.object(CgroupMeter, "for_pid", side_effect=ValueError):
            result = PROFILE.profile([sys.executable, "-c", "pass"], container="example")
        self.assertEqual(result["worker"]["unavailable_reason"], "host_cgroup_v2_unavailable")
        self.assertIsNone(result["worker"]["sampled_peak_memory_bytes"])


class RecorderTests(unittest.IsolatedAsyncioTestCase):
    async def test_failed_observer_stays_optional(self):
        async def fail():
            raise RuntimeError("fixture error, not a model failure")
        recorder = ResourceRecorder()
        recorder.task = asyncio.create_task(fail())
        await asyncio.sleep(0)
        result = await recorder.finish()
        self.assertEqual(result["observer_error"], "RuntimeError")

    async def test_unavailable_engine_does_not_block_the_agent(self):
        recorder = ResourceRecorder()
        await recorder.prepare(object())
        result = await recorder.finish()
        self.assertIsNone(result["host"]["sampled_peak_rss_bytes"])
        self.assertIsNone(result["worker"]["cpu_time_delta_us"])
