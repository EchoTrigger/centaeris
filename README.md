<p align="center">
  <img src="assets/centaeris-mark.svg" width="112" alt="Centaeris logo">
</p>

# Centaeris

English | [简体中文](README.zh-CN.md)

Centaeris is a host-agnostic agent runtime framework written in Rust. Desktop,
terminal, and hosted products use the same runtime contracts for sessions, model
requests, tools, events, persistence, and durable continuation.

This repository contains the public runtime, local and hosted products, their
user interfaces, and separate first-party System Skill bundles. Private plugins,
credentials, customer data and private deployment state remain external.

## Appearance

Choose System, Light, or Dark in Settings > Appearance. The preference persists on this device. Replies and process headings use 14px; process details, code, and tables use 13px. Process headings and content share one gray in each theme.

## Features

- One canonical runtime for model, tool, session, and continuation semantics.
- Local Electron and terminal hosts over a strict host protocol.
- Typed tool contracts, terminal outcomes, safety decisions, and observable runtime events.
- SQLite storage adapter and MCP adapter behind runtime-owned contracts.
- Package and skill loading, with three public built-in System Skills and no
  bundled commercial package content.

## Repository layout

| Package | Responsibility |
| --- | --- |
| [`packages/core`](packages/core/README.md) | Runtime semantics and contracts |
| [`packages/agent`](packages/agent/README.md) | Rust Agent SDK and local execution adapters |
| [`packages/sdk`](packages/sdk/README.md) | Typed TypeScript Session client |
| [`packages/desktop/src/services`](packages/desktop/src/services/README.md) | Shared Desktop and CLI product services |
| [`packages/sqlite`](packages/sqlite/README.md) | SQLite RuntimeStore adapter |
| [`packages/mcp`](packages/mcp/README.md) | MCP adapter |
| [`packages/model-catalog`](packages/model-catalog/README.md) | Rust model catalog |
| [`packages/model-http`](packages/model-http/Cargo.toml) | Shared model HTTP and streaming transport |
| [`packages/desktop`](packages/desktop/README.md) | Electron host |
| [`packages/cli`](packages/cli/README.md) | Interactive, Print, JSON and RPC entry points |
| [`packages/ui`](packages/ui/README.md) | Shared frontend foundations |
| [`packages/visuals`](packages/visuals/package.json) | Shared visualization rendering |
| [`packages/workspace/api`](packages/workspace/api/README.md) | Hosted control plane |
| [`packages/workspace/runtime`](packages/workspace/runtime/README.md) | Hosted Runtime adapter |
| [`packages/workspace/executor`](packages/workspace/executor/README.md) | Hosted execution agent and sandbox tools |
| [`packages/workspace/web`](packages/workspace/web/README.md) | Hosted Web interface |
| [`packages/workspace/documents`](packages/workspace/documents/README.md) | Material processing service |

`system-skills/` holds local-product built-in System Skills; `skills/system/`
holds hosted-product built-in System Skills. Neither is a workspace package.

See [source and product boundaries](docs/development/SourceUnification.md) and
[hosted product documentation](docs/workspace/README.md). Root dependency files
and Compose entries serve independently selectable products.

## Current release scope

Local product distributions currently build and verify Windows x64 artifacts:

- an assisted, per-user Desktop installer named `Centaeris-Desktop-0.1.0-windows-x64-setup.exe`;
- a portable Desktop archive named `centaeris-desktop-windows-x64.zip`;
- a standalone TUI archive named `centaeris-windows-x64.zip`;
- corresponding source named `centaeris-source.zip`.

The first release is planned as **v0.1.0 Windows Preview**, a GitHub pre-release.
A local candidate build does not mean a public download already exists.
See [Windows setup](docs/getting-started/Windows.md) for installation and model configuration.
There is no supported macOS/Linux download. Source code may compile elsewhere,
but that is not a release-platform claim.

## Build from source

Requirements: the Rust toolchain in `rust-toolchain.toml`, Node.js 22.21.0,
pnpm 12.10.1 and Python 3.12.

```sh
python3 scripts/ci.py Source --frontend-tests
```

Use `python` on Windows. See [portable source development](docs/development/Building.md)
for the common gates and [Windows setup](docs/getting-started/Windows.md) for the complete build and
first-run paths. The runtime and tests do not require an installed Plugin. The
Desktop and TUI distributions include the public System Skills in this tree;
other extensions are separate versioned assets.

## Interface language

The Desktop UI currently ships in English. Its `react-i18next` foundation and
English resources live in `packages/desktop/src/renderer/i18n.ts` and `packages/desktop/src/renderer/locales/`.
No incomplete language option is exposed. User content, model responses, commands,
and protocol identifiers retain their original text.

## Documentation

Start with [the documentation index](docs/README.md). It separates user setup,
runtime concepts, public contracts, development, and release verification.

## Contributing

Contributions are welcome. Small documentation fixes can go directly to a pull
request. For bug fixes, link a reproducible issue; discuss features, substantial
UI changes, and architecture changes with a maintainer before implementation.
Contribution commits use DCO sign-off, and pull requests pass CI and review
before merging.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the contribution policy and license.

## License

Copyright (C) 2026 EchoTrigger. See [COPYRIGHT](COPYRIGHT) for the ownership
notice.

Except where a file or notice says otherwise, the original source code and
documentation in this repository are licensed under the
[GNU Affero General Public License v3.0 only](LICENSE).

The Core, model catalog, MCP, and SQLite foundation packages are licensed under
Apache-2.0. See [LICENSING.md](LICENSING.md) for directory exceptions and
third-party notices. Complete Desktop, TUI, and Workspace products remain AGPL.

The Centaeris name, logo, and official visual identity are not licensed under
the AGPL, and the software license grants no trademark rights. Third-party
materials remain under their stated licenses.

Bundled font licenses are indexed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
