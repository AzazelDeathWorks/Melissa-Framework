"""Summarize a Harbor job run with MelissaAgent: verifier pass rate (Wilson 95%
CI), outcome categories and Melissa telemetry. Usage: summarize.py <job_dir>"""
import collections, glob, json, math, os, sys

job = sys.argv[1]
rows = []
for f in sorted(glob.glob(os.path.join(job, "*", "result.json"))):
    r = json.load(open(f))
    d = os.path.dirname(f)
    m = os.path.join(d, "agent", "melissa-result.json")
    tel = json.load(open(m)) if os.path.exists(m) else {}
    rew = ((r.get("verifier_result") or {}).get("rewards") or {}).get("reward")
    exc = (r.get("exception_info") or {}).get("exception_type")
    rows.append(dict(task=r["task_name"], reward=rew, exc=exc, status=tel.get("status"),
                     requests=tel.get("requests"), tools=tel.get("tool_calls"),
                     tin=tel.get("reported_input_tokens", tel.get("input_tokens")),
                     tout=tel.get("reported_output_tokens", tel.get("output_tokens")),
                     cached=tel.get("cached_input_tokens"), ms=tel.get("wall_ms")))
n = len(rows)
k = sum(1 for x in rows if x["reward"] == 1)
z = 1.959964
c = (k + z * z / 2) / (n + z * z)
h = z * math.sqrt(k * (n - k) / n + z * z / 4) / (n + z * z) if n else 0
print(f"trials={n} passed={k} rate={k / n:.3f} wilson95=[{c - h:.3f}, {c + h:.3f}]")
cat = collections.Counter(
    "pass" if x["reward"] == 1 else (f"exception:{x['exc']}" if x["exc"] else f"fail:{x['status']}")
    for x in rows)
print("outcomes:", dict(cat.most_common()))
tel = [x for x in rows if x["requests"] is not None]
if tel:
    s = lambda key: sum(x[key] or 0 for x in tel)
    print(f"with telemetry={len(tel)} input={s('tin')} cached={s('cached')} output={s('tout')} "
          f"requests={s('requests')} tool_calls={s('tools')}")
    solved = [x for x in tel if x["reward"] == 1]
    if solved:
        med = sorted(x["ms"] for x in solved)[len(solved) // 2]
        print(f"solved: median wall {med / 1000:.0f}s, mean tokens "
              f"{sum((x['tin'] or 0) + (x['tout'] or 0) for x in solved) / len(solved):.0f}")
print()
for x in sorted(rows, key=lambda x: (x["reward"] != 1, x["task"])):
    print(f"{x['task']:40} {('PASS' if x['reward'] == 1 else 'fail'):4} {x['exc'] or x['status'] or '-':22} "
          f"req={x['requests']} tools={x['tools']} in={x['tin']} out={x['tout']}")
