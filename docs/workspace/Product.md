# Centaeris Workspace

English | [简体中文](README.zh-CN.md)

Hosted product for running Centaeris agents with workspace membership, durable
jobs, managed execution, document processing, a Django control plane, and a web
client. The public source is developed under `AGPL-3.0-only`.

The host-agnostic Runtime Framework is an external Rust Git dependency pinned to
one public commit. Cargo and Docker builds use the same locked source without
requiring a sibling checkout. There are no npm or Python cross-repository source
dependencies. See [Core development](docs/development/CoreDependency.md) for
portable setup, pin updates and explicit local co-development.

Superusers install Plugins from validated ZIP files. Extension source repositories
are not included in Workspace image contexts.

## Appearance

Choose **System default**, **Dark theme**, or **Light theme** in **Settings → General → Theme**. System default is the initial choice. Manual choices persist on this device and override system changes. Replies and process headings use 14px; process details, code, and tables use 13px. Process headings and content share one gray in each theme.

## Interface language

Workspace supports English and Simplified Chinese through `react-i18next`.
The default is Simplified Chinese. Change it in **Settings → General → Language**;
the choice is saved in this browser. Sign-in pages also provide a language selector.
Changing the interface language preserves drafts and does not translate user content,
model responses, commands, or protocol identifiers.

Translation resources live in `packages/web/src/locales/`. Run
`npm run test:unit --workspace packages/web` for resource parity and plural checks.
The main browser regression suite explicitly selects English; dedicated language
tests cover the Chinese default, switching, and persistence.

## Develop

```sh
cp .env.example .env
# Fill every blank secret.
uv sync --locked
npm ci
cargo check --workspace --locked
docker compose config --quiet
```

Start the complete local stack:

```sh
docker compose build && docker compose up -d
```

The build command prepares required execution and document-processor images before
starting persistent services. These images are part of normal operation, not
optional development extras. Runtime resolves the configured execution image to
an immutable Docker identity before authorizing an AgentRun.

Run all local gates with `python scripts/ci.py` (Python 3.12). Start documentation at
[docs/README.md](docs/README.md).

Bundled web font copyright, source, and license records are indexed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md); the linked license files are
included in the deployed web artifact.

## Contributing

Issues are welcome for bug reports, natural-language reproduction steps,
redacted logs, feature requests, and high-level design suggestions. External
code, patches, documentation drafts, and other works for incorporation into
the project are temporarily not accepted. Pull requests are limited to
collaborators for maintainer development.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the temporary policy and plans for
future contributions and commercial licensing.

## License

Except where a file or notice says otherwise, this repository's original source
code and documentation are licensed under the
[GNU Affero General Public License v3.0 only](LICENSE).

The Centaeris name, logo, and official visual identity are not licensed under
the AGPL, and the software license grants no trademark rights. Third-party
materials remain under their stated licenses.
