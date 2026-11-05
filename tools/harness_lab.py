#!/usr/bin/env python3
"""Freeze and run paired Melissa CLI experiments from operator-owned fixtures."""

import argparse
import hashlib
import json
import os
import platform
import random
import re
import shutil
import stat
import subprocess
import sys
import time
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from pathlib import Path


PLAN = "melissa.lab-plan/v1"
FROZEN = "melissa.lab-frozen/v1"
COHORTS = {"small", "repository", "long_horizon", "large_rust"}
MIN_FULL_COHORTS = {"small": 50, "repository": 50, "long_horizon": 30, "large_rust": 20}
AXES = {"context_state", "compaction", "repository_index_cache", "project_memory",
        "adaptive_replanning", "model_routing", "tool_profile", "runtime_revision"}
IDENTIFIER = re.compile(r"[a-z0-9][a-z0-9_-]{0,47}\Z")
IMAGE_ID = re.compile(r"sha256:[0-9a-f]{64}\Z")
MIN_FREE_BYTES = 4 * 1024**3


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path):
    with Path(path).open(encoding="utf-8-sig") as handle:
        return json.load(handle)


def write(path, value):
    with Path(path).open("x", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, ensure_ascii=False, sort_keys=True)
        handle.write("\n")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"),
                      ensure_ascii=False).encode("utf-8")


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def local_entry(path):
    attrs = path.lstat()
    reparse = getattr(attrs, "st_file_attributes", 0) & 0x400
    require(not stat.S_ISLNK(attrs.st_mode) and not reparse, "linked lab input")
    return attrs


def file_hash(path):
    require(stat.S_ISREG(local_entry(path).st_mode), "lab input must be a regular file")
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def tree(root):
    require(stat.S_ISDIR(local_entry(root).st_mode), "lab source must be a directory")
    entries = {}
    for folder, dirs, files in os.walk(root, followlinks=False):
        for name in dirs:
            local_entry(Path(folder) / name)
        for name in files:
            item = Path(folder) / name
            relative = item.relative_to(root).as_posix()
            require(relative.split("/")[0].casefold() != ".git", "supply an exported source tree")
            entries[relative] = file_hash(item)
            require(len(entries) <= 20_000, "lab source exceeds the file count limit")
    require(entries, "empty lab source")
    return entries


def absolute(parent, name):
    path = Path(os.path.abspath(parent / name))
    for entry in (path, *path.parents):
        local_entry(entry)
    return path


def inputs(parent, spec, fields):
    return {field: absolute(parent, spec[field]) for field in fields}


def axis_settings(axis, arm_names, settings, hashes):
    require(arm_names == ("baseline", "treatment"), "arms must be baseline and treatment")
    baseline, treatment = (settings[name] for name in arm_names)
    require(all(isinstance(value, dict) for value in settings.values()), "invalid model settings")
    if axis == "runtime_revision":
        require(baseline == treatment and hashes[0] != hashes[1], "runtime comparison mismatch")
        require(not baseline.get("model_pool"), "runtime comparison must pin one model")
        return
    require(hashes[0] == hashes[1], "feature comparisons must use the same binary")
    differences = {key for key in baseline.keys() | treatment.keys()
                   if baseline.get(key) != treatment.get(key)}
    field = {"model_routing": "model_pool", "tool_profile": "tool_profile"}.get(axis, "lab_variant")
    require(differences == {field}, "experiment changes settings outside its declared axis")
    if axis == "model_routing":
        require(not baseline.get("model_pool") and treatment.get("model_pool"),
                "routing trial needs a single-model baseline and an alternative pool")
        return
    require(not baseline.get("model_pool") and not treatment.get("model_pool"),
            "same-model experiment requires an empty model pool")
    if field == "lab_variant":
        require(baseline.get(field, "production") == axis + "_off"
                and treatment.get(field, "production") == "production",
                "baseline must disable only the declared feature")
    if field == "tool_profile":
        profiles = [value.get(field, "precision") for value in (baseline, treatment)]
        require(all(value in {"precision", "terminal", "hybrid"} for value in profiles)
                and profiles[0] != profiles[1], "effective tool profiles must differ")


