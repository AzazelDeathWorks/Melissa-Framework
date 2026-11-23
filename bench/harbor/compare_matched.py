"""Offline paired comparison of pre-registered, content-bound Harbor trials.
No provider calls, retries or result selection. Missing/error trials remain visible.
Controls are operator declarations, not independent runtime attestation.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import random
import re


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def sha(value):
    return hashlib.sha256(value).hexdigest()


def pairs(values):
    result = {}
    for key, value in values:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def load(path):
    with Path(path).open("rb") as source:
        raw = source.read(8 * 1024 * 1024 + 1)
    if len(raw) > 8 * 1024 * 1024:
        raise ValueError("JSON artifact exceeds size bound")
    return raw, json.loads(raw, object_pairs_hook=pairs, parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite JSON")))


def keys(value, expected):
    if not isinstance(value, dict) or set(value) != set(expected.split()):
        raise ValueError("manifest object has missing/unknown fields")


def integer(value, lower, upper):
    if type(value) is not int or not lower <= value <= upper:
        raise ValueError("manifest integer bounds")


def digest(value, length=64):
    if not isinstance(value, str) or not re.fullmatch("[0-9a-f]{" + str(length) + "}", value):
        raise ValueError("invalid content digest")


def text(value):
    if not isinstance(value, str) or not value or len(value) > 2048 or any(ord(c) < 32 for c in value):
        raise ValueError("invalid manifest text")


def validate(manifest):
    keys(manifest, "schema_version controls agent trials")
    if manifest["schema_version"] != "melissa.matched-harbor/v1":
        raise ValueError("unsupported manifest version")
    controls = manifest["controls"]
    keys(controls, "dataset tasks repeats model context_tokens max_output_tokens max_turns deadline_policy_sha256 retry_policy_sha256 authority_sha256 environment_sha256 concurrency seed")
    keys(controls["dataset"], "name version source_url git_commit")
    for key in ("name", "version", "source_url"):
        text(controls["dataset"][key])
    digest(controls["dataset"]["git_commit"], 40)
    keys(controls["model"], "id endpoint_sha256 protocol")
    text(controls["model"]["id"])
    text(controls["model"]["protocol"])
    digest(controls["model"]["endpoint_sha256"])
    for key in ("deadline_policy_sha256", "retry_policy_sha256", "authority_sha256", "environment_sha256"):
        digest(controls[key])
    for key in ("context_tokens", "max_output_tokens", "max_turns", "concurrency", "repeats"):
        integer(controls[key], 1, {"repeats": 20, "concurrency": 64, "max_turns": 10000}.get(key, 10_000_000))
    integer(controls["seed"], 0, 2**32 - 1)
    if controls["max_output_tokens"] > controls["context_tokens"]:
        raise ValueError("output reserve exceeds capacity")
    tasks = controls["tasks"]
    if not isinstance(tasks, list) or not tasks or len(tasks) * controls["repeats"] > 2048:
        raise ValueError("task/pair count bounds")
    for task in tasks:
        if not isinstance(task, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", task):
            raise ValueError("invalid task identifier")
    if len(set(tasks)) != len(tasks):
        raise ValueError("duplicate scheduled task")
    keys(manifest["agent"], "id harbor_agent revision_sha256")
    for key in ("id", "harbor_agent"):
        text(manifest["agent"][key])
    digest(manifest["agent"]["revision_sha256"])
    if not isinstance(manifest["trials"], list) or len(manifest["trials"]) > len(tasks) * controls["repeats"]:
        raise ValueError("trial count exceeds schedule")


def observations(path, trial_ids=None, content_hashes=None):
    _, manifest = load(path)
    validate(manifest)
    controls = manifest["controls"]
    root = Path(path).resolve().parent
    results, seen = {}, set()
    trial_ids = set() if trial_ids is None else trial_ids
    content_hashes = set() if content_hashes is None else content_hashes
    for entry in manifest["trials"]:
        keys(entry, "task repeat result result_sha256")
        integer(entry["repeat"], 0, controls["repeats"] - 1)
        key = entry["task"], entry["repeat"]
        if entry["task"] not in controls["tasks"] or key in results:
            raise ValueError("duplicate or unscheduled trial; retries need their own pre-registered study")
        text(entry["result"])
        digest(entry["result_sha256"])
        result_path = (root / entry["result"]).resolve()
        if Path(entry["result"]).is_absolute() or not result_path.is_relative_to(root) or result_path in seen:
            raise ValueError("trial artifact escapes manifest root or is reused")
        seen.add(result_path)
        raw, trial = load(result_path)
        if sha(raw) != entry["result_sha256"]:
            raise ValueError("trial content digest mismatch")
        trial_id = trial.get("id")
        text(trial_id)
        if trial_id in trial_ids or entry["result_sha256"] in content_hashes:
            raise ValueError("reused Harbor trial ID or copied result content")
        trial_ids.add(trial_id)
        content_hashes.add(entry["result_sha256"])
        config = trial.get("config") or {}
        task = config.get("task") or {}
        agent = config.get("agent") or {}
        if (trial.get("task_name") != key[0] or task.get("git_commit_id") != controls["dataset"]["git_commit"] or task.get("git_url") != controls["dataset"]["source_url"] or agent.get("model_name") != controls["model"]["id"] or (agent.get("import_path") or agent.get("name")) != manifest["agent"]["harbor_agent"]):
            raise ValueError("trial task/model/agent/dataset identity differs from registration")
        reward = ((trial.get("verifier_result") or {}).get("rewards") or {}).get("reward")
        state = "error" if trial.get("exception_info") is not None else "missing_verifier"
        if trial.get("exception_info") is None and type(reward) in (int, float) and reward in (0, 1):
            state = "pass" if reward == 1 else "fail"
        results[key] = state
    return manifest, results


def mcnemar(left_only, right_only):
    n = left_only + right_only
    return min(1.0, 2 * sum(math.comb(n, i) for i in range(min(left_only, right_only) + 1)) / 2**n) if n else 1.0


def compare(left_path, right_path, bootstrap=10000):
    integer(bootstrap, 100, 20000)
    trial_ids, content_hashes = set(), set()
    left, a = observations(left_path, trial_ids, content_hashes)
    right, b = observations(right_path, trial_ids, content_hashes)
    if canonical(left["controls"]) != canonical(right["controls"]):
        raise ValueError("unmatched task/model/limit/retry/deadline/concurrency/environment controls")
    if left["agent"]["id"] == right["agent"]["id"]:
        raise ValueError("comparison needs two distinct agent identities")
    controls = left["controls"]
    tasks, repeats = controls["tasks"], controls["repeats"]
    schedule = [(task, repeat) for task in tasks for repeat in range(repeats)]
    rows = [{"task": task, "repeat": repeat, "left": a.get((task, repeat), "missing_trial"), "right": b.get((task, repeat), "missing_trial")} for task, repeat in schedule]
    counts = {side: dict(Counter(row[side] for row in rows)) for side in ("left", "right")}
    complete = all(row[side] in ("pass", "fail") for row in rows for side in ("left", "right"))
    stats = None
    if complete:
        left_only = sum(row["left"] == "pass" and row["right"] == "fail" for row in rows)
        right_only = sum(row["left"] == "fail" and row["right"] == "pass" for row in rows)
        differences = [sum((a[(task, r)] == "pass") - (b[(task, r)] == "pass") for r in range(repeats)) / repeats for task in tasks]
        rng = random.Random(controls["seed"])
        samples = sorted(sum(rng.choice(differences) for _ in tasks) / len(tasks) for _ in range(bootstrap))
        stats = {"pass_rate_delta_left_minus_right": sum(differences) / len(tasks),
                 "task_cluster_bootstrap_95_percent_interval": [samples[int(.025 * (bootstrap - 1))], samples[int(.975 * (bootstrap - 1))]],
                 "bootstrap_resamples": bootstrap,
                 "exact_mcnemar_p": mcnemar(left_only, right_only) if repeats == 1 else None,
                 "mcnemar_scope": "single-repeat paired binary outcomes only; correlated repeats use task-cluster bootstrap",
                 "left_only_passes": left_only, "right_only_passes": right_only}
    return {"schema_version": "melissa.matched-harbor-comparison/v1", "controls_sha256": sha(canonical(controls)),
            "agents": {"left": left["agent"], "right": right["agent"]},
            "scheduled_pairs": len(schedule), "counts": counts, "all_trials_verified": complete,
            "statistics": stats, "trials": rows,
            "limitations": "Not an official benchmark score. No results are selected/retried or silently excluded. Incomplete/error studies get no significance/ranking claim. Task/model/agent/git identity and artifact contents are checked; endpoint, limits, retry/authority/environment policy digests are operator declarations, not runtime attestation. Degenerate bootstrap intervals do not prove generalization; tasks are a fixed suite."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("left", type=Path)
    parser.add_argument("right", type=Path)
    args = parser.parse_args()
    print(json.dumps(compare(args.left, args.right), indent=2))


if __name__ == "__main__":
    main()
