import { useEffect, useMemo, useRef, useState } from "react";
import { apiJson, apiResponse } from "../api";
import { useTranslation } from "../i18n";
import { sortSessionNavigation } from "./sessionNavigation.mjs";

// Both Agent and ordinary Session entry points act on the same hosted records.
// Callers own their projection and any navigation after deleting the open chat.
export function useSessionNavigationActions({ scopeKey, sessions, onChangeSessions, onError, onDeleted }) {
  const { t } = useTranslation();
  const [editingSessionId, setEditingSessionId] = useState("");
  const [savingSessionId, setSavingSessionId] = useState("");
  const [openSessionMenuId, setOpenSessionMenuId] = useState("");
  const [deletingSessionId, setDeletingSessionId] = useState("");
  const [deleteConfirmationSessionId, setDeleteConfirmationSessionId] = useState("");
  const scope = useMemo(() => ({ key: scopeKey }), [scopeKey]);
  const currentScope = useRef(scope); currentScope.current = scope;
  const mutation = useRef(null);
  const pendingRead = useRef(null);
  const sessionRecords = useRef(sessions); sessionRecords.current = sessions;
  const afterDelete = useRef(onDeleted); afterDelete.current = onDeleted;

  useEffect(() => {
    setEditingSessionId(""); setSavingSessionId(""); setOpenSessionMenuId("");
    setDeletingSessionId(""); setDeleteConfirmationSessionId("");
    return () => { if (mutation.current?.scope === scope) { mutation.current.controller.abort(); mutation.current = null; } if (pendingRead.current?.scope === scope) pendingRead.current = null; };
  }, [scope]);

  function flushPendingRead() {
    const queued = pendingRead.current; pendingRead.current = null;
    if (queued?.scope === scope && currentScope.current === scope) markSessionRead(queued.session);
  }

  async function updateSession(targetSessionId, metadata, observedSession) {
    if (mutation.current) return null;
    const expected = observedSession || sessionRecords.current.find(item => item.id === targetSessionId);
    if (!expected || expected.id !== targetSessionId) throw new Error("session_metadata_identity_invalid");
    const controller = new AbortController(); mutation.current = { scope, controller };
    setSavingSessionId(targetSessionId);
    try {
      const data = await apiJson(`/api/sessions/${encodeURIComponent(targetSessionId)}`, {
        method: "PATCH", body: JSON.stringify(metadata), signal: controller.signal,
      });
      if (controller.signal.aborted || currentScope.current !== scope) return null;
      if (!data?.session || data.session.id !== targetSessionId || data.session.workspaceId !== expected.workspaceId || data.session.agentId !== expected.agentId) throw new Error("session_metadata_response_invalid");
      onChangeSessions(items => sortSessionNavigation(items.map(item => item.id === targetSessionId ? data.session : item)));
      return data.session;
    } finally {
      if (currentScope.current === scope && mutation.current?.controller === controller) { mutation.current = null; setSavingSessionId(""); flushPendingRead(); }
    }
  }

  function markSessionRead(session) {
    if (!session.isUnread) return;
    if (mutation.current) { pendingRead.current = { scope, session }; return; }
    void updateSession(session.id, { isUnread: false }, session).catch(() => {
      if (currentScope.current === scope) onError(t("appRoute.unableToUpdateUnreadStatus"));
    });
  }

  async function renameSession(session, rawTitle) {
    const title = rawTitle.trim(); setEditingSessionId("");
    if (!title || title === session.title || mutation.current) return;
    try { await updateSession(session.id, { title }); }
    catch { if (currentScope.current === scope) onError(t("appRoute.unableToRenameTheConversationPleaseTryAgain")); }
  }

  function toggleMetadata(session, field) {
    setOpenSessionMenuId("");
    void updateSession(session.id, { [field]: !session[field] }).catch(() => {
      if (currentScope.current === scope) onError(t(field === "isPinned" ? "appRoute.unableToUpdatePinnedStatus" : "appRoute.unableToUpdateUnreadStatus"));
    });
  }

  async function deleteSession(targetSessionId) {
    if (mutation.current) return;
    const controller = new AbortController(); mutation.current = { scope, controller };
    setDeletingSessionId(targetSessionId); onError("");
    try {
      await apiResponse(`/api/sessions/${encodeURIComponent(targetSessionId)}`, { method: "DELETE", signal: controller.signal });
      if (controller.signal.aborted || currentScope.current !== scope) return;
      const remaining = sessionRecords.current.filter(item => item.id !== targetSessionId);
      onChangeSessions(items => items.filter(item => item.id !== targetSessionId));
      if (pendingRead.current?.session.id === targetSessionId) pendingRead.current = null;
      setDeleteConfirmationSessionId(""); setEditingSessionId(""); setOpenSessionMenuId("");
      afterDelete.current?.(targetSessionId, remaining);
    } catch {
      if (currentScope.current === scope && !controller.signal.aborted) onError(t("appRoute.unableToDeleteThisConversationPleaseTryAgain"));
    } finally {
      if (currentScope.current === scope && mutation.current?.controller === controller) { mutation.current = null; setDeletingSessionId(""); flushPendingRead(); }
    }
  }

  return {
    editingSessionId, savingSessionId, openSessionMenuId, deletingSessionId, deleteConfirmationSessionId,
    setEditingSessionId, setOpenSessionMenuId, setDeleteConfirmationSessionId,
    updateSession, markSessionRead, renameSession, deleteSession,
    onRename: session => { setEditingSessionId(session.id); setOpenSessionMenuId(""); },
    onTogglePin: session => toggleMetadata(session, "isPinned"),
    onToggleUnread: session => toggleMetadata(session, "isUnread"),
    onRequestDelete: session => { setOpenSessionMenuId(""); setDeleteConfirmationSessionId(session.id); },
  };
}
