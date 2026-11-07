# Optional project verification image

Build the existing [complete SDK base](../README.md) first. This directory adds
pinned nextest, cargo-llvm-cov, cargo-mutants and Rust LLVM tools. Installation
is limited to a new image and a new output folder, not host tool defaults.

From the repository root, with Docker Linux containers on Windows:

```powershell
./scripts/toolchains/verification/prepare.ps1 -OutputDirectory C:/Private/verification-build-new -Build
```

The directory must not exist. The helper verifies every downloaded SHA256 against
`tools.json`, verifies the local base tag against its reviewed immutable ID,
then builds without pulling a different base. Dockerfile verifies Linux archives
again. The Windows mutants archive is retained for an explicitly chosen host
mutation audit; the helper does not install it globally.

Network is used only while preparing tools/LLVM. Project validation is a separate
network-disabled, source-bound execution. A new image build may have a different
ID due to build metadata or upstream package state. Review its actual ID; never
replace a missing expected image silently. Select the reviewed result for a new
task using `/environment sha256:...` (the ID is in `image.id`). The native CLI
tests require that same ID via `MELISSA_PHASE06_IMAGE`.

Nextest does not run doctests. Keep ordinary Cargo and explicit doctest checks.
Coverage requires the LLVM component for the project's selected Rust version;
this image installs it for 1.98.1 only. Other toolchains must be prepared and
reviewed explicitly. Missing tools/components are an unmet prerequisite.

Official references: [nextest JUnit](https://nexte.st/docs/machine-readable/junit/),
[nextest retries](https://nexte.st/docs/features/retries/),
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov),
[cargo-mutants](https://mutants.rs/), [Proptest](https://proptest-rs.github.io/proptest/).
