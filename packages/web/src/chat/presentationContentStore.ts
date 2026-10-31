import { validateTranscriptContentRange, type TranscriptBlock, type TranscriptContentRange, type TranscriptContentRef } from './transcriptContract.ts';

export const PRESENTATION_CONTENT_PREVIEW_BYTES = 16 * 1024 * 1024;
export const PRESENTATION_CONTENT_CACHE_BYTES = 16 * 1024 * 1024;
export type PresentationContentSnapshot = Readonly<{ content: string; complete: boolean; limited: boolean }>;
type Slot = { reference: TranscriptContentRef; offset: string; snapshot: PresentationContentSnapshot; bytes: number; evicted: boolean; ranges: Map<string, { end: string; digest: number }> };
const key = (reference: TranscriptContentRef) => [reference.refId, reference.revision, reference.byteLength].join('\0');
function rangeDigest(content: string) {
  let digest = 2166136261;
  for (let index = 0; index < content.length; index++) digest = Math.imul(digest ^ content.charCodeAt(index), 16777619);
  return digest >>> 0;
}
const prefixEnd = (reference: TranscriptContentRef) => {
  const bytes = BigInt(reference.byteLength);
  return bytes < BigInt(PRESENTATION_CONTENT_PREVIEW_BYTES) ? bytes : BigInt(PRESENTATION_CONTENT_PREVIEW_BYTES);
};
export function transcriptNormalContentReferences(blocks: readonly TranscriptBlock[]) {
  const references = new Map<string, TranscriptContentRef>();
  for (const block of blocks) {
    const body = block.body as Record<string, unknown>;
    const content = typeof body.content === 'object' && body.content !== null
      ? body.content as { sourceRef?: TranscriptContentRef | null } : null;
    const reference = body.kind === 'tool' ? body.summaryRef as TranscriptContentRef | null : content?.sourceRef;
    if (reference && reference.refId.startsWith('session-event:')) references.set(key(reference), reference);
  }
  return [...references.values()];
}

// A bounded, view-owned content cache. Completion receipts do not depend on
// residency: evicted history can still use the ordinary demand reader later.
export function createPresentationContentStore() {
  const slots = new Map<string, Slot>();
  const residents = new Map<string, Slot>();
  const listeners = new Set<() => void>();
  let bytes = 0;
  let unfinished = 0;
  let version = 0;
  const notify = () => { version++; listeners.forEach(listener => listener()); };
  function reserve(references: readonly TranscriptContentRef[]) {
    for (const reference of references) {
      const id = key(reference);
      if (!slots.has(id)) {
        const complete = BigInt(reference.byteLength) === 0n;
        slots.set(id, { reference, offset: '0', bytes: 0, evicted: false, ranges: new Map(),
          snapshot: { content: '', complete, limited: false } });
        if (!complete) unfinished++;
      }
    }
    // Metadata is bounded by the loaded transcript. Removing blocks retires its
    // slots through retain(); it does not keep an unbounded history of revisions.
  }
  function retain(references: readonly TranscriptContentRef[]) {
    const retained = new Set(references.map(key));
    for (const [id, slot] of slots) if (!retained.has(id)) {
      bytes -= slot.bytes;
      if (!slot.snapshot.complete) unfinished--;
      residents.delete(id);
      slots.delete(id);
    }
  }
  function apply(raw: TranscriptContentRange, identity: { sessionId: string; projectionGeneration: string }, beforeNotify?: () => void) {
    const reference = { refId: raw.refId, revision: raw.revision, byteLength: raw.byteLength };
    const id = key(reference);
    const slot = slots.get(id);
    if (!slot) throw new Error('presentation content has no owning block');
    const page = validateTranscriptContentRange(raw, { ...identity, reference, offset: raw.startOffset });
    const start = BigInt(page.startOffset), end = BigInt(page.endOffset), applied = BigInt(slot.offset);
    // A fresh SSE connection replays from offset zero. Previously applied bytes
    // are idempotent; partial overlaps or gaps are invalid.
    if (end <= applied) {
      const previous = slot.ranges.get(page.startOffset);
      if (previous?.end !== page.endOffset || previous.digest !== rangeDigest(page.content)) {
        throw new Error('conflicting presentation content replay');
      }
      return;
    }
    if (start !== applied || end === start) throw new Error('presentation content continuation made no progress');
    const cap = prefixEnd(reference);
    if (start >= cap || end > cap) throw new Error('presentation content exceeds preview budget');
    const limited = BigInt(reference.byteLength) > BigInt(PRESENTATION_CONTENT_PREVIEW_BYTES) && end >= cap - 3n;
    const complete = !page.hasMore || limited;
    if (!slot.snapshot.complete && complete) unfinished--;
    if (!slot.evicted) {
      // Validation already checked this exact UTF-8 byte length.
      const pageBytes = Number(end - start);
      slot.bytes += pageBytes;
      bytes += pageBytes;
      slot.snapshot = { content: slot.snapshot.content + page.content, complete, limited };
      residents.delete(id); residents.set(id, slot);
    } else slot.snapshot = { content: '', complete, limited };
    slot.ranges.set(page.startOffset, { end: page.endOffset, digest: rangeDigest(page.content) });
    slot.offset = page.endOffset;
    while (bytes > PRESENTATION_CONTENT_CACHE_BYTES) {
      // The applied slot is last and cannot exceed the cache's preview cap.
      const [victimId, victim] = residents.entries().next().value!;
      residents.delete(victimId);
      bytes -= victim.bytes;
      victim.bytes = 0;
      victim.evicted = true;
      victim.snapshot = { ...victim.snapshot, content: '' };
    }
    beforeNotify?.();
    notify();
  }
  function ready() { return unfinished === 0; }
  function reset(references: readonly TranscriptContentRef[] = []) {
    slots.clear(); residents.clear(); bytes = 0; unfinished = 0; reserve(references); notify();
  }
  function clear() { reset(); }
  return { reserve, retain, apply, ready, reset, clear,
    get: (reference: TranscriptContentRef): PresentationContentSnapshot | null => {
      const slot = slots.get(key(reference));
      return slot && !slot.evicted ? slot.snapshot : null;
    },
    isEvicted: (reference: TranscriptContentRef) => slots.get(key(reference))?.evicted === true,
    isReady: (reference: TranscriptContentRef) => slots.get(key(reference))?.snapshot.complete === true,
    bytes: () => bytes,
    getVersion: () => version,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
  };
}
