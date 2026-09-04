# Release process

A release is a separate operation from finishing code or creating a local Git
checkpoint.

## 1. Freeze source

- identify the exact candidate revision;
- require a clean, self-contained tree;
- verify package and protocol versions;
- check that no credentials, customer data, private extension content, test
  results, or unsupported artifacts are tracked.

## 2. Verify

Run `scripts/ci.ps1` from a clean clone. Review the structural requirements in
`docs/eval/ReleaseGate.md`. Build only the Windows x64 targets currently in the
release matrix.

## 3. Inspect artifacts

Verify the Desktop directory and TUI archive contain the exact Runtime built
from the candidate source, the project license, third-party notices, dependency
licenses, and consumed package manifests. Smoke the packaged applications.

Checksums or digests are generated only where an installer, manifest validator,
container runtime, or release platform consumes them.

## 4. Publish

Tagging, pushing, changing a remote default branch, creating a GitHub Release,
and uploading assets require explicit authorization. Release notes must state
the verified platform and must not include private repository history,
deployment details, or unsupported performance claims.
