"""Harbor agent that drives Melissa's Gateway loop (``melissa-bench``) on the
host and serves its exec/read/write requests inside the task container.

Run from the repository root:
  PYTHONPATH=bench/harbor harbor run -d terminal-bench@2.0 \
    --agent-import-path melissa_agent:MelissaAgent -m <model> --ak endpoint=<url>
Provider settings come from agent kwargs or MELISSA_BENCH_* variables; the API
key is read from MELISSA_BENCH_API_KEY on the host and never enters the task.
"""

import asyncio
import base64
import json
import os
import posixpath
import shlex
import tempfile
import time
from pathlib import Path

from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

ROOT = Path(__file__).resolve().parents[2]
BIN = Path(os.environ.get("MELISSA_BENCH_BIN", ROOT / "target/release/melissa-bench"))


class MelissaAgent(BaseAgent):
    def __init__(self, *args, endpoint=None, protocol=None, max_turns=None, **kwargs):
        super().__init__(*args, **kwargs)
        self.endpoint = endpoint or os.environ.get("MELISSA_BENCH_ENDPOINT")
        self.protocol = protocol or os.environ.get("MELISSA_BENCH_PROTOCOL", "open_ai_chat_completions")
        self.max_turns = str(max_turns or os.environ.get("MELISSA_BENCH_MAX_TURNS", "200"))
        self.workdir = "/"

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
        local = scratch / "io"
        if op == "read":
            local.unlink(missing_ok=True)
            try:
                await env.download_file(path, local)
                return {"content_b64": base64.b64encode(local.read_bytes()).decode()}
            except Exception as e:
                return {"error": f"cannot read {path}: {type(e).__name__}"}
        if op == "write":
            local.write_bytes(base64.b64decode(req["content_b64"]))
            q = shlex.quote(path)
            pre = await env.exec(f"mkdir -p {shlex.quote(posixpath.dirname(path))} && stat -c %a {q} 2>/dev/null || true")
            try:
                await env.upload_file(local, path)
            except Exception as e:
                return {"error": f"cannot write {path}: {type(e).__name__}"}
            mode = (pre.stdout or "").strip()
            await env.exec(f"chmod {mode} {q}" if mode.isdigit() else f"chmod 644 {q}")
            return {}
        return {"error": "unknown op"}

    async def run(self, instruction: str, environment: BaseEnvironment, context: AgentContext) -> None:
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        trace = (self.logs_dir / "melissa-trace.jsonl").open("w", encoding="utf-8")
        with tempfile.TemporaryDirectory() as tmp:
            scratch = Path(tmp)
            (scratch / "instruction.md").write_text(instruction, encoding="utf-8")
            child_env = dict(os.environ)
            child_env.update(
                MELISSA_BENCH_MODEL=self.model_name.split("/", 1)[-1] if self.model_name else child_env.get("MELISSA_BENCH_MODEL", ""),
                MELISSA_BENCH_PROTOCOL=self.protocol,
                MELISSA_BENCH_MAX_TURNS=self.max_turns,
            )
            if self.endpoint:
                child_env["MELISSA_BENCH_ENDPOINT"] = self.endpoint
            started = time.monotonic()
            proc = await asyncio.create_subprocess_exec(
                str(BIN), str(scratch / "instruction.md"),
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
                stderr=(self.logs_dir / "melissa-stderr.log").open("wb"),
                env=child_env, limit=64 * 1024 * 1024,
            )
            done = {"status": "NoResult"}
            try:
                while line := await proc.stdout.readline():
                    req = json.loads(line)
                    if req.get("op") == "done":
                        done = req
                        break
                    t0 = time.monotonic()
                    rep = await self._serve(environment, req, scratch)
                    trace.write(json.dumps({"req": {k: v for k, v in req.items() if k != "content_b64"},
                                            "ms": int((time.monotonic() - t0) * 1000),
                                            "exit_code": rep.get("exit_code"), "error": rep.get("error")}) + "\n")
                    trace.flush()
                    proc.stdin.write((json.dumps(rep) + "\n").encode())
                    await proc.stdin.drain()
            finally:
                if proc.returncode is None:
                    proc.kill()
                await proc.wait()
                trace.close()
            done["wall_ms"] = int((time.monotonic() - started) * 1000)
            (self.logs_dir / "melissa-result.json").write_text(json.dumps(done, indent=2))
            context.n_input_tokens = done.get("input_tokens")
            context.n_output_tokens = done.get("output_tokens")
            context.metadata = {k: done.get(k) for k in ("status", "requests", "tool_calls", "elapsed_ms", "error")}
