import { readFileSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, test } from "vitest";
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
