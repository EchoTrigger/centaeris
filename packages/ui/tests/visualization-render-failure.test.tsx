import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { readFileSync } from "node:fs";
import { afterEach, expect, test, vi } from "vitest";
import { MarkdownContent } from "../src/components/chat/MarkdownContent";
vi.mock("../../visualization-react/src/Renderer", () => {
	throw new Error("Visualization chunk unavailable");
});
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
let renderer: ReactTestRenderer | undefined;
afterEach(() => {
	act(() => renderer?.unmount());
	vi.restoreAllMocks();
});
test("a renderer loading failure keeps surrounding prose and the original visual source", async () => {
	vi.spyOn(console, "error").mockImplementation(() => {});
	const cases = JSON.parse(
		readFileSync(
			new URL("../../core/tests/fixtures/visualizations.json", import.meta.url),
			"utf8",
		),
	);
	const fence = String.fromCharCode(96).repeat(3);
	const text =
		"Before\n\n" +
		fence +
		"centaeris_visual\n" +
		cases[0].source +
		"\n" +
		fence +
		"\n\nAfter";
	await act(async () => {
		renderer = create(<MarkdownContent text={text} allowVisuals />);
	});
	await act(async () => {
		await new Promise((resolve) => setTimeout(resolve, 0));
	});
	const rendered = JSON.stringify(renderer!.toJSON());
	expect(rendered).toContain("Before");
	expect(rendered).toContain("After");
	expect(rendered).toContain("Unable to display visualization");
	expect(renderer!.root.findByType("pre").children.join("")).toContain(
		cases[0].source,
	);
});
