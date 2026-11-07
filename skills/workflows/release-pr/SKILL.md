---
name: release-pr
description: Prepare a reviewable change and release proposal from actual diff and verification evidence.
metadata:
  version: "1.0.0"
---

# release-pr

Inspect the complete final diff, unresolved defects and source-bound test evidence. Check configured dependency, license and secret gates with available tools. Write a focused PR/release draft describing behavior and limitations. Follow the user authorization for remote publication. Verify returned PR/commit/CI identifiers instead of treating a local draft as a published result.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
