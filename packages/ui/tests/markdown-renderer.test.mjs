import assert from "node:assert/strict";
import { createElement, Fragment } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { test } from "vitest";
import { renderMarkdownNodes } from "../src/components/MarkdownRenderer.tsx";

test("renders the shared Markdown grammar for bold text, quotes, and tables", () => {
  const markup = renderToStaticMarkup(
    createElement(
      Fragment,
      null,
      renderMarkdownNodes(
        "## 标题\n\n**粗体**\n\n> 蓝色引用\n\n| 名称 | 值 |\n| --- | --- |\n| Centaeris | 1 |",
      ),
    ),
  );

  assert.match(markup, /<h2>标题<\/h2>/);
  assert.match(markup, /<strong>粗体<\/strong>/);
  assert.match(markup, /<blockquote class="markdown-blockquote">蓝色引用<\/blockquote>/);
  assert.match(markup, /<table>/);
});
