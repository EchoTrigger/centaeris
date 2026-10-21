# Centaeris documentation

This is the documentation index for the public Runtime Framework. These pages
describe supported behavior and current boundaries. They do not preserve old
implementation plans or compatibility aliases.

## Get started

- [Windows setup](getting-started/Windows.md): build and run the verified Desktop
  and TUI targets.
- [Local configuration](getting-started/Configuration.md): user data, working
  directories, model credentials, Plugins, and System Skills.
- [Troubleshooting](Troubleshooting.md): common startup and build failures.

## Concepts and contracts

- [Architecture](architecture/Architecture.md): ownership and request flow.
- [Public API index](reference/API.md): where public contracts live.
- [Runtime protocol](reference/RuntimeProtocol.md): Local Runtime host boundary.
- [Model HTTP](reference/ModelHTTP.md): response budgets, credential isolation and SSE contracts.
- [Runtime boundary maintenance](development/RuntimeBoundaries.md): atomic file ownership and replacement acceptance.
- [Session events](reference/SessionEvents.md): durable and live projections.
- [Extensions](reference/Extensions.md): Plugin, Skill, MCP, CLI, and Hook boundary.
- [Plugin and Skill package spec](reference/SkillPackageSpec.md): package layout
  and strict manifest shape.
- [Provider catalog](reference/ProviderCatalog.md): model identities, routes, and reasoning preferences.
- [Versioning](reference/Versioning.md): clean-slate v1 and release versions.

## Development and release

- [Building](development/Building.md): toolchains and artifact commands.
- [Testing](development/Testing.md): focused and full local gates.
- [Frontend foundations](development/FrontendFoundations.md): mature component,
  animation, layout, sorting and state implementations; Desktop acceptance precedes Web migration.
- [Inline visualizations](development/InlineVisualizations.md): React and TUI
  content contract, implementation boundaries and acceptance requirements.
- [ExecutionHost contract](architecture/ExecutionHostContract.md):
  execution semantics and Host boundaries for the native local Host.
- [Release process](development/ReleaseProcess.md): source freeze, clean-clone
  verification, artifacts, and publication boundary.
- [Release gate](eval/ReleaseGate.md): the current executable release gate.
- [Persistence acceptance](eval/PersistenceAcceptance.md): schema validation, restart recovery, and history reconstruction.

Hosted deployment, identity, workspace access control, commercial extension
content, credentials, customer data, and private operations are outside this
repository.

## Hosted product and shared source

- [Workspace documentation](workspace/README.md)
- [Source unification and product gates](development/SourceUnification.md)
