# Public API index

This page maps public contracts to their implementation and focused reference.
It does not duplicate every schema. The clean-slate release supports one v1
shape; unknown schemas, identities, and fields fail loudly.

Core constants and contract implementations are the source of truth for runtime semantics. Start audits at:

- `packages/core/src/runtime.rs` and `packages/core/src/runtime/contracts.rs` for the Core protocol and runtime contracts;
- `packages/core/src/session.rs` and `packages/core/src/session/wire.rs` for session events and manifests;
- `packages/core/src/runtime/event.rs` for runtime events;
- `packages/core/src/extension/` for extension contracts;
- `packages/core/src/tool/` for model-visible tool contracts.

`packages/runtime/src/host_protocol.rs` owns the local Host protocol identity,
and `packages/runtime/src/runtime_command_registry.rs` owns its method registry.
See [Runtime protocol](RuntimeProtocol.md) for the stable boundary and
[Session events](SessionEvents.md) for event projection.

JSON, event, host-protocol, and Electron bridge fields use exact `camelCase`. Built-in model tool names and model-visible parameter keys use canonical `lower_snake_case`. Storage identifiers remain storage-native.

`file_mutation_pre_apply_fact_v1.path` and its optional `targetPath` carry the exact opaque identities supplied by the `ExecutionHost`. Core validates the generic mutation schema, operation, hashes, Session identity, and execution owner without classifying Host-private namespaces.

## Contract families

| Contract | Source |
| --- | --- |
| Session manifests and events | `packages/core/src/session.rs`, `packages/core/src/session/wire.rs` |
| Runtime events and continuation | `packages/core/src/runtime/event.rs`, `packages/core/src/runtime/` |
| Built-in model tools | `packages/core/src/tool/` |
| Plugin, Skill, MCP, CLI, and Hook declarations | `packages/core/src/extension/` |
| Local Host protocol | `packages/runtime/src/host_protocol.rs`, `packages/runtime/src/runtime_command_registry.rs` |
| Electron bridge | `packages/desktop/src/hostContract.mjs` |

## MCP contracts

The declaration remains `mcp_servers_v1`. Every server requires `modelContractDigest`; every tool requires the exact model-visible `description` and `inputSchema` alongside its `sourceName` and canonical model name. Core validates the digest and builds the model catalog without connecting to the server. The adapter connects and discovers only on the first tool call, then rejects missing, extra, duplicate, or changed live tools. A contract mismatch is sticky for that activation; transport failures remain retryable. There is no discovery fallback or old-declaration alias.

The package-catalog publisher validates every declared MCP contract offline before emitting a catalog. Missing fields or stale model-contract digests block catalog generation; file digests alone do not establish a valid tool contract. Generic package resolution freezes resource identities without repeating MCP semantic validation; runtime MCP loading remains strict. Image builds must run the catalog check explicitly before copying release assets.

First-use diagnostics identify the provider and separate single-flight queue wait, initialize, and tools/list elapsed time. They never log bearer credentials or endpoint URLs.

Publishers can synchronize declarations from complete, authorized JSON-RPC `tools/list` response snapshots and check the result offline:

```powershell
cargo run --locked -p centaeris-runtime --bin centaeris-mcp-contract -- --sync <declaration.json> <server-id>=<tools-list.json>
cargo run --locked -p centaeris-runtime --bin centaeris-mcp-contract -- --check <declaration.json>
```

Supply one snapshot binding per declared server. Paginated or incomplete snapshots fail; synchronization preserves exact description text (including surrounding whitespace) and schema array order and writes atomically. Descriptions must contain non-whitespace text; they are never trimmed before hashing or live comparison. `--write` only recomputes digests for an already complete declaration.

Tool descriptions are limited to 4096 Unicode characters and 16384 UTF-8 bytes; each input schema is limited to 65536 serialized JSON bytes. MCP declarations and discovery responses are bounded to 4 MiB before decoding. Dynamic registration, activated MCP declarations, and the complete resolved tool catalog each enforce a 4 MiB cumulative serialized budget before accumulation or hashing. Oversized inputs fail explicitly without truncating contracts or changing digests. These byte budgets do not impose a message-count limit. Plugin resource hashing streams file bytes using the unchanged length-prefixed digest format.

## Model-request persistence and context

One `model_request_started` record owns the ordered observations for one request; standalone `model_observation` records are not supported. Storage-private content/manifest references must be hydrated before Core decodes the record and must not enter host history or streams. A local Session document consists of its `.jsonl` file and matching `.observations` directory; copy or back up both. Missing or corrupted content fails loudly.

Automatic prompt compaction is driven by token pressure, not message count; explicit manual compaction remains available. There is no message-count truncation or ceiling. Requests use the full active projection, including any compaction summary/replay prefix, which remains pinned until the next compaction. Compaction preserves atomic tool groups. Compaction failure must not silently discard history, and the hard token budget can reject an oversized request. Reusable-prefix measurements describe projection stability, not a guarantee of a provider's cache-hit rate. The protocol remains v1 with no compatibility aliases.

Hosted REST/SSE endpoints, database models, workspace authorization, concrete
package contents, and commercial extensions are intentionally outside this
public API.
