import { useTranslation } from "../i18n";
import { useEffect, useRef, useState } from "react";
import { Link, useNavigate, useParams, useRevalidator, useRouteLoaderData } from "react-router";
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
    || typeof session.agentId !== "string"
    || !["active", "deleted"].includes(session.status)) {
    throw new Error("session_response_invalid");
  }
  return session;
}

export default function TrashSessionRoute() {
  const { t } = useTranslation();
  const { sessionId = "" } = useParams();
  const { workspace, agents } = useRouteLoaderData("workspace");
  const base = `/w/${encodeURIComponent(workspace.id)}`;
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
  const [error, setError] = useState("");

  useEffect(() => {
    const controller = new AbortController();
    olderRequestRef.current?.controller.abort();
    olderRequestRef.current = null;
    store.clear();
    setSession(null);
    setLoading(true);
    setLoadingOlder(false);
    setError("");
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
      if (loadedSession.status === "active"
        && agents.some((agent) => agent.id === loadedSession.agentId)) {
        navigate(`${base}/agents/${encodeURIComponent(loadedSession.agentId)}?sessionId=${encodeURIComponent(sessionId)}`, { replace: true });
        return;
      }
      setSession(loadedSession);
      store.openTail(tailPage);
    }).catch((requestError) => {
      if (controller.signal.aborted) return;
      if (requestError.status === 404) navigate(`${base}/app`, { replace: true });
      else setError(t("trashSessionRoute.unableToLoadConversationHistoryValue", { value1: requestError.message }));
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });
    return () => {
      controller.abort();
      olderRequestRef.current?.controller.abort();
      olderRequestRef.current = null;
      store.clear();
    };
  }, [agents, base, navigate, sessionId, store, transport, workspace.id, t]);

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
    try {
      const page = await transport.loadOlder({
        sessionId: current.sessionId,
        projectionVersion: current.projectionVersion,
        projectionGeneration: current.projectionGeneration,
        sourceHighWater: current.sourceHighWater,
      }, current.olderCursor, controller.signal);
      if (olderRequestRef.current !== request) return;
      store.prependPage(page, current.viewEpoch);
    } catch (requestError) {
      if (!controller.signal.aborted) {
        setError(t("trashSessionRoute.unableToLoadEarlierMessagesValue", { value1: requestError.message }));
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
    setError("");
    try {
      await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/restore`, { method: "POST" });
      await revalidator.revalidate();
      navigate(`${base}/agents/${encodeURIComponent(session.agentId)}?sessionId=${encodeURIComponent(session.id)}`);
    } catch (requestError) {
      if (requestError.status === 404 || requestError.status === 410) navigate(`${base}/app`, { replace: true });
      else if (requestError.message === "agent_deleted") setError(t("trashSessionRoute.restoreThisConversationSAgentFirst"));
      else if (requestError.message === "session_not_deleted") navigate(`${base}/agents/${encodeURIComponent(session.agentId)}?sessionId=${encodeURIComponent(session.id)}`, { replace: true });
      else setError(t("trashSessionRoute.unableToRestoreValue", { value1: requestError.message }));
    } finally {
      setRestoring(false);
    }
  }

  async function purge() {
    if (!session || purging) return;
    setPurging(true);
    setError("");
    try {
      await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/trash`, { method: "DELETE" });
      await revalidator.revalidate();
      navigate(`${base}/app`, { replace: true });
    } catch (requestError) {
      if (requestError.status === 404 || requestError.status === 410 || requestError.message === "session_not_deleted") navigate(`${base}/app`, { replace: true });
      else setError(t("trashSessionRoute.unableToDeletePermanentlyValue", { value1: requestError.message }));
    } finally {
      setPurging(false);
    }
  }

  const parentActive = session && agents.some((agent) => agent.id === session.agentId);
  const remainingDays = session?.deletedAt ? Math.max(1, Math.ceil((new Date(session.deletedAt).valueOf() + 30 * 86400000 - Date.now()) / 86400000)) : null;

  return (
    <ShellPage>
      <div className="shDeletedTopbar"><Link to={`${base}/app`}><ArrowLeft aria-hidden="true" />{t("trashSessionRoute.back")}</Link><span>{session?.title || t("trashSessionRoute.deletedConversation")}</span></div>
      <div className="shDeletedBanner">
        <span>{parentActive ? remainingDays ? t("trash.remaining", { count: remainingDays }) : t("trash.inTrash") : t("trashSessionRoute.thisConversationIsHiddenWithItsDeletedAgentRestore")}</span>
        {session?.status === "deleted" && parentActive ? <button type="button" disabled={restoring} onClick={restore}>{restoring ? <LoaderCircle className="statusIcon" aria-hidden="true" /> : <RotateCcw aria-hidden="true" />}{t("trashSessionRoute.restoreConversation")}</button> : null}
        {session?.status === "deleted" ? <button type="button" disabled={purging} onClick={() => setPurgeOpen(true)}><Trash2 aria-hidden="true" />{t("trashSessionRoute.deletePermanently")}</button> : null}
      </div>
      {error ? <div className="errorBanner" role="alert">{error}</div> : null}
      <section className="shTrashHistory" aria-label={t("trashSessionRoute.readOnlyConversationHistory")}>
        <TranscriptBlockList
          store={store}
          sessionId={sessionId}
          loadingHistory={loading}
          loadingOlderHistory={loadingOlder}
          onLoadOlderHistory={loadOlder}
        />
      </section>
      <ConfirmDialog open={purgeOpen} title={t("appRoute.deleteThisConversation")} busy={purging} onCancel={() => setPurgeOpen(false)} onConfirm={() => void purge()} />
    </ShellPage>
  );
}
