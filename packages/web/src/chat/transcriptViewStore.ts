import {
  canonicalTranscriptValue as canonical,
  cloneTranscriptValue as clone,
  compareTranscriptBlocks as compareBlocks,
  parseTranscriptLiveOverlay,
  transcriptOrderIdentity as orderIdentity,
  validateTranscriptPage,
  validateTranscriptPatchPage,
  validateTranscriptTailPage,
  type TranscriptBlock,
  type TranscriptIdentity,
  type TranscriptListSnapshot,
  type TranscriptLiveOverlay,
} from "./transcriptContract.ts";

export {
  validateTranscriptPage,
  validateTranscriptPatchPage,
  type TranscriptBlock,
  type TranscriptListSnapshot,
  type TranscriptLiveOverlay,
  type TranscriptPage,
  type TranscriptPatchPage,
} from "./transcriptContract.ts";

type Listener = () => void;
const transcriptByteEncoder = new TextEncoder();

export type TranscriptViewStore = ReturnType<typeof createTranscriptViewStore>;

export function createTranscriptViewStore() {
  let viewEpoch = 0;
  let identity: TranscriptIdentity | null = null;
  let blocks = new Map<string, TranscriptBlock>();
  let blockBytes = new Map<string, number>();
  let managedBytes = 0;
  let tailBlockIds = new Set<string>();
  let postBaseOverrideIds = new Set<string>();
  let tailOlderCursor: string | null = null;
  let revisions = new Map<string, string>();
  let orderOwners = new Map<string, string>();
  let noticeHighWater = 0n;
  let blockIds: readonly string[] = Object.freeze([]);
  let listSnapshot: TranscriptListSnapshot = Object.freeze({
    sessionId: null,
    projectionVersion: null,
    projectionGeneration: null,
    sourceHighWater: "0",
    viewEpoch,
    blockIds,
    olderCursor: null,
    hasOlder: false,
    appliedSourceHighWater: "0",
  });
  let liveSnapshot: TranscriptLiveOverlay | null = null;
  const listListeners = new Set<Listener>();
  const managedContentListeners = new Set<Listener>();
  const blockListeners = new Map<string, Set<Listener>>();
  const liveListeners = new Set<Listener>();

  function notifyList() {
    listListeners.forEach((listener) => listener());
  }

  function notifyManagedContent() {
    managedContentListeners.forEach((listener) => listener());
  }

  function notifyBlocks(ids: Iterable<string>) {
    for (const id of ids) blockListeners.get(id)?.forEach((listener) => listener());
  }

  function setLiveSnapshot(next: TranscriptLiveOverlay | null) {
    if (canonical(liveSnapshot) === canonical(next)) return;
    liveSnapshot = next;
    liveListeners.forEach((listener) => listener());
  }

  function sortedIds(candidate: Map<string, TranscriptBlock>) {
    const ordered = [...candidate.values()].sort(compareBlocks);
    for (let index = 1; index < ordered.length; index += 1) {
      if (compareBlocks(ordered[index - 1], ordered[index]) === 0) {
        throw new Error("conflicting transcript block orderKey");
      }
    }
    return Object.freeze(ordered.map((item) => item.blockId));
  }

  function indexOrders(candidate: Map<string, TranscriptBlock>) {
    const indexed = new Map<string, string>();
    for (const item of candidate.values()) {
      const order = orderIdentity(item);
      const owner = indexed.get(order);
      if (owner !== undefined && owner !== item.blockId) {
        throw new Error("conflicting transcript block orderKey");
      }
      indexed.set(order, item.blockId);
    }
    return indexed;
  }

  function latestNoticeHighWater(candidate: Map<string, TranscriptBlock>) {
    let highWater = 0n;
    for (const item of candidate.values()) {
      if (item.body.kind === "notice") {
        highWater = [highWater, BigInt(item.orderKey.sourceSequence)].reduce((left, right) => (
          left > right ? left : right
        ));
      }
    }
    return highWater;
  }

  function replaceManagedBlock(blockId: string, block: TranscriptBlock | null) {
    managedBytes -= blockBytes.get(blockId) ?? 0;
    if (block === null) {
      blockBytes.delete(blockId);
      return;
    }
    const bytes = transcriptByteEncoder.encode(JSON.stringify(block)).byteLength;
    blockBytes.set(blockId, bytes);
    managedBytes += bytes;
  }

  function mergeBlock(
    candidateBlocks: Map<string, TranscriptBlock>,
    candidateRevisions: Map<string, string>,
    next: TranscriptBlock,
    changed: Set<string>,
  ) {
    const currentRevision = candidateRevisions.get(next.blockId);
    if (currentRevision !== undefined) {
      const relation = BigInt(next.blockRevision) - BigInt(currentRevision);
      if (relation < 0n) return;
      if (relation === 0n) {
        const current = candidateBlocks.get(next.blockId);
        if (current === undefined || canonical(current) !== canonical(next)) {
          throw new Error("conflicting transcript block revision");
        }
        return;
      }
    }
    const current = candidateBlocks.get(next.blockId);
    if (current !== undefined && compareBlocks(current, next) !== 0) {
      throw new Error("transcript block revision changed orderKey");
    }
    candidateBlocks.set(next.blockId, clone(next));
    candidateRevisions.set(next.blockId, next.blockRevision);
    changed.add(next.blockId);
  }

  function openTail(rawPage: unknown, rawOlderPages: readonly unknown[] = []) {
    const page = validateTranscriptTailPage(rawPage);
    const candidateBlocks = new Map(page.blocks.map((item) => [item.blockId, item]));
    const cursors = new Set(page.olderCursor === null ? [] : [page.olderCursor]);
    let previousPage = page;
    let oldest = page.blocks[0];
    for (const rawOlderPage of rawOlderPages) {
      if (previousPage.olderCursor === null) throw new Error("transcript replacement extends beyond its older cursor");
      const olderPage = validateTranscriptPage(rawOlderPage, { sessionId: page.sessionId });
      if (olderPage.projectionVersion !== page.projectionVersion
        || olderPage.projectionGeneration !== page.projectionGeneration
        || olderPage.sourceHighWater !== page.sourceHighWater) throw new Error("transcript page identity changed");
      const last = olderPage.blocks.at(-1);
      if ((last !== undefined && oldest !== undefined && compareBlocks(last, oldest) >= 0)
        || (olderPage.blocks.length === 0 && olderPage.hasOlder)
        || (olderPage.olderCursor !== null && cursors.has(olderPage.olderCursor))) {
        throw new Error("transcript replacement older pages made no progress");
      }
      for (const item of olderPage.blocks) {
        if (candidateBlocks.has(item.blockId)) throw new Error("transcript replacement duplicates blockId");
        candidateBlocks.set(item.blockId, item);
      }
      if (olderPage.olderCursor !== null) cursors.add(olderPage.olderCursor);
      oldest = olderPage.blocks[0] ?? oldest;
      previousPage = olderPage;
    }
    const nextIds = sortedIds(candidateBlocks);
    const candidateOrderOwners = indexOrders(candidateBlocks);
    const candidateNoticeHighWater = latestNoticeHighWater(candidateBlocks);
    const candidateBlockBytes = new Map<string, number>();
    let candidateManagedBytes = 0;
    for (const item of candidateBlocks.values()) {
      const bytes = transcriptByteEncoder.encode(JSON.stringify(item)).byteLength;
      candidateBlockBytes.set(item.blockId, bytes);
      candidateManagedBytes += bytes;
    }
    const changed = new Set([...blockIds, ...nextIds]);
    // Validate every frozen page before replacing any public snapshot. A reader
    // never sees just the fresh tail while its already loaded history is staged.
    viewEpoch += 1;
    identity = {
      sessionId: page.sessionId,
      projectionVersion: page.projectionVersion,
      projectionGeneration: page.projectionGeneration,
      sourceHighWater: page.sourceHighWater,
    };
    blocks = candidateBlocks;
    blockBytes = candidateBlockBytes;
    managedBytes = candidateManagedBytes;
    postBaseOverrideIds = new Set();
    revisions = new Map();
    for (const item of candidateBlocks.values()) {
      revisions.set(item.blockId, item.blockRevision);
    }
    tailBlockIds = new Set(page.blocks.map((item) => item.blockId));
    tailOlderCursor = page.olderCursor;
    blockIds = nextIds;
    orderOwners = candidateOrderOwners;
    noticeHighWater = candidateNoticeHighWater;
    listSnapshot = Object.freeze({
      sessionId: page.sessionId,
      projectionVersion: page.projectionVersion,
      projectionGeneration: page.projectionGeneration,
      sourceHighWater: page.sourceHighWater,
      viewEpoch,
      blockIds,
      olderCursor: previousPage.olderCursor,
      hasOlder: previousPage.hasOlder,
      appliedSourceHighWater: page.resumeCursors[0]?.cursor || "0",
    });
    setLiveSnapshot(null);
    notifyManagedContent();
    notifyList();
    notifyBlocks(changed);
    return viewEpoch;
  }

  function prependPage(rawPage: unknown, expectedEpoch: number) {
    if (expectedEpoch !== viewEpoch || identity === null) return false;
    const page = validateTranscriptPage(rawPage, { sessionId: identity.sessionId });
    if (page.projectionVersion !== identity.projectionVersion
      || page.projectionGeneration !== identity.projectionGeneration
      || page.sourceHighWater !== identity.sourceHighWater) throw new Error("transcript page identity changed");
    const candidateBlocks = new Map(blocks);
    const candidateRevisions = new Map(revisions);
    const changed = new Set<string>();
    page.blocks.forEach((item) => mergeBlock(candidateBlocks, candidateRevisions, item, changed));
    const nextIds = sortedIds(candidateBlocks);
    for (const blockId of changed) {
      replaceManagedBlock(blockId, candidateBlocks.get(blockId) ?? null);
    }
    blocks = candidateBlocks;
    revisions = candidateRevisions;
    blockIds = nextIds;
    orderOwners = indexOrders(candidateBlocks);
    for (const item of page.blocks) {
      if (item.body.kind === "notice") {
        const sequence = BigInt(item.orderKey.sourceSequence);
        if (sequence > noticeHighWater) noticeHighWater = sequence;
      }
    }
    listSnapshot = Object.freeze({
      ...listSnapshot,
      blockIds,
      olderCursor: page.olderCursor,
      hasOlder: page.hasOlder,
    });
    if (changed.size > 0) notifyManagedContent();
    notifyList();
    notifyBlocks(changed);
    return true;
  }

  function applyPatchPage(rawPage: unknown, expectedEpoch: number) {
    if (expectedEpoch !== viewEpoch || identity === null) return false;
    const page = validateTranscriptPatchPage(rawPage, {
      ...identity,
      afterSourceHighWater: listSnapshot.appliedSourceHighWater,
    });
    const stagedBlocks = new Map<string, TranscriptBlock | null>();
    const stagedRevisions = new Map<string, string>();
    const changed = new Set<string>();
    let nextLive = liveSnapshot;
    let noticeAffected = false;
    const currentBlock = (blockId: string) => (
      stagedBlocks.has(blockId) ? stagedBlocks.get(blockId) : blocks.get(blockId)
    );
    const currentRevision = (blockId: string) => (
      stagedRevisions.get(blockId) ?? revisions.get(blockId)
    );
    for (const patch of page.patches) {
      for (const item of patch.upserts) {
        const previousRevision = currentRevision(item.blockId);
        const previous = currentBlock(item.blockId);
        if (previousRevision !== undefined) {
          const relation = BigInt(item.blockRevision) - BigInt(previousRevision);
          if (relation < 0n) continue;
          if (relation === 0n) {
            if (previous === undefined || previous === null || canonical(previous) !== canonical(item)) {
              throw new Error("conflicting transcript block revision");
            }
            continue;
          }
        }
        const original = blocks.get(item.blockId);
        if (original !== undefined && compareBlocks(original, item) !== 0) {
          throw new Error("transcript block revision changed orderKey");
        }
        if (previous?.body.kind === "notice" || item.body.kind === "notice") noticeAffected = true;
        if (blocks.has(item.blockId)
          && BigInt(item.orderKey.sourceSequence) <= BigInt(identity.sourceHighWater)) {
          postBaseOverrideIds.add(item.blockId);
        }
        stagedBlocks.set(item.blockId, clone(item));
        stagedRevisions.set(item.blockId, item.blockRevision);
        changed.add(item.blockId);
        if (nextLive !== null && (
          item.blockId === nextLive.messageId
          || item.blockId === nextLive.reasoning?.blockId
          || (item.body.kind === "notice"
            && BigInt(item.orderKey.sourceSequence) > BigInt(nextLive.afterSourceHighWater))
        )) nextLive = null;
      }
      for (const removal of patch.removals) {
        const previousRevision = currentRevision(removal.blockId);
        const previous = currentBlock(removal.blockId);
        if (previousRevision !== undefined && BigInt(removal.blockRevision) < BigInt(previousRevision)) continue;
        if (previousRevision === removal.blockRevision && previous !== undefined && previous !== null) {
          throw new Error("conflicting transcript block revision");
        }
        if (previousRevision === removal.blockRevision && (previous === undefined || previous === null)) continue;
        if (previous?.body.kind === "notice") noticeAffected = true;
        stagedBlocks.set(removal.blockId, null);
        stagedRevisions.set(removal.blockId, removal.blockRevision);
        if (previous !== undefined && previous !== null) changed.add(removal.blockId);
        if (nextLive !== null && (
          removal.blockId === nextLive.messageId || removal.blockId === nextLive.reasoning?.blockId
        )) nextLive = null;
      }
    }
    const structuralChange = [...stagedBlocks].some(([blockId, item]) => blocks.has(blockId) !== (item !== null));
    let nextIds = blockIds;
    let nextOrderOwners = orderOwners;
    if (structuralChange) {
      nextOrderOwners = new Map(orderOwners);
      for (const [blockId, item] of stagedBlocks) {
        const previous = blocks.get(blockId);
        if (previous !== undefined && item === null) nextOrderOwners.delete(orderIdentity(previous));
      }
      const additions = [...stagedBlocks]
        .filter(([blockId, item]) => !blocks.has(blockId) && item !== null)
        .map(([, item]) => item as TranscriptBlock)
        .sort(compareBlocks);
      for (const item of additions) {
        const order = orderIdentity(item);
        const owner = nextOrderOwners.get(order);
        if (owner !== undefined && owner !== item.blockId) {
          throw new Error("conflicting transcript block orderKey");
        }
        nextOrderOwners.set(order, item.blockId);
      }
      const retainedIds = blockIds.filter((blockId) => stagedBlocks.get(blockId) !== null);
      const mergedIds: string[] = [];
      let retainedIndex = 0;
      let additionIndex = 0;
      while (retainedIndex < retainedIds.length || additionIndex < additions.length) {
        const retainedId = retainedIds[retainedIndex];
        const addition = additions[additionIndex];
        if (retainedId === undefined) {
          mergedIds.push(addition.blockId);
          additionIndex += 1;
        } else if (addition === undefined) {
          mergedIds.push(retainedId);
          retainedIndex += 1;
        } else {
          const relation = compareBlocks(blocks.get(retainedId) as TranscriptBlock, addition);
          if (relation === 0) throw new Error("conflicting transcript block orderKey");
          if (relation < 0) {
            mergedIds.push(retainedId);
            retainedIndex += 1;
          } else {
            mergedIds.push(addition.blockId);
            additionIndex += 1;
          }
        }
      }
      nextIds = Object.freeze(mergedIds);
    }
    for (const [blockId, item] of stagedBlocks) {
      replaceManagedBlock(blockId, item);
      if (item === null) {
        blocks.delete(blockId);
        postBaseOverrideIds.delete(blockId);
      }
      else blocks.set(blockId, item);
    }
    for (const [blockId, revision] of stagedRevisions) revisions.set(blockId, revision);
    blockIds = nextIds;
    orderOwners = nextOrderOwners;
    if (noticeAffected) noticeHighWater = latestNoticeHighWater(blocks);
    listSnapshot = Object.freeze({
      ...listSnapshot,
      blockIds,
      appliedSourceHighWater: page.nextSourceHighWater,
    });
    setLiveSnapshot(nextLive);
    if (stagedBlocks.size > 0) notifyManagedContent();
    if (structuralChange) notifyList();
    notifyBlocks(changed);
    return true;
  }

  function applyLiveOverlay(raw: unknown, expectedEpoch: number) {
    if (expectedEpoch !== viewEpoch || identity === null) return false;
    const next = parseTranscriptLiveOverlay(raw);
    if (next === null) return false;
    if (liveSnapshot !== null) {
      const afterRelation = BigInt(next.afterSourceHighWater) - BigInt(liveSnapshot.afterSourceHighWater);
      if (afterRelation < 0n || (afterRelation === 0n && next.revision <= liveSnapshot.revision)) return false;
    }
    const sealed = blocks.has(next.messageId)
      || (next.reasoning !== null && blocks.has(next.reasoning.blockId))
      || noticeHighWater > BigInt(next.afterSourceHighWater);
    if (sealed) return false;
    setLiveSnapshot(next);
    return true;
  }

  function clearLiveOverlay(expectedEpoch: number) {
    if (expectedEpoch !== viewEpoch) return false;
    setLiveSnapshot(null);
    return true;
  }

  function clear() {
    viewEpoch += 1;
    identity = null;
    blocks = new Map();
    blockBytes = new Map();
    managedBytes = 0;
    tailBlockIds = new Set();
    postBaseOverrideIds = new Set();
    tailOlderCursor = null;
    revisions = new Map();
    orderOwners = new Map();
    noticeHighWater = 0n;
    blockIds = Object.freeze([]);
    listSnapshot = Object.freeze({
      sessionId: null,
      projectionVersion: null,
      projectionGeneration: null,
      sourceHighWater: "0",
      viewEpoch,
      blockIds,
      olderCursor: null,
      hasOlder: false,
      appliedSourceHighWater: "0",
    });
    setLiveSnapshot(null);
    notifyManagedContent();
    notifyList();
  }

  function releaseLoadedHistory() {
    if (identity === null) return;
    const removed = new Set<string>();
    for (const [blockId, block] of blocks) {
      if (!tailBlockIds.has(blockId)
        && !postBaseOverrideIds.has(blockId)
        && BigInt(block.orderKey.sourceSequence) <= BigInt(identity.sourceHighWater)) {
        blocks.delete(blockId);
        replaceManagedBlock(blockId, null);
        revisions.delete(blockId);
        removed.add(blockId);
      }
    }
    blockIds = sortedIds(blocks);
    orderOwners = indexOrders(blocks);
    listSnapshot = Object.freeze({
      ...listSnapshot,
      blockIds,
      olderCursor: tailOlderCursor,
      hasOlder: tailOlderCursor !== null,
    });
    if (removed.size > 0) notifyManagedContent();
    notifyList();
    notifyBlocks(removed);
  }

  function managedContentBytes() {
    return managedBytes;
  }

  return {
    openTail,
    prependPage,
    applyPatchPage,
    applyLiveOverlay,
    clearLiveOverlay,
    clear,
    releaseLoadedHistory,
    managedContentBytes,
    getListSnapshot: () => listSnapshot,
    getBlockSnapshot: (blockId: string) => blocks.get(blockId) || null,
    getLiveSnapshot: () => liveSnapshot,
    subscribeList(listener: Listener) {
      listListeners.add(listener);
      return () => listListeners.delete(listener);
    },
    subscribeManagedContent(listener: Listener) {
      managedContentListeners.add(listener);
      return () => managedContentListeners.delete(listener);
    },
    subscribeBlock(blockId: string, listener: Listener) {
      const listeners = blockListeners.get(blockId) || new Set<Listener>();
      listeners.add(listener);
      blockListeners.set(blockId, listeners);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) blockListeners.delete(blockId);
      };
    },
    subscribeLive(listener: Listener) {
      liveListeners.add(listener);
      return () => liveListeners.delete(listener);
    },
  };
}
