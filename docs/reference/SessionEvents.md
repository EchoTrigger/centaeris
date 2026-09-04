# Session events

This document defines the clean-slate v1 Session document, its durable records,
and the projections delivered to Desktop and TUI. Core owns these semantics.
Hosts may choose a storage engine and transport, but they must preserve the
identities, ordering, validation, and reduction rules below.

The three related contracts have different jobs:

- `session.manifest.v1` identifies one Session document;
- `session.event.v1` records durable facts in that document;
- `runtime_event` reports live process activity and is not durable Session
  truth.

Hosted Workspace persistence and its Redis delivery cache are outside this
local Runtime contract. Redis is not part of the public Session format.

## Session document

A local Session is UTF-8 JSONL. The first line is exactly one manifest. Every
following line is exactly one wire record. The file ends with `LF`; blank lines
and a truncated final line are invalid.

The v1 manifest has this exact shape:

```json
{
  "schemaVersion": "session.manifest.v1",
  "sessionId": "session-id",
  "protocolMajor": 1,
  "createdAtMs": 1,
  "writerVersion": "1.0.0",
  "requiredFeatures": [],
  "integrityMode": "record"
}
```

`sessionId` and `writerVersion` are non-empty. `createdAtMs` is a non-negative
Unix timestamp in milliseconds. v1 supports no `requiredFeatures`; a non-empty
array therefore means the reader does not implement the document. Unknown
manifest fields fail.

Storage-private model observation content may live in the matching
`.observations` directory. A Host must hydrate those references before handing
the record to Core. A local Session backup consists of both the JSONL document
and that directory.

## Wire record

Every durable record has the exact envelope below. Optional identity fields are
omitted, not serialized as aliases.

```json
{
  "schemaVersion": "session.event.v1",
  "eventVersion": 1,
  "sequence": 1,
  "type": "user_message",
  "eventId": "event-user",
  "sessionId": "session-id",
  "turnId": "turn-id",
  "agentRunId": "agent-run-id",
  "createdAtMs": 1,
  "payload": {}
}
```

- `sequence` starts at 1 and is contiguous across the complete Session
  document. It is the only durable record order. Timestamp order is not a
  substitute.
- `eventId` is non-empty, at most 160 bytes, and unique within the Session. It
  is opaque to readers.
- `sessionId` equals the manifest identity on every record.
- `turnId` identifies one conversational turn. `agentRunId` identifies the
  AgentRun that owns the fact. Records projected into an AgentRun stream require
  both unless the catalog below says otherwise.
- `createdAtMs` is a Unix timestamp in milliseconds; it does not determine
  replay position or idempotence.
- `payload` is always an object. Fields use exact `camelCase`.
- Unknown envelope fields, record types, event versions, and payload fields
  fail. Tool-owned objects such as `normalizedInput` and `operations` retain
  their declared tool contract rather than acquiring Session fields.

Some Core writers derive stable event IDs with:

```text
evt_v1_<kind>:sha256:<lowercase hex sha256(JSON([kind, components]))>
```

Other writers use a different opaque ID form. Consumers compare the complete
`eventId`; they must not recreate it, identify events by text, or treat a hash
as a request to emit a checksum sidecar.

Parsing the primitive shape is only the first validation stage. A valid Session
must also reduce successfully: references resolve, AgentRun and Execution
lifecycles are ordered, tool calls pair with their results, identities do not
conflict, and terminal invariants hold.

## Durable record catalog

`Run identity` describes the identities required by a semantically valid v1
Session. `Stream` says whether the record is projected into AgentRun replay.