def freeze(plan_path):
    spec = read(plan_path)
    parent = plan_path.parent
    require(spec.get("schema_version") == PLAN, "unexpected lab plan schema")
    axis = spec["axis"]
    require(axis in AXES, "unknown experiment axis")
    image = spec["worker_image"]
    require(IMAGE_ID.fullmatch(image), "worker environment needs an immutable Docker image ID")
    seeds = spec["order_seeds"]
    require(isinstance(seeds, list) and 2 <= len(seeds) <= 32
            and all(type(seed) is int for seed in seeds)
            and len(set(seeds)) == len(seeds), "provide 2–32 distinct ordering seeds")
    arms = {}
    settings = {}
    for name in ("baseline", "treatment"):
        paths = inputs(parent, spec["arms"][name], ("binary", "settings"))
        arms[name] = {key: {"path": str(value), "sha256": file_hash(value)}
                      for key, value in paths.items()}
        settings[name] = read(paths["settings"])
    require(set(spec["arms"]) == set(arms), "unexpected arm")
    axis_settings(axis, tuple(arms), settings, [arm["binary"]["sha256"] for arm in arms.values()])
    cases = {}
    for case in spec["cases"]:
        key, category, goal = case["id"], case["cohort"], case["goal"]
        require(IDENTIFIER.fullmatch(key) and key not in cases, "invalid or duplicate case ID")
        require(category in COHORTS and 0 < len(goal.strip()) <= 16_384, "invalid cohort or goal")
        paths = inputs(parent, case, ("source", "negative", "recipe"))
        recipe = read(paths["recipe"])
        cost, wall = recipe["max_cost_micro_usd"], recipe["wall_time_ms"]
        require(type(cost) is int and cost > 0 and type(wall) is int and wall > 0,
                "invalid operator task limits")
        require(paths["source"] != paths["negative"], "negative control must be separate")
        require("domain" not in recipe, "this runner supports the isolated program SDK recipes")
        source, negative = tree(paths["source"]), tree(paths["negative"])
        require(source != negative, "negative control matches the initial source")
        cases[key] = {
            "cohort": category, "goal": goal, "paths": {k: str(v) for k, v in paths.items()},
            "source_sha256": digest(source), "negative_sha256": digest(negative),
            "recipe_sha256": file_hash(paths["recipe"]), "max_cost_micro_usd": cost,
        }
    require(0 < len(cases) <= 500, "supply 1–500 cases")
    campaign_kind = spec.get("campaign_kind", "pilot")
    require(campaign_kind in {"pilot", "full"}, "unknown campaign kind")
    cohorts = Counter(case["cohort"] for case in cases.values())
    if campaign_kind == "full":
        require(all(cohorts[key] >= value for key, value in MIN_FULL_COHORTS.items()),
                "full campaign does not meet the minimum coding cohorts")
    cap = spec["max_campaign_micro_usd"]
    ceiling = sum(case["max_cost_micro_usd"] for case in cases.values()) * len(arms) * len(seeds)
    require(type(cap) is int and cap >= max(case["max_cost_micro_usd"] for case in cases.values()),
            "campaign cap cannot admit even the largest individual trial")
    plan = {"schema_version": FROZEN, "axis": axis, "worker_image": image, "cases": cases,
            "arms": arms, "order_seeds": seeds, "campaign_ceiling_micro_usd": ceiling,
            "max_campaign_micro_usd": cap, "environment": environment(),
            "campaign_kind": campaign_kind, "cohorts": dict(cohorts)}
    return {"identity_sha256": digest(plan), "plan": plan}


def environment():
    return {"system": platform.system(), "version": platform.version(),
            "machine": platform.machine(), "python": platform.python_version()}


def verify_environment(plan):
    require(environment() == plan["environment"], "host environment changed after freeze")
    client = ["docker"]
    if os.name == "nt":
        client.extend(["--host", "npipe:////./pipe/dockerDesktopLinuxEngine"])
    env = subprocess.run([*client, "image", "inspect", plan["worker_image"], "--format", "{{.Id}}"],
                         capture_output=True, check=True, text=True, timeout=30)
    require(env.stdout.strip() == plan["worker_image"], "pinned SDK image not found")
    server = subprocess.run([*client, "version", "--format", "{{.Server.Version}}"],
                            capture_output=True, check=True, text=True, timeout=30)
    require(bool(server.stdout.strip()), "Docker server version unavailable")
    return {"image": plan["worker_image"], "docker_server_version": server.stdout.strip()}


