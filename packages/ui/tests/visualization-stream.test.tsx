import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { readFileSync } from "node:fs";
import { expect, test } from "vitest";
import { MarkdownContent } from "../src/components/chat/MarkdownContent";
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
test("a mounted visual completes once and remains mounted when prose is appended", async () => {
	globalThis.IS_REACT_ACT_ENVIRONMENT = true;
	const pending = source.slice(0, source.lastIndexOf(fence));
	let renderer: ReactTestRenderer;
	await act(async () => {
		renderer = create(
			<MarkdownContent text={pending} allowVisuals isStreaming />,
		);
	});
	expect(JSON.stringify(renderer!.toJSON())).not.toContain("elements");
	await act(async () => {
		renderer!.update(
			<MarkdownContent text={source} allowVisuals isStreaming />,
		);
		await import("../../visualization-react/src/Renderer");
	});
	await act(async () => {
		await new Promise((resolve) => setTimeout(resolve, 0));
	});
	const sequence = renderer!.root.findByType("ol");
	expect(JSON.stringify(renderer!.toJSON())).toContain("Run A");
	await act(async () => {
		renderer!.update(
			<MarkdownContent
				text={source + " More prose"}
				allowVisuals
				isStreaming
			/>,
		);
	});
	expect(renderer!.root.findByType("ol")).toBe(sequence);
	expect(JSON.stringify(renderer!.toJSON())).toContain("More prose");
	await act(async () => {
		renderer!.unmount();
	});
});
