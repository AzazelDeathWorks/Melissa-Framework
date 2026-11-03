"""Closed native build adapter. No expected tests or API credentials enter here."""
import os
import shutil
import subprocess
import sys

name = sys.argv[1]
environment = dict(os.environ)
environment["RUSTC"] = "/usr/local/cargo/bin/rustc"
result = subprocess.run(
    ["/usr/local/cargo/bin/cargo", "build", "--frozen", "--offline", "--release",
     "--bin", name, "--target-dir", "qa-build"],
    env=environment,
    check=False,
)
if result.returncode != 0:
    raise SystemExit(result.returncode)
shutil.copyfile("qa-build/release/" + name, "program")
# Only the reserved, owned guest build directory is removed. The host workspace
# is never mounted and no project inputs may use this reserved directory.
shutil.rmtree("qa-build")
