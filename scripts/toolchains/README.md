# Worker images

Use Docker with Linux containers. Build the base first from the repository root:

```text
docker build -t melissa-code05-base:20261019 scripts/toolchains/code05
docker image inspect melissa-code05-base:20261019 --format "{{.Id}}"
```

Expected base image ID on linux/amd64:
`sha256:b8b264f0d1c889fe9510197a0b16a85d06685b2d112bb4115f7b33072ce46ec4`.
The FROM-only recipe pins the upstream manifest. Tests check immutable image
IDs; a matching local tag alone does not satisfy that check.

The extended toolchains are built in dependency order:

```text
docker build -t melissa-code06-complete:20261020 scripts/toolchains/code06
docker build -t melissa-code10-web:20261021 scripts/toolchains/code10
```

Builds may need network access to install the pinned dependencies. Worker task
execution remains separate and network-restricted. A successful build does not
establish byte-for-byte reproducibility of the extended images: package-manager
metadata and image timestamps can differ. Verify the resulting ID against each
test's registered image, or review a new immutable registration. Never silently
replace a test's expected ID with whichever image happens to exist locally.

Native Windows process tests also require their helper binary in the same Cargo
target directory and build profile as the tests:

```text
cargo build --locked -p melissa-windows-executor --bins
```

Docker, Windows-native, loopback protocol, and live-provider acceptance are
different test scopes. Missing prerequisites must be reported, not counted as
passing tests.
