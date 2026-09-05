# Contributing

## Contribution policy

Centaeris is maintained by the GitHub account EchoTrigger. External code and
other works intended for incorporation into the project are currently not
accepted. Issues are welcome for problem reports, natural-language reproduction
steps, necessary redacted logs, feature requests, and high-level suggestions.
Pull requests are limited to collaborators for maintainer development.

## License

Except where a file or notice says otherwise, the original source code and
documentation are licensed under `AGPL-3.0-only`. Third-party materials remain
under their stated licenses. The software license grants no trademark rights.

## Maintainer development

The following guidance is for maintainer development; it does not reopen
external contribution intake.

Discuss changes to a public protocol, persisted schema, extension contract, or
architecture boundary before implementation.

Keep `packages/core` host-agnostic. Public JSON and Host fields use exact
`camelCase`; model-visible tools and parameters use canonical
`lower_snake_case`. Unsupported versions and unknown fields fail rather than
gaining compatibility aliases.

Use the locked toolchains and dependencies described in
[Building](docs/development/Building.md). Add the smallest focused regression
that proves a behavior or contract boundary. Do not add tests that only mirror
the implementation.

Every Rust change runs:

```powershell
cargo test --locked -p centaeris-core query_loop
```

Before merging a change, run the relevant focused gate and the full commands
described in [Testing](docs/development/Testing.md) and the
[release gate](docs/eval/ReleaseGate.md).

## Repository boundary and third-party material

Do not commit credentials, customer data, private deployment configuration,
concrete commercial extensions, ignored test results, generated build output,
or unrelated binary documents. Preserve required third-party license and NOTICE
files.

When maintainers incorporate third-party material, verify its license and
identify the source, exact version or revision, applicable license, local
modifications, and required notices. Keep vendored third-party material
separate from original Centaeris source.

Maintainers remain responsible for AI-assisted work. Review its provenance and
license risk, and do not incorporate material without the necessary rights.

The Centaeris name, logo, and official visual identity are outside the software
license, and the software license grants no trademark rights. Third-party
materials remain under their stated licenses.