def windows_process_metrics(child):
    """Read counters for this CLI process, excluding the worker and model."""
    import ctypes as c
    from ctypes import wintypes as w

    class Memory(c.Structure):
        _fields_ = [("cb", w.DWORD), ("page_faults", w.DWORD)] + [
            (name, c.c_size_t) for name in ("peak_working_set", "working_set", "peak_paged_pool",
                                          "paged_pool", "peak_nonpaged_pool", "nonpaged_pool",
                                          "pagefile", "peak_pagefile")]

    class Io(c.Structure):
        _fields_ = [(name, c.c_uint64) for name in ("reads", "writes", "others", "read_bytes",
                                                   "written_bytes", "other_bytes")]

    kernel, psapi = c.WinDLL("kernel32"), c.WinDLL("psapi")
    kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    kernel.OpenProcess.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    kernel.GetProcessIoCounters.argtypes = [w.HANDLE, c.POINTER(Io)]
    psapi.GetProcessMemoryInfo.argtypes = [w.HANDLE, c.POINTER(Memory), w.DWORD]
    handle = kernel.OpenProcess(0x0400 | 0x0010, False, child.pid)
    try:
        child.wait()
        memory, io = Memory(), Io()
        memory.cb = c.sizeof(memory)
        rss = memory.peak_working_set if handle and psapi.GetProcessMemoryInfo(
            handle, c.byref(memory), memory.cb) else None
        have_io = handle and kernel.GetProcessIoCounters(handle, c.byref(io))
        return {"peak_host_rss_bytes": rss,
                "host_io_read_transfer_bytes": io.read_bytes if have_io else None,
                "host_io_write_transfer_bytes": io.written_bytes if have_io else None,
                "scope": "Melissa CLI process; I/O transfers are not physical disk measurements"}
    finally:
        if handle:
            kernel.CloseHandle(handle)


def invoke(binary, directory, stage, *args):
    started = time.monotonic_ns()
    process = None
    with (directory / f"{stage}.stdout.json").open("xb") as stdout, (
            directory / f"{stage}.stderr.json").open("xb") as stderr:
        command = [str(binary), "--home", str(directory / "home"), "--json", *args]
        if stage == "run" and os.name == "nt":
            with subprocess.Popen(command, cwd=directory, stdout=stdout, stderr=stderr) as child:
                process = windows_process_metrics(child)
        else:
            child = subprocess.run(command, cwd=directory, stdout=stdout, stderr=stderr,
                                   timeout=None if stage == "run" else 120)
    elapsed = (time.monotonic_ns() - started) // 1_000_000
    write(directory / f"{stage}.exit.json",
          {"code": child.returncode, "elapsed_ms": elapsed, "host_process": process})
    value = read(directory / f"{stage}.stdout.json") if child.returncode == 0 else None
    return child.returncode, value, elapsed


def recorded_error_kind(directory):
    try:
        value = read(directory / "run.stderr.json")
    except (OSError, ValueError):
        return None
    kind = value.get("kind") if isinstance(value, dict) else None
    return kind if kind in {"usage", "denied", "conflict", "unavailable",
                            "requires_reconciliation", "store_integrity",
                            "repository_boundary", "io"} else None


