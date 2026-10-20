# Inline visualizations

Design for implementation. Runtime, React and TUI support remains pending.

Assistant answers need a shared way to explain sequences, groups and state
without losing information when a conversation moves between Desktop, Web and
TUI. The initial scope is compact cards, status rows, ordered flows and nested
groups. Desktop and Web use json-render with a shared React registry; TUI derives
readable text from the same component tree.

## Content format

An assistant answer may interleave ordinary Markdown and fenced
`centaeris_visual` blocks. Each block contains one versioned envelope:

```json
{
  "schema": "centaeris.visual.v1",
  "spec": {
    "root": "flow",
    "elements": {
      "flow": {
        "type": "Sequence",
        "props": { "title": "Request flow" },
        "children": ["validate", "execute"]
      },
      "validate": {
        "type": "Text",
        "props": { "text": "Validate the request", "role": "body" },
        "children": []
      },
      "execute": {
        "type": "Text",
        "props": { "text": "Execute the task", "role": "body" },
        "children": []
      }
    }
  }
}
```

The `spec` uses json-render's flat `root` and `elements` structure. Component names
are registry identifiers; public field names use exact camelCase. `children`
contains ordered element IDs, including an empty array for leaves. Every element
must belong to the tree rooted at `root`, and each non-root element has exactly
one parent. Missing references, duplicate ownership, cycles and unreachable
nodes are invalid.

The initial grammar accepts literal properties from the component catalog.
Actions, expressions, state bindings, conditional visibility, named slots,
external resources and arbitrary HTML or styling are outside this version's
accepted grammar. Unsupported versions, components and fields produce an
explicit error with access to the original block; they gain no aliases.

Initial input budgets are 32 KiB of UTF-8 JSON, 128 elements, eight element levels
and 8 KiB per text property. Validation checks these budgets before mounting the
renderer. Budget changes require corresponding contract and conformance updates.

## Component semantics

| Component | Accepted properties | React presentation | TUI projection |
| --- | --- | --- | --- |
| `Card` | Optional `title` and `description` strings | Outer card | Title, description and indented children |
| `Group` | Required `title`, optional `description` | Nested group | Named subtree with every child |
| `Sequence` | Optional `title` | Ordered children with connectors | Ordered steps and explicit direction |
| `Columns` | Optional `title` | Responsive sibling columns | Sibling groups in source order |
| `Text` | Required `text`; `role` is `title`, `body` or `caption` | Text with the selected role | Complete text with appropriate emphasis |
| `Status` | Required `label`; `tone` is `neutral`, `info`, `success`, `warning` or `error` | Label with a status icon | Label with an explicit tone marker |
| `Badge` | Required `text` | Compact label | Label text |
| `Icon` | Required catalog `name` | Decorative Lucide icon | Optional decorative symbol |
| `Divider` | No properties | Section separator | Section break |

`Card`, `Group`, `Sequence` and `Columns` accept children. Other components are
leaves. Columns indicate presentation alongside siblings; they imply no
concurrency or execution order. Sequence order and group membership remain
meaningful in both renderers. Decorative icons cannot be the only carrier of
information; adjacent text must state their meaning.

The icon catalog contains `checkCircle`, `circleAlert`, `hardDrive`, `gitBranch`
and `arrowDown`. A fixed mapping selects the corresponding Lucide components;
model input cannot choose module paths or arbitrary SVG. Client themes own
spacing, colors, typography and responsive layout.

Status labels are assistant-authored explanatory content. Actual execution
status remains authoritative in Runtime events and receipts. A visual status
never updates a Run, Tool, Environment or resource ledger.

## Ownership and integration

Core owns the versioned format, component semantics, validation outcomes and
canonical text projection. It exports JSON Schema, TypeScript types and catalog
metadata. Generated artifacts and shared fixtures keep Rust and browser
acceptance aligned. Core retains pure Rust dependencies and no React runtime.

A shared `packages/visualization-react` package owns the json-render wrapper,
registry, theme styles, generated browser contract and block error boundary.
Both `packages/ui`, used by Desktop, and `packages/web` consume that package.
Its public interface takes a visual spec and presentation state; session
transport and hosted authentication remain in their existing owners.

TUI calls the Core parser and text projection, then adapts the resulting logical
lines to Ratatui styles and terminal width. It preserves all titles,
descriptions, labels, ordered relationships and group membership. Text remains
available through copying and narrow terminal layouts. It uses the spec itself,
not a separately generated model summary, as its information source.

The React adapter validates against the generated contract and matching graph
rules before passing the inner spec to json-render. Shared conformance fixtures
cover both accepted trees and every rejection category. Catalog definitions,
model instructions and registry coverage are checked against generated metadata
rather than maintained as independent component inventories.

