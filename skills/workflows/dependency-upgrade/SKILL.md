---
name: dependency-upgrade
description: Upgrade a dependency using documentation for the exact version and verify its compatibility and supply chain.
metadata:
  version: "1.0.0"
---

# dependency-upgrade

Probe the actual installed version and inspect locked dependency state. Resolve approved version-specific documentation and record source URLs and content digests. Make a scoped manifest/lock change. Run relevant build, regression, dependency and secret checks with available tools. Preserve breaking changes and unavailable security/license checks in the result.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
