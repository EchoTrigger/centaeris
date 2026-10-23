import { ApiError } from "../api.ts";
import { abortableDelay } from "./abortableDelay.ts";

type Range = Readonly<{ content: string; startOffset: string; endOffset: string; hasMore: boolean }>;
type Snapshot = Readonly<{ content: string; loading: boolean; error: boolean; unavailable: boolean; hasMore: boolean }>;

// Tool output can be arbitrarily large. Stop automatic continuation once this
// many bytes have been read; the caller still holds what was already loaded.
const MAX_ACCUMULATED_BYTES = 16 * 1024 * 1024;

function retryable(error: unknown) {
  return error instanceof TypeError
    || (error instanceof ApiError && [429, 500, 502, 503, 504].includes(error.status));
}

function waitUntilOnline(signal: AbortSignal) {
  signal.throwIfAborted();
  if (typeof window === "undefined" || navigator.onLine !== false) return Promise.resolve();
  return new Promise<void>((resolve, reject) => {
    const cleanup = () => {
      window.removeEventListener("online", onOnline);
      signal.removeEventListener("abort", onAbort);
    };
    const onOnline = () => {
      if (navigator.onLine === false) return;
      cleanup(); resolve();
    };
    const onAbort = () => { cleanup(); reject(signal.reason); };
    window.addEventListener("online", onOnline);
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

export class TranscriptContentReader {
  private snapshot: Snapshot = { content: "", loading: false, error: false, unavailable: false, hasMore: true };
  private readonly listeners = new Set<() => void>();
  private readonly controller = new AbortController();
  private endOffset = "0";
  private pending: Promise<void> | null = null;
  private readonly read: (offset: string, signal: AbortSignal) => Promise<Range>;

  constructor(read: (offset: string, signal: AbortSignal) => Promise<Range>) {
    this.read = read;
  }

  getSnapshot = () => this.snapshot;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };
  dispose() { this.controller.abort(); this.listeners.clear(); }
  private publish(update: Partial<Snapshot>) {
    if (this.controller.signal.aborted) return;
    this.snapshot = { ...this.snapshot, ...update };
    this.listeners.forEach((listener) => listener());
  }

  loadMore(): Promise<void> {
    if (this.pending) return this.pending;
    if (this.snapshot.loading || this.snapshot.error || this.controller.signal.aborted || !this.snapshot.hasMore) return Promise.resolve();
    const pending = this.loadPage().finally(() => {
      if (this.pending === pending) this.pending = null;
    });
    this.pending = pending;
    return pending;
  }

  private async loadPage() {
    if (BigInt(this.endOffset) >= BigInt(MAX_ACCUMULATED_BYTES)) {
      this.publish({ hasMore: false });
      return;
    }
    this.publish({ loading: true, error: false });
    const signal = this.controller.signal;
    // Each page gets an initial attempt plus five retries, matching the stream's
    // 500/1000/2000/4000/8000 ms backoff. Only a successful page resets the budget;
    // exhaustion stops this reader until the view is recreated.
    let retries = 0;
    try {
      let page: Range;
      while (true) {
        if (typeof navigator !== "undefined" && navigator.onLine === false) await waitUntilOnline(signal);
        if (signal.aborted) return;
        try {
          page = await this.read(this.endOffset, signal);
          break;
        } catch (error) {
          if (signal.aborted) return;
          const transient = retryable(error);
          if (!transient || retries === 5) {
            this.publish({ loading: false, error: true, unavailable: !transient });
            return;
          }
          await abortableDelay(500 * 2 ** retries++, signal);
        }
      }
      if (signal.aborted) return;
      if (page.startOffset !== this.endOffset
        || (page.hasMore && BigInt(page.endOffset) <= BigInt(this.endOffset))) {
        throw new Error("transcript content continuation made no progress");
      }
      this.endOffset = page.endOffset;
      // Completion subscribers may immediately request the next page. Retire
      // this request before notifying them that loading has finished.
      this.pending = null;
      this.publish({
        content: this.snapshot.content + page.content,
        hasMore: page.hasMore,
        loading: false,
      });
    } catch {
      if (this.controller.signal.aborted) return;
      this.publish({ loading: false, error: true, unavailable: true });
    }
  }

  async loadAll() {
    while (!this.controller.signal.aborted && this.snapshot.hasMore && !this.snapshot.error) {
      await this.loadMore();
    }
  }
}
