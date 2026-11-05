#!/usr/bin/env python3
"""Export and verify an already-present, exact SDK worker image."""

import argparse
import gzip
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile


def image_id_from_archive(path):
    with tarfile.open(path, "r:gz") as archive:
        manifests = json.load(archive.extractfile("manifest.json"))
        if not isinstance(manifests, list) or len(manifests) != 1:
            raise ValueError("Expected exactly one worker image in the archive")
        config = archive.getmember(manifests[0]["Config"])
        if not config.isfile() or config.size > 1024 * 1024:
            raise ValueError("Invalid worker image configuration")
        with archive.extractfile(config) as stream:
            raw = stream.read()
        meta = json.loads(raw)
        if (meta.get("os"), meta.get("architecture")) != ("linux", "amd64"):
            raise ValueError("The worker archive must contain linux/amd64")
        return "sha256:" + hashlib.sha256(raw).hexdigest()


def export(image, output):
    actual = subprocess.check_output(
        ["docker", "image", "inspect", image, "--format", "{{.Id}}"],
        text=True,
    ).strip()
    if actual != image:
        raise ValueError("Installed worker image differs from the requested immutable ID")

    output.mkdir()
    archive = output / "melissa-sdk-linux-amd64.tar.gz"
    with subprocess.Popen(["docker", "image", "save", image], stdout=subprocess.PIPE) as process:
        with archive.open("xb") as sink:
            with gzip.GzipFile(filename="", fileobj=sink, mode="wb", compresslevel=3, mtime=0) as compressed:
                shutil.copyfileobj(process.stdout, compressed, length=1024 * 1024)
        if process.wait() != 0:
            raise RuntimeError("Docker image export failed; the partial file is not a release artifact")

    if image_id_from_archive(archive) != image:
        raise ValueError("Exported worker image configuration did not match the immutable ID")
    with archive.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    record = {
        "schema_version": "melissa.worker-release/v1",
        "archive": archive.name,
        "archive_sha256": digest,
        "image_id": image,
        "platform": "linux/amd64",
    }
    (output / "worker-manifest.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    (output / "SHA256SUMS.txt").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
    print(json.dumps(record))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", args.image):
        parser.error("--image must be a full sha256 image ID")
    export(args.image, args.output)


if __name__ == "__main__":
    main()
