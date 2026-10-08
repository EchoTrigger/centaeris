import { useTranslation } from "../i18n";
import { useCallback, useEffect, useEffectEvent, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { matchPath, useNavigate, useRouteLoaderData } from "react-router";
import { apiResponse } from "../api";
import { OperationClient, acceptedConversationReviewLink, consumeReviewedOperation, loadAcceptedConversation } from "../chat/operationReceipts";
import { WorkspaceContextPanel } from "../components/WorkspaceContextPanel";
import { DocumentPreview } from "../components/DocumentPreview";
import { officeFileType } from "../chat/officeFormats.mjs";
import { officePreviewUrl } from "../chat/attachments.mjs";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { useModalDialog } from "../components/useModalDialog";
import { streamWorkspaceAgentRun } from "../chat/workspaceWebTransport";
import { createTranscriptViewStore } from "../chat/transcriptViewStore";
import { compareTranscriptBlocks } from "../chat/transcriptContract";
import { hasCommittedPendingMessage } from "../chat/transcriptTurns";
import { TranscriptToolOperationRegistry } from "../chat/transcriptToolOperations";
import { createWorkspaceTranscriptTransport } from "../chat/transcriptTransport";
import { WorkspaceTranscriptController } from "../chat/workspaceTranscriptController";
import { TranscriptBlockList } from "../chat/TranscriptBlockList";
import { parseSessionInputOrigin } from "../agent-chat/agentWorkSessions";
import {
  clearTranscriptContentRangeCache,
  subscribeTranscriptContentRangeCache,
  transcriptContentRangeCacheBytes,
} from "../chat/transcriptContentRanges";
import { WorkspaceComposer } from "../chat/WorkspaceComposer";
import {
  attachmentCanPreview,
  attachmentDownloadUrl,
  attachmentIsImage,
  attachmentPreviewUrl,
} from "../chat/attachments.mjs";
import { MAX_UPLOAD_BATCH_FILES } from "../upload";
import {
  readModelThinkingMode,
  readPreferredModelIdentity,
  useEnterStartsNewLine,
  writeModelThinkingMode,
  writePreferredModelIdentity,
} from "../preferences";
import { HomePlane, HomeQuickActions } from "../shell/HomePlane";
import { SessionAgentBreadcrumb } from "../agent-chat/SessionAgentBreadcrumb";
import { ShellSidebar } from "../shell/ShellSidebar";
import { groupSessionNavigation, sortSessionNavigation as sortSessions } from "../shell/sessionNavigation.mjs";
import { SessionNavigationRow } from "../shell/SessionNavigationRow";
import { useSessionNavigationActions } from "../shell/useSessionNavigationActions";
import {
  Image as ImageIcon,
  PanelLeft,
  RefreshCw,
  X,
} from "lucide-react";

const CLOSED_CONTEXT_PANEL = Object.freeze({ mode: "closed" });
const TRANSCRIPT_MEMORY_WARNING_BYTES = 32 * 1024 * 1024;

const ARTIFACT_PREVIEW_MAX_BYTES = 1024 * 1024;
const ARTIFACT_PREVIEW_CONTENT_TYPES = new Set([
  "text/plain",
  "text/markdown",
  "application/pdf",
  "image/png",
  "image/jpeg",
  "image/webp",
]);

function upsertBy(items, key, value) {
  const index = items.findIndex((item) => item[key] === value[key]);
  if (index < 0) return [...items, value];
  return items.map((item, itemIndex) => (itemIndex === index ? value : item));
}

function ConnectedWorkspaceContextPanel({ panel, browserWidthPx, onBrowserWidthChange, onClose, onReturn }) {
  useTranslation();
  return (
    <WorkspaceContextPanel
      panel={panel}
      browserWidthPx={browserWidthPx}
      onBrowserWidthChange={onBrowserWidthChange}
      onClose={onClose}
      onReturn={onReturn}
    />
  );
}

export function AppPageContent({ agentId, workspaceDraft, location, modelsVersion, onSessionAccepted }) {
  const { t } = useTranslation();
  const { user } = useRouteLoaderData("authenticated");
  const { workspace, agents } = useRouteLoaderData("workspace");
  const operations = useMemo(() => new OperationClient({ userId: user.id, workspaceId: workspace.id, command: "submitMessage" }), [user.id, workspace.id]);
  const [operationReviewLink, setOperationReviewLink] = useState(null);
  const navigate = useNavigate();
  const searchParams = new URLSearchParams(location.search);
  const requestedSessionId = searchParams.get("sessionId") || "";
  const requestedProjectId = searchParams.get("projectId") || "";
  const requestedPrompt = searchParams.get("prompt") || "";
  const startFresh = searchParams.get("new") === "1";
  const activeAgent = agents.find((agent) => agent.id === agentId);
  if (!activeAgent) throw new Error("agent_not_found");
  const workspaceBase = `/w/${encodeURIComponent(workspace.id)}`;
  const requestScopeKey = JSON.stringify([workspace.id, activeAgent.id, workspaceDraft, requestedSessionId, startFresh, requestedProjectId]);
  const requestScopeRef = useRef({ key: requestScopeKey, active: true });
  if (requestScopeRef.current.key !== requestScopeKey) requestScopeRef.current = { key: requestScopeKey, active: true };
  const streamAbortRef = useRef(null);
  const chatControllerRef = useRef(null);
  const acceptedSessionRef = useRef(null);
  const acceptedRouteSessionIdRef = useRef("");
  const transcriptStoreRef = useRef(null);
  if (!transcriptStoreRef.current) transcriptStoreRef.current = createTranscriptViewStore();
  const transcriptStore = transcriptStoreRef.current;
  const toolOperationsRef = useRef(null);
  if (!toolOperationsRef.current) toolOperationsRef.current = new TranscriptToolOperationRegistry();
  const toolOperations = toolOperationsRef.current;
  const transcriptTransportRef = useRef(null);
  if (!transcriptTransportRef.current) transcriptTransportRef.current = createWorkspaceTranscriptTransport();
  const transcriptTransport = transcriptTransportRef.current;
  const fileInputRef = useRef(null);
  const composerRef = useRef(null);
  const composerStartRectRef = useRef(null);
  const [models, setModels] = useState([]);
  const modelsRef = useRef([]);
  const [modelId, setModelId] = useState("");
  const [thinkingMode, setThinkingMode] = useState("");
  const [sessions, setSessions] = useState([]);
  const [projects, setProjects] = useState([]);
  const [sessionId, setSessionId] = useState("");
  const [initialInputOrigin, setInitialInputOrigin] = useState(null);
  const [assets, setAssets] = useState([]);
  const [pendingAttachmentIds, setPendingAttachmentIds] = useState([]);
  const [pendingUploadFiles, setPendingUploadFiles] = useState([]);
  const [draft, setDraft] = useState("");
  const [pendingUserMessage, setPendingUserMessage] = useState(null);
  const pendingUserMessageRef = useRef(null);
  useEffect(() => { pendingUserMessageRef.current = pendingUserMessage; }, [pendingUserMessage]);
  const [workTime, setWorkTime] = useState(null);
  const enterStartsNewLine = useEnterStartsNewLine(user.id);
  const [error, setError] = useState("");
  const [streamIssue, setStreamIssue] = useState(null);
  const [sending, setSending] = useState(false);
  const [uploadingAttachment, setUploadingAttachment] = useState(false);
  const [attachmentPreview, setAttachmentPreview] = useState(null);
  const sessionActions = useSessionNavigationActions({ scopeKey: JSON.stringify([workspace.id, activeAgent.id]), sessions, onChangeSessions: setSessions, onError: setError, onDeleted: onSessionDeleted });
  const { deletingSessionId, deleteConfirmationSessionId, setDeleteConfirmationSessionId, setEditingSessionId, setOpenSessionMenuId, updateSession, markSessionRead, deleteSession } = sessionActions;
  const [cancellingAgentRunId, setCancellingRunId] = useState("");
  const [loadingHistory, setLoadingHistory] = useState(!workspaceDraft);
  const [loadingOlderHistory, setLoadingOlderHistory] = useState(false);
  const [activeTranscriptRun, setActiveTranscriptRun] = useState(null);
  const [contextPanel, setContextPanel] = useState(CLOSED_CONTEXT_PANEL);
  const [browserPanelWidthPx, setBrowserPanelWidthPx] = useState(270);
  const [isSidebarOpen, setIsSidebarOpen] = useState(true);
  const attachmentDialogRef = useModalDialog({ open: Boolean(attachmentPreview), onClose: () => setAttachmentPreview(null) });
  const previewRequestIdRef = useRef(0);
  const activeSessionIdRef = useRef(sessionId);
  const activeTranscriptRunRef = useRef(activeTranscriptRun);
  const olderHistoryRequestRef = useRef(null);
  activeSessionIdRef.current = sessionId;
  activeTranscriptRunRef.current = activeTranscriptRun;
  const updateActiveTranscriptRun = useCallback((next) => {
    activeTranscriptRunRef.current = next;
    setActiveTranscriptRun(next);
  }, []);

  const transcriptList = useSyncExternalStore(
    transcriptStore.subscribeList,
    transcriptStore.getListSnapshot,
    transcriptStore.getListSnapshot,
  );
  const transcriptBlockBytes = useSyncExternalStore(
    transcriptStore.subscribeManagedContent,
    transcriptStore.managedContentBytes,
    transcriptStore.managedContentBytes,
  );
  const transcriptReferencedContentBytes = useSyncExternalStore(
    subscribeTranscriptContentRangeCache,
    transcriptContentRangeCacheBytes,
    transcriptContentRangeCacheBytes,
  );
  const transcriptManagedBytes = transcriptBlockBytes + transcriptReferencedContentBytes;
  const currentModel = useMemo(() => models.find((model) => model.id === modelId), [models, modelId]);
  const modelGroups = useMemo(() => {
    const groups = new Map();
    models.forEach((model) => {
      const provider = model.providerId || "builtin";
      const providerModels = groups.get(provider) || [];
      providerModels.push(model);
      groups.set(provider, providerModels);
    });
    return [...groups].map(([provider, providerModels]) => ({
      provider,
      label: providerModels[0].providerDisplayName || t("appRoute.models"),
      models: providerModels,
    }));
  }, [models, t]);
  useEffect(() => {
    setThinkingMode(readModelThinkingMode(user.id, workspace.id, currentModel));
  }, [currentModel, user.id, workspace.id]);
  const selectThinkingMode = useCallback((mode) => {
    if (!currentModel) return;
    writeModelThinkingMode(user.id, workspace.id, currentModel, mode);
    setThinkingMode(mode);
  }, [currentModel, user.id, workspace.id]);
  const selectModel = useCallback((id) => {
    const selected = models.find((model) => model.id === id);
    if (selected) writePreferredModelIdentity(user.id, workspace.id, selected);
    setModelId(id);
  }, [models, user.id, workspace.id]);
  const hasActiveAgentRun = activeTranscriptRun !== null;
  const activeAgentRunId = activeTranscriptRun?.agentRunId || "";
  const pendingAttachments = useMemo(
    () => pendingAttachmentIds
      .map((id) => assets.find((link) => link.id === id))
      .filter(Boolean),
    [assets, pendingAttachmentIds],
  );
  const activeSession = sessions.find((candidate) => candidate.id === sessionId);

  const acceptModels = useCallback((nextModels) => {
    const previousModels = modelsRef.current;
    modelsRef.current = nextModels;
    setModels(nextModels);
    setModelId((current) => {
      if (nextModels.some((model) => model.id === current)) return current;
      if (!current) {
        if (previousModels.length) return "";
        const preferred = readPreferredModelIdentity(user.id, workspace.id);
        return nextModels.find((model) => JSON.stringify([model.providerId, model.modelName]) === preferred)?.id
          || nextModels[0]?.id || "";
      }
      const previous = previousModels.find((model) => model.id === current);
      return nextModels.find((model) =>
        model.providerId === previous?.providerId && model.modelName === previous?.modelName
      )?.id || "";
    });
  }, [user.id, workspace.id]);

  const clearContextPanel = useCallback(() => {
    previewRequestIdRef.current += 1;
    setContextPanel(CLOSED_CONTEXT_PANEL);
  }, []);
  const clearConversationProjection = useCallback(() => {
    setInitialInputOrigin(null);
    transcriptStore.clear();
    toolOperations.clear();
    clearTranscriptContentRangeCache();
    updateActiveTranscriptRun(null);
    olderHistoryRequestRef.current?.controller.abort();
    olderHistoryRequestRef.current = null;
    setAssets([]);
    setPendingAttachmentIds([]);
    setPendingUploadFiles([]);
  }, [transcriptStore, toolOperations, updateActiveTranscriptRun]);
  const stopActiveStream = useCallback(() => {
    streamAbortRef.current?.abort();
    chatControllerRef.current?.dispose();
  }, []);
  const reportLoadError = useEffectEvent((key) => setError(t(key)));
  const markSelectedSessionRead = useEffectEvent(markSessionRead);
  const connectLoadedAgentRun = useEffectEvent(connectAgentRun);
  const handleLoadedAgentRunStreamFailure = useEffectEvent(handleAgentRunStreamFailure);

  // biome-ignore lint/correctness/useExhaustiveDependencies: The version is an explicit refresh signal from retained settings routes.
  useEffect(() => {
    const controller = new AbortController();
    async function load() {
      try {
        const response = await apiResponse("/api/models", { signal: controller.signal });
        const data = await response.json();
        if (!controller.signal.aborted) acceptModels(data.models);
      } catch {
        if (!controller.signal.aborted) reportLoadError("appRoute.unableToRefreshModelsPleaseTryAgain");
      }
    }
    load();
    return () => controller.abort();
  }, [modelsVersion, acceptModels]);

  useEffect(() => {
    if (contextPanel.mode !== "filePreview" || !contextPanel.objectUrl) return undefined;
    return () => URL.revokeObjectURL(contextPanel.objectUrl);
  }, [contextPanel]);

  useEffect(() => {
    if (contextPanel.mode === "closed") return undefined;
    const closeOnEscape = (event) => {
      if (event.key === "Escape") clearContextPanel();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [contextPanel.mode, clearContextPanel]);

  useEffect(() => {
    requestScopeRef.current.active = true;
    return () => {
      requestScopeRef.current.active = false;
      stopActiveStream();
    };
  }, [stopActiveStream]);

  useEffect(() => {
    if (!startFresh && requestedSessionId && acceptedRouteSessionIdRef.current === requestedSessionId) {
      acceptedRouteSessionIdRef.current = "";
      setLoadingHistory(false);
      return undefined;
    }
    let active = true;
    setInitialInputOrigin(null);
    const navigationScopeKey = requestScopeKey;
    setLoadingHistory(!workspaceDraft);
    acceptedSessionRef.current = null;
    acceptedRouteSessionIdRef.current = "";
    setSessions([]);
    setSessionId("");
    clearConversationProjection();
    setDraft("");
    setSending(false);
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
    Promise.all([
      apiResponse(`/api/workspaces/${workspace.id}/sessions?agentId=${encodeURIComponent(activeAgent.id)}`).then((response) => response.json()),
      apiResponse(`/api/workspaces/${workspace.id}/session-projects?agentId=${encodeURIComponent(activeAgent.id)}`).then((response) => response.json()),
    ])
      .then(([data, projectData]) => {
        if (!active || requestScopeRef.current.key !== navigationScopeKey) return;
        if (!Array.isArray(data.sessions) || !Array.isArray(projectData.projects)) throw new Error("session_navigation_response_invalid");
        setSessions(data.sessions);
        setProjects(projectData.projects);
        const selectedSession = workspaceDraft || startFresh
          ? null
          : data.sessions.find((session) => session.id === requestedSessionId)
            || data.sessions[0]
            || null;
        const selectedSessionId = selectedSession?.id || "";
        setSessionId(selectedSessionId);
        if (selectedSession?.isUnread) markSelectedSessionRead(selectedSession);
        if (!selectedSessionId) setLoadingHistory(false);
      })
      .catch(() => {
        if (!active || requestScopeRef.current.key !== navigationScopeKey) return;
        reportLoadError("appRoute.unableToLoadConversationsRefreshThePageAndTry");
        setLoadingHistory(false);
      });
    return () => { active = false; };
  }, [workspace.id, workspaceDraft, requestedSessionId, requestScopeKey, startFresh, activeAgent.id, clearConversationProjection, clearContextPanel, setEditingSessionId, setOpenSessionMenuId]);

  useEffect(() => {
    if (requestedPrompt) setDraft(requestedPrompt);
  }, [requestedPrompt]);

  useEffect(() => {
    if (!startFresh || loadingHistory || !workspace || !modelId) return;
    window.requestAnimationFrame(() => document.getElementById("messageDraft")?.focus());
  }, [loadingHistory, modelId, startFresh, workspace]);

  useEffect(() => {
    if (!sessionId) {
      clearConversationProjection();
      return;
    }
    acceptedSessionRef.current = null;
    stopActiveStream();
    const controller = new AbortController();
    streamAbortRef.current = controller;
    let active = true;
    const originScopeKey = requestScopeRef.current.key;
    setInitialInputOrigin(null);
    transcriptStore.clear();
    toolOperations.clear();
    updateActiveTranscriptRun(null);
    setLoadingHistory(true);
    setError("");
    Promise.all([
      transcriptTransport.loadTail(sessionId, controller.signal),
      apiResponse(`/api/sessions/${sessionId}/assets`, { signal: controller.signal }),
      apiResponse(`/api/sessions/${encodeURIComponent(sessionId)}`, { signal: controller.signal }),
    ])
      .then(async ([tailPage, assetsResponse, sessionResponse]) => {
        const assetData = await assetsResponse.json();
        const origin = parseSessionInputOrigin(await sessionResponse.json(), workspace.id, activeAgent.id, sessionId);
        if (!active || controller.signal.aborted || requestScopeRef.current.key !== originScopeKey) return;
        const viewEpoch = transcriptStore.openTail(tailPage);
        const identity = {
          sessionId,
          projectionVersion: tailPage.projectionVersion,
          projectionGeneration: tailPage.projectionGeneration,
        };
        await transcriptTransport.loadPatches({
          ...identity,
          afterSourceHighWater: transcriptStore.getListSnapshot().appliedSourceHighWater,
        }, controller.signal, (patchPage) => {
          if (!transcriptStore.applyPatchPage(patchPage, viewEpoch)) {
            throw new Error("transcript view epoch changed while loading the tail");
          }
        });
        const appliedSourceHighWater = transcriptStore.getListSnapshot().appliedSourceHighWater;
        const activeEnvelope = await transcriptTransport.loadActiveAgentRun({
          ...identity,
          sourceHighWater: appliedSourceHighWater,
        }, controller.signal);
        if (!active || controller.signal.aborted || activeSessionIdRef.current !== sessionId) return;
        setInitialInputOrigin({ scope: originScopeKey, sessionId, value: origin });
        setAssets(assetData.assets);
        setPendingAttachmentIds([]);
        setPendingUploadFiles([]);
        updateActiveTranscriptRun(activeEnvelope.agentRun);
        const pendingOperation = operations.pending();
        if (pendingOperation?.receipt?.sessionId === sessionId) operations.complete(pendingOperation.operationId);
        if (activeEnvelope.agentRun) {
          connectLoadedAgentRun(activeEnvelope.agentRun.agentRunId, workspace.id, sessionId, controller, {
            cursor: activeEnvelope.agentRun.streamCursor,
            viewEpoch,
            identity,
          }).catch((streamError) => {
            handleLoadedAgentRunStreamFailure(streamError, activeEnvelope.agentRun.agentRunId);
          });
        }
      })
      .catch(errorValue => {
        if (!active || controller.signal.aborted) return;
        setInitialInputOrigin(null);
        if ([401, 403, 404, 409, 410].includes(errorValue?.status)) clearConversationProjection();
        reportLoadError("appRoute.unableToLoadConversationHistoryRefreshThePageAnd");
      })
      .finally(() => active && setLoadingHistory(false));
    return () => {
      active = false;
      controller.abort();
      chatControllerRef.current?.dispose();
    };
  }, [sessionId, workspace?.id, activeAgent.id, operations, clearConversationProjection, stopActiveStream, transcriptStore, transcriptTransport, toolOperations, updateActiveTranscriptRun]);

  function showStreamError(errorValue) {
    if (errorValue?.name === "AbortError") return;
    setError(t("appRoute.theActionCouldNotBeCompletedPleaseTryAgain"));
  }

  function handleAgentRunStreamFailure(errorValue, agentRunId) {
    if (errorValue?.name === "AbortError") return;
    if (activeTranscriptRunRef.current?.agentRunId === agentRunId) {
      if ([401, 403, 404, 410].includes(errorValue?.status)) {
        clearConversationProjection();
        setStreamIssue(null);
        setWorkTime(null);
        setError(t("appRoute.unableToLoadConversationHistoryRefreshThePageAnd"));
        return;
      }
      console.error("Transcript stream stopped", { agentRunId, error: errorValue });
      if (errorValue?.message === "agent_run_not_admitted") {
        const pending = pendingUserMessageRef.current;
        if (pending) setDraft((value) => value ? `${pending.text}\n\n${value}` : pending.text);
        setPendingUserMessage(null);
        updateActiveTranscriptRun(null);
        setStreamIssue(null);
        setWorkTime(null);
        setError(t("appRoute.runNotAdmitted"));
        return;
      }
      setStreamIssue({ sessionId: activeSessionIdRef.current, agentRunId, error: errorValue, reconnecting: false });
    }
  }

  async function reloadTranscriptStream(agentRunId, targetSessionId, signal) {
    const previous = transcriptStore.getListSnapshot();
    const stillCurrent = () => {
      if (signal.aborted || activeSessionIdRef.current !== targetSessionId
        || transcriptStore.getListSnapshot().viewEpoch !== previous.viewEpoch) {
        throw new DOMException("Aborted", "AbortError");
      }
    };
    const tail = await transcriptTransport.loadTail(targetSessionId, signal);
    stillCurrent();
    const identity = {
      sessionId: targetSessionId,
      projectionVersion: tail.projectionVersion,
      projectionGeneration: tail.projectionGeneration,
    };
    const envelope = await transcriptTransport.loadActiveAgentRun({
      ...identity, sourceHighWater: tail.sourceHighWater,
    }, signal);
    stillCurrent();
    const olderPages = [];
    const cursors = new Set();
    let oldest = tail.blocks[0];
    let cursor = tail.olderCursor;
    while (cursor !== null) {
      stillCurrent();
      const current = transcriptStore.getListSnapshot();
      const boundary = transcriptStore.getBlockSnapshot(current.blockIds[0]);
      if (!boundary || (oldest && compareTranscriptBlocks(oldest, boundary) <= 0)) break;
      if (cursors.has(cursor)) throw new Error("transcript recovery older cursor made no progress");
      cursors.add(cursor);
      const olderPage = await transcriptTransport.loadOlder({ ...identity, sourceHighWater: tail.sourceHighWater }, cursor, signal);
      stillCurrent();
      if ((olderPage.blocks.length === 0 && olderPage.hasOlder) || olderPage.olderCursor === cursor) {
        throw new Error("transcript recovery older page made no progress");
      }
      const last = olderPage.blocks.at(-1);
      if (last && oldest && compareTranscriptBlocks(last, oldest) >= 0) {
        throw new Error("transcript recovery older pages overlap");
      }
      olderPages.push(olderPage);
      oldest = olderPage.blocks[0] ?? oldest;
      cursor = olderPage.olderCursor;
    }
    stillCurrent();
    // Re-read just the loaded range from this frozen authority. Publishing the
    // tail and its older pages together preserves the user's reading position.
    const viewEpoch = transcriptStore.openTail(tail, olderPages);
    olderHistoryRequestRef.current?.controller.abort();
    olderHistoryRequestRef.current = null;
    setLoadingOlderHistory(false);
    updateActiveTranscriptRun(envelope.agentRun);
    setStreamIssue(null);
    if (!envelope.agentRun || envelope.agentRun.agentRunId !== agentRunId) return null;
    return { identity, viewEpoch, cursor: envelope.agentRun.streamCursor };
  }

  async function retryTranscriptStream() {
    const issue = streamIssue;
    if (!issue || issue.reconnecting || issue.sessionId !== activeSessionIdRef.current) return;
    stopActiveStream();
    const abortController = new AbortController();
    streamAbortRef.current = abortController;
    setStreamIssue({ ...issue, reconnecting: true });
    try {
      const resume = await reloadTranscriptStream(issue.agentRunId, issue.sessionId, abortController.signal);
      if (resume) await connectAgentRun(issue.agentRunId, workspace.id, issue.sessionId, abortController, resume);
    } catch (errorValue) {
      handleAgentRunStreamFailure(errorValue, issue.agentRunId);
    }
  }

  async function refreshSessions(targetWorkspaceId) {
    const response = await apiResponse(`/api/workspaces/${targetWorkspaceId}/sessions?agentId=${encodeURIComponent(activeAgent.id)}`);
    const data = await response.json();
    setSessions(data.sessions);
    return data.sessions;
  }

  async function refreshAgentRunResumeState(agentRunId, _targetWorkspaceId, targetSessionId) {
    const snapshot = transcriptStore.getListSnapshot();
    const envelope = await transcriptTransport.loadActiveAgentRun({
      sessionId: targetSessionId,
      projectionVersion: snapshot.projectionVersion,
      projectionGeneration: snapshot.projectionGeneration,
      sourceHighWater: snapshot.appliedSourceHighWater,
    }, streamAbortRef.current?.signal || new AbortController().signal);
    if (activeSessionIdRef.current === targetSessionId
      && activeTranscriptRunRef.current?.agentRunId === agentRunId) {
      updateActiveTranscriptRun(envelope.agentRun);
    }
  }

  async function connectAgentRun(agentRunId, targetWorkspaceId, targetSessionId, abortController, resume = {}) {
    chatControllerRef.current?.dispose();
    if (!resume.identity || !Number.isInteger(resume.viewEpoch)) {
      throw new Error("transcript stream identity is missing");
    }
    const createController = (snapshot) => new WorkspaceTranscriptController({
      store: transcriptStore,
      viewEpoch: snapshot.viewEpoch,
      identity: snapshot.identity,
      agentRunId,
      initialCursor: snapshot.cursor || "0-0",
      transport: transcriptTransport,
      signal: abortController.signal,
      onCommittedEvent: (event) => toolOperations.applyEvent(event),
      onTerminal: () => {
        if (activeSessionIdRef.current === targetSessionId) setWorkTime((value) => value ? { ...value, completedAtMs: Date.now() } : null);
        if (activeSessionIdRef.current === targetSessionId) updateActiveTranscriptRun(null);
      },
    });
    let controller = createController(resume);
    chatControllerRef.current = controller;
    let streamCompleted = false;
    try {
      await streamWorkspaceAgentRun({
        controller,
        signal: abortController.signal,
        onConnection: (connection) => {
          if (abortController.signal.aborted || activeSessionIdRef.current !== targetSessionId) return;
          setStreamIssue(connection === "reconnecting"
            ? { sessionId: targetSessionId, agentRunId, reconnecting: true } : null);
        },
        recover: async (errorValue) => {
          console.error("Transcript stream resynchronizing", {
            sessionId: targetSessionId, agentRunId, cursor: controller.lastCursor, error: errorValue,
          });
          await controller.whenIdle().catch(() => {});
          controller.dispose();
          const snapshot = await reloadTranscriptStream(agentRunId, targetSessionId, abortController.signal);
          if (!snapshot) return null;
          controller = createController(snapshot);
          chatControllerRef.current = controller;
          return controller;
        },
      });
      streamCompleted = true;
    } finally {
      try {
        if (streamCompleted && !abortController.signal.aborted) {
          if (activeTranscriptRunRef.current?.agentRunId === agentRunId) updateActiveTranscriptRun(null);
          try {
            const nextSessions = await refreshSessions(targetWorkspaceId);
            const current = nextSessions.find((item) => item.id === targetSessionId);
            if (activeSessionIdRef.current === targetSessionId && current?.isUnread) {
              await updateSession(targetSessionId, { isUnread: false });
            }
          } catch {
            setError(t("appRoute.unableToRefreshConversations"));
          }
        }
      } finally {
        if (chatControllerRef.current === controller) chatControllerRef.current = null;
        controller.dispose();
      }
    }
  }

  function startNewChat(projectId = "", sidebarTab = "chat") {
    if (sending) return;
    acceptedSessionRef.current = null;
    acceptedRouteSessionIdRef.current = "";
    stopActiveStream();
    setSessionId("");
    setLoadingHistory(false);
    clearConversationProjection();
    setDraft("");
    setError("");
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
    const newSessionSearch = new URLSearchParams({ new: "1" });
    if (projectId) newSessionSearch.set("projectId", projectId);
    navigate(workspaceDraft
      ? `/w/${encodeURIComponent(workspace.id)}/app`
      : `/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(activeAgent.id)}?${newSessionSearch}`, { state: { sidebarTab } });
  }

  function selectSession() {
    acceptedRouteSessionIdRef.current = "";
    setPendingUploadFiles([]);
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
  }

  async function createProject(name) {
    const response = await apiResponse(`/api/workspaces/${workspace.id}/session-projects`, {
      method: "POST",
      body: JSON.stringify({ agentId: activeAgent.id, name }),
    });
    const data = await response.json();
    if (
      !data?.project
      || data.project.workspaceId !== workspace.id
      || data.project.agentId !== activeAgent.id
    ) throw new Error("session_project_response_invalid");
    setProjects((items) => [...items, data.project]);
    return data.project;
  }

  function onSessionDeleted(targetSessionId, remainingSessions) {
    if (targetSessionId === sessionId) {
      acceptedSessionRef.current = null;
      acceptedRouteSessionIdRef.current = "";
      clearConversationProjection();
      setDraft("");
      setLoadingHistory(true);
      const next = remainingSessions[0];
      navigate(next
        ? `/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(activeAgent.id)}?sessionId=${encodeURIComponent(next.id)}`
        : `/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(activeAgent.id)}?new=1`);
      if (!next) setLoadingHistory(false);
      clearContextPanel();
    }
  }

  async function cancelActiveAgentRun() {
    if (!sessionId || !activeAgentRunId || cancellingAgentRunId) return;
    const targetSessionId = sessionId;
    const targetAgentRunId = activeAgentRunId;
    setCancellingRunId(targetAgentRunId);
    setError("");
    try {
      const response = await apiResponse(`/api/sessions/${targetSessionId}/agent-runs/${targetAgentRunId}/cancel`, {
        method: "POST",
      });
      const result = await response.json();
      if (
        result.agentRunId !== targetAgentRunId
        || !["requested", "terminal"].includes(result.disposition)
        || !["queued", "running", "completed", "failed", "cancelled"].includes(result.status)
      ) {
        throw new Error("AgentRun cancellation response is invalid");
      }
      if (result.disposition === "terminal" && activeSessionIdRef.current === targetSessionId) {
        await refreshAgentRunResumeState(targetAgentRunId, workspace.id, targetSessionId);
      }
    } catch (requestError) {
      setError(requestError?.message === "agent_run_cancel_unavailable" ? t("appRoute.unableToStopThisRunRightNowPleaseTry") : t("appRoute.unableToStopRun"));
    } finally {
      setCancellingRunId("");
    }
  }

  async function sendMessage(event, replaceUnconfirmedInput = false) {
    event?.preventDefault();
    const text = draft.trim();
    if (!workspace || !text || sending || loadingHistory) return;
    const requestScope = requestScopeRef.current;
    const isCurrentRequest = () => {
      const pathname = window.location.pathname;
      const currentAgentId = matchPath(`${workspaceBase}/agents/:agentId`, pathname)?.params.agentId;
      const locationOwnsRequest = pathname === `${workspaceBase}/app`
        || currentAgentId === activeAgent.id
        || pathname.startsWith(`${workspaceBase}/settings/`);
      return locationOwnsRequest && requestScope.active && requestScopeRef.current === requestScope;
    };
    if (hasActiveAgentRun && !operations.pending()) {
      if (!sessionId || !activeAgentRunId) return;
      if (pendingAttachmentIds.length || pendingUploadFiles.length) {
        setError(t("appRoute.supplementalInputDoesNotSupportAttachmentsYetRemoveThe"));
        return;
      }
      setSending(true);
      setError("");
      const supplementId = crypto.randomUUID();
      try {
        const response = await apiResponse(`/api/sessions/${sessionId}/agent-runs/${activeAgentRunId}/supplements`, {
          method: "POST",
          body: JSON.stringify({ supplementId, message: text }),
        });
        const result = await response.json();
        if (!isCurrentRequest()) return;
        if (
          !result
          || typeof result !== "object"
          || Object.keys(result).sort().join("|") !== "agentRunId|disposition|queuedCount|sessionId|supplementId"
          || result.agentRunId !== activeAgentRunId
          || result.sessionId !== sessionId
          || result.supplementId !== supplementId
          || !["accepted", "duplicate"].includes(result.disposition)
          || !Number.isInteger(result.queuedCount)
          || result.queuedCount < 0
          || result.queuedCount > 8
        ) {
          throw new Error("AgentRun supplement response is invalid");
        }
        setDraft("");
      } catch (requestError) {
        if (isCurrentRequest()) setError(requestError?.message === "agent_run_supplement_unavailable" ? t("appRoute.unableToDeliverSupplementalInputRightNowPleaseTry") : t("appRoute.unableToSendSupplementalInput"));
      } finally {
        if (isCurrentRequest()) setSending(false);
      }
      return;
    }
    if (!modelId) return;
    setSending(true);
    const targetSessionId = sessionId || "new";
    const attachmentRefs = [...pendingAttachmentIds].sort();
    const uploadFiles = pendingUploadFiles;
    setError("");
    if (targetSessionId === "new") composerStartRectRef.current = composerRef.current?.getBoundingClientRect() || null;
    const startedAtMs = Date.now();
    setWorkTime({ startedAtMs, sessionId: targetSessionId });
    const pendingMessage = { text, startedAtMs, sessionId: sessionId || null, agentRunId: null };
    setPendingUserMessage(pendingMessage);
    setDraft("");
    let accepted = false;
    try {
      const messageData = await operations.submit(`/api/workspaces/${workspace.id}/sessions/${targetSessionId}/messages`, {
          text,
          agentId: activeAgent.id,
          ...(targetSessionId === "new" && requestedProjectId ? { projectId: requestedProjectId } : {}),
          modelConfigRef: modelId,
          ...(thinkingMode ? { thinkingMode } : {}),
          ...(uploadFiles.length ? {} : { attachmentRefs }),
      }, uploadFiles, [], replaceUnconfirmedInput);
      accepted = true;
      // Navigation invalidates only this UI result, never the server's accepted work.
      if (!isCurrentRequest()) return;
      const { session: acceptedSession } = await (await apiResponse(`/api/sessions/${encodeURIComponent(messageData.sessionId)}`)).json();
      if (!isCurrentRequest()) return;
      if (
        acceptedSession?.id !== messageData.sessionId
        || acceptedSession.agentId !== activeAgent.id
        || (targetSessionId === "new" && (acceptedSession.projectId || "") !== requestedProjectId)
      ) {
        throw new Error("AgentRun acceptance identity is invalid");
      }
      const resolvedSessionId = messageData.sessionId;
      setPendingUserMessage((value) => value === pendingMessage
        ? { ...value, sessionId: resolvedSessionId, agentRunId: messageData.agentRunId } : value);
      setPendingAttachmentIds([]);
      setPendingUploadFiles([]);
      setSessions((items) =>
        sortSessions(
          sessionId
            ? items.map((session) => (session.id === resolvedSessionId ? acceptedSession : session))
            : [acceptedSession, ...items.filter((session) => session.id !== resolvedSessionId)],
        ),
      );
      if (!sessionId) {
        setWorkTime((value) => value ? { ...value, sessionId: resolvedSessionId } : null);
        acceptedSessionRef.current = { sessionId: resolvedSessionId, agentRunId: messageData.agentRunId };
        acceptedRouteSessionIdRef.current = resolvedSessionId;
        setSessionId(resolvedSessionId);
        setSending(false);
        onSessionAccepted(`/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(activeAgent.id)}?sessionId=${encodeURIComponent(resolvedSessionId)}`);
      } else {
        stopActiveStream();
        const controller = new AbortController();
        streamAbortRef.current = controller;
        const resume = await loadAcceptedConversation(resolvedSessionId, { transport: transcriptTransport, store: transcriptStore, setActive: updateActiveTranscriptRun, isCurrent: isCurrentRequest }, controller.signal);
        operations.complete(messageData.operationId);
        if (!resume || resume.agentRunId !== messageData.agentRunId) setPendingUserMessage(null);
        if (resume) connectAgentRun(resume.agentRunId, workspace.id, resolvedSessionId, controller, resume).catch((streamError) => {
          handleAgentRunStreamFailure(streamError, resume.agentRunId);
        });
      }
    } catch (errorValue) {
      if (!isCurrentRequest()) return;
      if (errorValue?.message === "operation_result_stale") return;
      setPendingUserMessage(null);
      setDraft((value) => value || text);
      if (accepted) {
        setError(t("operations.acceptedReadFailed"));
      } else if (errorValue?.message === "operation_pending_input_changed") {
        setError(t("operations.changedInput"));
        if (operations.pending()?.receipt) await checkPendingOperation(true);
      } else if (errorValue?.message === "operation_input_acceptance_unconfirmed") {
        setError(t("operations.inputUnconfirmed"));
        await checkPendingOperation(true);
      } else if (errorValue?.message === "model_not_found") {
        try {
          const response = await apiResponse("/api/models");
          const data = await response.json();
          if (isCurrentRequest()) {
            acceptModels(data.models);
            setError(t("appRoute.modelListChangedSelectModel"));
          }
        } catch {
          if (isCurrentRequest()) showStreamError(errorValue);
        }
      } else {
        showStreamError(errorValue);
      }
    } finally {
      if (isCurrentRequest()) setSending(false);
    }
  }

  async function checkPendingOperation(preserveChangedDraft = false) {
    const operationId = operations.pending()?.operationId;
    const scope = requestScopeRef.current;
    const isCurrent = () => scope.active && requestScopeRef.current === scope;
    setSending(true);
    try {
      const accepted = await operations.recover();
      if (accepted) {
        const { session } = await (await apiResponse(`/api/sessions/${encodeURIComponent(accepted.sessionId)}`)).json();
        if (!isCurrent()) return;
        if (session?.id !== accepted.sessionId || typeof session.agentId !== "string" || !session.agentId) throw new Error("session_response_invalid");
        if (session.id === activeSessionIdRef.current && session.agentId === activeAgent.id) {
          stopActiveStream();
          const controller = new AbortController();
          streamAbortRef.current = controller;
          const resume = await loadAcceptedConversation(session.id, { transport: transcriptTransport, store: transcriptStore, setActive: updateActiveTranscriptRun, isCurrent }, controller.signal);
          if (!isCurrent()) return;
          operations.complete(accepted.operationId);
          setError(preserveChangedDraft ? t("operations.inputUnconfirmed") : "");
          if (resume) connectAgentRun(resume.agentRunId, workspace.id, session.id, controller, resume).catch((failure) => handleAgentRunStreamFailure(failure, resume.agentRunId));
        } else {
          setOperationReviewLink({ operationId: accepted.operationId, ...acceptedConversationReviewLink(workspace.id, session) });
          setError(t("operations.inputUnconfirmed"));
        }
      }
    } catch (requestError) {
      if (!isCurrent()) return;
      if (requestError?.message === "operation_resource_unavailable" && requestError.status === 410) {
        if (operationId) operations.complete(operationId);
        setError(t("operations.unavailable"));
      } else setError(t(requestError?.message === "operation_not_found" ? "operations.notFound" : "operations.checkFailed"));
    } finally { if (isCurrent()) setSending(false); }
  }

  async function consumeOperationReview(operationId) {
    const scope = requestScopeRef.current;
    const isCurrent = () => scope.active && requestScopeRef.current === scope;
    setSending(true);
    try {
      await consumeReviewedOperation(operations, operationId, async (id) => (await (await apiResponse(`/api/sessions/${encodeURIComponent(id)}`)).json()).session);
      if (isCurrent()) setError("");
    } catch { if (isCurrent()) setError(t("operations.checkFailed")); }
    finally { if (isCurrent()) setSending(false); }
  }

  async function loadOlderHistory() {
    const current = transcriptStore.getListSnapshot();
    if (!sessionId || !current.projectionVersion || !current.projectionGeneration
      || !current.hasOlder || !current.olderCursor || olderHistoryRequestRef.current) return;
    const requestedForSessionId = sessionId;
    const requestKey = `${sessionId}:${current.viewEpoch}:${current.olderCursor}`;
    const controller = new AbortController();
    olderHistoryRequestRef.current = { key: requestKey, controller };
    setLoadingOlderHistory(true);
    try {
      const page = await transcriptTransport.loadOlder({
        sessionId,
        projectionVersion: current.projectionVersion,
        projectionGeneration: current.projectionGeneration,
        sourceHighWater: current.sourceHighWater,
      }, current.olderCursor, controller.signal);
      if (activeSessionIdRef.current !== requestedForSessionId
        || olderHistoryRequestRef.current?.key !== requestKey) return;
      transcriptStore.prependPage(page, current.viewEpoch);
    } catch {
      if (!controller.signal.aborted) setError(t("appRoute.unableToLoadEarlierMessagesPleaseTryAgain"));
    } finally {
      if (olderHistoryRequestRef.current?.key === requestKey) {
        olderHistoryRequestRef.current = null;
        setLoadingOlderHistory(false);
      }
    }
  }

  function releaseLoadedTranscriptHistory() {
    olderHistoryRequestRef.current?.controller.abort();
    olderHistoryRequestRef.current = null;
    setLoadingOlderHistory(false);
    clearTranscriptContentRangeCache();
    transcriptStore.releaseLoadedHistory();
  }

  async function uploadAttachment(event) {
    const input = event.currentTarget;
    const files = Array.from(input.files || []);
    if (!files.length || uploadingAttachment) return;
    if (files.length + pendingUploadFiles.length > MAX_UPLOAD_BATCH_FILES) {
      input.value = "";
      setError(t("appRoute.addUpToValueMaterialsAtATime", { value1: MAX_UPLOAD_BATCH_FILES }));
      return;
    }
    if (!sessionId) {
      setError("");
      setPendingUploadFiles((items) => [...items, ...files]);
      input.value = "";
      return;
    }
    const uploadSessionId = sessionId;
    const body = new FormData();
    files.forEach((file) => body.append("files", file));
    setError("");
    setUploadingAttachment(true);
    try {
      const response = await apiResponse(`/api/sessions/${sessionId}/uploads`, { method: "POST", body });
      const uploaded = await response.json();
      if (
        !Array.isArray(uploaded.libraryObjects)
        || !Array.isArray(uploaded.assets)
        || uploaded.libraryObjects.length !== files.length
        || uploaded.assets.length !== files.length
        || uploaded.assets.some((asset, index) => (
          !asset?.id
          || !uploaded.libraryObjects[index]?.id
          || asset.asset?.id !== uploaded.libraryObjects[index].id
          || asset.assetKind !== "userLibraryObject"
          || asset.displayName !== files[index].name
          || uploaded.libraryObjects[index].displayName !== files[index].name
        ))
        || new Set(uploaded.assets.map((asset) => asset.id)).size !== files.length
      ) {
        throw new Error("attachment_upload_response_invalid");
      }
      if (activeSessionIdRef.current !== uploadSessionId) return;
      setAssets((items) => uploaded.assets.reduce(
        (current, asset) => upsertBy(current, "id", asset),
        items,
      ));
      setPendingAttachmentIds((items) => [
        ...new Set([...items, ...uploaded.assets.map((asset) => asset.id)]),
      ]);
    } catch {
      setError(t("appRoute.attachmentUploadFailed"));
    } finally {
      input.value = "";
      setUploadingAttachment(false);
    }
  }

  function removePendingAttachment(link) {
    setPendingAttachmentIds((items) => items.filter((id) => id !== link.id));
    if (attachmentPreview?.id === link.id) setAttachmentPreview(null);
  }

  function removePendingUploadFile(index) {
    setPendingUploadFiles((items) => items.filter((_file, itemIndex) => itemIndex !== index));
  }

  function returnFromFilePreview() {
    if (contextPanel.mode !== "filePreview") return;
    const { origin } = contextPanel;
    previewRequestIdRef.current += 1;
    setContextPanel(CLOSED_CONTEXT_PANEL);
    window.requestAnimationFrame(() => {
      document.getElementById(origin.elementId)?.focus({ preventScroll: true });
    });
  }

  async function showCitation(citation, origin) {
    const requestId = previewRequestIdRef.current + 1;
    previewRequestIdRef.current = requestId;
    setError("");
    setContextPanel({
      mode: "filePreview",
      citationId: citation.citationId,
      displayName: citation.displayName,
      origin,
      status: "loading",
    });
    try {
      const response = await apiResponse(citation.sourceUrl);
      const detail = (await response.json()).citation;
      const previewResponse = await apiResponse(detail.previewUrl);
      const contentType = (previewResponse.headers.get("Content-Type") || "").split(";", 1)[0].trim().toLowerCase();
      let preview;
      if (contentType === "text/plain" || contentType === "text/markdown" || codePreviewCanRender(detail.displayName, contentType)) {
        preview = { kind: "text", content: await previewResponse.text(), contentType, objectUrl: "" };
      } else if (contentType === "application/pdf") {
        const objectUrl = URL.createObjectURL(await previewResponse.blob());
        const pageNumber = Number.isInteger(detail.locator?.page)
          ? detail.locator.page
          : Number.isInteger(detail.locator?.pageStart)
            ? detail.locator.pageStart
            : null;
        const page = pageNumber ? `#page=${pageNumber}` : "";
        preview = { kind: "pdf", objectUrl, src: `${objectUrl}${page}` };
      } else if (contentType.startsWith("image/")) {
        const objectUrl = URL.createObjectURL(await previewResponse.blob());
        preview = { kind: "image", objectUrl, src: objectUrl };
      } else {
        throw new Error(`unsupported citation preview content type: ${contentType || "missing"}`);
      }
      if (previewRequestIdRef.current !== requestId) {
        if (preview.objectUrl) URL.revokeObjectURL(preview.objectUrl);
        return;
      }
      setContextPanel({
        mode: "filePreview",
        citationId: citation.citationId,
        origin,
        status: "ready",
        ...detail,
        preview,
        objectUrl: preview.objectUrl || "",
      });
    } catch (requestError) {
      if (previewRequestIdRef.current !== requestId) return;
      const reason = requestError instanceof Error ? requestError.message : String(requestError);
      const message = reason === "citation_preview_unsupported"
        ? t("appRoute.inlinePreviewIsUnavailableForThisFileType")
        : reason === "citation_source_stale"
          ? t("appRoute.theReferencedFileVersionHasChangedTheOriginalEvidence")
          : reason === "citation_source_not_available" || reason === "citation_not_found"
            ? t("appRoute.theReferenceFileIsMissingDeletedOrInaccessible")
            : t("appRoute.unableToLoadTheReferenceFile");
      setContextPanel({
        mode: "filePreview",
        citationId: citation.citationId,
        displayName: citation.displayName,
        origin,
        status: "error",
        error: message,
      });
    }
  }

  async function showArtifact(artifact, origin) {
    const requestId = previewRequestIdRef.current + 1;
    previewRequestIdRef.current = requestId;
    const panelIdentity = {
      artifactRef: artifact.artifactRef,
      displayName: artifact.filename,
      downloadUrl: artifact.downloadUrl,
      origin,
      originLabel: "Artifact",
    };
    setError("");
    setContextPanel({
      mode: "filePreview",
      ...panelIdentity,
      status: "loading",
    });
    try {
      if (officeFileType(artifact.filename)) {
        const id = artifact.artifactRef.replace(/^artifact:/, "");
        setContextPanel({ mode: "filePreview", ...panelIdentity, status: "ready",
          preview: { kind: "office", src: officePreviewUrl("artifact", id), objectUrl: "" }, objectUrl: "" });
        return;
      }
      const previewResponse = await apiResponse(artifact.downloadUrl);
      const contentType = (previewResponse.headers.get("Content-Type") || "").split(";", 1)[0].trim().toLowerCase();
      const contentLength = Number(previewResponse.headers.get("Content-Length") || "");
      let preview;
      const previewable = !Number.isNaN(contentLength) && contentLength > 0 && contentLength <= ARTIFACT_PREVIEW_MAX_BYTES;
      const codePreviewable = codePreviewCanRender(artifact.filename, contentType);
      if (previewable && (ARTIFACT_PREVIEW_CONTENT_TYPES.has(contentType) || codePreviewable)) {
        if (contentType === "text/plain" || contentType === "text/markdown" || codePreviewable) {
          preview = { kind: "text", content: await previewResponse.text(), contentType, objectUrl: "" };
        } else if (contentType === "application/pdf") {
          const objectUrl = URL.createObjectURL(await previewResponse.blob());
          preview = { kind: "pdf", objectUrl, src: objectUrl };
        } else {
          const objectUrl = URL.createObjectURL(await previewResponse.blob());
          preview = { kind: "image", objectUrl, src: objectUrl };
        }
      } else {
        preview = { kind: "unsupported", objectUrl: "" };
      }
      if (previewRequestIdRef.current !== requestId) {
        if (preview.objectUrl) URL.revokeObjectURL(preview.objectUrl);
        return;
      }
      setContextPanel({
        mode: "filePreview",
        ...panelIdentity,
        status: "ready",
        preview,
        objectUrl: preview.objectUrl || "",
      });
    } catch {
      if (previewRequestIdRef.current !== requestId) return;
      setContextPanel({
        mode: "filePreview",
        ...panelIdentity,
        status: "error",
        error: t("appRoute.unableToLoadThisFile"),
      });
    }
  }

  const hasContextPanel = contextPanel.mode !== "closed";
  const contextPanelClass = hasContextPanel ? "withContextPanel withFilePreview" : "";
  const isHome = !requestedSessionId && !sessionId && transcriptList.blockIds.length === 0 && !pendingUserMessage;
  useLayoutEffect(() => {
    const startRect = composerStartRectRef.current;
    if (isHome || !startRect) return;
    composerStartRectRef.current = null;
    const composer = composerRef.current;
    if (!composer || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const endRect = composer.getBoundingClientRect();
    composer.animate([
      { transform: `translate(${startRect.left - endRect.left}px, ${startRect.top - endRect.top}px)` },
      { transform: "translate(0, 0)" },
    ], { duration: 320, easing: "cubic-bezier(.2, .8, .2, 1)" });
  }, [isHome]);
  useEffect(() => {
    if (!pendingUserMessage) return;
    if (hasCommittedPendingMessage(pendingUserMessage, sessionId || null, transcriptList.blockIds, transcriptStore.getBlockSnapshot)) {
      setPendingUserMessage(null);
    }
  }, [pendingUserMessage, sessionId, transcriptList.blockIds, transcriptStore]);
  const updateBrowserPanelWidth = useCallback((widthPx) => {
    const sidebarWidth = isSidebarOpen && window.innerWidth > 760 ? 270 : 0;
    const mainWidth = window.innerWidth - sidebarWidth;
    const wide = window.innerWidth >= (isSidebarOpen ? 1170 : 900);
    const maxWidthPx = Math.max(270, Math.floor(wide ? Math.min(window.innerWidth * 0.75, mainWidth - 480) : mainWidth - 32));
    setBrowserPanelWidthPx(Math.min(maxWidthPx, Math.max(270, Math.round(widthPx))));
  }, [isSidebarOpen]);
  const groupedSessions = useMemo(() => groupSessionNavigation(sessions, projects), [projects, sessions]);
  function handleQuickAction(prompt) {
    setDraft(prompt);
    window.requestAnimationFrame(() => {
      document.getElementById("messageDraft")?.focus();
    });
  }

  function renderSessionRow(session, options = {}) {
    return <SessionNavigationRow key={session.id} workspaceId={workspace.id} session={session} actions={sessionActions} active={session.id === sessionId} running={session.id === sessionId ? hasActiveAgentRun : session.hasActiveAgentRun} onSelect={selectSession} {...options} />;
  }

  return (
    <main
      className={`workspaceWorkbench ${isSidebarOpen ? "withSidebar" : "withoutSidebar"} ${contextPanelClass}`}
      style={{ "--browser-panel-width": `${browserPanelWidthPx}px` }}
    >
      <div className="workspaceSidebarSlot">
        {isSidebarOpen ? <ShellSidebar
          workspace={workspace}
          agents={agents}
          activeAgent={activeAgent}
          onStartNewChat={startNewChat}
          onCollapse={() => setIsSidebarOpen(false)}
          initialTab={location.state?.sidebarTab || (workspaceDraft ? "home" : "chat")}
          sessionProps={{
            agentId: activeAgent.id,
            sessions,
            projects,
            groupedSessions,
            renderSessionRow,
            onCreateProject: createProject,
          }}
        /> : null}
      </div>

      <header className={`workspaceTopbar ${isHome ? "isHome" : ""}`}>
        <div className="workspaceTopbarStart">
          {!isSidebarOpen ? <button
            className="workspacePanelToggle"
            type="button"
            onClick={() => setIsSidebarOpen(true)}
            aria-label={t("appRoute.showSidebar")}
            aria-expanded="false"
            title={t("appRoute.showSidebar")}
          >
            <PanelLeft aria-hidden="true" />
          </button> : null}
          {isSidebarOpen && activeSession ? <SessionAgentBreadcrumb workspaceId={workspace.id} agent={activeAgent} session={activeSession} sessionId={sessionId} label={t("appRoute.currentConversation")} /> : null}
        </div>
        {streamIssue?.sessionId === sessionId && !streamIssue.reconnecting ? <button
          className="workspacePanelToggle"
          type="button"
          onClick={() => void retryTranscriptStream()}
          aria-label={t("appRoute.retryStream")}
          title={t("appRoute.streamPaused")}
        ><RefreshCw aria-hidden="true" /></button> : null}
      </header>

      <section className="workspaceChatColumn">
        {error ? <div className="errorBanner" role="alert">{error}
          {operations.pending() ? <button type="button" disabled={sending} onClick={() => void checkPendingOperation()}>{t("agentChat.retry")}</button> : null}
          {operationReviewLink && operationReviewLink.operationId === operations.pending()?.operationId ? <a
            href={operationReviewLink.href} target={operationReviewLink.target} rel={operationReviewLink.rel}
            onClick={() => void consumeOperationReview(operationReviewLink.operationId)}
          >{t("operations.reviewSeparate")}</a> : null}
        </div> : null}

        <div className={`workspaceConversationPlane ${isHome ? "isEmpty" : ""}`}>
          <TranscriptBlockList
            store={transcriptStore}
            toolOperations={toolOperations}
            sessionId={sessionId || null}
            initialInputOrigin={initialInputOrigin?.scope === requestScopeKey && initialInputOrigin.sessionId === sessionId ? initialInputOrigin.value : null}
            loadingHistory={loadingHistory}
            loadingOlderHistory={loadingOlderHistory}
            onLoadOlderHistory={loadOlderHistory}
            emptyState={isHome ? <HomePlane agent={activeAgent} /> : null}
            onShowCitation={showCitation}
            onShowArtifact={(artifact) => showArtifact(artifact, "conversation")}
            pendingUserMessage={pendingUserMessage}
            historyMemoryPressure={transcriptManagedBytes >= TRANSCRIPT_MEMORY_WARNING_BYTES}
            onReleaseLoadedHistory={releaseLoadedTranscriptHistory}
            running={hasActiveAgentRun || sending}
            startedAtMs={workTime?.sessionId === sessionId ? workTime.startedAtMs : undefined}
            completedAtMs={workTime?.sessionId === sessionId ? workTime.completedAtMs : undefined}
          />

          <WorkspaceComposer
            formRef={composerRef}
            fileInputRef={fileInputRef}
            isHome={isHome}
            onSubmit={(event) => void sendMessage(event, true)}
            pendingAttachments={pendingAttachments}
            pendingUploadFiles={pendingUploadFiles}
            onPreviewAttachment={setAttachmentPreview}
            onRemoveAttachment={removePendingAttachment}
            onRemoveUpload={removePendingUploadFile}
            draft={draft}
            onDraftChange={setDraft}
            enterStartsNewLine={enterStartsNewLine}
            activeAgentName={activeAgent.name}
            workspaceAvailable={Boolean(workspace)}
            sending={sending}
            loadingHistory={loadingHistory}
            hasActiveAgentRun={hasActiveAgentRun}
            uploadingAttachment={uploadingAttachment}
            onUploadAttachment={uploadAttachment}
            sessionId={sessionId}
            currentModel={currentModel}
            thinkingMode={thinkingMode}
            onThinkingModeChange={selectThinkingMode}
            modelGroups={modelGroups}
            modelId={modelId}
            onModelIdChange={selectModel}
            activeAgentRunId={activeAgentRunId}
            cancellingAgentRunId={cancellingAgentRunId}
            onCancelActiveAgentRun={cancelActiveAgentRun}
          />
          {isHome ? <HomeQuickActions onQuickAction={handleQuickAction} /> : null}
        </div>
      </section>

      <ConnectedWorkspaceContextPanel
        panel={contextPanel}
        browserWidthPx={browserPanelWidthPx}
        onBrowserWidthChange={updateBrowserPanelWidth}
        onClose={clearContextPanel}
        onReturn={returnFromFilePreview}
      />
      <ConfirmDialog
        open={Boolean(deleteConfirmationSessionId)}
        title={t("appRoute.deleteThisConversation")}
        busy={Boolean(deletingSessionId)}
        onCancel={() => setDeleteConfirmationSessionId("")}
        onConfirm={() => void deleteSession(deleteConfirmationSessionId)}
      />
      {attachmentPreview ? (
        <div className="attachmentPreviewBackdrop" role="presentation" onMouseDown={() => setAttachmentPreview(null)}>
          <section className="attachmentPreviewDialog" ref={attachmentDialogRef} role="dialog" aria-modal="true" aria-label={t("attachmentCard.previewValue", { value1: attachmentPreview.displayName })} tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
            <header><strong>{attachmentPreview.displayName}</strong><a href={attachmentDownloadUrl(attachmentPreview)}>{t("libraryObjectRoute.downloadFile")}</a><button type="button" onClick={() => setAttachmentPreview(null)} aria-label={t("attachmentCard.closePreview")}><X aria-hidden="true" /></button></header>
            {attachmentCanPreview(attachmentPreview) ? attachmentIsImage(attachmentPreview) ? <img src={attachmentPreviewUrl(attachmentPreview)} alt={attachmentPreview.displayName} /> : <DocumentPreview src={attachmentPreviewUrl(attachmentPreview)} title={attachmentPreview.displayName} contentType={attachmentPreview.contentType || attachmentPreview.asset?.contentType || ""} /> : <div className="attachmentPreviewUnavailable"><ImageIcon aria-hidden="true" /><span>{t("appRoute.onlinePreviewIsUnavailableForThisMaterial")}</span><a href={attachmentDownloadUrl(attachmentPreview)}>{t("appRoute.downloadMaterial")}</a></div>}
          </section>
        </div>
      ) : null}
    </main>
  );
}
