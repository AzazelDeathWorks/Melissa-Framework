---
name: performance-triage
description: Investigate a performance regression with a reproducible workload and comparable measurements.
metadata:
  version: "1.0.0"
---

# performance-triage

Define the workload, observed symptom and measurement method before optimization. Record machine/worker capacity, source, runtime and dependency versions. Reproduce the baseline and isolate a plausible bottleneck using available profilers. Change one meaningful cause and remeasure under comparable conditions. Keep memory, latency, throughput and cost measurements separate.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
