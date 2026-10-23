import { expect, test } from "vitest";
import cases from "./fixtures/visualization-conformance.json";
import {
	parseVisual,
	visualBlocks,
	visualText,
} from "../../visualization-react/src/contract";

test("browser validates the same examples and preserves Core reading order", () => {
	for (const fixture of cases) {
		if (fixture.valid) {
			expect(visualText(parseVisual(fixture.source)), fixture.name).toBe(
				fixture.text,
			);
		} else expect(() => parseVisual(fixture.source), fixture.name).toThrow();
	}
});
test("only top-level visual fences are extracted, including interrupted streams", () => {
	const fence = String.fromCharCode(96).repeat(3);
	const active = fence + "centaeris_visual\n" + cases[0].source + "\n" + fence;
	expect(visualBlocks(active)[0].complete).toBe(true);
	expect(visualBlocks(active.slice(0, -3))[0].complete).toBe(false);
	for (const source of [
		fence +
			String.fromCharCode(96) +
			"text\n" +
			active +
			"\n" +
			fence +
			String.fromCharCode(96),
		active
			.split("\n")
			.map((line) => "> " + line)
			.join("\n"),
		active
			.split("\n")
			.map((line) => "    " + line)
			.join("\n"),
		"- Example:\n\n" +
			active
				.split("\n")
				.map((line) => "  " + line)
				.join("\n"),
	]) {
		expect(visualBlocks(source)).toEqual([]);
	}
});
test("literal strings escape safely and executable json-render fields are rejected", () => {
	const value = JSON.parse(cases[0].source);
	value.spec.elements.root.props.title =
		"<script>globalThis.pwned=true</script>";
	expect(
		parseVisual(JSON.stringify(value)).spec.elements.root.props.title,
	).toContain("<script>");
	value.spec.elements.root.visible = { $state: "/token" };
	expect(() => parseVisual(JSON.stringify(value))).toThrow();
});
