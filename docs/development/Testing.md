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

```powershell
.\scripts\ci.ps1
```

The gate checks formatting, the Rust workspace, Clippy with warnings denied,
focused Core and SQLite integration, all Rust tests, Desktop/UI acceptance, and
the Windows TUI package.

## Test data

Tests must use temporary roots and synthetic credentials. They must not read a
developer's real `~/.centaeris`, production data, private extension content, or
customer files. A test result is evidence for the exact tested source tree; it
is not a source artifact and should not be committed.

## Clean-clone gate

Before release, run the same gate from a clean clone of the exact candidate
revision. A passing dirty working tree does not prove that ignored files,
adjacent repositories, or previously built binaries are unnecessary.
