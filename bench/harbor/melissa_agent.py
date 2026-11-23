"""Harbor agent that drives Melissa's Gateway loop (``melissa-bench``) on the
host and serves its exec/read/write requests inside the task container.

Run from the repository root:
  PYTHONPATH=bench/harbor harbor run -d terminal-bench@2.0 \
    --agent-import-path melissa_agent:MelissaAgent -m <model> --ak endpoint=<url> \
    --plugin melissa_agent:MelissaDeadlinePlugin
Provider settings come from agent kwargs or MELISSA_BENCH_* variables; the API
key is read from MELISSA_BENCH_API_KEY on the host and never enters the task.
"""

import asyncio
import base64
import json
import math
import os
import posixpath
import shlex
import tempfile
import time
from pathlib import Path

from harbor.agents.base import BaseAgent
from harbor.constants import MAIN_SERVICE_NAME
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.models.job.plugin import BaseJobPlugin
from harbor.models.task.config import TaskConfig
from harbor.tasks.client import TaskClient
from harbor.trial.hooks import TrialEvent

from melissa_runtime import atomic_bytes, atomic_json, remaining_wall_ms, update_context
from melissa_resources import ResourceRecorder

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MELISSA_BENCH_BIN", ROOT / "target/release/melissa-bench"))


class MelissaAgent(BaseAgent):
    def __init__(self, *args, endpoint=None, protocol=None, max_turns=None, wall_time_sec=None, **kwargs):
        super().__init__(*args, **kwargs)
        self.endpoint = endpoint or os.environ.get("MELISSA_BENCH_ENDPOINT")
        self.protocol = protocol or os.environ.get("MELISSA_BENCH_PROTOCOL", "open_ai_chat_completions")
        self.max_turns = str(max_turns or os.environ.get("MELISSA_BENCH_MAX_TURNS", "200"))
        self.workdir = "/"
        self.wall_time_sec = float(wall_time_sec) if wall_time_sec is not None else None

    def _wall_ms(self) -> int:
        path = self.logs_dir.parent / "melissa-budget.json"
        budget = json.loads(path.read_text()) if path.exists() else None
        configured = self.wall_time_sec
        if configured is None and "MELISSA_BENCH_WALL_MS" in os.environ:
            configured = float(os.environ["MELISSA_BENCH_WALL_MS"]) / 1000
        return remaining_wall_ms(budget, configured, time.monotonic())

    @staticmethod
    def name() -> str:
        return "melissa"

    def version(self) -> str:
        return "0.1.0"

    async def setup(self, environment: BaseEnvironment) -> None:
        r = await environment.exec("pwd")
        self.workdir = (r.stdout or "/").strip() or "/"

    def _abs(self, path: str) -> str:
        return posixpath.normpath(posixpath.join(self.workdir, path))

    async def _file_output(self, env: BaseEnvironment, command: str) -> str:
        marker = "melissa_" + os.urandom(16).hex()
        result = await env.exec(
            f"printf '\\n{marker}:start\\n'; {{ {command}; }} 2>/dev/null; "
            f"status=$?; printf '\\n{marker}:end\\n'; exit $status"
        )
        if result.return_code != 0:
            raise OSError("file command failed")
        _, start, body = (result.stdout or "").partition(f"\n{marker}:start\n")
        payload, end, _ = body.partition(f"\n{marker}:end\n")
        if not start or not end:
            raise ValueError("missing file output frame")
        return payload

    async def _serve(self, env: BaseEnvironment, req: dict, scratch: Path) -> dict:
        op = req.get("op")
        if op == "exec":
            t = int(req.get("timeout_sec", 180))
            try:
                r = await env.exec(req["command"], timeout_sec=t)
            except Exception as e:  # harbor raises on timeout
                return {"exit_code": -1, "stdout": "", "stderr": f"{type(e).__name__}: {e}"}
            return {"exit_code": r.return_code, "stdout": r.stdout or "", "stderr": r.stderr or ""}
        path = self._abs(req["path"])
        if op == "read":
            try:
                q = shlex.quote(path)
                encoded = await self._file_output(env, f"set -o pipefail; test -f {q} && head -c 16777217 -- {q} | base64 -w0")
                if len(base64.b64decode(encoded, validate=True)) > 16 * 1024 * 1024:
                    return {"error": "file exceeds read limit; use bash"}
                return {"content_b64": encoded}
            except Exception as e:
                return {"error": f"cannot read {path}: {type(e).__name__}"}
        if op == "write":
            q = shlex.quote(path)
            stage = None
            try:
                data = base64.b64decode(req["content_b64"], validate=True)
                parent = posixpath.dirname(path)
                stage = (await self._file_output(env, f"test ! -d {q} && test ! -L {q} && mkdir -p {shlex.quote(parent)} && mktemp -d {shlex.quote(parent + '/.melissa-write-XXXXXXXX')}")).strip()
                if not stage:
                    return {"error": f"cannot stage file write: {path}"}
                mode = (await self._file_output(env, f"stat -c %a -- {q} 2>/dev/null || true")).strip()
                if any(c not in "01234567" for c in mode):
                    raise ValueError("invalid file mode")
                if not mode:
                    mode = "644"
                ready = shlex.quote(stage + "/ready")
                # Pinned Harbor Docker stdin preserves the task UID without exposing data in argv.
                command = ["exec", "-T"]
                user = env._resolve_user(None)
                if user is not None:
                    command.extend(["-u", str(user)])
                command.extend([MAIN_SERVICE_NAME, "sh", "-c", f"cat > {ready}"])
                result = await env._run_docker_compose_command(command, stdin_data=data, check=False)
                if result.return_code != 0:
                    return {"error": f"cannot stage file contents: {path}"}
                result = await env.exec(f"chmod {mode} -- {ready} && test ! -L {q} && mv -T -- {ready} {q}")
                if result.return_code != 0:
                    return {"error": f"cannot replace file: {path}"}
                return {}
            except Exception as e:
                return {"error": f"cannot write {path}: {type(e).__name__}"}
            finally:
                if stage:
                    try:
                        async with asyncio.timeout(1):
                            await env.exec(f"rm -rf -- {shlex.quote(stage)}", timeout_sec=1)
                    except Exception:
                        pass
        return {"error": "unknown op"}

    async def run(self, instruction: str, environment: BaseEnvironment, context: AgentContext) -> None:
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        started = time.monotonic()
        with tempfile.TemporaryDirectory() as tmp, \
                (Path(tmp) / "trace.jsonl").open("w", encoding="utf-8") as trace, \
                (Path(tmp) / "stderr.log").open("wb") as stderr:
            scratch = Path(tmp).resolve()
            (scratch / "instruction.md").write_text(instruction, encoding="utf-8")
            progress_path = self.logs_dir.parent / "melissa-progress.json"
            progress_path.unlink(missing_ok=True)
            child_env = dict(os.environ)
            child_env.update(
                MELISSA_BENCH_MODEL=self.model_name or child_env.get("MELISSA_BENCH_MODEL", ""),
                MELISSA_BENCH_PROTOCOL=self.protocol,
                MELISSA_BENCH_MAX_TURNS=self.max_turns,
                MELISSA_BENCH_PROGRESS_PATH=str(progress_path),
                TMPDIR=str(scratch), TMP=str(scratch), TEMP=str(scratch),
            )
            if self.endpoint:
                child_env["MELISSA_BENCH_ENDPOINT"] = self.endpoint
            proc = None
            done = {"status": "NoResult", "telemetry_complete": False}
            final_received = False
            resources = ResourceRecorder()
            try:
                wall_ms = self._wall_ms()
                child_env["MELISSA_BENCH_WALL_MS"] = str(wall_ms)
                deadline = time.monotonic() + wall_ms / 1000
                await resources.prepare(environment)
                remaining_ms = math.floor((deadline - time.monotonic()) * 1000)
                if remaining_ms <= 0:
                    raise TimeoutError("deadline elapsed before model process launch")
                child_env["MELISSA_BENCH_WALL_MS"] = str(remaining_ms)
                proc = await asyncio.create_subprocess_exec(
                    str(BIN), str(scratch / "instruction.md"),
                    stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
                    stderr=stderr, env=child_env, limit=64 * 1024 * 1024,
                )
                resources.start(proc.pid)
                async with asyncio.timeout_at(deadline):
                    done = await self._drive(proc, environment, scratch, trace)
                final_received = True
            except asyncio.CancelledError:
                done["status"] = "Cancelled"
                raise
            except TimeoutError:
                done["status"] = "Deadline"
            except BaseException as e:
                done["status"] = "BridgeError"
                done["error_type"] = type(e).__name__
                raise
            finally:
                try:
                    if proc is not None:
                        if proc.returncode is None:
                            proc.kill()
                        await proc.wait()
                finally:
                    if progress_path.exists() and (not final_received or done.get("status") == "GatewayError"):
                        try:
                            progress = json.loads(progress_path.read_text())
                            if done.get("tool_calls") is None:
                                done["tool_calls"] = progress.get("tool_calls")
                            done = {**progress, **done}
                        except (OSError, ValueError):
                            done["progress_unreadable"] = True
                    if not final_received or done.get("status") == "GatewayError":
                        done["telemetry_complete"] = False
                    done["wall_ms"] = int((time.monotonic() - started) * 1000)
                    try:
                        atomic_json(self.logs_dir / "melissa-resources.json", await resources.finish())
                    except Exception:
                        done["resource_measurement_unavailable"] = True
                    trace.flush()
                    stderr.flush()
                    atomic_bytes(self.logs_dir / "melissa-trace.jsonl", (scratch / "trace.jsonl").read_bytes())
                    atomic_bytes(self.logs_dir / "melissa-stderr.log", (scratch / "stderr.log").read_bytes())
                    atomic_json(self.logs_dir / "melissa-result.json", done)
                    update_context(context, done)

    async def _drive(self, proc, environment, scratch, trace) -> dict:
        while line := await proc.stdout.readline():
            req = json.loads(line)
            if req.get("op") == "done":
                return req
            t0 = time.monotonic()
            rep = await self._serve(environment, req, scratch)
            trace.write(json.dumps({"req": {k: v for k, v in req.items() if k != "content_b64"},
                                    "ms": int((time.monotonic() - t0) * 1000),
                                    "exit_code": rep.get("exit_code"), "error": rep.get("error")}) + "\n")
            trace.flush()
            proc.stdin.write((json.dumps(rep) + "\n").encode())
            await proc.stdin.drain()
        raise RuntimeError("Melissa exited without a final result")


