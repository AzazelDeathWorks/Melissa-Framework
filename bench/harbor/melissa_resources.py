"""Read-only Linux host/cgroup-v2 observations; no credentials or model calls."""
import asyncio
import os
from pathlib import Path, PurePosixPath
import re


def read(path):
    try:
        with Path(path).open("r", encoding="ascii") as source:
            value = source.read(65537)
        return value if len(value) <= 65536 else None
    except (OSError, UnicodeError):
        return None


def uint(value):
    if not isinstance(value, str) or len(value) > 20 or not re.fullmatch(r"[0-9]+", value):
        return None
    number = int(value)
    return number if number < 2**64 else None


def fields(text):
    result = {}
    for line in (text or "").splitlines():
        words = line.split()
        if len(words) != 2 or words[0] in result:
            return {}
        result[words[0]] = uint(words[1])
    return result


def delta(before, after):
    return after - before if before is not None and after is not None and after >= before else None


def pid_sample(pid):
    status = read(Path("/proc") / str(pid) / "status")
    rss = None
    for line in (status or "").splitlines():
        if line.startswith("VmRSS:"):
            words = line.split()
            value = uint(words[1]) if len(words) == 3 and words[2] == "kB" else None
            rss = None if value is None else value * 1024
    stat = read(Path("/proc") / str(pid) / "stat")
    words = (stat or "").rpartition(") ")[2].split()
    cpu = None
    if len(words) > 19:
        user, system = uint(words[11]), uint(words[12])
        if user is not None and system is not None:
            cpu = (user + system) * 1_000_000 // os.sysconf("SC_CLK_TCK")
    return {"rss_bytes": rss, "observed_cpu_time_us": cpu, "start_time_ticks": uint(words[19]) if len(words) > 19 else None}


def unavailable_worker(reason):
    return {"unavailable_reason": reason, "cpu_time_delta_us": None,
            "read_block_bytes_delta": None, "write_block_bytes_delta": None,
            "sampled_peak_memory_bytes": None, "lifetime_peak_memory_bytes": None}


class CgroupMeter:
    def __init__(self, path):
        self.path = Path(path)
        stat = self.path.stat()
        self.identity = (stat.st_dev, stat.st_ino)
        self.identity_stable = True
        self.samples = 0
        self.missing_samples = 0
        self.sampled_peak = None
        self.baseline = self.sample()

    @classmethod
    def for_pid(cls, pid, container_id):
        text = read(Path("/proc") / str(pid) / "cgroup")
        paths = [line[3:] for line in (text or "").splitlines() if line.startswith("0::")]
        if len(paths) != 1 or not paths[0].startswith("/"):
            raise ValueError("cgroup v2 is not visible on this host")
        path = PurePosixPath(paths[0])
        if ".." in path.parts or len(path.parts) <= 1 or not re.fullmatch(r"[a-f0-9]{64}", container_id) or not any(re.fullmatch(r"(?:docker-|libpod-)?" + container_id + r"(?:\.scope)?", part) for part in path.parts):
            raise ValueError("cgroup must be local and bound to the full container id")
        return cls(Path("/sys/fs/cgroup").joinpath(*path.parts[1:]))

    def sample(self):
        self.samples += 1
        try:
            stat = self.path.stat()
            same = (stat.st_dev, stat.st_ino) == self.identity
        except OSError:
            same = False
        if not same:
            self.identity_stable = False
            self.missing_samples += 1
            return {}
        cpu = fields(read(self.path / "cpu.stat"))
        memory = read(self.path / "memory.current")
        peak = read(self.path / "memory.peak")
        result = {
            "cpu_us": cpu.get("usage_usec"),
            "memory_bytes": uint(memory.strip()) if memory is not None else None,
            "lifetime_peak_memory_bytes": uint(peak.strip()) if peak is not None else None,
        }
        io = read(self.path / "io.stat")
        totals = {"rbytes": 0, "wbytes": 0} if io is not None else {}
        devices = set()
        for line in (io or "").splitlines():
            words = line.split()
            if not words or not re.fullmatch(r"[0-9]+:[0-9]+", words[0]) or words[0] in devices:
                totals = {}; break
            devices.add(words[0])
            values = {}
            for word in words[1:]:
                pair = word.split("=")
                if len(pair) != 2 or pair[0] in values:
                    values = {}; break
                values[pair[0]] = uint(pair[1])
            if any(values.get(key) is None for key in ("rbytes", "wbytes")):
                totals = {}; break
            for key in totals:
                totals[key] += values[key]
        result.update(read_block_bytes=totals.get("rbytes"), write_block_bytes=totals.get("wbytes"))
        current = result["memory_bytes"]
        if current is not None:
            self.sampled_peak = max(current, self.sampled_peak or 0)
        else:
            self.missing_samples += 1
        return result

    def finish(self, identity_stable=True):
        latest = self.sample()
        identity_stable = identity_stable and self.identity_stable
        return {
            "identity_stable": identity_stable,
            "samples": self.samples,
            "missing_samples": self.missing_samples,
            "cpu_time_delta_us": delta(self.baseline.get("cpu_us"), latest.get("cpu_us")) if identity_stable else None,
            "read_block_bytes_delta": delta(self.baseline.get("read_block_bytes"), latest.get("read_block_bytes")) if identity_stable else None,
            "write_block_bytes_delta": delta(self.baseline.get("write_block_bytes"), latest.get("write_block_bytes")) if identity_stable else None,
            "sampled_peak_memory_bytes": self.sampled_peak if identity_stable else None,
            "lifetime_peak_memory_bytes": latest.get("lifetime_peak_memory_bytes") if identity_stable else None,
            "scope": "Container cgroup (all its processes), not host CLI. Sampled RAM is a lower bound on the workload peak; memory.peak is the whole cgroup lifetime, not reset. Block-layer I/O sums devices (stacked devices may double-count); it is not physical disk I/O and excludes writes flushed after the observation window. Missing/reset counters stay null.",
        }


