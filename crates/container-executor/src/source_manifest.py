def verify_source_manifest(root, program):
    import hashlib
    import os
    import stat

    if program.get("source_manifest_version") != "melissa.worker-source-manifest/v1":
        return False
    try:
        for name, seal in program["sources"].items():
            path = root / name
            if not path.is_relative_to(root) or path.resolve(strict=True) != path:
                return False
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as file:
                before = os.fstat(file.fileno())
                if not stat.S_ISREG(before.st_mode) or before.st_size != seal["bytes"]:
                    return False
                remaining = seal["bytes"]
                digest = hashlib.sha256()
                while remaining:
                    block = file.read(min(remaining, 64 * 1024))
                    if not block:
                        return False
                    digest.update(block)
                    remaining -= len(block)
                after = os.fstat(file.fileno())
                if (file.read(1) or digest.hexdigest() != seal["sha256"]
                        or before.st_mtime_ns != after.st_mtime_ns
                        or before.st_ctime_ns != after.st_ctime_ns
                        or not os.path.samestat(after, path.stat(follow_symlinks=False))):
                    return False
    except (OSError, ValueError, RuntimeError, KeyError, TypeError):
        return False
    return True
