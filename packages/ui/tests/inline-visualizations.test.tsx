import DesktopVisualRenderer from "../src/components/chat/DesktopVisualRenderer";
import { previewExamples } from "../src/visualizationPreviewExamples";
import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
import { VisualMarkdown } from "@centaeris/visualization-react";
import { MarkdownContent } from "../src/components/chat/MarkdownContent";
import { VisualRenderer } from "../../visualization-react/src/Renderer";
import { parseVisual } from "../../visualization-react/src/contract";
const cases = JSON.parse(
	readFileSync(
		new URL("../../core/tests/fixtures/visualizations.json", import.meta.url),
		"utf8",
	),
);
const fence = String.fromCharCode(96).repeat(3);
const source =
	"Before\n\n" +
	fence +
	"centaeris_visual\n" +
	cases[0].source +
	"\n" +
	fence +
	"\n\nAfter";
test("json-render registry preserves ordered content and escapes literal text", () => {
	const visual = parseVisual(cases[0].source);
	const markup = renderToStaticMarkup(
		createElement(VisualRenderer, { visual }),
	);
	expect(markup).toContain("<ol");
	expect(markup).toContain("Run A");
	expect(markup).toContain("Run B");
	expect(markup).not.toContain("&quot;elements&quot;");
	visual.spec.elements.root.props.title = "<script>alert(1)</script>";
	const escaped = renderToStaticMarkup(
		createElement(VisualRenderer, { visual }),
	);
	expect(escaped).toContain("&lt;script&gt;");
	expect(escaped).not.toContain("<script>");
});
test("only opted-in assistant Markdown loads visuals, preserving surrounding prose", () => {
	const markup = renderToStaticMarkup(
		createElement(MarkdownContent, { text: source, allowVisuals: true }),
	);
	expect(markup).toContain("Before");
	expect(markup).toContain("After");
	expect(markup).not.toContain("&quot;elements&quot;");
	const literal = renderToStaticMarkup(
		createElement(MarkdownContent, { text: source }),
	);
	expect(literal).toContain("&quot;elements&quot;");
});
test("pending JSON stays hidden until completion and interrupted content remains inspectable", () => {
	const pending = source.slice(0, source.lastIndexOf(fence));
	const streaming = renderToStaticMarkup(
		createElement(MarkdownContent, {
			text: pending,
			allowVisuals: true,
			isStreaming: true,
		}),
	);
	expect(streaming).not.toContain("&quot;elements&quot;");
	const interrupted = renderToStaticMarkup(
		createElement(MarkdownContent, { text: pending, allowVisuals: true }),
	);
	expect(interrupted).toContain("&quot;elements&quot;");
	expect(interrupted).toContain("Incomplete");
});


test("a host can present validated visuals while other hosts keep their default presentation", () => {
  let calls = 0;
  const renderVisual = () => {
    calls += 1;
    return createElement("article", { "aria-label": "Host presentation" }, "Rendered by host");
  };
  const custom = renderToStaticMarkup(createElement(VisualMarkdown, {
    text: source, finalized: true, renderMarkdown: (text) => createElement("p", null, text), renderVisual,
  }));
  expect(calls).toBe(1);
  expect(custom).toContain('aria-label="Host presentation"');
  expect(custom.indexOf("Before")).toBeLessThan(custom.indexOf("Rendered by host"));
  expect(custom.indexOf("Rendered by host")).toBeLessThan(custom.indexOf("After"));
  const ordinary = renderToStaticMarkup(createElement(VisualMarkdown, {
    text: source, finalized: true, renderMarkdown: (text) => createElement("p", null, text),
  }));
  expect(ordinary).toContain("Loading visualization");
  expect(calls).toBe(1);
  const pending = source.slice(0, source.lastIndexOf(fence));
  renderToStaticMarkup(createElement(VisualMarkdown, {
    text: pending, finalized: false, renderMarkdown: (text) => text, renderVisual,
  }));
  const invalid = renderToStaticMarkup(createElement(VisualMarkdown, {
    text: fence + 'centaeris_visual\n{"schema":"unsupported"}\n' + fence,
    finalized: true, renderMarkdown: (text) => text, renderVisual,
  }));
  expect(calls).toBe(1);
  expect(invalid).toContain("Original source");
});


test("Desktop library components retain every visible value in the accepted reference examples", () => {
  const escapeText = (value: string) => value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#x27;");
  for (const example of previewExamples) {
    const visual = parseVisual(example.source);
    const markup = renderToStaticMarkup(createElement(DesktopVisualRenderer, { visual }));
    for (const element of Object.values(visual.spec.elements)) {
      for (const key of ["title", "description", "label", "text"]) {
        const value = element.props[key];
        if (value !== undefined) expect(markup, example.id + ": " + value).toContain(escapeText(value));
      }
    }
  }
});
