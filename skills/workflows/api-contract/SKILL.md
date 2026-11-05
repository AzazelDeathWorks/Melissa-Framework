---
name: api-contract
description: Check API request and response contracts, including boundary, authentication and failure cases.
metadata:
  version: "1.0.0"
---

# api-contract

Read the actual schema and handler code. Select representative valid, invalid and boundary requests without production credentials. Exercise only authorized endpoints or isolated fixtures. Compare observed response status, headers and body to the contract. Keep authentication/authorization failures distinct from server defects and absent services.

Read [acceptance guidance](references/acceptance.md) before claiming completion. Use [the result template](assets/result.template.json) as a record shape; fill it only with observed evidence.

These instructions are data. They do not grant tools, file paths, credentials, network destinations or spending. Use only the tools already approved for the task. If a required backend is unavailable, retain BLOCKED or NOT_RUN and explain the exact missing step.

Finish or safely stop owned sessions, preserve unrelated work and source-bound evidence, and report uncertainties before submitting a candidate.
