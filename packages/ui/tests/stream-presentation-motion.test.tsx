// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
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
