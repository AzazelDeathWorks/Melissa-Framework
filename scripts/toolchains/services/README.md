# Optional service/API toolchain

Start with the reviewed [web/SQL SDK](../README.md). This layer installs
Schemathesis 4.33.1 into its own `/opt/melissa-contracts` environment, using pinned
CPython 3.13 wheels whose SHA256 hashes were checked against official PyPI.
Existing SDK Python packages are preserved.

```powershell
./scripts/toolchains/services/prepare.ps1 -OutputDirectory C:/Private/services-build-new -Build
```

The destination must be new. Preparation downloads only the assets in
`tools.json`, verifies their hashes and uses `--require-hashes --no-index` during
image construction. The base must already exist by its exact immutable ID. A
new local build alias is added only if absent; an existing different alias is
never overwritten. No global host package defaults change. Review `image.id`
and select that immutable ID for a new task using `/environment`.

Image preparation may access the network. Service workloads use none-network
isolation; they never receive daemon access or host mounts. A rebuilt image can
have a different ID due to metadata. Do not silently substitute a missing SDK.

Compose is the pinned Docker Desktop plugin on the host, not an image package.
The native executor creates a finite one-container service capsule and verifies
all normal isolation settings. It does not run arbitrary user Compose files.
Developer Testcontainers is separate; see [service guidance](../../../docs/SERVICES_TR.md).

Official references: [Compose services](https://docs.docker.com/reference/compose-file/services/),
[Compose application model](https://docs.docker.com/compose/intro/compose-application-model/),
[Schemathesis data generation](https://github.com/schemathesis/schemathesis/blob/master/docs/explanations/data-generation.md),
[Testcontainers Python](https://testcontainers-python.readthedocs.io/en/latest/).
