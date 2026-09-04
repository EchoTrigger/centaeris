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
- [Session events](reference/SessionEvents.md): durable and live projections.
- [Extensions](reference/Extensions.md): Plugin, Skill, MCP, CLI, and Hook boundary.
- [Plugin and Skill package spec](reference/SkillPackageSpec.md): package layout
  and strict manifest shape.
- [Versioning](reference/Versioning.md): clean-slate v1 and release versions.

## Development and release

- [Building](development/Building.md): toolchains and artifact commands.
- [Testing](development/Testing.md): focused and full local gates.
- [Release process](development/ReleaseProcess.md): source freeze, clean-clone
  verification, artifacts, and publication boundary.
- [Release gate](eval/ReleaseGate.md): the current executable release gate.

Hosted deployment, identity, workspace access control, commercial extension
content, credentials, customer data, and private operations are outside this
repository.
