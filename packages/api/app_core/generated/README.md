# Generated transcript wire contracts

`transcript_block.schema.json` is generated from Core's `TranscriptBlockV1`
and its nested Rust types. It describes serialization: camelCase keys, exact
variants, nullable fields that must still be present, and rejected unknown keys.
`transcript_block.samples.json` contains values serialized by the same Rust types.

Run `python scripts/transcript-schema.py` to regenerate using the Core dependency
resolved by Cargo. CI runs `--check`; Python contract tests consume the samples.
Do not edit generated JSON or add a second hand-written field registry.

The block boundary now extends to `/internal/transcript/page`, `/patches`, and
`/content`. `transcript_envelopes.schema.json` and `.samples.json` are exported
by the Runtime server example from the actual HTTP wire types and Core types.
Core owns pages, patches, removals, resume cursors and content ranges; Workspace
owns HTTP request envelopes, patch pages and error responses. The exporter
imports those types, rather than copying their fields into another registry.

The two Rust examples share the name `transcript_schema`. The generator isolates
their build outputs below `target/transcript-schema/core` and
`target/transcript-schema/workspace` (or the same suffixes under
`CARGO_TARGET_DIR`) so cached executables cannot be confused. The local gate
exercises repeated generation/checks with two dependency-free Cargo exporters.

Request schemas describe Rust deserialization (including optional fields that
may be omitted); response schemas describe serialization (nullable fields must
still be present). Rust remains the request validator; Python uses the generated
response schemas before its existing semantic checks. This does not add a new
outbound request rejection policy or change HTTP status codes.

The optional `contract-schema` feature is only needed for export; normal Runtime
builds do not require it. The existing generation gate checks all four artifacts.
Authorization, jobs and extension catalogs are outside this change.

Python retains
semantic checks for identities, canonical decimal ranges, UTF-8 byte budgets,
exclusive inline/reference content, ordering and request/response binding.
Schema identifiers, stream identifiers and error disposition are still checked
as semantic values where the authoritative Rust field is a string.
JSON Schema is not the lifecycle state machine and does not replace those tests.
