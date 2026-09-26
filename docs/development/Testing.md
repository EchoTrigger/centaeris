# Testing

## Focused Core gate

Every Rust change runs at least:

```powershell
cargo test --locked -p centaeris-core query_loop
```

Add the smallest focused test owned by the changed contract or adapter. Core
tests runtime semantics; adapter tests cover storage, transport, and platform
integration.

## Full local gate

```sh
python3 scripts/ci.py Source --frontend-tests
```

On Windows use `python`. Windows distribution acceptance additionally runs
`pwsh -File scripts/ci.ps1 Release`.

The gate checks formatting, the Rust workspace, Clippy with warnings denied,
focused Core and SQLite integration, all Rust tests and Desktop/UI source checks.
Packaging and packaged-application smoke checks remain in the Windows release stage.

## Test data

Tests must use temporary roots and synthetic credentials. They must not read a
developer's real `~/.centaeris`, production data, private extension content, or
customer files. A test result is evidence for the exact tested source tree; it
is not a source artifact and should not be committed.

## Clean-clone gate

Before release, run the same gate from a clean clone of the exact candidate
revision. A passing dirty working tree does not prove that ignored files,
adjacent repositories, or previously built binaries are unnecessary.
