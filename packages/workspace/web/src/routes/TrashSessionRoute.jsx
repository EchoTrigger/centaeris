import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Link, useLocation, useNavigate, useParams, useRevalidator, useRouteLoaderData } from "react-router";
import { ArrowLeft, LoaderCircle, RotateCcw, Trash2 } from "lucide-react";
import { apiJson } from "../api";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { createTranscriptViewStore } from "../chat/transcriptViewStore";
import { createWorkspaceTranscriptTransport } from "../chat/transcriptTransport";
import { TranscriptBlockList } from "../chat/TranscriptBlockList";
import { ShellPage } from "../shell/ShellPage";

function requireSessionEnvelope(value, sessionId, workspaceId) {
  const session = value?.session;
  if (!session || session.id !== sessionId || session.workspaceId !== workspaceId
    || !["active", "deleted"].includes(session.status)) {
    throw new Error("session_response_invalid");
  }
  return session;
}

export default function TrashSessionRoute() {
  const { t } = useTranslation();
  const { sessionId = "" } = useParams();
  const { workspace } = useRouteLoaderData("workspace");
  const base = `/w/${encodeURIComponent(workspace.id)}`;
  const deletedView = `${base}/settings/archive?view=deleted`;
  const { state } = useLocation();
  const navigate = useNavigate();
  const revalidator = useRevalidator();
  const storeRef = useRef(null);
  if (!storeRef.current) storeRef.current = createTranscriptViewStore();
  const store = storeRef.current;
  const transportRef = useRef(null);
  if (!transportRef.current) transportRef.current = createWorkspaceTranscriptTransport();
  const transport = transportRef.current;
  const olderRequestRef = useRef(null);
  const [session, setSession] = useState(null);
  const [loading, setLoading] = useState(true);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [purging, setPurging] = useState(false);
  const [purgeOpen, setPurgeOpen] = useState(false);
  const [readError, setReadError] = useState("");
  const [actionError, setActionError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    olderRequestRef.current?.controller.abort();
    olderRequestRef.current = null;
    store.clear();
    setSession(null);
    setLoading(true);
    setLoadingOlder(false);
    setReadError("");
    setActionError("");
    Promise.all([
      apiJson(`/api/sessions/${encodeURIComponent(sessionId)}`, {
        signal: controller.signal,
      }),
      transport.loadTail(sessionId, controller.signal),
    ]).then(([sessionResult, tailPage]) => {
      if (controller.signal.aborted) return;
      const loadedSession = requireSessionEnvelope(
        sessionResult,
        sessionId,
        workspace.id,
      );
      if (loadedSession.status === "active") {
        navigate(`${base}/sessions/${encodeURIComponent(sessionId)}`, { replace: true });
        return;
      }
      setSession(loadedSession);
      store.openTail(tailPage);
    }).catch((requestError) => {
      if (controller.signal.aborted) return;
      if (requestError.status === 404) navigate(`${base}/app`, { replace: true });
      else setReadError(t("trashSessionRoute.unableToLoadConversationHistory"));
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });
    return () => {
      controller.abort();
      olderRequestRef.current?.controller.abort();
      olderRequestRef.current = null;
      store.clear();
    };
  }, [base, navigate, sessionId, store, transport, workspace.id, t]);

  async function loadOlder() {
    const current = store.getListSnapshot();
    if (!current.hasOlder || !current.olderCursor || loadingOlder
      || !current.sessionId || !current.projectionVersion
      || !current.projectionGeneration) return;
    const controller = new AbortController();
    const request = {
      controller,
      viewEpoch: current.viewEpoch,
      olderCursor: current.olderCursor,
    };
    olderRequestRef.current = request;
    setLoadingOlder(true);
    setReadError("");
    try {
      const page = await transport.loadOlder({
        sessionId: current.sessionId,
        projectionVersion: current.projectionVersion,
        projectionGeneration: current.projectionGeneration,
        sourceHighWater: current.sourceHighWater,
      }, current.olderCursor, controller.signal);
      if (olderRequestRef.current !== request) return;
      store.prependPage(page, current.viewEpoch);
    } catch {
      if (!controller.signal.aborted) {
        setReadError(t("trashSessionRoute.unableToLoadEarlierMessages"));
      }
    } finally {
      if (olderRequestRef.current === request) {
        olderRequestRef.current = null;
        setLoadingOlder(false);
      }
    }
  }

  async function restore() {
    if (!session || session.status !== "deleted" || restoring) return;
    setRestoring(true);
    setActionError("");
    try {
      await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/restore`, { method: "POST" });
      await revalidator.revalidate();
      navigate(`${base}/sessions/${encodeURIComponent(session.id)}`);
    } catch (requestError) {
      if (requestError.status === 404 || requestError.status === 410) navigate(deletedView, { replace: true, state });
      else if (requestError.message === "session_not_deleted") navigate(`${base}/sessions/${encodeURIComponent(session.id)}`, { replace: true });
      else setActionError(t("trashSessionRoute.unableToRestore"));
    } finally {
      setRestoring(false);
    }
  }

  async function purge() {
    if (!session || purging) return;
    setPurging(true);
    setActionError("");
    try {
      await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/trash`, { method: "DELETE" });
      await revalidator.revalidate();
      navigate(deletedView, { replace: true, state });
    } catch (requestError) {
      if (requestError.status === 404 || requestError.status === 410 || requestError.message === "session_not_deleted") navigate(deletedView, { replace: true, state });
      else setActionError(t("trashSessionRoute.unableToDeletePermanently"));
    } finally {
      setPurging(false);
    }
  }

  const remainingDays = session?.deletedAt ? Math.max(1, Math.ceil((new Date(session.deletedAt).valueOf() + 30 * 86400000 - Date.now()) / 86400000)) : null;

  return (
    <ShellPage mainClassName="shDeletedSessionPage">
      <div className="shDeletedTopbar">
        <Link to={deletedView} state={state}><ArrowLeft aria-hidden="true" />{t("settingsRoute.recentlyDeleted")}</Link>
        <span>{session?.title || t("trashSessionRoute.deletedConversation")}</span>
        {session?.status === "deleted" ? <div className="shDeletedActions">
          <small>{remainingDays ? t("trash.remaining", { count: remainingDays }) : t("trash.inTrash")}</small>
          {!purgeOpen && actionError ? <small className="error" role="alert">{actionError}</small> : null}
          <button type="button" className="pluginEnableButton" disabled={restoring} onClick={restore}>{restoring ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : <RotateCcw aria-hidden="true" />}{t("trashSessionRoute.restoreConversation")}</button>
          <button type="button" className="pluginEnableButton" disabled={purging} onClick={() => { setActionError(""); setPurgeOpen(true); }}><Trash2 aria-hidden="true" />{t("trashSessionRoute.deletePermanently")}</button>
        </div> : null}
      </div>
      <section className="shTrashHistory" aria-label={t("trashSessionRoute.readOnlyConversationHistory")}>
        {session && readError ? <p className="shTrashReadError" role="alert">{readError} <button type="button" className="pluginEnableButton" onClick={() => void loadOlder()}>{t("agentChat.retry")}</button></p> : null}
        <TranscriptBlockList
          store={store}
          sessionId={sessionId}
          loadingHistory={loading}
          loadingOlderHistory={loadingOlder}
          onLoadOlderHistory={loadOlder}
          emptyState={readError ? <p className="libraryCatalogEmpty" role="alert">{readError}</p> : null}
        />
      </section>
      <ConfirmDialog open={purgeOpen} title={t("appRoute.deleteThisConversation")} message={actionError} busy={purging} onCancel={() => { setActionError(""); setPurgeOpen(false); }} onConfirm={() => void purge()} />
    </ShellPage>
  );
}
