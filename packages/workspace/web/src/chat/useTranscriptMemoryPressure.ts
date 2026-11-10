import { useCallback, useSyncExternalStore } from 'react';
import { subscribeTranscriptContentRangeCache, transcriptContentRangeCacheBytes } from './transcriptContentRanges.ts';
import type { TranscriptViewStore } from './transcriptViewStore.ts';

export function useTranscriptMemoryPressure(store: TranscriptViewStore) {
  const subscribe = useCallback((listener: () => void) => {
    const dispose = [store.subscribeManagedContent(listener), store.subscribePresentationContent(listener), subscribeTranscriptContentRangeCache(listener)];
    return () => dispose.forEach(stop => stop());
  }, [store]);
  const read = useCallback(() => store.managedContentBytes() + store.presentationContentBytes()
    + transcriptContentRangeCacheBytes() >= 32 * 1024 * 1024, [store]);
  return useSyncExternalStore(subscribe, read, read);
}