Implementation pins tested json-render and validation dependencies in the root
pnpm lockfile, records applicable license notices and verifies their peer
requirements against both React applications. The shared package is registered
with the existing pnpm workspace and product checks.

## Message handling and persistence

Only assistant answer content enables visual blocks. User messages, reasoning,
raw tool output and quoted code examples retain their existing presentation.
Markdown parsing must honor real fence boundaries, indentation and nested
literal examples instead of matching a substring throughout the message.

The original Markdown, including the envelope and spec, remains authoritative
assistant text in the existing message and transcript stores. Inline content
and referenced content use the same extraction and validation behavior.
Restart, pagination, reconnect and history replay reconstruct visuals from that
source. The visual feature does not increase existing transcript inline or
transport budgets; content references continue to carry larger answers.

Rendering diagnostics are local to the affected block. Invalid data produces a
visible reason and a way to inspect or copy the original content while the
surrounding answer remains readable. Copy and export preserve both a readable
text representation and access to the original Markdown and JSON.

This content extension uses the existing assistant text records. Enabling it
requires no rewrite of historical records. Future changes to released persistent
schemas require the project's explicit migration or reconstruction process.

## Streaming behavior

Ordinary prose continues to stream through the existing transport. A visual
block has a pending presentation while its fence is open. It becomes visible
once the closing fence has arrived and the complete envelope passes validation.
A finalized or cancelled answer with an unfinished block retains the source and
reports that the visual is incomplete.

Completed blocks have stable identities within their source message. Appended
prose does not remount them or repeat their parsing. History replay uses the
same completed-block path. The React renderer loads when visual content is
present and retains completed render results across subsequent prose updates.

The initial implementation commits complete blocks. Progressive node updates
can use json-render SpecStream after a separate review of partial references,
interruption, replay and validation behavior. Existing message streaming remains
the transport owner; visual rendering does not start another generation request.

## Model instructions

Generate the supported vocabulary and property descriptions from the same
catalog metadata used for validation. Add concise examples for a Session with
two Runs and for a resource-control hierarchy. The response instructions ask for
visuals when ordered relationships, containment or state explanations benefit
from them; prose remains available for the surrounding explanation.

The model emits one portable format for every client. A conversation opened
later in TUI retains the information originally generated for Desktop or Web.
Format errors remain inspectable rather than silently requesting another model
completion or changing the answer's meaning.

## Acceptance requirements

Add focused failing behavior tests before implementation. Protect each completed
boundary before expanding scope, following [Testing](Testing.md).

- Use shared fixtures representing both reference layouts: consecutive Runs
  with retained explanatory context, and a layered resource diagram with two
  sibling task groups. Assert node text, ordering and containment in React and
  the TUI projection.
- Exercise complete, fragmented and interrupted visual blocks mixed with prose,
  several visuals in one answer, literal examples and ordinary fenced code.
- Reject unknown versions, fields, components and icons; invalid property types;
  duplicate JSON object keys; broken references; cycles; multiple parents;
  unreachable elements; and every input budget overflow. Rust and browser
  validators must agree on the fixtures, including graph rules.
- Observe reload, restart, pagination, referenced-content loading and reconnect
  recovery. Confirm original source, block identity and ordering survive.
- Exercise actual Desktop and Web views in light and dark themes with long CJK
  labels and narrow available space. Confirm readable wrapping, containment,
  text selection, copying and assistive-technology reading order. Status meaning
  must remain available without color or an icon.
- Exercise TUI resizing and Unicode text. Confirm every semantic value and
  relationship remains readable and copyable without a browser or network.
- Check generated schemas, types, catalog metadata and registry coverage. Verify
  a representative behavioral regression is detected, beyond artifact freshness.
- Run directly affected Core, TUI, React and message recovery tests, both React
  builds, the focused Core `query_loop` gate for Rust implementation changes,
  and applicable [ReleaseGate](../eval/ReleaseGate.md) checks before acceptance.

Implementation commits must report their actual checks and results. This design
establishes acceptance criteria; renderer behavior, cross-client recovery,
performance and target-surface acceptance remain to be demonstrated.

## References

- [json-render Specs](https://json-render.dev/docs/specs): flat spec structure.
- [json-render Catalog](https://json-render.dev/docs/catalog): allowed vocabulary.
- [json-render Registry](https://json-render.dev/docs/registry): React mappings.
- [json-render Streaming](https://json-render.dev/docs/streaming): SpecStream.
- [Runtime boundaries](RuntimeBoundaries.md): shared semantics and adapters.
- [Testing](Testing.md): behavioral coverage and generated-contract checks.
