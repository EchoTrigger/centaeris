import {
  getTranscriptContentRange,
  TRANSCRIPT_PROJECTION_VERSION_V1,
  type TranscriptContentRangeV1,
  type TranscriptContentRefV1,
} from "../../lib/chatBridge";

export const TRANSCRIPT_CONTENT_RANGE_BYTES = 64 * 1024;
export const TRANSCRIPT_CONTENT_CACHE_MAX_BYTES = 16 * 1024 * 1024;
const TRANSCRIPT_CONTENT_PAGES_PER_REF = 4;

type Identity = Readonly<{
  sessionId: string;
  projectionGeneration: string;
  reference: TranscriptContentRefV1;
}>;

const pages = new Map<string, TranscriptContentRangeV1>();
const pageKeysByRef = new Map<string, string[]>();
let cachedBytes = 0;
const listeners = new Set<() => void>();

function notify() {
  for (const listener of listeners) listener();
}

function refKey(identity: Identity) {
  return `${identity.sessionId}\0${identity.projectionGeneration}\0${identity.reference.refId}\0${identity.reference.revision}`;
}

function pageKey(identity: Identity, offset: string) {
  return `${refKey(identity)}\0${offset}`;
}

function remember(key: string, identity: Identity, page: TranscriptContentRangeV1) {
  if (pages.has(key)) return;
  const bytes = new TextEncoder().encode(page.content).byteLength;
  pages.set(key, page);
  cachedBytes += bytes;
  const referenceKey = refKey(identity);
  const referencePages = pageKeysByRef.get(referenceKey) ?? [];
  referencePages.push(key);
  pageKeysByRef.set(referenceKey, referencePages);
  while (referencePages.length > TRANSCRIPT_CONTENT_PAGES_PER_REF) {
    remove(referencePages.shift() as string);
  }
  while (cachedBytes > TRANSCRIPT_CONTENT_CACHE_MAX_BYTES && pages.size > 0) {
    remove(pages.keys().next().value as string);
  }
  notify();
}

function remove(key: string) {
  const page = pages.get(key);
  if (!page) return;
  cachedBytes -= new TextEncoder().encode(page.content).byteLength;
  pages.delete(key);
  for (const [referenceKey, keys] of pageKeysByRef) {
    const next = keys.filter((candidate) => candidate !== key);
    if (next.length === 0) pageKeysByRef.delete(referenceKey);
    else if (next.length !== keys.length) pageKeysByRef.set(referenceKey, next);
  }
}

function validate(page: TranscriptContentRangeV1, identity: Identity, offset: string) {
  const contentBytes = new TextEncoder().encode(page.content).byteLength;
  if (page.schema !== "transcript.content.range.v1"
    || page.sessionId !== identity.sessionId
    || page.projectionVersion !== TRANSCRIPT_PROJECTION_VERSION_V1
    || page.projectionGeneration !== identity.projectionGeneration
    || page.refId !== identity.reference.refId
    || page.revision !== identity.reference.revision
    || page.byteLength !== identity.reference.byteLength
    || page.startOffset !== offset
    || BigInt(page.endOffset) < BigInt(offset)
    || BigInt(page.endOffset) > BigInt(page.byteLength)
    || BigInt(page.endOffset) - BigInt(offset) !== BigInt(contentBytes)
    || contentBytes > TRANSCRIPT_CONTENT_RANGE_BYTES
    || page.hasMore !== (BigInt(page.endOffset) < BigInt(page.byteLength))) {
    throw new Error("invalid transcript content range");
  }
  return page;
}

export async function loadTranscriptContentRange(identity: Identity, offset: string) {
  const key = pageKey(identity, offset);
  const cached = pages.get(key);
  if (cached) return cached;
  const page = validate(await getTranscriptContentRange({
    schema: "transcript.content.range.read.v1",
    sessionId: identity.sessionId,
    projectionVersion: TRANSCRIPT_PROJECTION_VERSION_V1,
    projectionGeneration: identity.projectionGeneration,
    refId: identity.reference.refId,
    revision: identity.reference.revision,
    byteLength: identity.reference.byteLength,
    offset,
    maxBytes: TRANSCRIPT_CONTENT_RANGE_BYTES,
  }), identity, offset);
  remember(key, identity, page);
  return page;
}

export function clearTranscriptContentRangeCache() {
  pages.clear();
  pageKeysByRef.clear();
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
