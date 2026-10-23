// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { URL as FileURL } from "node:url";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { MarkdownContent } from "../src/components/chat/MarkdownContent";

vi.mock("../src/components/chat/DesktopVisualRenderer", () => {
  throw new Error("Desktop visualization chunk unavailable");
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

test("a renderer loading failure keeps surrounding prose and the original visual source", async () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  const cases = JSON.parse(readFileSync(new FileURL("../../core/tests/fixtures/visualizations.json", import.meta.url), "utf8"));
  const fence = String.fromCharCode(96).repeat(3);
  const visualSource = fence + "centaeris_visual\n" + cases[0].source + "\n" + fence;
  render(<MarkdownContent text={"Before\n\n" + visualSource + "\n\nAfter"} allowVisuals />);

  const failure = await screen.findByText("Unable to display visualization");
  expect(failure.getAttribute("role")).toBe("status");
  const before = screen.getByText("Before");
  const after = screen.getByText("After");
  expect(before.compareDocumentPosition(failure) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(failure.compareDocumentPosition(after) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(screen.queryByText("Loading visualization…")).toBeNull();

  const disclosure = screen.getByText("Original source");
  await userEvent.setup().click(disclosure);
  expect(disclosure.parentElement?.hasAttribute("open")).toBe(true);
  const original = disclosure.parentElement?.querySelector("pre");
  expect(original?.textContent?.trim()).toBe(visualSource);
  expect(screen.getByText("Before")).toBe(before);
  expect(screen.getByText("After")).toBe(after);
});
