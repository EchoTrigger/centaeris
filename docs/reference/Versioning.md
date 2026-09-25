# Versioning

Centaeris uses separate identities for product releases, package releases,
protocols, and stored schemas. None of them silently selects an older shape for
another contract.

## Source and product version

Current Rust workspace and Node package metadata is `0.1.0`. This describes the
source candidate; it does not assert that a public tag, package-registry
publication, or GitHub Release already exists.

Before a release, the exact source revision, tag, Desktop metadata, TUI package
manifest, bundled Runtime, third-party license payload, and release notes all
carry the approved product version. An artifact built from another revision is
not repaired by renaming the file.

The current build matrix produces only:

- a Windows x64 TUI archive named `centaeris-windows-x64.zip`;
- an unpacked Windows Desktop directory containing `Centaeris Desktop.exe`.

The source may compile on another operating system, and some Unix transport or
sandbox code may exist, without creating a supported download or release
claim. A platform enters the release matrix only with an explicit build target,
packaging rules, license assembly, and acceptance tests.

The Rust crates and Node workspaces are repository components. The product does not
claim that they are independently published to crates.io or npm.

## Plugin package version

`plugin.json.version` is the Plugin's release version. Runtime v1 accepts the
exact numeric `major.minor.patch` form without leading zeroes. The field does
not select a manifest parser and is not the Plugin's Activation identity by
itself; package and resource digests bind the actual bytes.

A Host may use a package version to present update information. It must still
stage, validate, digest, and freeze the complete replacement. Equal versions
with different bytes are different package generations and cannot mutate an
active AgentRun.

## Protocol and schema identities

The current public identities include:

| Contract | Current identity |
| --- | --- |
| Core protocol | `1.0.0` |
| Local Runtime protocol | `centaeris.runtime`, `protocolVersion: 1` |
| Session manifest | `session.manifest.v1` |
| Session event | `session.event.v1`, `eventVersion: 1` |
| Runtime event projection | `version: v1` |
| Plugin manifest | `.centaeris-plugin/plugin.json` with the current manifest contract |
| Plugin Activation | `plugin_activation_snapshot_v1` |
| MCP declaration | `mcp_servers_v1` |
| MCP model contract | `mcp_model_contract_v1` |
| Hook declaration | `plugin_hooks_v1` |

The product version and protocol identities change independently. A patch
release can preserve every protocol. Conversely, changing a public JSON shape,
field meaning, ordering rule, digest preimage, replay rule, or lifecycle
invariant is a protocol change even if a package version was not bumped.

## Clean-slate v1 policy

For every current contract:

- one exact v1 shape is supported;
- exact serialized types reject unknown fields, types, schemas, and unsupported
  versions; any current transport-decoder tolerance is documented in its owning
  protocol reference rather than generalized to other contracts;
- renamed fields do not retain compatibility aliases;
- a reader never guesses a schema from nearby fields;
- an adapter does not translate old persisted semantics into Core behind the
  public contract.

The Plugin manifest has no serialized schema field. Its exact path and the
published manifest contract identify its parser independently of product versions.

## Stored data

Storage adapters own physical schemas. Runtime initializes an empty database
with the current schema and validates an existing database before using it.
Configuration loading validates the current schema and its model identities.
Unsupported formats fail explicitly; readers do not guess or rewrite them.

Session logs and observation content are authoritative. Derived transcript
projections can be rebuilt without rewriting source records or replaying tools.
See [persistence acceptance](../eval/PersistenceAcceptance.md) for validation.

## Release change rule

A public contract change updates, in the same candidate revision:

1. the owning Core or Host type and validator;
2. exact serialization, rejection, replay, and reduction tests;
3. the public reference and code-navigation index;
4. the local release gate and checked-in CI;
5. every artifact or package manifest that advertises the changed identity.

A new protocol version, release tag, remote branch change, or published asset
requires an explicit project decision. It is not inferred from editing source
metadata.