| `type` | Run identity | Stream | Exact payload fields |
| --- | --- | --- | --- |
| `session_meta` | none | no | `recordId`, `title`, `cwd`, `sessionKind`, `parentSessionId`, `runtimeJobId`, `sortOrder`, `isPinned`, `isUnread` |
| `agent_run_started` | turn + run | yes | `userObjective` |
| `agent_run_execution_started` | turn + run | no | `executionId`, `authorizationDigest`, `recoveredFromCheckpointId` |
| `agent_run_execution_ended` | turn + run | no | `executionId`, `outcome`, `reasonCode`, `retryable`, `lastCheckpointId`, `indeterminateToolCallIds` |
| `user_message` | turn + run | yes | `messageId`, `text`, `attachments` |
| `turn_supplement` | turn + run | yes | `supplementId`, `messageId`, `message` |
| `assistant_message` | turn + run | yes | `messageId`, `modelMarkdown`, `artifactRefs`, `status` |
| `tool_call` | turn + run | yes | `callId`, `toolName`, `toolContractDigest`, `providerId`, `normalizedInput`, `displayTarget` |
| `tool_result` | turn + run | yes | `callId`, `toolName`, `resultState`, `modelContent`, `fullOutputPath`, `outputStartByte`, `outputByteLength`, `outputComplete`, `summary`, `operations`, `modelInputImages`, `latencyMs` |
| `model_request_started` | turn + run | no | `requestId`, `purpose`, `loopIndex`, `toolChoice`, `maxOutputTokens`, `promptCacheKey`, `promptCacheRetention`, `preparedPromptSchema`, `contextTokenEstimate`, `contextTokenBreakdown`, `agentComposition`, `observations` |
| `provider_usage` | turn + run | no | `inputTokens`, `outputTokens`, `totalTokens`, `promptCacheHitTokens`, `promptCacheMissTokens` |
| `phase_event` | turn + run | yes | `stage`, `message` |
| `external_evidence_ref` | turn + run | yes | `objectRef`, `contentType`, `sha256`, `byteLength`, `sourceKind`, `locator` |
| `citation_recorded` | turn + run | yes | base citation fields and the optional derived identity group defined below |
| `artifact_published` | turn + run | yes | `publicationId`, `artifactRef`, `toolCallId`, `filename`, `sizeBytes`, `sha256` |
| `compaction` | turn + run | yes | `compactionId`, `summaryMessageId`, `summaryMarkdown`, `firstKeptMessageId`, `createdReason` |
| `checkpoint_ref` | turn + run | no | `checkpointId`, `kind`, `objectRef`, `status`, `payloadSha256`, `payloadByteLength`, `updatedAtMs` |
| `tombstone` | owning rewrite turn + run | yes | `tombstoneId`, `targetEventIds`, `reasonType` |
| `file_fact` | turn + run | no | `schema`, `toolName`, `toolCallId`, `operation`, `path`, `targetPath`, `previousFileHash`, `readSnapshotHash`, `fileHash`, `bytesWritten`, `addedLines`, `removedLines`, `sessionId`, `executionOwner` |
| `agent_run_completed` | turn + run | yes | `doneReason` |
| `agent_run_failed` | turn + run | yes | `reasonType`, `message` |
| `agent_run_interrupted` | turn + run | yes | `reasonType`, `message`, `retryable` |

### Metadata and message rules

`session_meta.sessionKind` is `main` or `subagent`. A main Session has null
`parentSessionId` and `runtimeJobId` and a non-null `sortOrder`. A subagent has
non-empty parent and job identities and a null `sortOrder`.

A message attachment has exact fields `inputRef`, `displayName`, `contentType`,
and optional `placeholder`. A placeholder is valid only for an `image/*`
attachment, occurs exactly once in the message text, and is unique among that
message's image attachments.

An assistant message ID is exactly `message:<turnId>:assistant`; one AgentRun
has at most one sealed assistant message. `status` is `done` or `error`.
`artifactRefs` contains at most 64 unique `artifact:` identities and exactly
matches the publication order accumulated for that AgentRun.

`phase_event.stage` is exactly `model_process_summary`. An AgentRun commits at
most one phase event for a given turn identity.

### Execution rules

An Execution starts only while its AgentRun is running and no other Execution
is active. `authorizationDigest` is `sha256:` plus 64 lowercase hexadecimal
characters. A replacement Execution names an existing recovery checkpoint;
one checkpoint cannot start two replacements.

`agent_run_execution_ended.outcome` is `completed`, `failed`, `lost`, or
`cancelled`. `lastCheckpointId` is a non-empty string or null.
`indeterminateToolCallIds` is a duplicate-free string array. The active
Execution must end before the AgentRun can become terminal.

### Tool records

A `tool_result` follows exactly one earlier `tool_call` with the same `callId`,
`toolName`, `turnId`, and `agentRunId`. The result state is one of:

