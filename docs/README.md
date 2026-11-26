# Centaeris documentation

This index covers the Runtime Framework, local CLI, shared frontend packages,
and hosted Workspace product. The linked contracts describe current behavior.
Frozen source snapshots record provenance rather than current requirements.

## Get started

- [Windows setup](getting-started/Windows.md): build and run the CLI.
- [Local configuration](getting-started/Configuration.md): user data, working
  directories, model credentials, Plugins, and System Skills.
- [Troubleshooting](Troubleshooting.md): common startup and build failures.

## Concepts and contracts

- [Architecture](architecture/Architecture.md): ownership and request flow.
- [Ownership and invariants](development/Invariants.md): contract owners,
  existing checks, and validation limits.
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
  animation, layout, sorting and state implementations.
- [Inline visualizations](development/InlineVisualizations.md): React and TUI
  content contract, implementation boundaries and acceptance requirements.
- [ExecutionHost contract](architecture/ExecutionHostContract.md):
  execution semantics and Host boundaries for the native local Host.
- [Release process](development/ReleaseProcess.md): source freeze, clean-clone
  verification, artifacts, and publication boundary.
- [Release gate](eval/ReleaseGate.md): the current executable release gate.
- [Persistence acceptance](eval/PersistenceAcceptance.md): schema validation, restart recovery, and history reconstruction.

Hosted identity, workspace access control, deployment templates, and operations
guides live in this repository. Credentials, customer data, concrete commercial
extensions, and private deployment configuration remain outside it.

## Hosted product and shared source

- [Workspace documentation](workspace/README.md)
- [Source unification and product gates](development/SourceUnification.md)
