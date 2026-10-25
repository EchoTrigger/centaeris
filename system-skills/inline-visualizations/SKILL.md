---
name: inline-visualizations
description: Explain ordered processes, status, ownership and nested relationships with portable inline diagrams in assistant answers when a diagram improves understanding. Use ordinary prose for simple answers; this skill does not build application UI or generate images.
---

# Inline visualizations

Make relationships easier to read while preserving the same information in Desktop, Web and TUI.

## Choose when a diagram helps

Use a visual when order, containment, ownership or several related states would be harder to follow in prose. A short answer, a single fact or a simple instruction usually needs no diagram. Follow the user's requested format and language. Use facts established by the conversation or inspected sources; do not invent success states, resources or relationships.

Before emitting a visual, read [the generated contract and examples](references/catalog.md). It is generated from Core's validator and contains the supported schema, components, properties, icons and budgets.

## Organize the explanation

- Use `Sequence` only when order matters. Keep reading order vertical and consistent with the surrounding answer.
- Nest `Group` for actual containment or ownership. Use sibling groups for peers. `Columns` expresses peers, but a client may stack them vertically.
- Use `Card` for one coherent explanation, not for every sentence. Use `Text` for supporting detail and `Status` for a state with an explicit label. Add `Divider` only where it distinguishes meaningful sections.
- Keep titles short and descriptions useful. Avoid repeated headings, decorative numbering and unnecessary nesting. Icons are optional decoration; labels must carry the meaning without them.

## Emit portable content

Write a brief introduction when needed, then a complete top-level `centaeris_visual` fenced block containing one valid JSON envelope. Surrounding prose remains ordinary Markdown. Do not wrap a visual in another code fence or a blockquote unless the user wants a literal example.

Use the exact envelope and flat element references in the generated examples. Every node has explicit `type`, literal string `props` and ordered `children`; leaves have an empty array. Every referenced node must exist and have one parent, and every node must be reachable from the root. Keep within the generated limits.

Use only catalog properties and enum values. Do not add HTML, JavaScript, CSS, event handlers, executable expressions, arbitrary SVG, external assets or interactive actions. Presentation belongs to the client; do not encode theme colors, spacing, connectors or desktop-specific layout options in the payload.

Check the JSON and relationships before finishing the block. Preserve all important names, ordering and status meaning as text so TUI can project the same tree. Do not emit a second alternative TUI payload or claim a process completed merely because it is shown in a diagram.
