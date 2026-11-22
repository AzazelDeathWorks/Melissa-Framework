"""Offline bridge tests. Harbor-specific cases run when Harbor 0.26 is installed."""
import asyncio
import base64
import importlib.util
import json
import logging
import os
import shutil
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

    async def test_directory_write_is_rejected_and_read_only_reads_do_not_poison_writes(self):
        class LocalContainer:
            async def exec(self, command, env=None, **_):
                if env is not None or "MELISSA_WRITE_CHUNK" in command or base64.b64encode(b"new content").decode() in command:
                    raise AssertionError("file data must not enter process arguments")
                proc = await asyncio.create_subprocess_shell(command, env={**os.environ, **(env or {})}, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
                out, err = await proc.communicate()
                return SimpleNamespace(return_code=proc.returncode, stdout=out.decode(), stderr=err.decode())
            async def upload_file(self, source, dest):
                if source.parent.stat().st_mode & 0o777 != 0o700:
                    raise AssertionError("host transfer files require a private directory")
                if source.stat().st_mode & 0o444 != 0o444:
                    raise PermissionError("root-owned uploads must be readable by the agent")
                shutil.copy2(source, dest)
            async def download_file(self, source, dest):
                shutil.copy2(source, dest, follow_symlinks=False)
        env = LocalContainer()
        directory = self.root / "directory"
        directory.mkdir()
        mode = directory.stat().st_mode
        content = base64.b64encode(b"new content").decode()
        request = {"op": "write", "path": str(directory), "content_b64": content}
        result = await self.agent._serve(env, request, self.root)
        self.assertIn("error", result)
        self.assertEqual(list(directory.iterdir()), [])
        self.assertEqual(directory.stat().st_mode, mode)
        file = self.root / "read-only"
        file.write_text("old")
        file.chmod(0o444)
        read = await self.agent._serve(env, {"op": "read", "path": str(file)}, self.root)
        self.assertEqual(base64.b64decode(read["content_b64"]), b"old")
        request["path"] = str(file)
        self.assertEqual(await self.agent._serve(env, request, self.root), {})
        self.assertEqual(file.read_text(), "new content")
        self.assertEqual(file.stat().st_mode & 0o777, 0o444)
        link = self.root / "link"
        link.symlink_to(file)
        request["path"] = str(link)
        self.assertIn("error", await self.agent._serve(env, request, self.root))
        self.assertTrue(link.is_symlink())
        self.assertEqual(file.read_text(), "new content")
        request["path"] = str(file)
        for payload in (b"", b"a" * 140000, "ğ".encode() * 40000):
            request["content_b64"] = base64.b64encode(payload).decode()
            self.assertEqual(await self.agent._serve(env, request, self.root), {})
            self.assertEqual(file.read_bytes(), payload)

    async def test_task_writable_log_links_cannot_truncate_host_files(self):
        self.agent.logs_dir.mkdir()
        sentinel = self.root / "host-file"
        sentinel.write_text("unchanged")
        for name in ("melissa-trace.jsonl", "melissa-stderr.log", "melissa-result.json"):
            (self.agent.logs_dir / name).symlink_to(sentinel)
        await self.agent.run(self.fixture + "print('{\"op\":\"done\"}')\n", None, self.context)
        self.assertEqual(sentinel.read_text(), "unchanged")
        self.assertFalse((self.agent.logs_dir / "melissa-trace.jsonl").is_symlink())

    @unittest.skipUnless(os.environ.get("MELISSA_HARBOR_DOCKER_IMAGE"), "opt-in local Docker image")
    async def test_real_harbor_upload_handles_nonroot_tmpfs_without_content_in_argv(self):
        from harbor.constants import MAIN_SERVICE_NAME
        from harbor.environments.docker.docker_unix import UnixOps

        project = "melissa-transfer-" + os.urandom(6).hex()
        config = self.root / "compose.yaml"
        config.write_text(
            f"services:\n  {MAIN_SERVICE_NAME}:\n"
            f"    image: {json.dumps(os.environ['MELISSA_HARBOR_DOCKER_IMAGE'])}\n"
            '    user: "10001:10001"\n    network_mode: none\n'
            '    read_only: true\n    tmpfs: ["/tmp:rw,nosuid,nodev"]\n'
            '    entrypoint: ["/bin/sh", "-c", "tail -f /dev/null"]\n'
        )
        argv = []

        class Container:
            logger = logging.getLogger(__name__)

            async def _run_docker_compose_command(self, command, check=True, stdin_data=None, **_):
                args = ["docker", "compose", "-p", project, "-f", str(config), *command]
                argv.append(args)
                proc = await asyncio.to_thread(subprocess.run, args, input=stdin_data, capture_output=True, timeout=30)
                if check and proc.returncode:
                    raise RuntimeError(proc.stderr.decode())
                return SimpleNamespace(return_code=proc.returncode, stdout=proc.stdout.decode(), stderr=proc.stderr.decode())

            async def exec(self, command, **_):
                return await self._run_docker_compose_command(
                    ["exec", "-T", "-u", "10001:10001", MAIN_SERVICE_NAME, "sh", "-c", command], check=False
                )

            async def upload_file(self, source, dest):
                await UnixOps(self).upload_file(source, dest)

        env = Container()
        marker = b"private-transfer-content-not-for-argv"
        try:
            await env._run_docker_compose_command(["up", "-d", "--pull", "never"])
            await env.exec("printf old > /tmp/data; chmod 640 /tmp/data")
            for data in (marker, b"", marker + "ğ".encode() * 70000):
                request = {"op": "write", "path": "/tmp/data", "content_b64": base64.b64encode(data).decode()}
                self.assertEqual(await self.agent._serve(env, request, self.root), {})
                self.assertEqual((await env.exec("cat /tmp/data")).stdout.encode(), data)
                self.assertEqual((await env.exec("stat -c %u:%a /tmp/data")).stdout.strip(), "10001:640")
            await env.exec("ln -s /tmp/data /tmp/link")
            request["path"] = "/tmp/link"
            self.assertIn("error", await self.agent._serve(env, request, self.root))
            self.assertEqual((await env.exec("readlink /tmp/link")).stdout.strip(), "/tmp/data")
            self.assertEqual((await env.exec("cat /tmp/data")).stdout.encode(), data)
            self.assertFalse((await env.exec("find /tmp -name '.melissa-write-*'")).stdout)
            commands = "\0".join(arg for args in argv for arg in args)
            self.assertNotIn(marker.decode(), commands)
            self.assertNotIn(base64.b64encode(marker).decode(), commands)
        finally:
            await env._run_docker_compose_command(["down", "--timeout", "1"], check=False)

    async def test_timeout_saves_last_snapshot_without_faking_complete_usage(self):
        self.agent.wall_time_sec = 0.3
        record = self.root / "scratch-path"
        code = self.fixture + (
            f"open({str(record)!r}, 'w').write(os.environ['TMPDIR'])\n"
            "import tempfile\n"
            "store = tempfile.mkdtemp()\n"
            "open(store + '/orphan-store', 'w').write('data')\n"
            "time.sleep(30)\n"
        )
        await self.agent.run(code, None, self.context)
        result = self.result()
        self.assertEqual(result["status"], "Deadline")
        self.assertFalse(result["telemetry_complete"])
        self.assertEqual(result["reported_input_tokens"], 12)
        self.assertEqual(result["pending_requests"], 1)
        self.assertIsNone(self.context.n_input_tokens)
        self.assertFalse(Path(record.read_text()).exists())

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
