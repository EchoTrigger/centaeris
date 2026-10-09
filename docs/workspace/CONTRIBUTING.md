# Contributing to hosted products

The repository-wide [contribution guide](../../CONTRIBUTING.md) is authoritative
for issue-first planning, pull requests, review, `AGPL-3.0-only` licensing,
DCO sign-off, and third-party material. Hosted products use the same policy.

## Hosted development and validation

Discuss changes to a public protocol, persisted schema, Plugin or Skill
contract, or architecture boundary before implementation. Follow the public
[test policy](../development/Testing.md) and [release gate](eval/ReleaseGate.md).

Use the locked toolchains and dependencies in the repository. Run the smallest
focused regression that proves the change, followed by the relevant portions of
the local gate:

```powershell
pwsh -File scripts/workspace/ci.ps1
```
