"""Private snapshot supervisor. All project output is untrusted observation.

No host mount, daemon, network or host environment; Rust owns permits/evidence.
Protocol output is framed separately from the child stdout/stderr byte streams.
"""
import json
import hashlib
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import subprocess
import sys
import threading

ROOT = Path.cwd().resolve()
PRIVATE = ROOT / ".melissa-project"
P = json.loads((PRIVATE / "program.json").read_bytes())
EMIT_LOCK = threading.Lock()
ENVIRONMENT = P["environment"]
ENV = {
    "PATH": "/opt/node/bin:/opt/melissa-python/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin",
    "HOME": str(PRIVATE / "home"), "TMPDIR": str(PRIVATE / "tmp"),
    "CARGO_HOME": str(PRIVATE / "cargo"), "RUSTUP_HOME": "/usr/local/rustup",
    "CARGO_TARGET_DIR": str(PRIVATE / "target"), "CARGO_NET_OFFLINE": "true",
    "UV_OFFLINE": "1", "UV_NO_PYTHON_DOWNLOADS": "1",
    "UV_CACHE_DIR": str(PRIVATE / "uv-cache"),
    "PIP_NO_INDEX": "1", "PIP_DISABLE_PIP_VERSION_CHECK": "1",
    "npm_config_offline": "true", "npm_config_audit": "false", "npm_config_fund": "false",
    "npm_config_cache": str(PRIVATE / "npm-cache"), "COREPACK_ENABLE_NETWORK": "0",
    "YARN_ENABLE_NETWORK": "0", "MISE_OFFLINE": "1", "MISE_AUTO_INSTALL": "0",
    "YARN_CACHE_FOLDER": str(PRIVATE / "yarn-cache"),
    "npm_config_store_dir": str(PRIVATE / "pnpm-store"),
    "MISE_EXEC_AUTO_INSTALL": "0", "MISE_DATA_DIR": "/opt/mise",
    "MISE_CACHE_DIR": str(PRIVATE / "mise-cache"),
    "PYTHONUNBUFFERED": "1", "PYTHONDONTWRITEBYTECODE": "1", "NO_COLOR": "1",
}
for p in ["home", "tmp", "cargo", "target", "uv-cache", "npm-cache", "mise-cache", "yarn-cache", "pnpm-store"]:
    (PRIVATE / p).mkdir(parents=True, exist_ok=True)
# Only prepared image caches, never host credentials/configuration. Copies live
# under the same kernel-enforced private disk budget as the project snapshot.
CACHE_ROOT = Path("/opt/melissa-project-cache")
for src, dst in [(CACHE_ROOT / name, PRIVATE / target) for name, target in
                 [("uv", "uv-cache"), ("npm", "npm-cache"), ("pnpm", "pnpm-store"), ("yarn", "yarn-cache")]]:
    if src.is_dir():
        shutil.copytree(src, dst, dirs_exist_ok=True)
for name in ["registry", "git"]:
    src = CACHE_ROOT / "cargo" / name
    if not src.is_dir():
        src = Path("/usr/local/cargo") / name
    if src.is_dir():
        shutil.copytree(src, PRIVATE / "cargo" / name, dirs_exist_ok=True)
if (CACHE_ROOT / "wheels").is_dir():
    ENV["PIP_FIND_LINKS"] = str(CACHE_ROOT / "wheels")
PIP_ENVS = {}
for name, value in ENVIRONMENT["executables"].items():
    ENV["PATH"] = str(Path(value).parent) + ":" + ENV["PATH"]
if "rust" in ENVIRONMENT["versions"]:
    ENV["RUSTUP_TOOLCHAIN"] = ENVIRONMENT["versions"]["rust"]
if "python" in ENVIRONMENT["versions"]:
    ENV["UV_PYTHON"] = ENVIRONMENT["versions"]["python"]

def emit(event, **fields):
    with EMIT_LOCK:
        sys.stdout.write(json.dumps({"event": event, **fields}, separators=(",", ":")) + "\n")
        sys.stdout.flush()

