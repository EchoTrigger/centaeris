import type { MarkdownDocument } from "./parse";

export type ParseResult = { source: string; document: MarkdownDocument | null; error: string | null };
type Client = { source: string; revision: number; error: string | null; receive(result: ParseResult): void };
type Task = { id: number; revision: number; source: string };
type Reply = { id: number; revision: number; document?: Omit<MarkdownDocument, "source">; error?: string };
const clients = new Map<number, Client>();
const pending = new Map<number, Task>();
let nextId = 0;
let worker: Worker | null = null;
let active: Task | null = null;

function fail(message: string) {
  const affected = new Set([...pending.keys(), ...(active ? [active.id] : [])]);
  worker?.terminate();
  worker = null;
  active = null;
  pending.clear();
  for (const id of affected) {
    const client = clients.get(id);
    if (client) {
      client.error = message;
      client.receive({ source: client.source, document: null, error: message });
    }
  }
}

function pump() {
  if (active || !pending.size) return;
  const [id, task] = pending.entries().next().value!;
  pending.delete(id);
  active = task;
  try {
    if (!worker) {
      worker = new Worker(new URL("./parse.worker.ts", import.meta.url), { type: "module" });
      const instance = worker;
      worker.onmessage = (event: MessageEvent<Reply>) => {
        if (worker !== instance) return;
        const task = active;
        if (!task || event.data.id !== task.id || event.data.revision !== task.revision) {
          fail("Markdown parser worker returned another request identity");
          return;
        }
        active = null;
        const client = clients.get(task.id);
        if (client) {
          const { document, error } = event.data;
          if (error !== undefined) {
            client.error = error;
            pending.delete(task.id);
            client.receive({ source: client.source, document: null, error });
          } else if (document && client.source.startsWith(task.source)) {
            client.receive({ source: task.source, document: { ...document, source: task.source }, error: null });
          }
        }
        pump();
      };
      worker.onerror = event => { if (worker === instance) fail(event.message || "Markdown parser worker failed"); };
      worker.onmessageerror = () => { if (worker === instance) fail("Markdown parser worker response could not be read"); };
    }
    worker.postMessage(task);
  } catch (error) { fail(error instanceof Error ? error.message : String(error)); }
}

// One shared worker, one active parse and only the latest waiting text per view.
export function registerMarkdownParser(receive: Client["receive"]) {
  const id = nextId++;
  const client: Client = { source: "", revision: 0, error: null, receive };
  clients.set(id, client);
  return {
    update(source: string) {
      client.source = source;
      if (client.error !== null) {
        client.receive({ source, document: null, error: client.error });
        return;
      }
      pending.set(id, { id, revision: ++client.revision, source });
      pump();
    },
    close() {
      clients.delete(id);
      pending.delete(id);
      if (!clients.size) {
        worker?.terminate();
        worker = null;
        active = null;
      }
    },
  };
}
