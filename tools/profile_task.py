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
import selectors
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "bench" / "harbor"))
from melissa_resources import CgroupMeter, pid_sample, unavailable_worker

OUTPUT_LIMIT_BYTES = 64 * 1024 * 1024


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
    with selectors.DefaultSelector() as streams:
        child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        totals = {"stdout": 0, "stderr": 0}
        for name, stream in (("stdout", child.stdout), ("stderr", child.stderr)):
            os.set_blocking(stream.fileno(), False)
            streams.register(stream, selectors.EVENT_READ, name)
        def drain():
            for ready, _ in streams.select(0):
                for _ in range(64):
                    try: block = os.read(ready.fd, 65536)
                    except BlockingIOError: break
                    if not block:
                        streams.unregister(ready.fileobj)
                        break
                    totals[ready.data] += len(block)
        def output_exceeded():
            return max(totals.values()) > OUTPUT_LIMIT_BYTES
        next_sample = start
        try:
            while True:
                now = time.perf_counter_ns()
                if now >= next_sample:
                    rss = pid_sample(child.pid)["rss_bytes"]
                    if rss is not None: sampled_rss = max(rss, sampled_rss or 0)
                    if meter: meter.sample()
                    next_sample = now + round(interval * 1e9)
                drain()
                pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                if pid:
                    drain()
                    if output_exceeded(): reason = "output_limit"
                    break
                if time.perf_counter_ns() - start >= timeout * 1_000_000_000:
                    reason = "host_timeout"
                if output_exceeded():
                    reason = "output_limit"
                if reason:
                    try: os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError: pass
                    _, status, usage = os.wait4(child.pid, 0)
                    drain()
                    break
                now = time.perf_counter_ns()
                streams.select(max(0, min(next_sample - now, start + round(timeout * 1e9) - now) / 1e9))
            child.returncode = os.waitstatus_to_exitcode(status)
        finally:
            if child.returncode is None:
                try: os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError: pass
                child.wait()
            child.stdout.close()
            child.stderr.close()
        wall_us = (time.perf_counter_ns() - start) // 1000
        output_bytes, error_bytes = totals["stdout"], totals["stderr"]
        counts_complete = not streams.get_map()
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
            "stdout_bytes": output_bytes, "stderr_bytes": error_bytes, "output_counts_complete": counts_complete,
            "scope": "wait4 CPU for the direct child and waited descendants; sampled RSS only the root process (Linux), not aggregate tree RAM. Lifetime RSS may include pre-exec launcher memory. Sampling/launch overhead included; monitor excluded. Output is drained and discarded without disk capture; byte counts are observed lower bounds when output_counts_complete is false."},
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
    return 0 if result["exit_code"] == 0 and result["stop_reason"] is None else 1


if __name__ == "__main__":
    sys.exit(main())
