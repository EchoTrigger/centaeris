import { useCallback, useSyncExternalStore } from 'react';
import { useTranslation } from '../i18n';

export function ConversationReadFeedback({ store, placement, error, paused, reconnecting, ready, loading, checking, onCheck, onRetry }) {
  const { t } = useTranslation();
  const list = useSyncExternalStore(store.subscribeList, store.getListSnapshot, store.getListSnapshot);
  const live = useSyncExternalStore(store.subscribeLive, store.getLiveSnapshot, store.getLiveSnapshot);
  const readContent = useCallback(() => store.getListSnapshot().blockIds.some(id => {
    const body = store.getBlockSnapshot(id)?.body;
    if (!body) return false;
    const content = body.content;
    return Boolean(content?.inlineContent || body.summary || body.message
      || content?.sourceRef && store.getPresentationContentSnapshot(content.sourceRef)?.content
      || body.summaryRef && store.getPresentationContentSnapshot(body.summaryRef)?.content);
  }), [store]);
  const pushedContent = useSyncExternalStore(store.subscribePresentationContent, readContent, readContent);
  const hasContent = pushedContent || readContent() || Boolean(live?.text || live?.reasoning?.text);
  const hasBlocks = list.blockIds.length > 0 || live !== null;
  if ((placement === 'content') !== hasBlocks) return null;
  const message = error || (paused ? hasContent ? 'appRoute.streamPaused' : 'appRoute.initialHistoryFailed'
    : !hasBlocks && ready && !reconnecting ? 'appRoute.emptyConversation' : null);
  if (!message || loading && !hasContent && !reconnecting) return null;
  return <div className={`workspaceConversationFeedback workspaceToolOutputStatus ${error || paused ? 'isError' : ''}`} role={error || paused ? 'alert' : 'status'}>
    <span>{t(message)}</span>
    {error === 'operations.acceptedReadFailed' ? <button type="button" disabled={checking} onClick={onCheck}>{t('agentChat.retry')}</button> : null}
    {paused ? <button type="button" onClick={onRetry}>{t('appRoute.retryStream')}</button> : null}
  </div>;
}
