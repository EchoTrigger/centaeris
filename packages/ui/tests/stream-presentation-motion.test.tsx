// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { AgentResultStream } from "../src/components/chat/AgentResultStream";
import { useStreamPresentation } from "../src/useStreamPresentation";

beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false }));
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });
function Answer({ text, live }: { text: string; live: boolean }) {
  return <p data-testid="answer">{useStreamPresentation(text, live)}</p>;
}

test("real Motion frames preserve complete graphemes and drain the terminal answer", async () => {
  const grapheme = "👩‍💻";
  const target = grapheme.repeat(100);
  const { rerender } = render(<Answer text={target} live />);
  const answer = screen.getByTestId("answer");
  expect(answer.textContent).toBe(grapheme);
  await waitFor(() => expect((answer.textContent ?? "").length).toBeGreaterThan(grapheme.length));
  expect((answer.textContent ?? "").length % grapheme.length).toBe(0);
  rerender(<Answer text={target} live={false} />);
  await waitFor(() => expect(answer.textContent).toBe(target));
});

test("corrections cancel real Motion presentation so stale text cannot reappear", async () => {
  const { rerender } = render(<Answer text={"draft ".repeat(100)} live />);
  rerender(<Answer text="correct answer" live />);
  const answer = screen.getByTestId("answer");
  expect(answer.textContent).toBe("correct answer");
  await new Promise(resolve => setTimeout(resolve, 50));
  expect(answer.textContent).toBe("correct answer");
});

test("the real message renderer reveals live text before completion and drains it afterward", async () => {
  const target = "Streaming response ".repeat(40);
  const turn = { id: "live-render", chunks: [], finalAnswer: target, isStreaming: true };
  const view = render(<AgentResultStream turn={turn} />);
  expect(view.container.textContent?.length).toBeGreaterThan(0);
  expect(view.container.textContent).not.toContain(target);
  await waitFor(() => expect((view.container.textContent ?? "").length).toBeGreaterThan(1));
  view.rerender(<AgentResultStream turn={{ ...turn, isStreaming: false }} />);
  await waitFor(() => expect(view.container.textContent).toContain(target.trimEnd()), { timeout: 3_000 });
});
