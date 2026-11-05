---
name: database-migration
description: Rehearse a database migration on separate test data and verify upgrade behavior and data consistency.
metadata:
  version: "1.0.0"
---

# database-migration

Identify old schema, target schema and invariants. Use an explicitly separate authorized database and preserve production/user data. Rehearse clean setup and upgrade with representative fixture data using available tools. Verify constraints, indexes and transformed records. Exercise rollback or recovery only if the approved migration supports it.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