```text
successWithOutput
successNoOutput
successNoMatches
failed
denied
aborted
```

Failed, denied, aborted, and cancelled work still receives a terminal tool
result. Context construction, compaction, replay, and presentation preserve the
call/result group and its order.

`toolContractDigest` uses the standard `sha256:` digest form.
`normalizedInput` is an object. `displayTarget` is at most 256 Unicode
characters. `operations` contains the tool's structured operation facts.
`modelInputImages` uses the exact `ModelInputImageSourceRefV1` union in
`packages/core/src/model/prepared_prompt.rs`.

A complete result stored inline has null `fullOutputPath` and
`outputStartByte`, and `outputByteLength` equals the UTF-8 byte length of
`modelContent`. A large complete result may reference the Runtime's bounded
temporary capture with a positive start byte. An incomplete result has no
capture path or start byte. These storage fields do not authorize a client to
read an arbitrary path.

### Model requests and usage

`model_request_started.purpose` is `main` or `compaction`. One record owns the
complete ordered observation list for that provider request; standalone
observation records do not exist.

Main observations are in canonical groups: at most one `system_prompt`, then
zero or more `message`, then zero or more `input_image`, then at most one
`tool_catalog`. A compaction request contains exactly one `compaction_prompt`
observation, uses `toolChoice: {"type":"none"}`, and has no tool catalog.
Observation unions and their nested message, image, and tool-definition shapes
are the strict v1 types in `packages/core/src/runtime/driver.rs` and
`packages/core/src/model/prepared_prompt.rs`.

`maxOutputTokens` is positive. `preparedPromptSchema` equals the Core v1
prepared-prompt identity. `contextTokenBreakdown` sums exactly to
`contextTokenEstimate`, and its per-MCP-tool entries are unique and sum to
`mcpToolTokens`. `agentComposition` is one validated
`ResolvedAgentCompositionV1`; its `compositionDigest` cannot change during the
AgentRun.

Every provider usage field is an unsigned integer or null. A record may contain
only the fields the provider actually reported. `totalTokens`, when present,
cannot be less than `inputTokens`. Cache fields are evidence only when supplied
by the provider; zero or null is not proof of a cache hit.

### Evidence, citations, and artifacts

All SHA-256 fields use `sha256:` plus 64 lowercase hexadecimal characters.
`external_evidence_ref` identifies captured bytes and their source locator; it
does not inline those bytes.

A citation has these base fields:

```text
citationId, inputRef, ownerRef, ownerKind, displayName, evidenceKind,
ownerSha256, sourceToolCallId, sourceToolName, locator
```

`ownerKind` is `sourceObject`, `userLibraryObject`, or `artifact`.
`evidenceKind` is `workspaceSource`, `userProvided`, or `generatedArtifact`.
The source tool call must exist, match the same turn and AgentRun, have the
declared tool name, and have a successful result.

`locator` is either an exact one-based line range (`startLine`, `endLine`), an
exact one-based page range (`pageStart`, `pageEnd`), or a validated
`KnowledgeLocatorV1`. A knowledge citation also carries all four derived
identity fields: `ownerGeneration`, `representationId`, `specDigest`, and
`evidenceSha256`. The group is all-or-none.

An artifact publication follows a successful source tool call. `publicationId`
is `pub_` plus 64 lowercase hexadecimal characters; `artifactRef` starts with
`artifact:`; `filename` is a basename of at most 255 bytes; `sizeBytes` is at
most 64 MiB. The publication digest describes the artifact bytes and is later
bound into the sealed assistant message through `artifactRefs`.

### Compaction, checkpoints, file facts, and tombstones

Compaction is additive. It records a summary and the first message retained
after the compacted prefix; `firstKeptMessageId` is a non-empty string or null.
It does not delete the underlying events and must preserve atomic tool groups.

A checkpoint reference never embeds a checkpoint, snapshot, message list, or
state object. `kind` is `wait` or `recovery`; `status` is `paused_question`,
`waiting`, or `committed`. Its digest and byte length bind the external
checkpoint object.

`file_fact.schema` is exactly `file_mutation_pre_apply_fact_v1`, and operation
is `create`, `overwrite`, or `update`. `path` and optional `targetPath` are
opaque ExecutionHost identities. Core does not infer Host-private namespaces.
Optional hashes and counters are null or valid values; the nested `sessionId`
equals the record Session.

