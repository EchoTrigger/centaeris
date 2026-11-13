# Plugin and Skill package specification

This document defines the clean-slate v1 package contract implemented by
`packages/core/src/extension`. Unknown fields in strict JSON manifests and
unsupported contribution types fail. There are no aliases for older manifests
or contribution schemas. Skill frontmatter follows its separately defined
rules below.

## Version identity

The v1 Plugin manifest does not carry a `schema` field. Its protocol identity
is the published v1 manifest contract at the exact path
`.centaeris-plugin/plugin.json`, independent of the product release version.
The manifest's `version` field is the Plugin's own release version and never
selects a manifest parser.

A Runtime v1 implementation must parse this file using only the exact v1 shape
below and reject unknown fields. The v1 shape cannot gain optional fields in
place. An incompatible manifest revision requires a new public contract;
v1 does not perform in-band schema negotiation.

## Plugin root and manifest

A Plugin is one directory containing:

```text
<plugin-root>/
  .centaeris-plugin/
    plugin.json
  ...declared resources...
```

```json
{
  "name": "example-plugin",
  "version": "1.0.0",
  "paths": {
    "skills": ["skills"],
    "cli": ["bin/example"],
    "mcpServers": ["mcp/servers.json"],
    "apps": [],
    "hooks": ["hooks/hooks.json"]
  },
  "interface": {
    "displayName": "Example Plugin",
    "shortDescription": "Example capabilities",
    "capabilities": ["Instructions", "CLI"]
  }
}
```

`name` and `version` are required. `paths` may be omitted and then means five
empty contribution lists. `interface` is optional presentation metadata with
only `displayName`, `shortDescription`, and `capabilities`.

Plugin names contain 1 to 64 ASCII bytes in lower-kebab-case, with no leading,
trailing, or repeated hyphen. Versions are exactly `major.minor.patch` numeric
triplets without leading zeroes.

## Resource paths

Every declared path is a non-empty NFC-normalized relative POSIX path.
Backslashes, colons, control characters, empty segments, `.`, `..`, absolute
paths, missing resources, and paths that resolve outside the Plugin root fail.

- `skills` entries name either one exact `SKILL.md` file or a Skill catalog
  directory as defined below.
- `cli` entries resolve to files and have unique executable basenames across one
  Activation.
- `mcpServers` entries resolve to strict `mcp_servers_v1` JSON files.
- `hooks` entries resolve to strict `plugin_hooks_v1` JSON files.
- `apps` is reserved. A non-empty Apps contribution fails in Runtime v1.

Hosts may apply additional platform packaging checks such as executable mode,
archive safety, or supported transport. They do not weaken the Core manifest.

## Content digests

Package and resource digests use the same tree algorithm. The result is
`sha256:` followed by 64 lowercase hexadecimal characters.

To digest a file or directory resource:

1. Walk only regular files. Reject symbolic links and other entry types. An
   empty directory contributes no entry.
2. Express every file path relative to the Plugin root, replace native
   separators with `/`, require UTF-8 and NFC, then sort paths in ascending
   UTF-8 lexicographic order.
3. Start the SHA-256 input with the ASCII bytes
   `centaeris.plugin.tree.v1`, followed by one zero byte.
4. For each sorted file append, without separators:

   ```text
   u64be(pathUtf8ByteLength)
   pathUtf8Bytes
   u64be(fileByteLength)
   fileBytes
   ```

   `u64be` is an unsigned 64-bit integer in network byte order. File bytes are
   streamed without newline or Unicode rewriting. A size or modification-time
   change while hashing fails the operation.

The package digest applies this algorithm to the Plugin root, so it covers the
manifest, every declared resource, supporting Skill files, CLI programs, Hook
programs, and any other file shipped in the package. A resource digest applies
the same algorithm to that declared file or subtree, while still encoding paths
relative to the Plugin root. File permissions, timestamps, directory entries,
and empty directories are not digest inputs.

The `plugin_activation_snapshot_v1` digest is SHA-256 over compact UTF-8 JSON
containing only its `schema` and `packages`, prefixed in the result with
`sha256:`. JSON object keys are lexicographically sorted recursively; arrays
retain their order. Packages are sorted by `name`, then `packageDigest`; each
resource list is sorted by `path`. The snapshot's own `digest` is not part of
the input.

These digests are protocol inputs consumed by Activation validation. Packages
must not ship unused checksum sidecar files merely to repeat them.

## Skill discovery and identity

A `skills` entry has exactly one of these meanings:

- A file entry must be named exactly `SKILL.md` and contributes that one Skill.
- A directory entry is a catalog. Core inspects only its immediate child
  directories and accepts `<catalog>/<skill-name>/SKILL.md`. Discovery is not
  recursive. Files at the catalog root and deeper nested Skills are ignored.

A directory that itself contains `SKILL.md` is therefore not a single-Skill
entry; declare the `SKILL.md` file or declare its parent catalog.

`SKILL.md` is UTF-8, at most 512 KiB, and starts with terminated YAML
frontmatter. A UTF-8 BOM before the opening `---` is allowed. The required
frontmatter fields are:

```markdown
---
name: example-skill
description: Use this Skill for a specific class of tasks.
---

Instructions begin here.
```

