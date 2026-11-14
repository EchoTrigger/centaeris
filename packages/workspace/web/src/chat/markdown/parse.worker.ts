import { parseMarkdown } from "./parse";

self.onmessage = (event: MessageEvent<{ id: number; revision: number; source: string }>) => {
  const { id, revision, source } = event.data;
  try {
    const { source: _source, ...document } = parseMarkdown(source);
    self.postMessage({ id, revision, document });
  } catch (error) {
    self.postMessage({ id, revision, error: error instanceof Error ? error.message : String(error) });
  }
};
