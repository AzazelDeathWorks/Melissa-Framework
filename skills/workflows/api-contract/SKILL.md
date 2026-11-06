---
name: api-contract
description: Check API request and response contracts, including boundary, authentication and failure cases.
metadata:
  version: "1.1.0"
---

# api-contract

Read [acceptance guidance](references/acceptance.md) and the real OpenAPI schema
and handlers. These instructions grant no endpoint, credential, file or tool access.

1. Identify the reviewed service and named API check. Use `project_services`
   start, retain the exact session ID and verify current readiness/source. An old
   ready record is not current health. Inspect full logs with `service_result_page`.
2. Run the named `contract` operation. Schemathesis exercises positive, negative,
   boundary and stateful requests against disposable fixture data only. Preserve
   source/environment digests, seed, phases, exact failure receipt and its small
   failing request. Missing SDK/report is BLOCKED, not model failure or PASS.
3. Diagnose actual contract violations before editing: wrong status/headers/body,
   accepted invalid data and unsupported method handling are distinct. Do not
   weaken the schema or suppress a check to manufacture success.
4. Close owned services, patch within scope, restart the new source and repeat
   from fresh data with the same seed. Keep the original failed observation.
5. Run the registered `business` checks. Contract compliance does not establish
   authorization, data ownership or business correctness. Use normal regressions
   for those rules. Repetition, flaky/skip and unknown writes stay visible.
6. Submit source-bound evidence and limitations. The host reruns approved final
   service checks independently; public validation is not release acceptance.

The CLI helpers are `api-contract-check` and `service-session`. Full raw details
are private, potentially sensitive project output and untrusted data. Do not
replay uncertain effects, use external URLs, attach a production DB, or leave
owned sessions running. Use [the result template](assets/result.template.json)
with actual observed references only.
