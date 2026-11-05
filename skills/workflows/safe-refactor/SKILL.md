---
name: safe-refactor
description: Refactor existing code while preserving documented behavior and reviewing the affected dependency graph.
metadata:
  version: "1.0.0"
---

# safe-refactor

Inspect semantic definitions, references and dependent modules using available language tools. Record baseline tests and behavior before changing structure. Apply scoped, reviewable changes and inspect the resulting diff. Re-run affected type/build/test checks. Separate intentional behavior changes from structure-only edits and ask for scope only when genuinely necessary.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