class MelissaDeadlinePlugin(BaseJobPlugin):
    """Pass Harbor's single-step task deadline to the host-only bridge."""

    async def on_job_start(self, job):
        job.add_hook(TrialEvent.AGENT_START, self._agent_start)

    async def on_job_end(self, job_result):
        pass

    async def _agent_start(self, event):
        if event.config.agent.name not in ("melissa_agent:MelissaAgent", "melissa") and \
                event.config.agent.import_path != "melissa_agent:MelissaAgent":
            return
        import tomllib

        started = time.monotonic()
        config = event.config
        base = config.agent.override_timeout_sec
        if base is None:
            downloaded = await TaskClient().download_tasks(
                [config.task.get_task_id()], output_dir=config.task.download_dir,
            )
            # Only task metadata is read; never solution, tests, or instruction.
            task = TaskConfig.model_validate(tomllib.loads(
                (downloaded.paths[0] / "task.toml").read_text()
            ))
            if task.steps:
                raise ValueError("Melissa deadline plugin supports single-step tasks only")
            base = task.agent.timeout_sec
        multiplier = config.agent_timeout_multiplier
        if multiplier is None:
            multiplier = config.timeout_multiplier
        seconds = None if base is None else min(base, config.agent.max_timeout_sec or math.inf) * multiplier
        atomic_json(config.trials_dir / config.trial_name / "melissa-budget.json", {
            "deadline_monotonic": None if seconds is None else started + seconds,
        })
