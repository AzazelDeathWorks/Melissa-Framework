"""Profile an explicit host command and optional existing Linux Docker worker.
No arguments, environment values or command output enter the JSON report.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "bench" / "harbor"))
from melissa_resources import CgroupMeter, pid_sample, unavailable_worker


def container_identity(engine, container):
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,127}", container):
        raise ValueError("invalid container identifier")
    output = subprocess.check_output([engine, "inspect", "--format", "{{.Id}} {{.State.Pid}} {{.State.StartedAt}}", container], stderr=subprocess.DEVNULL, timeout=10, text=True).split()
    if len(output) != 3 or not re.fullmatch(r"[a-f0-9]{64}", output[0]) or not output[1].isdigit() or int(output[1]) <= 0:
        raise ValueError("container must be running")
    return output[0], int(output[1]), output[2]


def profile(command, timeout=60, container=None, interval=0.05):
    if platform.system() not in ("Linux", "Darwin") or not hasattr(os, "wait4"):
        raise ValueError("host wait4 profiling requires Linux/macOS")
    if not command or not math.isfinite(timeout) or not 0 < timeout <= 86400 or not math.isfinite(interval) or not 0.005 <= interval <= 5:
        raise ValueError("command/timeout/sample interval bounds")
    binary = shutil.which(command[0])
    if binary is None:
        raise ValueError("executable not found")
    digest = hashlib.sha256()
    with open(binary, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    meter = identity = None
    engine = shutil.which("docker") if container else None
    unavailable = None
    if container:
        if engine is None:
            raise ValueError("Docker client not found")
        identity = container_identity(engine, container)
        try:
            meter = CgroupMeter.for_pid(identity[1], identity[0])
        except (OSError, ValueError):
            unavailable = "host_cgroup_v2_unavailable"
    sampled_rss = None
    reason = None
    start = time.perf_counter_ns()
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        child = subprocess.Popen(command, stdout=output, stderr=errors, start_new_session=True)
        try:
            while True:
                rss = pid_sample(child.pid)["rss_bytes"]
                if rss is not None: sampled_rss = max(rss, sampled_rss or 0)
                if meter: meter.sample()
                pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                if pid: break
                if time.perf_counter_ns() - start >= timeout * 1_000_000_000:
                    reason = "host_timeout"
                if max(os.fstat(output.fileno()).st_size, os.fstat(errors.fileno()).st_size) > 64 * 1024 * 1024:
                    reason = "output_limit"
                if reason:
                    try: os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError: pass
                    _, status, usage = os.wait4(child.pid, 0)
                    break
                time.sleep(min(interval, max(0, timeout - (time.perf_counter_ns() - start) / 1e9)))
            child.returncode = os.waitstatus_to_exitcode(status)
        finally:
            if child.returncode is None:
                try: os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError: pass
                child.wait()
        wall_us = (time.perf_counter_ns() - start) // 1000
        output_bytes = os.fstat(output.fileno()).st_size
        error_bytes = os.fstat(errors.fileno()).st_size
    stable = False
    if identity:
        try: stable = container_identity(engine, identity[0]) == identity
        except (OSError, ValueError, subprocess.SubprocessError): pass
    return {
        "schema_version": "melissa.task-resource-profile/v1", "host": platform.platform(),
        "executable_sha256": digest.hexdigest(), "exit_code": child.returncode,
        "stop_reason": reason, "wall_time_us": wall_us, "sample_interval_us": round(interval * 1e6),
        "host_command": {"cpu_time_us": round((usage.ru_utime + usage.ru_stime) * 1e6),
            "sampled_root_peak_rss_bytes": sampled_rss,
            "process_lifetime_peak_rss_bytes": usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024),
            "stdout_bytes": output_bytes, "stderr_bytes": error_bytes,
            "scope": "wait4 CPU for the direct child and waited descendants; sampled RSS only the root process (Linux), not aggregate tree RAM. Lifetime RSS may include pre-exec launcher memory. Sampling/launch overhead included; monitor excluded."},
        "worker": meter.finish(stable) if meter else unavailable_worker(unavailable or "no_container_selected"),
        "cleanup_scope": "Timeout kills only the host process group, not detached children or commands inside a container. The owning harness must cancel its own worker; this monitor never stops an existing container.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container")
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--interval", type=float, default=0.05)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    result = profile(command, args.timeout, args.container, args.interval)
    print(json.dumps(result, indent=2))
    return 0 if result["exit_code"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
