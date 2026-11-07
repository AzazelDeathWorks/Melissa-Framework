---
name: safe-refactor
description: Refactor existing code while preserving documented behavior and reviewing the affected dependency graph.
metadata:
  version: "1.1.0"
---

# safe-refactor

Inspect semantic definitions, references and dependent modules using available language tools. Record baseline tests and behavior before changing structure. Apply scoped, reviewable changes and inspect the resulting diff. Re-run affected type/build/test checks. Separate intentional behavior changes from structure-only edits and ask for scope only when genuinely necessary.

Use `code_impact_map` for source-bound import/package/test candidates, then
`code_semantic` to confirm definitions, true callers and type relations. An
unresolved alias, unsupported language or candidate graph is not proof that no
other callers exist. For monorepos inspect the captured project configuration.

Use `code_structural_search` before `code_structural_propose`. Select exact
visible files and a grammar; use bound `$NAME`/`$$$NAMES` captures. Review every
match, especially similarly named APIs, and preserve comments, strings and
unrelated changes. Structural matching does not establish symbol identity.
Overlapping matches must be narrowed deliberately, not reordered silently.

Read all advertised result pages before claiming the set is complete. Preserve
source hashes and receipt references. STALE evidence is historical: re-observe
source, regenerate the proposal, and review the new diff. Never reuse an old
preimage or patch offset. Apply through the advertised `fs_patch` schema only;
the host retains task/profile/path and exact-byte checks. A helper result grants
no new write paths. Check syntax/type/build/tests on the final source, including
actual caller files and affected package checks. Record unsupported checks.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
