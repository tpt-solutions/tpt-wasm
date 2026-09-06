# Dependency Tracking

All Cargo dependencies must satisfy the license allowlist in `.cargo/deny.toml`.

## Allowed licenses

- MIT
- MIT OR Apache-2.0
- Apache-2.0
- Apache-2.0 WITH LLVM-exception
- BSD-2-Clause
- BSD-3-Clause
- ISC
- Unicode-DFS-2016
- CC0-1.0
- Zlib

## Prohibited licenses

- GPL-2.0, GPL-3.0 (any version)
- LGPL (any version)
- AGPL-3.0
- BUSL-1.1

## Process

1. Before adding a dependency, check its license with `cargo deny check licenses`.
2. Update this file with the dependency's purpose.
3. Record any transitive dependencies of concern in `provenance.md`.

## Automation

CI runs `cargo deny check` on every push. A failing check blocks merge.

To audit the full dependency tree:

```bash
cargo deny list
cargo metadata --format-version 1 | jq '.packages[] | {name, version, license}'
```

## SBOM

A Software Bill of Materials (SBOM) will be generated at each release using
`cargo cyclonedx` or `cargo sbom`.
