# Contributing

## Before changing code

Open an issue before changing a public protocol, persisted schema, extension
contract, or architecture boundary. Small fixes that preserve those contracts
can proceed directly.

Keep `packages/core` host-agnostic. Public JSON and Host fields use exact
`camelCase`; model-visible tools and parameters use canonical
`lower_snake_case`. Unsupported versions and unknown fields fail rather than
gaining compatibility aliases.

## Development

Use the locked toolchains and dependencies described in
[Building](docs/development/Building.md). Add the smallest focused regression
that proves a behavior or contract boundary. Do not add tests that only mirror
the implementation.

Every Rust change runs:

```powershell
cargo test --locked -p centaeris-core query_loop
```

Before submitting a change, run the relevant focused gate and the full commands
described in [Testing](docs/development/Testing.md) and the
[release gate](docs/eval/ReleaseGate.md).

## Repository boundary

Do not commit credentials, customer data, private deployment configuration,
concrete commercial extensions, ignored test results, generated build output,
or unrelated binary documents. Preserve required third-party license and NOTICE
files.

## Contribution licensing

Except where a file or notice says otherwise, the original source code and
documentation in this repository are licensed under `AGPL-3.0-only`.

## Third-party material

Do not submit code, documentation, media, model files, datasets, fonts, or other
material that you do not own unless the maintainer has approved the inclusion
and its license in advance. Identify the source, exact version or revision,
applicable license, local modifications, and required notices. Keep vendored
third-party material separate from original Centaeris source.

AI-assisted Contributions remain the submitter's responsibility. Review their
provenance and license risk, and do not submit output that reproduces material
you are not entitled to submit under the repository license.

The Centaeris name, logo, and official visual identity are outside the software
license. Do not submit changes to them unless the project steward explicitly
requests that work under separate terms.
