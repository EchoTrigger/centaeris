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

## Contribution provenance

Contributors certify the [Developer Certificate of Origin 1.1](../../DCO)
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
Sign-off does not change the contribution intake policy above.

## Maintainer development

The following guidance is for maintainer development; it does not reopen
external contribution intake.

Discuss changes to a public protocol, persisted schema, Plugin or Skill
contract, or architecture boundary before implementation. Follow the repository
rules in `AGENTS.md` and the verification in `docs/eval/ReleaseGate.md`.

Use the locked toolchains and dependencies in the repository. Run the smallest
focused regression that proves the change, followed by the relevant portions of
the local gate:

```powershell
pwsh -File scripts/workspace/ci.ps1
```

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
