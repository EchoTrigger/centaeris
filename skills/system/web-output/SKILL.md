---
name: web-output
description: Format diagrams, mathematics and rich answers in Centaeris Workspace Web, which renders Markdown GFM, mermaid fenced code, KaTeX formulas and centaeris_visual inline. Use when the user asks to draw or show a diagram, explain with a visual, write a formula, or format an answer with these capabilities.
---

# Workspace Web output

The answer surface renders the formats below directly from assistant Markdown. For an inline diagram or formula, emit the supported syntax in the answer. Do not probe for or install diagram CLIs, browsers or rendering dependencies to display it. Use rendering tools to produce a file when the user requests an export, download or file deliverable.

## Markdown and code

Use ordinary Markdown, including GFM tables, task lists, strikethrough and footnotes. Give code fences their actual language for syntax highlighting.

## Mermaid diagrams

Use a complete top-level fenced block with the exact language identifier `mermaid`. The Web client renders it and provides diagram/source views.

```mermaid
flowchart LR
  Request[Request] --> Answer[Answer]
```

Use the diagram syntax the user asks for. For example, `mindmap` is a Mermaid diagram keyword inside a `mermaid` fence, not the fence language. Keep node labels and relationships faithful to the request.

## Mathematics

Use `\(E = mc^2\)` for inline KaTeX. For a display equation, put `$$` on separate lines:

$$
E = mc^2
$$

Single-dollar delimiters remain literal text, so currency amounts such as $5 do not become formulas.

## Portable visual explanations

For ordered processes, status, ownership or nested relationships expressed as `centaeris_visual`, read [the inline-visualizations skill](/opt/centaeris/system-skills/inline-visualizations/SKILL.md) and its generated contract before emitting the block. This format also preserves its information in Desktop and TUI. Follow an explicit request for Mermaid with Mermaid.