`name` contains 1 to 64 ASCII characters in lower-kebab-case, with no leading,
trailing, or repeated hyphen, and exactly matches the parent directory name.
`description` is required, is non-empty after trimming, and contains at most
1,024 Unicode scalar values. Frontmatter keys are case-insensitive and duplicate
keys fail. `disable-model-invocation`, when present, is `true` or `false`.
`allowed-tools` accepts a comma- or whitespace-separated scalar, bracketed
scalar, or YAML-style list; Core sorts and removes duplicate entries. Other
frontmatter keys are allowed for portable Skill metadata and ignored by the v1
catalog.

A Skill's stable catalog identity is `<sourceId>:<lowercase-name>`. Same-name
precedence is Workspace, User, System, then Plugin scope; `sourceId`
lexicographic order breaks ties within a scope. Losing entries remain visible
as shadowed catalog entries and are not offered for invocation.

Supporting scripts and references use paths relative to the directory
containing `SKILL.md`. Their bytes are frozen by the Plugin package digest even
though the Skill catalog's own content hash covers only `SKILL.md`.

A Skill can guide use of tools available in the current AgentRun. It cannot add
an undeclared tool, access a credential, or bypass Host policy.

## MCP declarations

An MCP declaration uses schema `mcp_servers_v1`. Every server freezes its
transport, credential reference, lifecycle and timeout fields. It selects one
of two explicit tool catalog modes:

- **Pinned:** declare `tools` sorted by unique `sourceName` and the matching
  `modelContractDigest`. Runtime connects lazily on first use and rejects
  missing, additional or changed live tools. Description comparison tolerates
  whitespace changes while retaining exact words and schema values.
- **Discovered:** declare `toolDiscovery` and omit `tools` and
  `modelContractDigest`. For example:

  ```json
  {"namePrefix":"records","concurrencySafe":false,"scopes":["records:read"]}
  ```

Discovery authorizes a directory from the installed server under the
package-owned scopes and concurrency policy. Upstream annotations cannot add
scopes or change concurrency. A newly discovered tool is not evidence that it
is read-only; only install discovery for a server whose capabilities belong
inside that package grant. Existing Host enablement and credentials are still
checked at dispatch.

Core requires discovered source names to already use canonical
`lower_snake_case`; the model name is exactly `<namePrefix>_<sourceName>`.
It validates descriptions, schemas, directory limits and duplicate identities.
No source-name alias or argument rewriting is applied.

Hosts discover dynamic directories when preparing a new activation. Connections
are reused within it. Native `notifications/tools/list_changed` invalidates
the cache; negotiated 2026-07-28 connections use `subscriptions/listen` when
the server advertises notifications. Hosts refresh an invalidated directory
before the next model request. A reconnect also discovers a new directory.
Servers without notifications are rediscovered for a new activation and after
an upstream name/parameter rejection. No metadata scan is required before
every tool call. Failed refreshes retain invalidation. An unavailable or invalid
remote directory contributes no executable tools and a Host diagnostic; it does
not block unrelated connectors or built-in tools. Later metadata refresh can
restore a validated directory without replaying any business call.

Each model request freezes its execution contracts alongside its tool
definitions. A stale call is rejected, and business calls are never replayed
automatically. Delegated work validates and freezes the exact contracts in its
work packet; later parent directory updates do not widen that delegation.

For pinned mode, `modelContractDigest` is SHA-256 over the UTF-8 domain
`centaeris.mcp_model_contract.v1`, a zero byte, and canonical JSON containing
schema `mcp_model_contract_v1`, `serverId`, and tools sorted by `sourceName`.
Each digest tool contains `sourceName`, `name`, `description`, and `inputSchema`.
Object keys are sorted recursively and array order is preserved. The package
resource digest separately binds transport, scopes and concurrency. Credentials
appear only as Host-resolved references.

Catalog publication validates both modes offline. The contract CLI `--sync`
updates pinned declarations from complete authorized `tools/list` snapshots;
for discovered declarations it validates snapshots without embedding them.
Neither mode is a fallback for an invalid declaration of the other mode.

## Hook declarations

A Hook file uses schema `plugin_hooks_v1` and contains `handlers`. Each handler
declares exact `id`, `event`, optional canonical tool `matcher`, package command
`program`, `args`, and bounded `timeoutMs`. Core validates event, matcher, path,
timeout, output, ordering, and failure semantics.

## Frozen Activation

Installation does not imply enablement. A Host resolves installed packages and
configuration into one Activation snapshot before an AgentRun. The snapshot
contains sorted package and resource identities and digests.

A snapshot alone does not make a mutable directory immutable. For the full
AgentRun lifetime, the Host must bind every activated package to bytes matching
the snapshot's `packageDigest` and prevent its lifecycle operations from
replacing, updating, or deleting those bytes. A conforming Host uses either:

- an immutable or content-addressed package snapshot retained until the last
  AgentRun lease releases it; or
- a package lifecycle lock plus a read-only package root retained until the
  last AgentRun lease releases it.

Re-hashing a mutable path and later opening it is not sufficient because the
bytes can change between those operations. Direct mutation detected during a
run fails the affected package operation; the Host must not silently continue
with bytes that differ from the frozen Activation. Configuration or package
changes become visible only to a later AgentRun.

One invalid enabled package fails its own Activation explicitly. Catalog and
management surfaces keep unrelated packages available and allow an
administrator to disable the failing package.
