import importlib.util
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "worker_profile_smoke.py"
sys.path.insert(0, str(SCRIPT.parent))
SPEC = importlib.util.spec_from_file_location("worker_profile_smoke", SCRIPT)
SMOKE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SMOKE)


class SmokeTests(unittest.TestCase):
    def test_boundaries_never_start_an_engine(self):
        with patch.object(SMOKE.platform, "system", return_value="Linux"), patch.object(SMOKE.subprocess, "run") as run:
            for count in (0, 11, True):
                with self.assertRaises(ValueError): SMOKE.smoke("local-image", count)
            run.assert_not_called()

    def test_cleanup_requires_ownership_and_uses_immutable_id(self):
        for matched in (True, False):
            nonce, container = "b" * 32, "a" * 64
            commands = []
            def run(command, **kwargs):
                commands.append(command)
                if command[0] == "cc":
                    Path(command[-1]).write_bytes(b"fixture")
                stdout = container + " " + (nonce if matched else "other-owner")
                return SimpleNamespace(returncode=0, stdout=stdout)
            with patch.object(SMOKE.platform, "system", return_value="Linux"), patch.object(SMOKE.shutil, "which", side_effect=lambda name: name), patch.object(SMOKE.subprocess, "check_output", return_value="sha256:" + "c" * 64), patch.object(SMOKE.subprocess, "run", side_effect=run), patch.object(SMOKE.uuid, "uuid4", return_value=SimpleNamespace(hex=nonce)), patch.object(SMOKE, "profile", return_value={"exit_code": 0, "worker": {"cpu_time_delta_us": 1, "sampled_peak_memory_bytes": 24 * 1024 * 1024}}):
                if matched:
                    self.assertEqual(len(SMOKE.smoke("local-image", 1)["records"]), 1)
                else:
                    with self.assertRaises(ValueError): SMOKE.smoke("local-image", 1)
            removes = [command for command in commands if command[:2] == ["docker", "rm"]]
            self.assertEqual(removes, [["docker", "rm", "-f", container]] if matched else [])
            starts = [command for command in commands if command[:2] == ["docker", "run"]]
            self.assertIn("never", starts[0])
            self.assertIn("no-new-privileges", starts[0])
            self.assertIn("65534:65534", starts[0])
