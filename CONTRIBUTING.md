# Contributing

## Contribution workflow

Contributions are welcome. Search existing issues and pull requests before
starting, and keep each pull request focused on one problem.

| Change | Before implementation |
| --- | --- |
| Typo, broken link, or small documentation correction | Open a pull request directly; a separate issue is optional. |
| Bug fix | Link an existing issue or open one with a reproduction. Add a focused regression test that fails before the fix and passes afterward. |
| New feature or substantial UI change | Open an issue describing the user need, proposed behavior, and scope. Wait for maintainer agreement. |
| Runtime semantics, public protocols, persisted schemas, or architecture boundaries | Discuss the contract, compatibility or migration needs, and validation before implementation. |
| Large refactor, dependency replacement, or performance work | Present the concrete problem or measurements and agree on scope before implementation. |

A maintainer's `accepted` label means the direction and scope are ready for
implementation. `good first issue` identifies suitable introductory tasks;
use it together with `accepted`. Comment on the issue before starting to avoid
duplicating work. Agreement on direction is not a promise to merge a particular
implementation.

Issue reports may include a minimal reproduction or test. Remove credentials,
personal information, and confidential data from examples and logs, and submit
only material you have the right to share.

## Pull requests and review

1. Create a focused branch in your fork and link the relevant issue in the PR.
2. Describe the problem, resulting behavior, and validation performed. Use a
   draft PR when the implementation is still in progress.
3. Follow the test policy below and sign off each contribution commit under
   the DCO. Documentation-only corrections need appropriate document checks,
   not unrelated runtime test suites.
4. Address review feedback and resolve discussions. Maintainers review the
   behavior, scope, tests, and contribution provenance before merging.
5. Rebase onto the current `main` when needed; do not merge `main` into the
   contribution branch. Coordinate any rewrite of shared work with its authors.

Merges require the `Required gates` CI check and resolved review discussions.
Maintainers use squash or rebase to keep `main` linear. They preserve contributor
attribution and applicable `Signed-off-by` trailers when squashing, and ask the
contributor to correct a missing certification rather than signing for them.
Sign-off is checked during review; no separate automated DCO gate is required.
Maintainer-authored changes also go through pull requests and CI. While there is
one maintainer, an additional approving review is not a required merge condition.

## License

Except where a file or notice says otherwise, the original source code and
documentation are licensed under `AGPL-3.0-only`. Third-party materials remain
under their stated licenses. The software license grants no trademark rights.

## Contribution provenance

Contributors certify the [Developer Certificate of Origin 1.1](DCO)
by adding a `Signed-off-by` trailer to each new contribution commit:

```text
Signed-off-by: Your Name <your.email@example.com>
```

Use `git commit --signoff` after reviewing the DCO and confirming that you can
make its certification. The sign-off records your certification of the
contribution's origin and your right to submit it under the applicable project
license. It does not transfer copyright or grant separate relicensing rights;
the project's original source code and documentation remain `AGPL-3.0-only`.

The DCO also records that contributions and sign-off information are public,
maintained indefinitely, and may be redistributed under the applicable license.

## Development and validation

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

Follow the [test-quality policy](docs/development/Testing.md#test-quality).
Do not turn removed implementation names or source spellings into permanent
blacklists. Preserve behavior and contract coverage when replacing brittle
tests; static architecture checks require a documented invariant.

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