A tombstone names existing event IDs to omit from the active projection. The
original records remain append-only. Tombstones are processed before ordinary
reduction, cannot target other tombstones, and cannot create ambiguous rewrite
history.

## AgentRun terminal contract

Exactly one terminal record ends an AgentRun:

- `agent_run_completed` maps to completed and requires one sealed `done`
  assistant message;
- `agent_run_failed` maps to failed and may have no assistant or a sealed
  `done`/`error` assistant;
- `agent_run_interrupted` maps to interrupted and follows the same assistant
  rule. Its `reasonType` is `cancelled`, `stopped`, `shutdown`, or
  `provider_interrupted`.

No Execution may remain active when the terminal record commits. Once a
terminal record is durable, clients remove all active process rows for that
AgentRun; a presentation-only "run stopped" message is not a Session fact.

## Stream projections

The Local Runtime delivers two wrappers through `session/update`.

A live item is:

```json
{
  "type": "runtime_event",
  "event": {
    "id": "runtime:ModelTextDelta:...",
    "version": "v1",
    "type": "ModelTextDelta",
    "at": 1,
    "sessionId": "session-id",
    "turnId": "turn-id",
    "taskId": "agent-run-or-operation-id",
    "parentTaskId": "turn-id",
    "status": "running",
    "visibility": "user",
    "processState": "thinking",
    "payload": {},
    "meta": {"source": "core.agent_runtime"}
  }
}
```

A durable replay item is:

```json
{
  "type": "session_event",
  "agentRunId": "agent-run-id",
  "cursor": "0",
  "event": {
    "id": "durable-event-id",
    "version": "v1",
    "type": "UserMessage",
    "at": 1,
    "sessionId": "session-id",
    "turnId": "turn-id",
    "taskId": "agent-run-id",
    "parentTaskId": "turn-id",
    "status": "done",
    "visibility": "internal",
    "payload": {},
    "meta": {"source": "core.session_log", "durable": true}
  }
}
```

`RuntimeEventProjection` is strict. `version` is `v1`; `payload` and `meta` are
objects; `visibility` is `user` or `internal`. The complete allowed event-type
set is owned by `packages/core/src/runtime/event.rs`.

The decimal `session_event.cursor` is the zero-based index in that AgentRun's
projected replay items. The replay request cursor is the same offset, not the
Session record `sequence`. Omitted cursor starts at zero. The default page is
200 items and the requested limit is clamped to 1 through 1000. A cursor beyond
the replay tail fails. `nextCursor` is the next offset when more items remain;
the full `session_projection.v1` reports the current replay length as each
AgentRun's `nextCursor`.

Durable events are idempotent by `event.id`. Reconnect may replay an event a
client already saw. Clients retain facts by identity and order; equal text in
two different durable events remains two facts.

## Live text and supersession

`ModelTextDelta` and `ModelTextReplace` are transient. The Local Runtime batches
them and journals their append/replace operations under the exact
`(sessionId, turnId, agentRunId)` identity so an interrupted process can recover
already produced text. The journal is a recovery mechanism, not Session
history and not an additional replay source.

When `assistant_message` commits, its durable `Final` projection supersedes all
live text for the same identity. Adding the final event and removing the live
projection is one logical client state transition. A delayed live event for an
already sealed identity is ignored. Text equality is never the supersession
key.

On an interrupted run, non-empty recovered live text may be sealed as an
`assistant_message` with `status: error` before the interrupted terminal record
commits. A corrupt live-text journal is isolated and reported; it must not stop
recovery of healthy AgentRuns.

Other live Runtime events describe current process state. A committed
`phase_event`, tool record, final assistant message, or terminal AgentRun record
becomes the durable fact for its corresponding operation. Presentation state
such as expanded tools, scroll tracking, and elapsed timers stays in the
client; it is not written as Session truth.

## Change policy

This is one strict v1 contract. A change to the manifest, event envelope,
payload fields, nested public types, projection mapping, or reduction rules
updates Core, exact fixtures, this reference, and the release gate together.
Old record names, optional compatibility aliases, and unknown fields are not
accepted.
