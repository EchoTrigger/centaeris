import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError, apiJson, apiResponse } from "../api";
import { useTranslation } from "../i18n";
import { sortSessionNavigation } from "./sessionNavigation.mjs";
import { mergeSessionAttention } from "./sessionReadState";

// Session views act on the same hosted records.
// Callers own their projection and any navigation after deleting the open chat.
export function useSessionNavigationActions({ scopeKey, sessions, onChangeSessions, onDeleted }) {
  const { t } = useTranslation();
  const [editingSessionId, setEditingSessionId] = useState("");
  const [savingSessionId, setSavingSessionId] = useState("");
  const [openSessionMenuId, setOpenSessionMenuId] = useState("");
  const [deletingSessionId, setDeletingSessionId] = useState("");
  const [archivingSessionId, setArchivingSessionId] = useState("");
  const [deleteConfirmationSessionId, setDeleteConfirmationSessionId] = useState("");
  const [actionError, setActionError] = useState(null);
  const scope = useMemo(() => ({ key: scopeKey }), [scopeKey]);
  const currentScope = useRef(scope); currentScope.current = scope;
  const mutation = useRef(null);
  const pendingRead = useRef(null);
  const sessionRecords = useRef(sessions); sessionRecords.current = sessions;
  const afterDelete = useRef(onDeleted); afterDelete.current = onDeleted;

  useEffect(() => {
    setEditingSessionId(""); setSavingSessionId(""); setOpenSessionMenuId("");
    setDeletingSessionId(""); setArchivingSessionId(""); setDeleteConfirmationSessionId("");
    setActionError(null);
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
    setActionError(value => value?.sessionId === targetSessionId ? null : value);
    try {
      const data = await apiJson(`/api/sessions/${encodeURIComponent(targetSessionId)}`, {
        method: "PATCH", body: JSON.stringify(metadata), signal: controller.signal,
      });
      if (controller.signal.aborted || currentScope.current !== scope) return null;
      if (!data?.session || data.session.id !== targetSessionId || data.session.workspaceId !== expected.workspaceId) throw new Error("session_metadata_response_invalid");
      onChangeSessions(items => sortSessionNavigation(items.map(item => item.id === targetSessionId ? mergeSessionAttention(item, data.session) : item)));
      return data.session;
    } finally {
      if (currentScope.current === scope && mutation.current?.controller === controller) { mutation.current = null; setSavingSessionId(""); flushPendingRead(); }
    }
  }

  function markSessionRead(session) {
    if (!session.isUnread) return;
    if (mutation.current) { pendingRead.current = { scope, session }; return; }
    void updateSession(session.id, { isUnread: false }, session).catch(() => {
      if (currentScope.current === scope) setActionError({ sessionId: session.id, message: t("appRoute.unableToUpdateUnreadStatus") });
    });
  }

  async function renameSession(session, rawTitle) {
    const title = rawTitle.trim(); setEditingSessionId("");
    if (!title || title === session.title || mutation.current) return;
    try { await updateSession(session.id, { title }); }
    catch { if (currentScope.current === scope) setActionError({ sessionId: session.id, message: t("appRoute.unableToRenameTheConversationPleaseTryAgain") }); }
  }

  function toggleMetadata(session, field) {
    setOpenSessionMenuId("");
    void updateSession(session.id, { [field]: !session[field] }).catch(() => {
      if (currentScope.current === scope) setActionError({ sessionId: session.id, message: t(field === "isPinned" ? "appRoute.unableToUpdatePinnedStatus" : "appRoute.unableToUpdateUnreadStatus") });
    });
  }

  async function removeFromNavigation(targetSessionId, archive = false) {
    if (mutation.current) return;
    const controller = new AbortController(); mutation.current = { scope, controller };
    const setBusy = archive ? setArchivingSessionId : setDeletingSessionId;
    setBusy(targetSessionId);
    setActionError(value => value?.sessionId === targetSessionId ? null : value);
    try {
      const url = `/api/sessions/${encodeURIComponent(targetSessionId)}`;
      await apiResponse(archive ? url + "/archive" : url, { method: archive ? "PUT" : "DELETE", signal: controller.signal });
      if (controller.signal.aborted || currentScope.current !== scope) return;
      const remaining = sessionRecords.current.filter(item => item.id !== targetSessionId);
      onChangeSessions(items => items.filter(item => item.id !== targetSessionId));
      if (pendingRead.current?.session.id === targetSessionId) pendingRead.current = null;
      setDeleteConfirmationSessionId(""); setEditingSessionId(""); setOpenSessionMenuId("");
      afterDelete.current?.(targetSessionId, remaining);
    } catch (error) {
      if (currentScope.current !== scope || controller.signal.aborted) return;
      const archiveError = error instanceof ApiError && error.message === "session_has_active_agent_run"
        ? "archive.activeRun" : "archive.archiveFailed";
      setActionError({ sessionId: targetSessionId, message: t(archive ? archiveError : "appRoute.unableToDeleteThisConversationPleaseTryAgain") });
    } finally {
      if (currentScope.current === scope && mutation.current?.controller === controller) { mutation.current = null; setBusy(""); flushPendingRead(); }
    }
  }

  return {
    editingSessionId, savingSessionId, openSessionMenuId, deletingSessionId, archivingSessionId, deleteConfirmationSessionId, actionError,
    setEditingSessionId, setOpenSessionMenuId, setDeleteConfirmationSessionId,
    updateSession, markSessionRead, renameSession,
    deleteSession: sessionId => removeFromNavigation(sessionId),
    onArchive: session => removeFromNavigation(session.id, true),
    onRename: session => { setEditingSessionId(session.id); setOpenSessionMenuId(""); },
    onTogglePin: session => toggleMetadata(session, "isPinned"),
    onToggleUnread: session => toggleMetadata(session, "isUnread"),
    onRequestDelete: session => {
      setActionError(value => value?.sessionId === session.id ? null : value);
      setOpenSessionMenuId("");
      setDeleteConfirmationSessionId(session.id);
    },
  };
}