def run_trial(case, arm, directory, image):
    directory.mkdir(exist_ok=True)
    require(not any(directory.iterdir()), "trial directory already contains evidence")
    binary, settings = (Path(arm[name]["path"]) for name in ("binary", "settings"))
    paths = {name: Path(path) for name, path in case["paths"].items()}
    for path in (binary, settings, *paths.values()):
        absolute(path.parent, path.name)
    require(file_hash(binary) == arm["binary"]["sha256"]
            and file_hash(settings) == arm["settings"]["sha256"]
            and file_hash(paths["recipe"]) == case["recipe_sha256"], "frozen input changed")
    for key in ("source", "negative"):
        require(digest(tree(paths[key])) == case[f"{key}_sha256"], "frozen source changed")
        shutil.copytree(paths[key], directory / key, symlinks=True)
        require(digest(tree(directory / key)) == case[f"{key}_sha256"], "source copy differs")
    recipe = read(paths["recipe"])
    recipe["negative_workspace"] = str(directory / "negative")
    write(directory / "recipe.json", recipe)
    commands = (
        ("doctor", ("doctor",)),
        ("init", ("init",)),
        ("config", ("config", "import", str(settings))),
        ("new", ("task", "new", "--workspace", str(directory / "source"), "--goal",
                 case["goal"], "--recipe", str(directory / "recipe.json"))),
    )
    for stage, args in commands:
        code, value, _ = invoke(binary, directory, stage, *args)
        if code or stage == "doctor" and (
                value.get("isolated_SDK_image_present") is not True
                or value.get("isolated_SDK_image_id") != image):
            return {"outcome": "blocked", "stage": stage, "exit_code": code}
    task = value["task_id"]
    code, show, _ = invoke(binary, directory, "show", "task", "show", task)
    if code or show.get("approval", {}).get("pending") is not True:
        return {"outcome": "blocked", "stage": "show", "exit_code": code}
    approval = show["approval"]
    code, _, _ = invoke(binary, directory, "approve", "task", "approve", task, "--request",
                        approval["request_id"], "--digest", approval["digest"])
    if code:
        return {"outcome": "blocked", "stage": "approve", "exit_code": code}
    code, _, elapsed = invoke(binary, directory, "run", "task", "run", task)
    metrics_exit, report, _ = invoke(binary, directory, "metrics", "task", "metrics", task)
    if metrics_exit:
        return {"outcome": "inconclusive", "stage": "metrics", "exit_code": metrics_exit,
                "run_exit_code": code, "wall_ms": elapsed, "task_id": task}
    require(report.get("schema_version") == "melissa.task-metrics/v1"
            and report.get("task_id") == task, "unexpected task metrics")
    metrics = report["metrics"]
    accepted = metrics["verified_acceptance"]
    error_kind = recorded_error_kind(directory) if code else None
    if metrics["model_unknown_outcomes"] or error_kind in {
            "unavailable", "requires_reconciliation", "store_integrity", "repository_boundary", "io",
            "model_protocol", "provider_unavailable", "model_execution"}:
        outcome = "inconclusive"
    elif not metrics["model_dispatch_attempts"]:
        outcome = "blocked"
    elif accepted is True and code == 0:
        outcome = "passed"
    elif accepted is False:
        outcome = "failed"
    else:
        outcome = "inconclusive"
    return {"outcome": outcome, "stage": "run", "exit_code": code, "task_id": task,
            "wall_ms": elapsed, "metrics": metrics,
            "error_kind": error_kind,
            "host_process": read(directory / "run.exit.json")["host_process"]}


def summarize(trials, expected):
    loads = sorted({row.get("parallelism", 1) for row in trials})
    per_arm = {}
    for arm in ("baseline", "treatment"):
        rows = [trial for trial in trials if trial["arm"] == arm]
        results = Counter(row["outcome"] for row in rows)
        successes = results["passed"]
        complete = len(rows) == expected and successes + results["failed"] == expected
        metrics = [row["metrics"] for row in rows if "metrics" in row]
        known = {}
        for name in ("input_tokens", "output_tokens", "accounted_cost_micro_usd"):
            samples = [metric[name] for metric in metrics]
            total = sum(sample["known_total"] or 0 for sample in samples)
            known[name] = total / successes if complete and successes and all(
                sample["missing_samples"] == 0 and sample["known_total"] is not None
                for sample in samples) else None
        per_arm[arm] = {"outcomes": dict(results), "all_trials_settled": complete,
                        "verified_success_rate": successes / expected if complete else None,
                        "wall_ms_per_verified_success": sum(row["wall_ms"] for row in rows)
                        / successes if complete and successes and len(loads) == 1 else None,
                        "known_usage_per_verified_success": known}
    paired = {}
    for row in trials:
        key = (row["case_id"], row["order_seed"])
        pair = paired.setdefault(key, {})
        require(row["arm"] not in pair, "duplicate trial arm")
        pair[row["arm"]] = row["outcome"]
    transitions = Counter(
        (pair["baseline"], pair["treatment"]) for pair in paired.values()
        if set(pair) == {"baseline", "treatment"})
    patterns = Counter((row["arm"], row["outcome"], row.get("stage"), row.get("exit_code"),
                        row.get("error_kind"))
                       for row in trials if row["outcome"] != "passed")
    return {"arms": per_arm, "failure_patterns": [
        {"arm": arm, "outcome": outcome, "stage": stage, "exit_code": code,
         "error_kind": error_kind, "count": count}
        for (arm, outcome, stage, code, error_kind), count in patterns.items()],
        "paired_outcomes": [
            {"baseline": left, "treatment": right, "count": count}
            for (left, right), count in transitions.items()
        ],
        "treatment_quality_regressions": transitions[("passed", "failed")],
        "treatment_quality_wins": transitions[("failed", "passed")],
        "parallelism_levels": loads,
        "coverage_complete": len(trials) == expected * 2,
        "status": "complete" if all(row["all_trials_settled"] for row in per_arm.values())
        else "incomplete", "production_change": False}