class ResourceRecorder:
    """Optional Harbor telemetry. Failures never grant authority or block work."""
    def __init__(self):
        self.worker = None
        self.host_peak = None
        self.host_cpu = None
        self.task = None

    async def prepare(self, environment):
        if not hasattr(type(environment), "_engine_cmd"):
            return
        process = None
        try:
            async with asyncio.timeout(2):
                result = await environment._run_docker_compose_command(["ps", "-q", "main"], check=False, timeout_sec=1)
                container = (result.stdout or "").strip()
                if result.return_code != 0 or not re.fullmatch(r"[a-f0-9]{64}", container):
                    return
                process = await asyncio.create_subprocess_exec(*type(environment)._engine_cmd("inspect", "--format", "{{.State.Pid}}", container), stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL)
                stdout, _ = await process.communicate()
                pid = uint(stdout.decode("ascii").strip())
                if process.returncode == 0 and pid:
                    self.worker = CgroupMeter.for_pid(pid, container)
        except Exception:
            pass
        finally:
            if process is not None and process.returncode is None:
                try:
                    process.kill()
                except ProcessLookupError:
                    pass
                await process.wait()

    def start(self, pid):
        identity = pid_sample(pid)["start_time_ticks"]
        async def observe():
            while True:
                sample = pid_sample(pid)
                same = identity is not None and sample["start_time_ticks"] == identity
                if same and sample["rss_bytes"] is not None:
                    self.host_peak = max(self.host_peak or 0, sample["rss_bytes"])
                if same and sample["observed_cpu_time_us"] is not None:
                    self.host_cpu = max(self.host_cpu or 0, sample["observed_cpu_time_us"])
                if self.worker:
                    self.worker.sample()
                await asyncio.sleep(0.1)
        self.task = asyncio.create_task(observe())

    async def finish(self):
        error = None
        if self.task:
            self.task.cancel()
            try:
                await self.task
            except asyncio.CancelledError:
                pass
            except Exception as exception:
                error = type(exception).__name__
        return {"schema_version": "melissa.harbor-resource-profile/v1", "interval_ms": 100,
                "observer_error": error,
                "host": {"sampled_peak_rss_bytes": self.host_peak, "observed_cpu_time_lower_bound_us": self.host_cpu,
                         "scope": "Rust host process only, not Python bridge or children. CPU and peak RSS are sampled lower bounds, not exact run totals."},
                "worker": self.worker.finish() if self.worker else unavailable_worker("host_cgroup_v2_unavailable")}
