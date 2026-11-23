"""Free, isolated Linux CPU/RAM worker check. Never pulls images or calls a model."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import uuid

from profile_task import profile

SOURCE = r"""
#include <stdlib.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc == 1) { sleep(180); return 0; }
    const size_t n = 24 * 1024 * 1024;
    volatile unsigned char *a = malloc(n);
    if (!a) return 2;
    for (size_t i = 0; i < n; ++i) a[i] = (unsigned char)i;
    struct timespec start, now;
    clock_gettime(CLOCK_MONOTONIC, &start);
    uint64_t sum = 0;
    do {
        for (size_t i = 0; i < n; i += 4096) sum += a[i];
        clock_gettime(CLOCK_MONOTONIC, &now);
    } while ((now.tv_sec - start.tv_sec) + (now.tv_nsec - start.tv_nsec) / 1e9 < .5);
    printf("%zu %llu\n", n, (unsigned long long)sum);
    free((void *)a);
    return 0;
}
"""


def smoke(image, repetitions=3):
    if platform.system() != "Linux" or type(repetitions) is not int or not 1 <= repetitions <= 10:
        raise ValueError("requires native Linux and 1..10 repetitions")
    engine, compiler = shutil.which("docker"), shutil.which("cc")
    if not engine or not compiler:
        raise ValueError("requires Docker and a C compiler supporting static linking")
    if not image or image.startswith("-"):
        raise ValueError("provide a local image name/digest")
    image_id = subprocess.check_output([engine, "image", "inspect", "--format", "{{.Id}}", image], timeout=10, text=True).strip()
    if not re.fullmatch(r"sha256:[a-f0-9]{64}", image_id):
        raise ValueError("unexpected image identity")
    records = []
    with tempfile.TemporaryDirectory(prefix="melissa-worker-profile-", dir=Path.home()) as root:
        source, binary = (Path(root) / name for name in ("worker.c", "worker"))
        source.write_text(SOURCE)
        subprocess.run([compiler, "-O2", "-static", str(source), "-o", str(binary)], check=True, timeout=60)
        binary.chmod(0o755)
        binary_hash = hashlib.sha256(binary.read_bytes()).hexdigest()
        for _ in range(repetitions):
            nonce = uuid.uuid4().hex
            owned = f"melissa-profile-{nonce}"
            try:
                subprocess.run([engine, "run", "-d", "--pull", "never", "--name", owned,
                    "--label", f"dev.melissa.profile.owner={nonce}",
                    "--read-only", "--network", "none", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                    "--memory", "128m", "--pids-limit", "64", "--cpus", "1", "--user", "65534:65534",
                    "--mount", f"type=bind,src={binary},dst=/fixture,readonly", "--entrypoint", "/fixture", image_id], check=True, timeout=30, stdout=subprocess.DEVNULL)
                record = profile([engine, "exec", owned, "/fixture", "load"], timeout=30, container=owned, interval=.02)
                if record["exit_code"] != 0 or not record["worker"]["cpu_time_delta_us"] or (record["worker"]["sampled_peak_memory_bytes"] or 0) < 24 * 1024 * 1024:
                    raise RuntimeError("worker fixture or cgroup measurement failed")
                records.append(record)
            finally:
                found = subprocess.run([engine, "inspect", "--format", '{{.Id}} {{index .Config.Labels "dev.melissa.profile.owner"}}', owned], timeout=10, capture_output=True, text=True)
                if found.returncode == 0:
                    parts = found.stdout.split()
                    if len(parts) != 2 or not re.fullmatch(r"[a-f0-9]{64}", parts[0]) or parts[1] != nonce:
                        raise ValueError("container ownership mismatch; cleanup refused")
                    subprocess.run([engine, "rm", "-f", "-v", parts[0]], check=True, timeout=30, stdout=subprocess.DEVNULL)
    return {"scope": "Fresh unprivileged, network-isolated C CPU/RAM fixtures, not model quality or a harness competitor benchmark.",
            "image_id": image_id, "fixture_source_sha256": hashlib.sha256(SOURCE.encode()).hexdigest(),
            "fixture_binary_sha256": binary_hash, "records": records}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--repetitions", type=int, default=3)
    args = parser.parse_args()
    print(json.dumps(smoke(args.image, args.repetitions), indent=2))
