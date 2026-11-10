import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { Trash2 } from "lucide-react";
import { ApiError, apiJson } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { useTranslation } from "../i18n";
import { RecentlyDeleted } from "./RecentlyDeleted";

export function SessionArchive({ workspace, returnTo }) {
  const [params] = useSearchParams();
  return params.get("view") === "deleted"
    ? <ArchiveLayout returnTo={returnTo} showDeleted><RecentlyDeleted workspace={workspace} /></ArchiveLayout>
    : <ArchivedSessions workspace={workspace} returnTo={returnTo} />;
}

function ArchiveLayout({ returnTo, showDeleted = false, action, children }) {
  const { t } = useTranslation();
  return <div className="preferenceSettings archiveSettings">
    <header className="settingsPageHeading"><h1>{t("archive.title")}</h1>{action}</header>
    <nav className="extensionTabs archiveViews" aria-label={t("archive.title")}>
      <Link to={{ search: "" }} state={{ returnTo }} aria-current={!showDeleted ? "page" : undefined}>{t("archive.archivedConversations")}</Link>
      <Link to={{ search: "?view=deleted" }} state={{ returnTo }} aria-current={showDeleted ? "page" : undefined}>{t("settingsRoute.recentlyDeleted")}</Link>
    </nav>
    {children}
  </div>;
}

function ArchivedSessions({ workspace, returnTo }) {
  const { t } = useTranslation();
  const [sessions, setSessions] = useState(null);
  const [error, setError] = useState("");
  const [restoreErrorId, setRestoreErrorId] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [busy, setBusy] = useState("");
  const [deleteTarget, setDeleteTarget] = useState(null);
  const [deleteFailed, setDeleteFailed] = useState(false);
  const id = encodeURIComponent(workspace.id);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Retry explicitly reloads the archive.
  useEffect(() => {
    const controller = new AbortController();
    apiJson("/api/workspaces/" + id + "/archived-sessions", { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setSessions(result.sessions); })
      .catch(() => { if (!controller.signal.aborted) setError(t("archive.loadFailed")); });
    return () => controller.abort();
  }, [id, attempt, t]);

  async function restore(session) {
    setBusy(session.id);
    setRestoreErrorId("");
    try {
      const result = await apiJson("/api/sessions/" + encodeURIComponent(session.id) + "/archive", { method: "DELETE" });
      if (result.session.id !== session.id || result.session.workspaceId !== workspace.id) throw new Error("archive_session_identity_invalid");
      setSessions(items => items.filter(item => item.id !== session.id));
    } catch {
      setRestoreErrorId(session.id);
    } finally {
      setBusy("");
    }
  }

  async function deleteSessions() {
    const { ids, all } = deleteTarget;
    setBusy(ids[0]);
    setDeleteFailed(false);
    const removedIds = new Set();
    const failedIds = [];
    for (const sessionId of ids) {
      try {
        const result = await apiJson("/api/sessions/" + encodeURIComponent(sessionId), { method: "DELETE" });
        if (result.deleted !== true) throw new Error("archive_delete_response_invalid");
      } catch (requestError) {
        if (!(requestError instanceof ApiError && requestError.status === 410 && requestError.message === "session_deleted")) {
          failedIds.push(sessionId);
          continue;
        }
      }
      removedIds.add(sessionId);
    }
    setSessions(items => items.filter(item => !removedIds.has(item.id)));
    setDeleteTarget(failedIds.length ? { ids: failedIds, all } : null);
    setDeleteFailed(Boolean(failedIds.length));
    setBusy("");
  }

  function openDelete(ids, all = false) {
    setDeleteFailed(false);
    setDeleteTarget({ ids, all });
  }

  const deleteErrorMessage = deleteTarget?.all ? "archive.bulkDeleteFailed" : "appRoute.unableToDeleteThisConversationPleaseTryAgain";
  return <ArchiveLayout returnTo={returnTo} action={sessions?.length ? <button type="button"
    className="settingsArchiveDeleteAll" disabled={Boolean(busy)} onClick={() => openDelete(sessions.map(item => item.id), true)}>
    <Trash2 aria-hidden="true" />{t("archive.deleteAll")}
  </button> : null}>
    {sessions === null ? error
      ? <div className="settingsPanel" role="alert">{error} <button className="pluginEnableButton" type="button" onClick={() => { setError(""); setAttempt(value => value + 1); }}>{t("agentChat.retry")}</button></div>
      : <p role="status">{t("agentChat.loading")}</p>
      : sessions.length ? <ul className="settingsPanel settingsListPanel settingsArchiveList">
        {sessions.map(session => <li className="settingsArchiveRow" key={session.id}>
          <span className="managementRowCopy"><strong>{session.title}</strong>{restoreErrorId === session.id ? <small className="error" role="alert">{t("archive.restoreFailed")}</small> : null}</span>
          <button type="button" className="settingsArchiveDelete" disabled={Boolean(busy)} aria-label={t("archive.deleteTitle", { title: session.title })}
            onClick={() => openDelete([session.id])}><Trash2 aria-hidden="true" /></button>
          <button type="button" className="pluginEnableButton" disabled={Boolean(busy)} aria-label={t("archive.restoreTitle", { title: session.title })}
            onClick={() => void restore(session)}>{t("archive.restore")}</button>
        </li>)}
      </ul> : <div className="settingsPanel" role="status">{t("archive.empty")}</div>}
    <ConfirmDialog open={Boolean(deleteTarget)} title={t(deleteTarget?.all ? "archive.confirmDeleteAll" : "appRoute.deleteThisConversation")}
      message={t(deleteFailed ? deleteErrorMessage : "archive.deleteConfirmDescription")}
      confirmLabel={t("appRoute.delete")} popupClassName="settingsArchiveConfirm" busy={Boolean(busy)}
      onCancel={() => setDeleteTarget(null)} onConfirm={() => void deleteSessions()} />
  </ArchiveLayout>;
}
