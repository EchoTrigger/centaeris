// @vitest-environment jsdom
import fixtures from "../../core/tests/fixtures/visualizations.json";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { VisualMarkdown } from "@centaeris/visualization-react";
afterEach(() => { cleanup(); vi.restoreAllMocks(); });
test("a synchronous host presentation failure is isolated from surrounding prose", async () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  const fixture = fixtures[0];
  const fence = String.fromCharCode(96).repeat(3);
  const text = "Before\n\n" + fence + "centaeris_visual\n" + fixture.source + "\n" + fence + "\n\nAfter";
  render(<VisualMarkdown text={text} finalized renderMarkdown={(source) => <p>{source}</p>}
    renderVisual={() => { throw new Error("Host presentation failed"); }} />);
  await screen.findByText("Unable to display visualization");
  expect(screen.getByText("Before")).toBeTruthy();
  expect(screen.getByText("After")).toBeTruthy();
  expect(screen.getByText("Original source").parentElement?.textContent).toContain(fixture.source);
});
