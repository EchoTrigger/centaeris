import type { TranscriptContentRange, TranscriptContentRef } from "./transcriptContract.ts";
import { createWorkspaceTranscriptTransport } from "./transcriptTransport.ts";

export const TRANSCRIPT_CONTENT_RANGE_BYTES = 64 * 1024;
export const TRANSCRIPT_CONTENT_CACHE_MAX_BYTES = 16 * 1024 * 1024;
const TRANSCRIPT_CONTENT_PAGES_PER_REF = 4;
const transport = createWorkspaceTranscriptTransport();
const encoder = new TextEncoder();
const pages = new Map<string, TranscriptContentRange>();
const keysByReference = new Map<string, string[]>();
let cachedBytes = 0;
let cacheEpoch = 0;
const listeners = new Set<() => void>();

function notify() {
  listeners.forEach((listener) => listener());
}

type Identity = Readonly<{
  sessionId: string;
  projectionGeneration: string;
  reference: TranscriptContentRef;
}>;

function referenceKey(identity: Identity) {
  return [
    identity.sessionId,
    identity.projectionGeneration,
    identity.reference.refId,
    identity.reference.revision,
  ].join("\0");
}

function remove(key: string) {
  const page = pages.get(key);
  if (page === undefined) return;
  cachedBytes -= encoder.encode(page.content).byteLength;
  pages.delete(key);
  for (const [reference, keys] of keysByReference) {
    const remaining = keys.filter((candidate) => candidate !== key);
    if (remaining.length === 0) keysByReference.delete(reference);
    else if (remaining.length !== keys.length) keysByReference.set(reference, remaining);
  }
}

function remember(identity: Identity, key: string, page: TranscriptContentRange) {
  if (pages.has(key)) return;
  pages.set(key, page);
  cachedBytes += encoder.encode(page.content).byteLength;
  const reference = referenceKey(identity);
  const keys = keysByReference.get(reference) ?? [];
  keys.push(key);
  keysByReference.set(reference, keys);
  while (keys.length > TRANSCRIPT_CONTENT_PAGES_PER_REF) remove(keys.shift() as string);
  while (cachedBytes > TRANSCRIPT_CONTENT_CACHE_MAX_BYTES && pages.size > 0) {
    remove(pages.keys().next().value as string);
  }
  notify();
}

export async function loadTranscriptContentRange(
  identity: Identity,
  offset: string,
  signal: AbortSignal,
) {
  signal.throwIfAborted();
  const epoch = cacheEpoch;
  const key = `${referenceKey(identity)}\0${offset}`;
  const cached = pages.get(key);
  if (cached !== undefined) return cached;
  const page = await transport.loadContentRange(identity, offset, signal);
  signal.throwIfAborted();
  if (epoch !== cacheEpoch) throw new Error("transcript content view was cleared");
  remember(identity, key, page);
  return page;
}

export function clearTranscriptContentRangeCache() {
  cacheEpoch++;
  pages.clear();
  keysByReference.clear();
  cachedBytes = 0;
  notify();
}

export function transcriptContentRangeCacheBytes() {
  return cachedBytes;
}

export function subscribeTranscriptContentRangeCache(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
