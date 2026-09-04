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

Except where a file or notice says otherwise, Contributions to this repository
are made to Material licensed under `AGPL-3.0-only`.

This repository uses the
[Developer's Certificate of Origin 1.1](DCO). By contributing, you certify the
DCO for every commit with a `Signed-off-by` trailer containing your name and
email address:

```text
Signed-off-by: Your Name <your.email@example.com>
```

Git can add this trailer automatically:

```powershell
git commit --signoff
```

The sign-off is a certification under the DCO, not a GPG signature, copyright
assignment, CLA, or separate patent grant. It confirms that you have the right
to submit the Contribution under the license indicated by the repository. The
DCO does not give the project steward a separate right to place an external
Contribution in a proprietary edition; that would require a separate written
license from its copyright holder.

## Third-party material

Do not submit code, documentation, media, model files, datasets, fonts, or other
material that you do not own unless the maintainer has approved the inclusion
and its license in advance. Identify the source, exact version or revision,
applicable license, local modifications, and required notices. Keep vendored
third-party material separate from original Centaeris source.

AI-assisted Contributions remain the submitter's responsibility. Review their
provenance and license risk, and do not submit output that reproduces material
you are not entitled to submit and certify under the DCO.

The Centaeris name, logo, and official visual identity are outside the software
license. Do not submit changes to them unless the project steward explicitly
requests that work under separate terms.
