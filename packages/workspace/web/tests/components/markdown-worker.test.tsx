// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { MarkdownContent } from "../../src/chat/MarkdownContent";
import { parseMarkdown } from "../../src/chat/markdown/parse";

type Request = { id: number; revision: number; source: string };
class ParserWorker {
  static instances: ParserWorker[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
  requests: Request[] = [];
  terminate = vi.fn();
  constructor() { ParserWorker.instances.push(this); }
  postMessage(request: Request) { this.requests.push(request); }
  reply(index: number) {
    const { id, revision, source } = this.requests[index];
    const { source: _source, ...document } = parseMarkdown(source);
    this.onmessage?.({ data: { id, revision, document } });
  }
}
vi.mock("../../src/i18n", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
beforeEach(() => { ParserWorker.instances = []; vi.stubGlobal("Worker", ParserWorker); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
const long = "# Long reply\n\n" + "A long paragraph. ".repeat(4000);

test("long live messages share one worker and replace waiting text with the latest complete input", async () => {
  const view = render(<MarkdownContent text={long} isStreaming />);
  const worker = ParserWorker.instances[0];
  expect(worker.requests).toHaveLength(1);
  expect(view.container.textContent).toBe(long);
  view.rerender(<MarkdownContent text={long + "First update"} isStreaming />);
  view.rerender(<MarkdownContent text={long + "Latest update"} isStreaming />);
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
  expect(worker.requests).toHaveLength(1);
  await act(async () => worker.reply(0));
  await waitFor(() => expect(worker.requests).toHaveLength(2));
  expect(worker.requests[1].source).toBe(long + "Latest update");
  expect(screen.getByRole("heading", { name: "Long reply" })).toBeTruthy();
  await act(async () => worker.reply(1));
  expect(view.container.textContent).toContain("Latest update");
  expect(view.container.querySelector('[aria-busy="true"]')).toBeNull();
});

test("a corrected final message is never replaced by a late worker result", async () => {
  const view = render(<MarkdownContent text={long} isStreaming />);
  const worker = ParserWorker.instances[0];
  view.rerender(<MarkdownContent text="## Correct final answer" />);
  expect(screen.getByRole("heading", { name: "Correct final answer" })).toBeTruthy();
  expect(worker.terminate).toHaveBeenCalledOnce();
  await act(async () => worker.reply(0));
  expect(screen.getByRole("heading", { name: "Correct final answer" })).toBeTruthy();
  expect(view.container.textContent).not.toContain("Long reply");
});

test("multiple large views have bounded worker ownership and release it when the last view unmounts", async () => {
  const first = render(<MarkdownContent text={long} />);
  const second = render(<MarkdownContent text={long + "Another view"} />);
  expect(ParserWorker.instances).toHaveLength(1);
  const worker = ParserWorker.instances[0];
  expect(worker.requests).toHaveLength(1);
  first.unmount();
  expect(worker.terminate).not.toHaveBeenCalled();
  await act(async () => worker.reply(0));
  expect(worker.requests).toHaveLength(2);
  second.unmount();
  expect(worker.terminate).toHaveBeenCalledOnce();
});

test("worker failure preserves the current escaped source and reports a local rendering error", async () => {
  const source = long + "\n<script>untrusted</script>";
  const view = render(<MarkdownContent text={source} />);
  await act(async () => ParserWorker.instances[0].onerror?.({ message: "Worker failed" }));
  expect(view.container.querySelector("script")).toBeNull();
  expect(view.container.textContent).toContain(source);
  expect(screen.getByRole("status").textContent).toBe("markdown.renderError");
  expect(view.container.querySelector('[aria-busy="true"]')).toBeNull();
  expect(ParserWorker.instances[0].terminate).toHaveBeenCalledOnce();
  view.rerender(<MarkdownContent text={source + "\nA later update"} isStreaming />);
  await waitFor(() => expect(view.container.textContent?.includes("A later update")).toBe(true));
  expect(view.container.querySelector('[aria-busy="true"]')).toBeNull();
  expect(screen.getByRole("status").textContent).toBe("markdown.renderError");
  expect(ParserWorker.instances).toHaveLength(1);
  expect(ParserWorker.instances[0].requests).toHaveLength(1);
});