@contextmanager
def campaign_lock(directory):
    with (directory / "campaign.lock").open("a+b") as handle:
        if handle.tell() == 0:
            handle.write(b"1")
            handle.flush()
        handle.seek(0)
        if os.name == "nt":
            import msvcrt
            msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def trial_charge(row):
    if row["outcome"] == "blocked" and "metrics" not in row:
        return 0
    value = row.get("metrics", {}).get("conservative_charged_micro_usd")
    require(type(value) is int and value >= 0, "trial charge cannot be accounted")
    return value


def independent_trial_stopped(arm, directory, row):
    task = row.get("task_id")
    if not isinstance(task, str) or not re.fullmatch(r"task_[0-9a-f]{32}", task):
        return False
    binary = Path(arm["binary"]["path"])
    require(file_hash(binary) == arm["binary"]["sha256"], "frozen binary changed")
    command = [str(binary), "--home", str(directory / "home"), "--json", "task", "progress", task]
    try:
        result = subprocess.run(command, capture_output=True, timeout=30, check=True)
        progress = json.loads(result.stdout)
    except (subprocess.SubprocessError, ValueError, OSError):
        return False
    return (progress.get("owner_alive") is False
            and progress.get("owned_workers_confirmed_stopped") is True
            and progress.get("model_budget", {}).get("charged_cost_micro_usd") == trial_charge(row))


def may_continue(plan, directory, row, allow_unsettled):
    if row["outcome"] in {"passed", "failed"}:
        return True
    return (allow_unsettled and row["stage"] == "run"
            and row["outcome"] in {"inconclusive", "blocked"} and "metrics" in row
            and independent_trial_stopped(plan["arms"][row["arm"]], directory, row))


def campaign_cap(output, header):
    cap = header["authorized_cap_micro_usd"]
    records = sorted(output.glob("budget-extension-*.json"))
    for index, path in enumerate(records, 1):
        local_entry(path)
        require(path.name == f"budget-extension-{index:04d}.json", "campaign budget history gap")
        entry = read(path)
        prior = entry["prior_summary"]
        require(re.fullmatch(r"prior-summary-\d{4}\.json", prior), "invalid prior budget report")
        local_entry(output / prior)
        new_cap = entry["new_cap_micro_usd"]
        require(entry["schema_version"] == "melissa.lab-budget-extension/v1"
                and entry["campaign_identity_sha256"] == header["identity_sha256"]
                and entry["previous_cap_micro_usd"] == cap and type(new_cap) is int and new_cap > cap
                and entry["prior_summary_sha256"] == file_hash(output / prior),
                "campaign budget evidence differs")
        cap = new_cap
    return cap, len(records)


