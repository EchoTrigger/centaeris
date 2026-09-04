<p align="center">
  <img src="assets/centaeris-mark.svg" width="112" alt="Centaeris logo">
</p>

# Centaeris

Centaeris is a host-agnostic agent runtime framework written in Rust. Desktop,
terminal, and hosted products use the same runtime contracts for sessions, model
requests, tools, events, persistence, and durable continuation.

This repository contains the public runtime, local hosts, and user interfaces. It does not contain first-party commercial packages, skills, hosted control-plane code, credentials, or customer data.

## Features

- One canonical runtime for model, tool, session, and continuation semantics.
- Local Electron and terminal hosts over a strict host protocol.
- Typed tool contracts, terminal outcomes, safety decisions, and observable runtime events.
- SQLite storage adapter and MCP adapter behind runtime-owned contracts.
- Package and skill loading without bundled package content.

## Repository layout

```text
packages/
  core/             Runtime semantics and contracts
  runtime/          Shared local runtime host
  runtime_sqlite/   SQLite RuntimeStore adapter
  mcp/              MCP adapter
  model-catalog/    Rust model catalog
  desktop/          Electron host
  tui/              Terminal host
  ui/               Shared desktop UI
```

## Current release scope

The repository currently builds and verifies Windows x64 artifacts only:

- a standalone TUI archive named `centaeris-windows-x64.zip`;
- an unpacked Windows Desktop directory containing `Centaeris Desktop.exe`.

There is no supported macOS/Linux download or Windows installer yet. Source code
may compile elsewhere, but that is not a release-platform claim.

## Build from source

Requirements: Rust 1.94.1, Node.js 22.21.0, and npm 10.9.4.

```powershell
cargo test --locked -p centaeris-core query_loop
npm ci
npm run gate --workspace centaeris-ui
```

See [Windows setup](docs/getting-started/Windows.md) for the complete build and
first-run paths. The runtime and tests do not require an installed package or
Skill. Extensions are separate versioned assets.

## Documentation

Start with [the documentation index](docs/README.md). It separates user setup,
runtime concepts, public contracts, development, and release verification.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Copyright (C) 2026 EchoTrigger. See [COPYRIGHT](COPYRIGHT) for the ownership
notice.

Except where a file or notice says otherwise, the original source code and
documentation in this repository are licensed under the
[GNU Affero General Public License v3.0 only](LICENSE).

The Centaeris name, logo, and official visual identity are not licensed under
the AGPL, and the software license grants no trademark rights. Third-party
materials remain under their stated licenses.

Code contributions use the
[Developer's Certificate of Origin 1.1](DCO). Contributors certify the DCO by
adding a `Signed-off-by` trailer to each commit; see
[CONTRIBUTING.md](CONTRIBUTING.md). The DCO does not transfer copyright or grant
the project steward a separate right to relicense an external Contribution.

Bundled font licenses are indexed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
