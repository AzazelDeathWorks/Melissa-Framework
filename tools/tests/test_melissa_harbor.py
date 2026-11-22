"""Offline bridge tests. Harbor-specific cases run when Harbor 0.26 is installed."""
import asyncio
import base64
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

BRIDGE = Path(__file__).resolve().parents[2] / "bench" / "harbor"
sys.path.insert(0, str(BRIDGE))
from melissa_runtime import atomic_json, remaining_wall_ms, update_context

HAS_HARBOR = importlib.util.find_spec("harbor") is not None


class RuntimeTests(unittest.TestCase):
    def test_missing_budget_is_not_silently_assumed_to_match_harbor(self):
        with self.assertRaises(ValueError):
            remaining_wall_ms(None, None, 100)
        for value in (0, -1, float("inf"), float("nan")):
            with self.assertRaises(ValueError):
                remaining_wall_ms(None, value, 100)

    def test_effective_deadline_clamps_host_run_and_leaves_cleanup_time(self):
        budget = {"deadline_monotonic": 160}
        self.assertEqual(remaining_wall_ms(budget, None, 100), 58000)
        self.assertEqual(remaining_wall_ms(budget, 10, 100), 9500)
        self.assertEqual(remaining_wall_ms(budget, 999, 100), 58000)
        with self.assertRaises(ValueError):
            remaining_wall_ms(budget, None, 161)
        self.assertEqual(remaining_wall_ms({"deadline_monotonic": None}, None, 100), 1798000)

    def test_atomic_snapshots_replace_prior_value_and_do_not_leave_temp_files(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "progress.json"
            atomic_json(path, {"requests": 1})
            atomic_json(path, {"requests": 2})
            self.assertEqual(json.loads(path.read_text()), {"requests": 2})
            self.assertEqual(list(path.parent.iterdir()), [path])
            if os.name == "posix":
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_partial_usage_is_not_a_total_or_a_conservative_reservation(self):
        context = SimpleNamespace()
        done = {"telemetry_complete": False, "reported_input_tokens": 12,
                "input_tokens": 1000000, "pending_requests": 1}
        update_context(context, done)
        self.assertIsNone(context.n_input_tokens)
        self.assertEqual(context.metadata["reported_input_tokens"], 12)
        done.update(telemetry_complete=True, reported_output_tokens=4)
        update_context(context, done)
        self.assertEqual(context.n_input_tokens, 12)
        self.assertEqual(context.n_output_tokens, 4)

    def test_summary_handles_empty_jobs_and_recovers_interrupted_snapshots(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            command = [sys.executable, str(BRIDGE / "summarize.py"), directory]
            self.assertIn("observed_trials=0", subprocess.check_output(command, text=True))
            trial = root / "trial"
            trial.mkdir()
            atomic_json(trial / "result.json", {
                "task_name": "task", "exception_info": {"exception_type": "AgentTimeoutError"},
            })
            atomic_json(trial / "melissa-progress.json", {
                "requests": 2, "reported_input_tokens": 12, "pending_requests": 1,
            })
            result = subprocess.check_output(command, text=True)
            self.assertIn("observed_trials=1 passed=0", result)
            self.assertIn("partial=1", result)
            self.assertIn("reported_input_lower_bound=12", result)
            (trial / "result.json").unlink()
            result = subprocess.check_output(command, text=True)
            self.assertIn("observed_trials=1 passed=0", result)
            self.assertIn("MissingTrialResult", result)


@unittest.skipUnless(HAS_HARBOR, "requires Harbor 0.26 (Python >=3.12)")
class BridgeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        import melissa_agent
        from harbor.models.agent.context import AgentContext
        self.module = melissa_agent
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.agent = melissa_agent.MelissaAgent(
            logs_dir=self.root / "agent", model_name="vendor/exact-model", wall_time_sec=2,
        )
        self.context = AgentContext()
        self.binary = patch.object(melissa_agent, "BIN", Path(sys.executable))
        self.binary.start()
        self.addCleanup(self.binary.stop)
        # Execute a deterministic Python fixture in place of the Rust binary.
        self.fixture = (
            "import json, os, time\n"
            "p = {'requests': 2, 'tool_calls': 1, 'reported_input_tokens': 12, "
            "'reported_output_tokens': 4, 'unreported_usage_requests': 1, 'pending_requests': 1}\n"
            "json.dump(p, open(os.environ['MELISSA_BENCH_PROGRESS_PATH'], 'w'))\n"
        )

    def result(self):
        return json.loads((self.agent.logs_dir / "melissa-result.json").read_text())

    async def test_downloaded_links_and_directories_are_not_read_on_host(self):
        sentinel = self.root / "host-only"
        sentinel.write_text("host sentinel")
        async def linked(_, local):
            local.symlink_to(sentinel)
        async def directory(_, local):
            local.mkdir()
        async def regular(_, local):
            local.write_bytes(b"task data")
            local.chmod(0o444)
        for download in (linked, directory):
            env = SimpleNamespace(download_file=download)
            result = await self.agent._serve(env, {"op": "read", "path": "/task"}, self.root)
            self.assertIn("error", result)
            self.assertNotIn("host sentinel", str(result))
        env = SimpleNamespace(download_file=regular)
        for _ in range(2):
            result = await self.agent._serve(env, {"op": "read", "path": "/task"}, self.root)
            self.assertEqual(base64.b64decode(result["content_b64"]), b"task data")
        self.assertEqual(sentinel.read_text(), "host sentinel")

    async def test_final_usage_and_model_name_are_preserved(self):
        code = self.fixture + (
            "assert os.environ['MELISSA_BENCH_MODEL'] == 'vendor/exact-model'\n"
            "print(json.dumps({'op': 'done', 'status': 'FinalCandidate', 'requests': 2, "
            "'reported_input_tokens': 12, 'reported_output_tokens': 4, "
            "'input_tokens': 1000000, 'telemetry_complete': True}), flush=True)\n"
        )
        await self.agent.run(code, None, self.context)
        self.assertEqual(self.result()["status"], "FinalCandidate")
        self.assertEqual(self.context.n_input_tokens, 12)

    async def test_task_writable_log_links_cannot_truncate_host_files(self):
        self.agent.logs_dir.mkdir()
        sentinel = self.root / "host-file"
        sentinel.write_text("unchanged")
        for name in ("melissa-trace.jsonl", "melissa-stderr.log", "melissa-result.json"):
            (self.agent.logs_dir / name).symlink_to(sentinel)
        await self.agent.run(self.fixture + "print('{\"op\":\"done\"}')\n", None, self.context)
        self.assertEqual(sentinel.read_text(), "unchanged")
        self.assertFalse((self.agent.logs_dir / "melissa-trace.jsonl").is_symlink())

    async def test_timeout_saves_last_snapshot_without_faking_complete_usage(self):
        self.agent.wall_time_sec = 0.3
        await self.agent.run(self.fixture + "time.sleep(30)\n", None, self.context)
        result = self.result()
        self.assertEqual(result["status"], "Deadline")
        self.assertFalse(result["telemetry_complete"])
        self.assertEqual(result["reported_input_tokens"], 12)
        self.assertEqual(result["pending_requests"], 1)
        self.assertIsNone(self.context.n_input_tokens)

    async def test_cancellation_reraises_after_persisting_partial_telemetry(self):
        task = asyncio.create_task(self.agent.run(self.fixture + "time.sleep(30)\n", None, self.context))
        async def wait_for_progress():
            while not (self.root / "melissa-progress.json").exists():
                await asyncio.sleep(0.01)
        await asyncio.wait_for(wait_for_progress(), timeout=2)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(self.result()["status"], "Cancelled")
        self.assertEqual(self.result()["requests"], 2)
        self.assertIsNone(self.context.n_input_tokens)

    async def test_nonzero_exit_is_not_an_empty_success(self):
        with self.assertRaises(RuntimeError):
            await self.agent.run(self.fixture + "raise SystemExit(2)\n", None, self.context)
        self.assertEqual(self.result()["status"], "BridgeError")
        self.assertEqual(self.result()["requests"], 2)

    async def test_spawn_failure_still_records_bridge_error(self):
        with patch.object(self.module, "BIN", self.root / "missing-binary"):
            with self.assertRaises(FileNotFoundError):
                await self.agent.run("", None, self.context)
        self.assertEqual(self.result()["status"], "BridgeError")

    async def test_deadline_plugin_matches_harbor_override_cap_and_multiplier(self):
        config = SimpleNamespace(
            agent=SimpleNamespace(name="melissa_agent:MelissaAgent", import_path=None,
                                  override_timeout_sec=200, max_timeout_sec=60),
            timeout_multiplier=3, agent_timeout_multiplier=2,
            trials_dir=self.root, trial_name="trial",
        )
        (self.root / "trial").mkdir()
        plugin = self.module.MelissaDeadlinePlugin()
        with patch.object(self.module, "time", SimpleNamespace(monotonic=lambda: 100)):
            await plugin._agent_start(SimpleNamespace(config=config))
        result = json.loads((self.root / "trial" / "melissa-budget.json").read_text())
        self.assertEqual(result["deadline_monotonic"], 220)

    async def test_deadline_plugin_uses_only_cached_task_metadata(self):
        task_dir = self.root / "task"
        task_dir.mkdir()
        (task_dir / "task.toml").write_text('[agent]\ntimeout_sec = 60\n')
        config = SimpleNamespace(
            agent=SimpleNamespace(name="melissa_agent:MelissaAgent", import_path=None,
                                  override_timeout_sec=None, max_timeout_sec=None),
            task=SimpleNamespace(download_dir=None, get_task_id=lambda: "task-id"),
            timeout_multiplier=3, agent_timeout_multiplier=None,
            trials_dir=self.root, trial_name="trial",
        )
        (self.root / "trial").mkdir()
        client = SimpleNamespace(download_tasks=AsyncMock(return_value=SimpleNamespace(paths=[task_dir])))
        with patch.object(self.module, "TaskClient", return_value=client), \
                patch.object(self.module, "time", SimpleNamespace(monotonic=lambda: 100)):
            await self.module.MelissaDeadlinePlugin()._agent_start(SimpleNamespace(config=config))
        self.assertEqual(json.loads((self.root / "trial" / "melissa-budget.json").read_text())["deadline_monotonic"], 280)


if __name__ == "__main__":
    unittest.main()
