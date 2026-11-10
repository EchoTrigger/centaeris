import assert from "node:assert/strict";
import test from "node:test";
import { ApiError } from "../../src/api.ts";
import { TranscriptContentReader } from "../../src/chat/transcriptContentReader.ts";
import { watchTranscriptLoads } from "../helpers/transcript-reader-watchdog.mjs";

const delays = [500, 1000, 2000, 4000, 8000];
const flush = async () => { for (let i = 0; i < 30; i++) await Promise.resolve(); };
function page(offset, total = 200000) {
  const end = Math.min(Number(offset) + 65536, total);
  return { content: "a".repeat(end - Number(offset)), startOffset: offset,
    endOffset: String(end), hasMore: end < total };
}
function network(t, online = true) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const timeout = t.mock.method(globalThis, "setTimeout");
  const clearTimeout = t.mock.method(globalThis, "clearTimeout");
  const descriptors = ["window", "navigator"].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]);
  const listeners = new Set();
  class Connection extends EventTarget {
    addEventListener(type, listener, options) {
      if (type === "online") listeners.add(listener);
      super.addEventListener(type, listener, options);
    }
    removeEventListener(type, listener, options) {
      listeners.delete(listener);
      super.removeEventListener(type, listener, options);
    }
  }
  const connection = new Connection();
  Object.defineProperty(globalThis, "window", { configurable: true, value: connection });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { get onLine() { return online; } } });
  t.after(() => {
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  });
  return { listeners, clearTimeout,
    get delays() { return timeout.mock.calls.map(call => call.arguments[1]); },
    async tick(ms) { t.mock.timers.tick(ms); await flush(); },
    setOnline(value) { online = value; connection.dispatchEvent(new Event(value ? "online" : "offline")); },
  };
}

test("a transient middle page recovers and automatically continues the entire document", async t => {
  const connection = network(t);
  const offsets = [];
  const reader = new TranscriptContentReader(async offset => {
    offsets.push(offset);
    if (offset === "65536" && offsets.length <= 3) throw new TypeError("Failed to fetch");
    return page(offset);
  });
  t.after(() => reader.dispose());
  const pending = reader.loadAll();
  await flush();
  assert.equal(reader.getSnapshot().content.length, 65536);
  assert.equal(reader.getSnapshot().error, false);
  assert.deepEqual(connection.delays, [500]);
  await connection.tick(499);
  assert.deepEqual(offsets, ["0", "65536"]);
  await connection.tick(1);
  assert.deepEqual(connection.delays, [500, 1000]);
  await connection.tick(1000);
  await pending;
  assert.deepEqual(offsets, ["0", "65536", "65536", "65536", "131072", "196608"]);
  assert.equal(reader.getSnapshot().content, "a".repeat(200000));
  assert.equal(reader.getSnapshot().hasMore, false);
});

for (const error of [new TypeError("Failed to fetch"), ...[429, 500, 502, 503, 504].map(status => new ApiError("temporarily_unavailable", status))]) {
  test(`initial request plus five retries is bounded for ${error.name}:${error.status ?? "network"}`, async t => {
    const connection = network(t);
    const offsets = [];
    const reader = new TranscriptContentReader(async offset => {
      offsets.push(offset);
      if (offset === "0") return page(offset);
      throw error;
    });
    t.after(() => reader.dispose());
    const pending = reader.loadAll();
    await flush();
    for (const delay of delays) await connection.tick(delay);
    await pending;
    assert.deepEqual(offsets, ["0", ...Array(6).fill("65536")]);
    assert.deepEqual(connection.delays, delays);
    assert.equal(reader.getSnapshot().content.length, 65536);
    assert.equal(reader.getSnapshot().loading, false);
    assert.equal(reader.getSnapshot().hasMore, true);
    assert.equal(reader.getSnapshot().error, true);
    assert.equal(reader.getSnapshot().unavailable, false);
    connection.setOnline(false); connection.setOnline(true);
    await reader.loadMore(); await reader.loadAll();
    await connection.tick(100000);
    assert.equal(offsets.length, 7, "exhausted readers cannot reset via scroll, loadAll, or online");
    assert.equal(connection.listeners.size, 0);
    const fresh = new TranscriptContentReader(async offset => page(offset));
    await fresh.loadAll();
    assert.equal(fresh.getSnapshot().content.length, 200000, "refresh starts a fresh reader");
    fresh.dispose();
  });
}