def run(frozen_path, output, allowed_cost, resume=False, jobs=1, allow_unsettled=False,
        raise_cap=False):
    require(type(jobs) is int and 1 <= jobs <= 16, "use 1–16 independent trials per wave")
    require(not raise_cap or resume, "a budget extension requires a prior campaign")
    frozen = read(frozen_path)
    plan = frozen["plan"]
    require(plan["schema_version"] == FROZEN and frozen["identity_sha256"] == digest(plan),
            "frozen campaign identity mismatch")
    expected_cap = sum(case["max_cost_micro_usd"] for case in plan["cases"].values())
    expected_cap *= len(plan["arms"]) * len(plan["order_seeds"])
    cap = plan["max_campaign_micro_usd"]
    require(0 < expected_cap == plan["campaign_ceiling_micro_usd"]
            and 0 < cap <= allowed_cost, "live campaign cap not authorized")
    worker = verify_environment(plan)
    for case in plan["cases"].values():
        for key in ("source", "negative"):
            require(not output.is_relative_to(Path(case["paths"][key])),
                    "experiment output belongs outside fixture trees")
    header = {"identity_sha256": frozen["identity_sha256"], "worker": worker, "axis": plan["axis"],
              "campaign_kind": plan["campaign_kind"], "cohorts": plan["cohorts"], "execution": "live",
              "authorized_cap_micro_usd": cap, "trial_layout": "short-v1"}
    if os.name == "nt":
        require(len(str(output / "t00000-t" / "home")) <= 120,
                "choose a shorter experiment output path for the Windows task store")
    if not resume:
        output.mkdir()
        write(output / "campaign.json", header)
    else:
        absolute(output.parent, output.name)
        require(read(output / "campaign.json") == header, "campaign identity or environment changed")
    with campaign_lock(output):
        cap, extensions = campaign_cap(output, header)
        require(cap <= allowed_cost, "effective campaign cap not authorized")
        summary = output / "summary.json"
        if raise_cap:
            require(summary.is_file() and allowed_cost > cap,
                    "budget extension needs a stopped campaign and a higher explicit cap")
        if summary.exists():
            saved = read(summary)
            reason = saved.get("stop_reason")
            require(resume and (reason == "host_disk_space"
                    or raise_cap and reason == "campaign_budget"
                    or allow_unsettled and reason in {"unsettled_trial", "blocked_trial"}),
                    "campaign already has a final report or needs a separate review")
            require(shutil.disk_usage(output).free >= MIN_FREE_BYTES, "free disk space before resuming")
            resume_rows(plan, output, allow_unsettled)
            index = 1
            while (output / f"prior-summary-{index:04d}.json").exists():
                index += 1
            prior = output / f"prior-summary-{index:04d}.json"
            summary.rename(prior)
            if raise_cap:
                write(output / f"budget-extension-{extensions + 1:04d}.json",
                      {"schema_version": "melissa.lab-budget-extension/v1",
                       "campaign_identity_sha256": header["identity_sha256"],
                       "prior_summary": prior.name, "prior_summary_sha256": file_hash(prior),
                       "previous_cap_micro_usd": cap, "new_cap_micro_usd": allowed_cost})
                cap = allowed_cost
        return run_schedule(plan, output, resume, jobs, allow_unsettled, cap)


def schedule(plan, output, short_layout=True):
    order = []
    # Each repetition keeps cases adjacent and randomizes the within-pair arm order.
    for seed in plan["order_seeds"]:
        rng = random.Random(seed)
        pairs = sorted(plan["cases"])
        rng.shuffle(pairs)
        for case in pairs:
            arms = ["baseline", "treatment"]
            rng.shuffle(arms)
            order.extend((case, seed, arm) for arm in arms)
    width = max(4, len(str(len(order))))
    return [(output / (f"t{index:0{width}d}-{arm[0]}" if short_layout
                       else f"{index:0{width}d}-{case}-{arm}"), case, seed, arm)
            for index, (case, seed, arm) in enumerate(order, 1)]


def recorded_trials(plan, output, short_layout=True):
    order = schedule(plan, output, short_layout)
    present = sorted(item for item in output.iterdir() if item.is_dir())
    require(present == [entry[0] for entry in order[:len(present)]],
            "prior trial directory order differs")
    rows, incomplete = [], []
    for directory, case, seed, arm in order[:len(present)]:
        absolute(directory.parent, directory.name)
        record = directory / "trial.json"
        if not record.is_file():
            incomplete.append(directory.name)
            continue
        local_entry(record)
        row = read(record)
        require((row["case_id"], row["order_seed"], row["arm"]) == (case, seed, arm),
                "prior trial identity differs")
        rows.append(row)
    return rows, incomplete


def review(frozen_path, output):
    frozen = read(frozen_path)
    plan = frozen["plan"]
    header = read(output / "campaign.json")
    require(plan["schema_version"] == FROZEN
            and frozen["identity_sha256"] == digest(plan) == header["identity_sha256"],
            "review campaign identity mismatch")
    rows, incomplete = recorded_trials(plan, output, header.get("trial_layout") == "short-v1")
    report = summarize(rows, len(plan["cases"]) * len(plan["order_seeds"]))
    report.update({"identity_sha256": frozen["identity_sha256"], "recorded_trials": len(rows),
                   "unsettled_directories": incomplete, "cohorts": plan["cohorts"],
                   "campaign_kind": plan["campaign_kind"], "release_authorized": False})
    return report


def resume_rows(plan, output, allow_unsettled):
    rows, incomplete = recorded_trials(plan, output)
    require(not incomplete, "prior trial has missing evidence; automatic replay is blocked")
    for row, (directory, _, _, _) in zip(rows, schedule(plan, output)):
        require(may_continue(plan, directory, row, allow_unsettled),
                "prior trial lacks a safe continuation disposition")
        trial_charge(row)
    return rows


