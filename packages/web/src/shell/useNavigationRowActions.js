import { useCallback, useMemo, useRef } from 'react';

/** Stable event entry points read the current owner without subscribing siblings. */
export function useLatestCallback(callback) {
  const latest = useRef(callback);
  latest.current = callback;
  return useCallback((...args) => latest.current(...args), []);
}

export function useNavigationRowActions(actions) {
  const latest = useRef(actions);
  latest.current = actions;
  const handlers = useMemo(() => Object.fromEntries([
    'setOpenSessionMenuId', 'renameSession', 'onRename', 'onTogglePin', 'onToggleUnread', 'onArchive',
  ].map(key => [key, (...args) => latest.current[key](...args)])), []);
  const { savingSessionId, openSessionMenuId, editingSessionId, archivingSessionId, actionError, deleteConfirmationSessionId } = actions;
  return useMemo(() => ({ ...handlers, savingSessionId, openSessionMenuId, editingSessionId, archivingSessionId, actionError, deleteConfirmationSessionId }),
    [handlers, savingSessionId, openSessionMenuId, editingSessionId, archivingSessionId, actionError, deleteConfirmationSessionId]);
}
