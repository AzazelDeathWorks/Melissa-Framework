"""Offline Linux/macOS CLI startup profile. Never invokes a model or worker."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import tempfile
import time


def footprint(home):
    files = [p.stat() for p in home.rglob("*") if not p.is_symlink() and p.is_file()]
    return {
        "state_logical_bytes": sum(s.st_size for s in files),
        "state_allocated_bytes": sum(s.st_blocks * 512 for s in files),
    }


def measure(command, home, timeout=30, self_report=False):
    reported = {}
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        start = time.perf_counter_ns()
        child = subprocess.Popen(command, stdout=output, stderr=errors)
        try:
            while True:
                pid, status, usage = os.wait4(child.pid, os.WNOHANG)
                if pid:
                    break
                if time.perf_counter_ns() - start >= timeout * 1_000_000_000:
                    os.kill(child.pid, signal.SIGKILL)
                    _, status, usage = os.wait4(child.pid, 0)
                    break
                time.sleep(0.001)
            child.returncode = os.waitstatus_to_exitcode(status)
        finally:
            if child.returncode is None:
                child.kill()
                child.wait()
        elapsed = time.perf_counter_ns() - start
        if child.returncode:
            raise RuntimeError(f"CLI profile command failed: exit {child.returncode}")
        if self_report:
            output.seek(0)
            sample = json.loads(output.read(65536))["sample"]
            reported = {"sampled_current_rss_bytes": sample["working_set_bytes"], "sampled_post_exec_peak_rss_bytes": sample["post_exec_peak_working_set_bytes"]}
    return {
        "wall_time_us": elapsed // 1000,
        "cpu_time_us": round((usage.ru_utime + usage.ru_stime) * 1_000_000),
        "process_lifetime_peak_rss_bytes": usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024),
        **reported,
        **footprint(home),
    }


def profile(binary, repetitions):
    if platform.system() not in ("Linux", "Darwin") or not hasattr(os, "wait4"):
        raise RuntimeError("This profiler requires Linux/macOS wait4; Windows supports melissa --json resources")
    if not 1 <= repetitions <= 100:
        raise ValueError("repetitions must be between 1 and 100")
    binary = binary.resolve(strict=True)
    hasher = hashlib.sha256()
    with binary.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(block)
    digest = hasher.hexdigest()
    cases = {}
    for label, arguments in {
        "version": ["--version"],
        "resources": ["--json", "resources"],
        "init_fresh_home": ["--json", "init"],
        "profiles": ["--json", "profiles"],
    }.items():
        samples = []
        for _ in range(repetitions):
            with tempfile.TemporaryDirectory(prefix="melissa-profile-") as temporary:
                home = Path(temporary).resolve() / "state"
                samples.append(measure([str(binary), "--home", str(home), *arguments], home, self_report=(label == "resources")))
        cases[label] = {
            "samples": samples,
            "median": {key: None if any(s[key] is None for s in samples) else statistics.median(s[key] for s in samples) for key in samples[0]},
        }
    return {
        "schema_version": "melissa.offline-cli-profile/v1",
        "host": platform.platform(),
        "binary_sha256": digest,
        "binary_bytes": binary.stat().st_size,
        "repetitions": repetitions,
        "scope": "Fresh CLI processes; filesystem caches not flushed; launch/measurement overhead and 1 ms exit-poll quantization included in wall time. CPU from wait4 per child; lifetime peak RSS can include launcher memory before exec. The resources case separately samples current and post-exec peak CLI RSS (null if unavailable). No model, tools, worker or interactive UI workload; not a competitor or task benchmark.",
        "disk_scope": "Snapshot of fresh CLI state after exit: logical bytes and st_blocks*512 allocated bytes. Not physical I/O, peak disk, Docker images or binary/build dependencies.",
        "cases": cases,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=10)
    args = parser.parse_args()
    print(json.dumps(profile(args.binary, args.repetitions), indent=2))


if __name__ == "__main__":
    main()