def execute_trial(plan, entry, parallelism):
    directory, case, seed, arm = entry
    try:
        result = run_trial(plan["cases"][case], plan["arms"][arm], directory, plan["worker_image"])
    except (KeyError, TypeError, ValueError, OSError, subprocess.SubprocessError) as error:
        result = {"outcome": "inconclusive", "stage": "runner", "error_kind": type(error).__name__}
    row = {"case_id": case, "order_seed": seed, "arm": arm, "parallelism": parallelism, **result}
    write(directory / "trial.json", row)
    return row


def run_schedule(plan, output, resume, jobs=1, allow_unsettled=False, effective_cap=None):
    trials = []
    count = len(plan["cases"]) * len(plan["order_seeds"])
    cap = plan["max_campaign_micro_usd"] if effective_cap is None else effective_cap
    order = schedule(plan, output)
    spent, stop = 0, None
    if resume:
        trials = resume_rows(plan, output, allow_unsettled)
        spent = sum(trial_charge(row) for row in trials)
    try:
        while len(trials) < len(order):
            disk_slots = shutil.disk_usage(output).free // MIN_FREE_BYTES
            if disk_slots == 0:
                stop = "host_disk_space"
                break
            wave, reserved = [], 0
            for entry in order[len(trials):len(trials) + min(jobs, disk_slots)]:
                cost = plan["cases"][entry[1]]["max_cost_micro_usd"]
                if spent + reserved + cost > cap:
                    break
                wave.append(entry)
                reserved += cost
            if not wave:
                stop = "campaign_budget"
                break
            for directory, _, _, _ in wave:
                directory.mkdir()
            if len(wave) == 1:
                rows = [execute_trial(plan, wave[0], 1)]
            else:
                with ThreadPoolExecutor(max_workers=len(wave)) as executor:
                    futures = [executor.submit(execute_trial, plan, entry, len(wave)) for entry in wave]
                    rows = [future.result() for future in futures]
            trials.extend(rows)
            for row, (directory, case, _, _) in zip(rows, wave):
                if "metrics" in row or row["outcome"] == "blocked":
                    spent += trial_charge(row)
                else:
                    spent += plan["cases"][case]["max_cost_micro_usd"]
                if not may_continue(plan, directory, row, allow_unsettled):
                    stop = stop or ("blocked_trial" if row["outcome"] == "blocked" else "unsettled_trial")
            if spent > cap:
                stop = "reported_budget_overrun"
            if stop:
                break
    finally:
        summary = summarize(trials, count)
        settled = sum(trial_charge(row) for row in trials if row["outcome"] in {"passed", "failed"})
        summary.update({"settled_campaign_charge_micro_usd": settled,
                        "campaign_accounted_or_reserved_micro_usd": spent, "stop_reason": stop,
                        "effective_campaign_cap_micro_usd": cap,
                        "planned_trials": len(order), "recorded_trials": len(trials)})
        write(output / "summary.json", summary)
    return 0 if stop is None and len(trials) == len(order) and all(
        row["outcome"] in {"passed", "failed"} for row in trials) else 2


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    freeze_command = sub.add_parser("freeze")
    freeze_command.add_argument("plan", type=Path)
    freeze_command.add_argument("output", type=Path)
    run_command = sub.add_parser("run")
    run_command.add_argument("frozen", type=Path)
    run_command.add_argument("output", type=Path)
    run_command.add_argument("--allow-live-micro-usd", required=True, type=int)
    run_command.add_argument("--resume", action="store_true")
    run_command.add_argument("--jobs", type=int, default=1)
    run_command.add_argument("--continue-after-inconclusive", action="store_true")
    run_command.add_argument("--raise-campaign-cap", action="store_true")
    review_command = sub.add_parser("review")
    review_command.add_argument("frozen", type=Path)
    review_command.add_argument("results", type=Path)
    review_command.add_argument("output", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.action == "freeze":
            write(args.output, freeze(args.plan.absolute()))
            return 0
        if args.action == "review":
            write(args.output, review(args.frozen, args.results.absolute()))
            return 0
        return run(args.frozen, args.output.absolute(), args.allow_live_micro_usd, args.resume,
                   args.jobs, args.continue_after_inconclusive, args.raise_campaign_cap)
    except (KeyError, TypeError, ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Harness Lab: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
