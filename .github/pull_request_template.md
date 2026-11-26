## Problem and result

Describe the concrete problem and resulting behavior. Link the issue and any
agreed design. Small documentation corrections do not require a separate issue.

For contract changes, name the affected invariant from `docs/development/Invariants.md`
and link its authoritative contract. Omit this detail for unrelated changes.

## Validation

List the checks performed and their results. For a bug fix, describe the
regression test that fails before the fix and passes afterward. Explain any
checks not run or material limitations.
For an affected invariant, identify the check that protects it and its observed result.

## Contribution checklist

- [ ] The scope follows CONTRIBUTING.md; substantial changes have maintainer agreement.
- [ ] I have read the DCO and signed off each contribution commit (`git commit -s`).
- [ ] Third-party material, if any, has its source and required license notices recorded.
- [ ] Relevant documentation and behavioral tests are updated, or this is a documentation-only change.

<!-- Use a draft PR for work in progress. Maintainers check provenance, CI,
review discussions, and contributor attribution before squash/rebase merging. -->
