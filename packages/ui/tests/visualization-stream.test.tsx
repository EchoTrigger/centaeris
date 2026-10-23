// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { URL as FileURL } from "node:url";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { MarkdownContent } from "../src/components/chat/MarkdownContent";

const rendererLoads = vi.hoisted(() => ({ count: 0 }));
vi.mock("../src/components/chat/DesktopVisualRenderer", async (importOriginal) => {
  rendererLoads.count += 1;
  return importOriginal();
});

const cases = JSON.parse(readFileSync(new FileURL("../../core/tests/fixtures/visualizations.json", import.meta.url), "utf8"));
const fence = String.fromCharCode(96).repeat(3);
const source = "Before\n\n" + fence + "centaeris_visual\n" + cases[0].source + "\n" + fence + "\n\nAfter";
afterEach(cleanup);

test("a mounted visual completes once and remains mounted when prose is appended", async () => {
  const pending = source.slice(0, source.lastIndexOf(fence));
  const view = render(<MarkdownContent text={source} />);
  expect(view.container.textContent).toContain(cases[0].source);
  expect(rendererLoads.count).toBe(0);
  view.rerender(<MarkdownContent text={fence + 'centaeris_visual\n{"schema":"unsupported"}\n' + fence} allowVisuals />);
  expect(screen.getByText("Original source")).toBeDefined();
  expect(rendererLoads.count).toBe(0);
  view.rerender(<MarkdownContent text={pending} allowVisuals isStreaming />);
  expect(screen.getByRole("status").textContent).toBe("Loading visualization…");
  expect(screen.queryByRole("list")).toBeNull();
  expect(screen.queryByText("Run A")).toBeNull();
  expect(view.container.textContent).not.toContain("elements");
  expect(rendererLoads.count).toBe(0);

  view.rerender(<MarkdownContent text={source} allowVisuals isStreaming />);
  const sequence = await screen.findByRole("list");
  expect(screen.getAllByRole("list")).toHaveLength(1);
  expect(rendererLoads.count).toBe(1);
  expect(within(sequence).getByText("Run A")).toBeDefined();
  expect(within(sequence).getByText("Run B")).toBeDefined();
  const completedSteps = within(sequence).getAllByRole("listitem");
  expect(screen.getByText("Before")).toBeDefined();
  expect(screen.getByText("After")).toBeDefined();
  expect(screen.queryByText("Loading visualization…")).toBeNull();

  view.rerender(<MarkdownContent text={source + " More prose"} allowVisuals isStreaming />);
  expect(screen.getByRole("list")).toBe(sequence);
  expect(screen.getAllByRole("list")).toHaveLength(1);
  expect(rendererLoads.count).toBe(1);
  expect(screen.getByText("After More prose")).toBeDefined();

  view.rerender(<MarkdownContent text={source + " More prose"} allowVisuals />);
  expect(screen.getByRole("list")).toBe(sequence);
  expect(sequence.isConnected).toBe(true);
  expect(within(sequence).getAllByRole("listitem")).toEqual(completedSteps);
});
