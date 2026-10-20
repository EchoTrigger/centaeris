import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createJsxSubjectLoader } from "./jsxSubject.mjs";

const loader = createJsxSubjectLoader();
const { MarkdownContent } = await import(await loader(fileURLToPath(new URL("../../src/chat/MarkdownContent.jsx", import.meta.url))));
const cases = JSON.parse(readFileSync(new URL("../../../core/tests/fixtures/visualizations.json", import.meta.url), "utf8"));
const fence = String.fromCharCode(96).repeat(3);
const source = "Before\n\n" + fence + "centaeris_visual\n" + cases[0].source + "\n" + fence + "\n\nAfter";
test("Web enables visual answers explicitly and preserves ordinary literal Markdown", () => {
  const markup = renderToStaticMarkup(createElement(MarkdownContent, { text: source, allowVisuals: true }));
  assert.match(markup, /Before/);
  assert.match(markup, /After/);
  assert.doesNotMatch(markup, /&quot;elements&quot;/);
  const literal = renderToStaticMarkup(createElement(MarkdownContent, { text: source }));
  assert.match(literal, /&quot;elements&quot;/);
});
test("Web keeps incomplete source inspectable after interruption", () => {
  const pending = source.slice(0, source.lastIndexOf(fence));
  const stream = renderToStaticMarkup(createElement(MarkdownContent, { text: pending, allowVisuals: true, isStreaming: true }));
  assert.doesNotMatch(stream, /&quot;elements&quot;/);
  const final = renderToStaticMarkup(createElement(MarkdownContent, { text: pending, allowVisuals: true }));
  assert.match(final, /&quot;elements&quot;/);
  assert.match(final, /Incomplete visualization/);
});
