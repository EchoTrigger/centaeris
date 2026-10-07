# Contributing

## Temporary contribution policy

Centaeris Workspace is currently maintained by the GitHub account EchoTrigger.
Until an operating company is established and a formal contributor license agreement
(CLA) is published, external code and other works intended for incorporation
into the project are not accepted.

Issues are welcome for bug reports, reproduction steps described in natural
language, necessary redacted logs, feature requests, and high-level design
suggestions. Remove credentials, personal information, and confidential data
before posting.

Please do not submit patches, source code, tests or reproduction programs,
documentation drafts, artwork, or other works intended for incorporation into
the project through pull requests, issues, comments, or other channels.
Maintainers will not substantively review or incorporate such unsolicited
submissions and may close them. Maintainers will independently investigate,
design, and implement solutions based on problem descriptions.

Pull request creation is restricted to collaborators for maintainer development.
Collaborator access does not exempt external works from this policy.

## Future contributions and commercial licensing

Centaeris plans to support ongoing maintenance through commercial products,
services, and licensing. External works are planned to be accepted after an
operating company is established and a formal CLA is published.

The intended arrangement is for contributors to retain copyright and grant the
operating company non-exclusive rights to reproduce, modify, prepare derivative
works, display, distribute, and sublicense their contributions, together with
necessary patent rights. Sublicensing may include commercial or proprietary
licenses. The intended arrangement also includes continued availability of
accepted contributions in Centaeris Workspace's public AGPL source version. The
precise scope of these rights and the public-source availability commitment will be
published before contributions reopen and will require contributors' explicit
agreement. Current submissions do not constitute acceptance of a future CLA.

This policy governs contribution intake only. It does not change Centaeris
Workspace's existing `AGPL-3.0-only` license or restrict anyone's rights to use,
modify, or distribute the software under that license.

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
pwsh -File scripts/ci.ps1
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
