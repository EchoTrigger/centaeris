# Extensions

Centaeris extends an AgentRun through versioned Plugin directories. Core owns
the manifest, catalog, composition, Activation, tool, and Hook semantics. A
Host owns how package bytes arrive, where they are retained, how credential
references resolve, and which platform resources can execute.

Concrete commercial Plugins, credentials, hosted configuration, and package
registry services are not part of this repository.

See [Plugin and Skill package specification](SkillPackageSpec.md) for the exact
manifest, path, digest, Skill discovery, MCP declaration, and frozen Activation
contracts.

## Contribution model

One Plugin may contribute declared resources of these kinds:

| Contribution | Effect in an AgentRun |
| --- | --- |
| Skill | Adds validated instruction content to the resolved Skill catalog. |
| CLI | Adds package files to the request-bound execution environment's `PATH`. |
| MCP server | Adds frozen model-visible tools backed by a lazily connected MCP adapter. |
| Hook | Runs a declared handler at a Core lifecycle event. |
| App | Reserved in Runtime v1; every non-empty App contribution fails. |

A contribution does not create another Agent loop, Session store, model client,
or permission system. It enters the existing Core request and tool lifecycle.

## Lifecycle

The lifecycle has distinct stages. A Host must not collapse installation,
enablement, and Activation into one implicit operation.

### 1. Acquire package bytes

The public Plugin contract starts at a package directory. It does not define a
ZIP upload, npm registry, marketplace, download URL, or update service.

The Local Runtime method `plugin/install` accepts one `sourcePath` naming a
directory already present on the local machine. The Host canonicalizes the
path, rejects overlap with its managed Plugin root, and rejects symbolic links
or unsupported filesystem entries while copying. ZIP and npm installation are
not Local Runtime v1 aliases.

A hosted product may add an authenticated ZIP or registry transport. That
adapter must safely extract or materialize a directory and then pass the same
bytes through the public validation below. Archive behavior does not change the
manifest or Activation schema.

### 2. Stage and validate

Installation copies into a private staging directory. Before the package
becomes visible, the Host:

1. resolves the source package and validates its exact manifest and resources;
2. computes the package and resource digests;
3. copies the complete regular-file tree without following links;
4. resolves the staged copy again and requires it to equal the validated source
   package;
5. verifies that the complete set of enabled packages can form one Activation;
6. moves the staged directory to its final managed identity.

A failed stage is not an installed package. Cleanup failure is diagnosed but
must not cause a partial package to appear in the active catalog.

### 3. Catalog

The Host catalogs read-only bundled roots and its managed root. Plugin identity
is the manifest `name`. Duplicate identities across roots fail; root order does
not silently select one package.

Catalog errors are isolated to the affected package. List and detail operations
continue to expose unrelated packages, and an invalid enabled package remains
manageable so it can be disabled or removed. Error isolation does not turn the
invalid package into a partial Activation.

### 4. Enable or disable

Installation and enablement are separate state. The Host stores the enabled
choice by Plugin identity. Changing the choice affects future Activation
resolution only.

Disabling a Plugin does not delete its bytes. Enabling it does not connect MCP,
run a Hook, execute a CLI program, or disclose a credential. Those effects
occur only through the owning AgentRun lifecycle.

### 5. Freeze an Activation

Before model execution, Core resolves enabled packages into one sorted
`plugin_activation_snapshot_v1`. The Activation binds every package and
resource identity to its digest. Skills, CLI basenames, MCP tool names, and Hook
handlers must be conflict-free across the complete Activation.

The Host retains immutable bytes for the full AgentRun lease. A configuration
reload or later package lifecycle operation cannot mutate a running AgentRun.
See the filesystem requirements in
[Frozen Activation](SkillPackageSpec.md#frozen-activation).

### 6. Execute contributions

Skills are prompt inputs and never executable authority by themselves. CLI
resources are exposed through the Host's existing process tool. MCP servers are
connected only when their first frozen tool is called. Hooks are invoked by
Core at their declared lifecycle event and use the existing timeout, receipt,
failure, and Session semantics.

Credential values remain Host-owned. A package contains a credential reference,
never the secret. Diagnostics may identify a provider or credential reference
but must not print bearer tokens or resolved endpoints containing secrets.

### 7. Update and remove

Runtime v1 has no in-place mutation of a managed package and no compatibility
update path. Installing an already present identity fails. A future update
operation must stage and validate replacement bytes, retain the old immutable
generation while any AgentRun uses it, then publish the replacement for later
runs.

`plugin/remove` applies only to a package owned by the managed root. Bundled
read-only packages are not removed through this method. Removal first detaches
the managed directory by rename, then deletes the detached tree; if deletion
fails, the Host attempts to restore the original directory and reports any
incomplete restoration.

Removal or replacement cannot invalidate an active AgentRun's frozen bytes. A
Host that has no immutable-generation store must block the lifecycle operation
until every affected AgentRun lease releases.

## MCP contract and failures

Every declared MCP tool freezes its source name, model-visible name,
description, input schema, provider identity, and model-contract digest.
Catalog construction validates the declaration offline and does not start the
server.

First use performs one lazy connection and live `tools/list` check. Discovery
order is irrelevant; identity and contract bytes must match. A contract
mismatch is deterministic and remains sticky for that Activation. A transport
availability failure is retryable only at the adapter's defined boundary.
Neither case may disable unrelated packages or replace the frozen declaration
with live server content.

## Strictness and change policy

Unknown manifest fields, path escape, links, duplicate identities, missing
resources, stale digests, unsupported contribution types, and conflicting
Activation resources fail explicitly. There are no old manifest aliases,
archive fallbacks, implicit PATH scanning, or MCP discovery fallbacks.

Host-specific acquisition may grow without changing this contract only when it
produces exactly the same validated package directory. A change to package
meaning, contribution semantics, or Activation identity requires a deliberate
protocol revision, focused conformance tests, this reference, and the release
gate to change together.
