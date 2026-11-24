"""Summarize a Harbor job run with MelissaAgent: verifier pass rate (Wilson 95%
CI), outcome categories and Melissa telemetry. Usage: summarize.py <job_dir>"""
import collections, glob, json, math, os, statistics, sys

job = sys.argv[1]
rows = []
directories = {os.path.dirname(f) for pattern in ("result.json", "melissa-progress.json")
               for f in glob.glob(os.path.join(job, "*", pattern))}
directories.update(os.path.dirname(os.path.dirname(f))
                   for f in glob.glob(os.path.join(job, "*", "agent", "melissa-result.json")))
for d in sorted(directories):
    f = os.path.join(d, "result.json")
    r = json.load(open(f)) if os.path.exists(f) else {}
    m = os.path.join(d, "agent", "melissa-result.json")
    progress = False
    if not os.path.exists(m):
        m = os.path.join(d, "melissa-progress.json")
        progress = True
    tel = json.load(open(m)) if os.path.exists(m) else {}
    rew = ((r.get("verifier_result") or {}).get("rewards") or {}).get("reward")
    exc = (r.get("exception_info") or {}).get("exception_type") or (None if r else "MissingTrialResult")
    rows.append(dict(task=r.get("task_name", os.path.basename(d)), reward=rew, exc=exc, status=tel.get("status", "Interrupted" if tel else None),
                     partial=progress or tel.get("telemetry_complete") is False,
                     requests=tel.get("requests"), tools=tel.get("tool_calls"),
                     tin=tel.get("reported_input_tokens"),
                     tout=tel.get("reported_output_tokens"),
                     cached=tel.get("cached_input_tokens"), ms=tel.get("wall_ms", tel.get("elapsed_ms"))))
n = len(rows)
k = sum(1 for x in rows if x["reward"] == 1)
z = 1.959964
c = (k + z * z / 2) / (n + z * z)
h = z * math.sqrt(k * (n - k) / n + z * z / 4) / (n + z * z) if n else 0
print(f"observed_trials={n} passed={k} rate={k / n if n else 0:.3f} wilson95=[{c - h if n else 0:.3f}, {c + h if n else 0:.3f}]")
print("Observed trials only; incomplete/retried runs are not an official full-dataset score.")
cat = collections.Counter(
    "pass" if x["reward"] == 1 else (f"exception:{x['exc']}" if x["exc"] else f"fail:{x['status']}")
    for x in rows)
print("outcomes:", dict(cat.most_common()))
tel = [x for x in rows if x["requests"] is not None]
if tel:
    s = lambda key: sum(x[key] or 0 for x in tel)
    print(f"with telemetry={len(tel)} partial={sum(x['partial'] for x in tel)} reported_input_lower_bound={s('tin')} cached_lower_bound={s('cached')} reported_output_lower_bound={s('tout')} "
          f"requests={s('requests')} tool_calls={s('tools')}")
    solved = [x for x in tel if x["reward"] == 1 and not x["partial"] and x["ms"] is not None and x["tin"] is not None and x["tout"] is not None]
    if solved:
        med = statistics.median(x["ms"] for x in solved)
        print(f"solved: median wall {med / 1000:.0f}s, mean tokens "
              f"{sum((x['tin'] or 0) + (x['tout'] or 0) for x in solved) / len(solved):.0f}")
print()
for x in sorted(rows, key=lambda x: (x["reward"] != 1, x["task"])):
    print(f"{x['task']:40} {('PASS' if x['reward'] == 1 else 'fail'):4} {x['exc'] or x['status'] or '-':22} "
          f"req={x['requests']} tools={x['tools']} in={x['tin']} out={x['tout']}")