def resolve(tool):
    name = {"rust": "rustc", "python3": "python"}.get(tool, tool)
    if ENVIRONMENT["use_mise"] and name in ["node", "python", "rustc", "cargo"] and name not in ENVIRONMENT["executables"]:
        return name
    return ENVIRONMENT["executables"].get(name) or shutil.which(name, path=ENV["PATH"])

def wrap(argv):
    if ENVIRONMENT["use_mise"]:
        mise = resolve("mise")
        if not mise:
            raise RuntimeError("Requested mise executable is not installed in the selected image")
        selectors_ = [k + "@" + v for k, v in ENVIRONMENT["versions"].items() if k in ["python", "node", "rust"]]
        return [mise, "exec", *selectors_, "--", *argv]
    return argv

def probe(tool):
    exe = resolve(tool)
    if not exe:
        raise RuntimeError("Missing selected worker tool: " + tool)
    argv = wrap([exe, "--version"])
    completed = subprocess.run(argv, cwd=ROOT, env=ENV, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    raw = completed.stdout + completed.stderr
    if completed.returncode or len(raw) > 65536:
        raise RuntimeError("Tool version probe failed: " + tool)
    text = raw.decode("utf-8", "replace").strip()
    found = re.search(r"(?<![0-9])([0-9]+\.[0-9]+(?:\.[0-9]+)?(?:-[a-zA-Z0-9.-]+)?)", text)
    if not found:
        raise RuntimeError("Unrecognized version: " + tool)
    version = found.group(1)
    required = ENVIRONMENT["versions"].get(tool)
    if required and version.split(".")[:len(required.split("."))] != required.split("."):
        raise RuntimeError("Version mismatch for " + tool + ": " + version + " requires " + required)
    emit("tool", tool=tool, executable=exe, version=version)
    return version

def preflight():
    requirements = P.get("service_requirements", {})
    if requirements.get("contract") and not Path("/opt/melissa-contracts/bin/schemathesis").is_file():
        raise RuntimeError("Selected service SDK has no reviewed Schemathesis executable")
    if requirements.get("postgres"):
        if not any(Path("/usr/lib/postgresql").glob("*/bin/postgres")):
            raise RuntimeError("Selected service SDK has no PostgreSQL engine")
        adapter_probe = subprocess.run(["/opt/melissa-python/bin/python", "-I", "-c", "import psycopg"], env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
        if adapter_probe.returncode:
            raise RuntimeError("Selected service SDK has no PostgreSQL adapter")
    tools = set(ENVIRONMENT["versions"])
    for command in [P["command"], *P.get("sync", []), *P.get("required_commands", [])]:
        name = Path(command["executable"]).name
        if name in ["python", "python3", "node", "cargo", "rustc", "uv", "npm", "pnpm", "yarn"]:
            tools.add({"python3": "python", "cargo": "rust", "rustc": "rust"}.get(name, name))
    versions = {tool: probe(tool) for tool in sorted(tools)}
    for key, spec in P.get("constraints", {}).items():
        tool = key.rsplit(":", 1)[-1]
        if tool == "rust-min":
            actual = versions.get("rust") or probe("rust")
            if tuple(map(int, actual.split("-")[0].split("."))) < tuple(map(int, spec.split("."))):
                raise RuntimeError("Rust rust-version minimum not met")
        elif tool == "python":
            actual = versions.get("python") or probe("python")
            py = resolve("python")
            code = "from packaging.specifiers import SpecifierSet; import sys; sys.exit(0 if sys.argv[1] in SpecifierSet(sys.argv[2]) else 1)"
            if subprocess.run(wrap([py, "-I", "-c", code, actual, spec]), cwd=ROOT, env=ENV,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
                raise RuntimeError("Python constraint not satisfied or packaging unavailable: " + spec)
        elif tool in ["node", "npm"]:
            actual = versions.get(tool) or probe(tool)
            code = "process.exit(require('/opt/melissa-tools/node_modules/semver').satisfies(process.argv[1],process.argv[2])?0:1)"
            if subprocess.run(wrap([resolve("node"), "-e", code, actual, spec]), cwd=ROOT, env=ENV,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
                raise RuntimeError("Node/npm constraint not satisfied or semver unavailable: " + spec)
        else:
            raise RuntimeError("Unsupported engine constraint requires explicit environment: " + key)
    memory = Path("/sys/fs/cgroup/memory.max")
    meminfo = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
    emit("preflight", status="PASS", versions=versions,
         cpus=os.cpu_count(), disk_free_bytes=shutil.disk_usage(ROOT).free,
         worker_vm_mem_available_bytes=int(meminfo["MemAvailable"].split()[0])*1024,
         cgroup_memory_current_bytes=int(Path("/sys/fs/cgroup/memory.current").read_text()),
         cgroup_cpu_max=Path("/sys/fs/cgroup/cpu.max").read_text().strip(),
         cgroup_pids_max=Path("/sys/fs/cgroup/pids.max").read_text().strip(),
         cgroup_memory_max=memory.read_text().strip() if memory.exists() else None)

def run_command(command, stage, interactive):
    exe = command["executable"]
    if "/" not in exe:
        exe = resolve(exe)
    if not exe:
        raise RuntimeError("Command executable unavailable: " + command["executable"])
    cwd = (ROOT / command["cwd"]).resolve()
    if not cwd.is_relative_to(ROOT) or not cwd.is_dir():
        raise RuntimeError("Command cwd is outside the private project snapshot or missing")
    env = ENV.copy()
    pip_install = command["executable"] in ["python", "python3"] and command["argv"][:2] == ["-m", "pip"]
    if pip_install and str(cwd) not in PIP_ENVS:
        venv = PRIVATE / "venvs" / hashlib.sha256(str(cwd).encode()).hexdigest()
        prepared = subprocess.run(wrap([resolve("python"), "-m", "venv", "--system-site-packages", str(venv)]),
                                  cwd=cwd, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if prepared.returncode:
            raise RuntimeError("Private pip virtual environment could not be prepared")
        PIP_ENVS[str(cwd)] = venv
    if str(cwd) in PIP_ENVS:
        venv = PIP_ENVS[str(cwd)]
        env["VIRTUAL_ENV"] = str(venv)
        env["PATH"] = str(venv / "bin") + ":" + env["PATH"]
        if command["executable"] in ["python", "python3"]:
            exe = str(venv / "bin" / "python")
    argv = wrap([exe, *command["argv"]])
    child = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE if interactive else subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    emit("started", pid=child.pid, pgid=child.pid, stage=stage, argv=argv)
    stopped = threading.Event()
    errors = []
    if interactive:
        def write_input():
            for line in sys.stdin:
                try:
                    request = json.loads(line)
                    if request["operation"] == "stdin":
                        value = bytes.fromhex(request["hex"])
                        child.stdin.write(value)
                        child.stdin.flush()
                        emit("input_delivered_to_pipe", request_id=request.get("request_id"), bytes=len(value))
                    elif request["operation"] == "eof":
                        child.stdin.close()
                        emit("input_eof", request_id=request.get("request_id"))
                        return
                    else:
                        raise ValueError("unknown input operation")
                except (BrokenPipeError, OSError):
                    return
                except Exception as error:
                    errors.append(type(error).__name__)
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    return
        threading.Thread(target=write_input, daemon=True).start()
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ, "stdout")
    selector.register(child.stderr, selectors.EVENT_READ, "stderr")
    while selector.get_map():
        for key, _ in selector.select(0.1):
            data = os.read(key.fd, 8192)
            if data:
                emit(key.data, hex=data.hex(), stage=stage)
            else:
                selector.unregister(key.fileobj)
    code = child.wait()
    stopped.set()
    emit("exit", exit_code=code, stage=stage, input_error=errors)
    return code if not errors else 74

try:
    preflight()
    if P.get("preflight_only"):
        sys.exit(0)
    for index, command in enumerate(P.get("sync", [])):
        code = run_command(command, "sync-" + str(index), False)
        if code:
            emit("blocked", reason="Dependency synchronization failed", command_ran=False)
            sys.exit(78)
    code = run_command(P["command"], "command", True)
    sys.exit(code if 0 <= code < 256 else 1)
except Exception as error:
    emit("blocked", reason=str(error), command_ran=False)
    sys.exit(78)