test("each successful page resets the next page's five-retry budget", async t => {
  const connection = network(t);
  const attempts = new Map();
  const reader = new TranscriptContentReader(async offset => {
    const count = (attempts.get(offset) ?? 0) + 1;
    attempts.set(offset, count);
    if (count <= 5) throw new ApiError("temporary", 503);
    return page(offset, 70000);
  });
  t.after(() => reader.dispose());
  const pending = reader.loadAll();
  await flush();
  for (let round = 0; round < 2; round++) for (const delay of delays) await connection.tick(delay);
  await pending;
  assert.deepEqual([...attempts], [["0", 6], ["65536", 6]]);
  assert.deepEqual(connection.delays, [...delays, ...delays]);
  assert.equal(reader.getSnapshot().content.length, 70000);
  assert.equal(reader.getSnapshot().error, false);
});

for (const error of [...[400, 401, 403, 404, 409, 410, 501].map(status => new ApiError("transcript_content_unavailable", status)), new SyntaxError("invalid JSON"), new Error("invalid transcript content range"), new DOMException("Aborted", "AbortError")]) {
  test(`permanent ${error.name}:${error.status ?? error.message} stops without network recovery`, async t => {
    const connection = network(t);
    let reads = 0;
    const reader = new TranscriptContentReader(async offset => {
      reads++;
      if (offset === "0") return page(offset);
      throw error;
    });
    t.after(() => reader.dispose());
    await reader.loadAll();
    assert.equal(reads, 2);
    assert.deepEqual(connection.delays, []);
    assert.equal(reader.getSnapshot().content.length, 65536);
    assert.equal(reader.getSnapshot().hasMore, true);
    assert.equal(reader.getSnapshot().unavailable, true);
    await reader.loadMore();
    assert.equal(reads, 2);
  });
}

test("offline pauses the current page until online, and disposal removes the listener", async t => {
  const connection = network(t, false);
  let reads = 0;
  const reader = new TranscriptContentReader(async offset => { reads++; return page(offset, 8); });
  t.after(() => reader.dispose());
  const pending = reader.loadAll();
  await flush();
  assert.equal(reads, 0);
  assert.equal(connection.listeners.size, 1);
  await connection.tick(100000);
  assert.equal(reads, 0);
  connection.setOnline(true);
  await pending;
  assert.equal(reads, 1);
  assert.equal(connection.listeners.size, 0);
  connection.setOnline(false);
  const cancelled = new TranscriptContentReader(async () => { throw new Error("must not request"); });
  const cancelledLoad = cancelled.loadAll();
  await flush();
  assert.equal(connection.listeners.size, 1);
  cancelled.dispose();
  await cancelledLoad;
  assert.equal(connection.listeners.size, 0);
});

test("going offline during backoff does not spend further attempts before online", async t => {
  const connection = network(t);
  let reads = 0;
  const reader = new TranscriptContentReader(async offset => {
    if (++reads === 1) throw new TypeError("Failed to fetch");
    return page(offset, 8);
  });
  t.after(() => reader.dispose());
  const pending = reader.loadAll();
  await flush();
  connection.setOnline(false);
  await connection.tick(500);
  await connection.tick(100000);
  assert.equal(reads, 1);
  assert.equal(connection.listeners.size, 1);
  connection.setOnline(true);
  await pending;
  assert.equal(reads, 2);
  assert.deepEqual(connection.delays, [500]);
  assert.equal(connection.listeners.size, 0);
});

test("disposal clears a pending backoff and duplicate triggers cannot create a second loop", async t => {
  const connection = network(t);
  const watcher = watchTranscriptLoads(TranscriptContentReader);
  let reads = 0;
  const reader = new TranscriptContentReader(async () => { reads++; throw new TypeError("Failed to fetch"); });
  t.after(() => { reader.dispose(); watcher.restore(); });
  const first = reader.loadAll();
  const duplicate = reader.loadAll();
  await flush();
  assert.equal(reads, 1);
  assert.deepEqual(connection.delays, [500]);
  const snapshot = reader.getSnapshot();
  reader.dispose();
  await Promise.all([first, duplicate]);
  assert.equal(connection.clearTimeout.mock.calls.length, 1);
  await connection.tick(100000);
  assert.equal(reads, 1);
  assert.equal(reader.getSnapshot(), snapshot);
});

test("on-demand output recovers only the requested page", async t => {
  const connection = network(t);
  const offsets = [];
  const reader = new TranscriptContentReader(async offset => {
    offsets.push(offset);
    if (offsets.length === 1) throw new TypeError("Failed to fetch");
    return page(offset);
  });
  t.after(() => reader.dispose());
  const pending = reader.loadMore();
  await flush();
  await connection.tick(500);
  await pending;
  assert.deepEqual(offsets, ["0", "0"]);
  assert.equal(reader.getSnapshot().content.length, 65536);
  assert.equal(reader.getSnapshot().hasMore, true);
});
