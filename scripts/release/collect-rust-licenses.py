"""Collect notices from the exact locked Windows CLI dependency closure, offline."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def collect(repo: Path, output: Path) -> None:
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--offline", "--locked", "--format-version", "1",
        "--filter-platform", "x86_64-pc-windows-msvc",
    ], cwd=repo))
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    pending = [i for i, p in packages.items() if p["name"] == "melissa-cli"]
    if len(pending) != 1:
        raise ValueError("Expected one CLI package")
    selected = set()
    while pending:
        key = pending.pop()
        if key in selected:
            continue
        selected.add(key)
        pending.extend(d["pkg"] for d in nodes[key]["deps"]
                       if any(k["kind"] in (None, "build") for k in d["dep_kinds"]))
    records = []
    for key in sorted(selected):
        p = packages[key]
        if p["source"] is None:
            continue
        base = Path(p["manifest_path"]).parent
        candidates = list(base.iterdir())
        if p.get("license_file"):
            candidates.append(base / p["license_file"])
        extra = repo / "third_party" / "licenses" / p["name"] / p["version"]
        if extra.is_dir():
            candidates.extend(extra.iterdir())
        texts = {}
        for path in sorted(set(candidates)):
            if not path.is_file() or not (path.name.lower().startswith(
                    ("license", "copying", "copyright", "notice"))
                    or path == base / str(p.get("license_file"))):
                continue
            raw = path.read_bytes()
            if not raw.strip():
                raise ValueError(f"Empty notice: {p['name']}/{path.name}")
            # Supplements are retained under a separate name, including when
            # their line endings or copyright notices differ from crate copies.
            name = ("upstream-" if path.parent == extra else "") + path.name
            if name in texts and texts[name] != raw:
                raise ValueError(f"Conflicting notice: {p['name']}/{name}")
            texts[name] = raw
        if not texts or not any(len(raw.strip()) > 100 for raw in texts.values()):
            raise ValueError(f"Missing notices: {p['name']} {p['version']}")
        dest = output / p["name"] / p["version"]
        dest.mkdir(parents=True, exist_ok=False)
        receipts = []
        for name, raw in texts.items():
            (dest / name).write_bytes(raw)
            receipts.append({"file": name, "sha256": hashlib.sha256(raw).hexdigest()})
        records.append({"name": p["name"], "version": p["version"],
                        "license": p["license"], "source": p["source"], "notices": receipts})
    output.mkdir(parents=True, exist_ok=True)
    (output / "inventory.json").write_text(json.dumps({
        "schema_version": "melissa.rust-notices/v1", "target": "x86_64-pc-windows-msvc",
        "scope": "CLI normal and build dependency closure; excludes dev-only dependencies",
        "packages": records,
    }, ensure_ascii=False, indent=2), encoding="utf-8")
    print(f"Collected notices for {len(records)} locked dependencies")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    collect(args.repo.resolve(strict=True), args.out.resolve())
