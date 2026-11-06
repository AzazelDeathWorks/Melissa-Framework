---
name: database-migration
description: Rehearse a database migration on separate test data and verify upgrade behavior and data consistency.
metadata:
  version: "1.1.0"
---

# database-migration

Read [acceptance guidance](references/acceptance.md). Identify the old schema,
target schema and concrete data invariants before changing a migration. These
instructions grant no production connection, credentials, files or commands.

1. Select an approved named `project_services` migration rehearsal. It uses
   separate disposable SQLite/PostgreSQL fixtures, not the app's or user's DB.
   Seed provenance must be synthetic or already anonymized; the declaration is
   not proof that arbitrary input was anonymized automatically.
2. Verify clean setup plus sample data, old-version baseline plus samples,
   upgrade, and actual post-upgrade invariant queries. Compare fresh/upgrade
   schemas. Inspect the real schema/data fingerprints and raw PG logs through
   the typed detail reader; never substitute an expected digest.
3. Test the failure path. A transactional error must demonstrate rolled_back
   and preserved original state. A nontransactional partial change remains FAIL
   with observed partial effects; do not label it safely rolled back.
4. If downgrade scripts are approved, verify actual old schema, data and
   invariants after downgrade. Null downgrade means NOT_SUPPORTED. A configured
   but unexecuted rollback is NOT_RUN; neither is a successful rollback.
5. Close the owned session before scoped edits, then repeat in fresh fixtures.
   Preserve every failing receipt and report which invariant or step changed.
6. Run business/API regressions that depend on the migrated data. Final host
   checks use the reviewed acceptance list, current source/environment and proven
   native cleanup. This is not a full production-data migration guarantee.

`migration-rehearsal` is the operator helper. SQL writes use registered statement
names; read queries use the existing read-only SQL gate. Do not add shell/DB
transaction control inside migration files or claim a backend is available when
it was not run. Physical disposable rehearsal data can be released only within
its owned worker after logical evidence/log capture. User data, old evidence and
other services remain outside this scope.

Use [the result template](assets/result.template.json) with observed evidence.
