#!/usr/bin/env python3
"""Paired statistics for `live_edit_tool_ab` JSONL output (stdlib only).

Success: per-arm rate with Wilson 95% CI and exact two-sided McNemar test on
discordant (case, rep) pairs. Cost: paired bootstrap 95% CI of the mean
difference in total tokens, requests and latency (treatment - baseline).
"""
import argparse
import json
import math
import random
import statistics
from collections import defaultdict


def wilson(k, n, z=1.959964):
    if n == 0:
        return (math.nan, math.nan)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (c - h, c + h)


def mcnemar_exact(b, c):
    n = b + c
    if n == 0:
        return 1.0
    tail = sum(math.comb(n, i) for i in range(0, min(b, c) + 1)) / 2**n
    return min(1.0, 2 * tail)


def bootstrap_mean_ci(diffs, iters, seed):
    if not diffs:
        return (math.nan, math.nan)
    rng = random.Random(seed)
    means = sorted(
        statistics.fmean(rng.choices(diffs, k=len(diffs))) for _ in range(iters)
    )
    return (means[int(0.025 * iters)], means[int(0.975 * iters) - 1])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("jsonl")
    ap.add_argument("--baseline", default="fs_patch")
    ap.add_argument("--treatment", default="fs_replace")
    ap.add_argument("--iters", type=int, default=10000)
    ap.add_argument("--seed", type=int, default=12345)
    a = ap.parse_args()

    runs = [json.loads(l) for l in open(a.jsonl, encoding="utf-8") if l.strip()]
    by_arm = defaultdict(list)
    pairs = defaultdict(dict)
    for r in runs:
        r["tokens"] = r["input_tokens"] + r["output_tokens"]
        by_arm[r["arm"]].append(r)
        pairs[(r["model"], r["case"], r["rep"])][r["arm"]] = r

    out = {"runs": len(runs), "arms": {}}
    for arm, rs in sorted(by_arm.items()):
        k = sum(r["success"] for r in rs)
        lo, hi = wilson(k, len(rs))
        ok = [r for r in rs if r["success"]]
        out["arms"][arm] = {
            "n": len(rs),
            "pass": k,
            "pass_rate": round(k / len(rs), 4),
            "pass_rate_ci95": [round(lo, 4), round(hi, 4)],
            "mean_tokens": round(statistics.fmean(r["tokens"] for r in rs)),
            "tokens_per_solved": round(sum(r["tokens"] for r in rs) / k) if k else None,
            "mean_requests": round(statistics.fmean(r["requests"] for r in rs), 2),
            "mean_tool_calls": round(statistics.fmean(r["tool_calls"] for r in rs), 2),
            "median_elapsed_ms": round(statistics.median(r["elapsed_ms"] for r in rs)),
            "statuses": dict(sorted(
                (s, sum(r["status"] == s for r in rs)) for s in {r["status"] for r in rs}
            )),
            "failed_cases": sorted({r["case"] for r in rs if not r["success"]}),
            "solved_mean_tokens": round(statistics.fmean(r["tokens"] for r in ok)) if ok else None,
        }

    full = [p for p in pairs.values() if a.baseline in p and a.treatment in p]
    b = sum(p[a.treatment]["success"] and not p[a.baseline]["success"] for p in full)
    c = sum(p[a.baseline]["success"] and not p[a.treatment]["success"] for p in full)
    paired = {
        "pairs": len(full),
        "treatment_only_success": b,
        "baseline_only_success": c,
        "mcnemar_exact_p": round(mcnemar_exact(b, c), 4),
    }
    for metric in ("tokens", "requests", "elapsed_ms"):
        diffs = [p[a.treatment][metric] - p[a.baseline][metric] for p in full]
        lo, hi = bootstrap_mean_ci(diffs, a.iters, a.seed)
        base = statistics.fmean(p[a.baseline][metric] for p in full) if full else math.nan
        paired[f"{metric}_mean_diff"] = round(statistics.fmean(diffs), 2) if diffs else None
        paired[f"{metric}_mean_diff_ci95"] = [round(lo, 2), round(hi, 2)]
        paired[f"{metric}_relative_change"] = (
            round(statistics.fmean(diffs) / base, 4) if diffs and base else None
        )
    out["paired"] = {"baseline": a.baseline, "treatment": a.treatment, **paired}
    print(json.dumps(out, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
