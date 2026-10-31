import { useTranslation } from "../i18n";
import { useCallback, useEffect, useEffectEvent, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { matchPath, useNavigate, useRouteLoaderData } from "react-router";
import { apiJson, apiResponse } from "../api";
import { OperationClient, acceptedConversationReviewLink, consumeReviewedOperation } from "../chat/operationReceipts";
import { WorkspaceContextPanel } from "../components/WorkspaceContextPanel";
import { DocumentPreview } from "../components/DocumentPreview";
import { officeFileType } from "../chat/officeFormats.mjs";
import { officePreviewUrl } from "../chat/attachments.mjs";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { useModalDialog } from "../components/useModalDialog";
import { streamWorkspacePresentation } from "../chat/workspacePresentationTransport";
import { createTranscriptViewStore } from "../chat/transcriptViewStore";
import { compareTranscriptBlocks } from "../chat/transcriptContract";
import { hasCommittedPendingMessage } from "../chat/transcriptTurns";
import { TranscriptToolOperationRegistry } from "../chat/transcriptToolOperations";
import { createWorkspaceTranscriptTransport } from "../chat/transcriptTransport";
import { WorkspacePresentationController } from "../chat/workspacePresentationController";
import { TranscriptBlockList } from "../chat/TranscriptBlockList";
import { createAgentModelSettingsClient } from "../agent-chat/agentModelSettings";
import { parseSessionInputOrigin } from "../agent-chat/agentWorkSessions";
import {
  clearTranscriptContentRangeCache,
  subscribeTranscriptContentRangeCache,
  transcriptContentRangeCacheBytes,
} from "../chat/transcriptContentRanges";
import { WorkspaceComposer } from "../chat/WorkspaceComposer";
import { SubmissionControl } from "../chat/submissionControl";
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
import { AgentDocumentIdentity } from "../shell/useAgentDocumentIdentity";
import { sessionChatPath } from "../agent-chat/agentChatNavigation";
import { SessionAgentBreadcrumb } from "../agent-chat/SessionAgentBreadcrumb";
import { ShellSidebar } from "../shell/ShellSidebar";
import { groupSessionNavigation, sortSessionNavigation as sortSessions } from "../shell/sessionNavigation.mjs";
import { SessionNavigationRow } from "../shell/SessionNavigationRow";
import { useSessionNavigationActions } from "../shell/useSessionNavigationActions";
import { SessionReadController, mergeSessionAttention, mergeSessionNavigation } from "../shell/sessionReadState";
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

export function AppPageContent({ agentId, initialSession = null, workspaceDraft, location, modelsVersion, onSessionAccepted }) {
  const { t } = useTranslation();
  const { user } = useRouteLoaderData("authenticated");
  const { workspace, agents, defaultAgentId } = useRouteLoaderData("workspace");
  const operations = useMemo(() => new OperationClient({ userId: user.id, workspaceId: workspace.id, command: "submitMessage" }), [user.id, workspace.id]);
  const cancelSubmission = useCallback(async (target) => {
    const response = await apiResponse(`/api/sessions/${encodeURIComponent(target.sessionId)}/agent-runs/${encodeURIComponent(target.agentRunId)}/cancel`, { method: "POST" });
    return response.json();
  }, []);
  const submissionControl = useMemo(() => new SubmissionControl({ operations,
    scope: { userId: user.id, workspaceId: workspace.id }, cancel: cancelSubmission,
  }), [operations, user.id, workspace.id, cancelSubmission]);
  const submissionSnapshot = useSyncExternalStore(submissionControl.subscribe, submissionControl.getSnapshot, submissionControl.getSnapshot);
  const submissionOriginRef = useRef(null);
  const messageRequestRef = useRef(null);
  const pendingBindingRef = useRef(null);
  const auxiliaryRequestRef = useRef(null);
  const knownRunControlsRef = useRef(new Map());
  const terminalFactsRef = useRef(new Map());
  const [operationReviewLink, setOperationReviewLink] = useState(null);
  const navigate = useNavigate();
  const searchParams = new URLSearchParams(location.search);
  const requestedSessionId = initialSession?.id || searchParams.get("sessionId") || "";
  const requestedProjectId = searchParams.get("projectId") || "";
  const requestedPrompt = searchParams.get("prompt") || "";
  const startFresh = searchParams.get("new") === "1";
  const conversationAgentId = initialSession?.agentId || agentId;
  const activeAgent = agents.find((agent) => agent.id === conversationAgentId && agent.status === "active") || null;
  if (!activeAgent && !initialSession) throw new Error("agent_not_found");
  const modelSelectionLocked = Boolean(activeAgent?.isBusinessRoot);
  const modelAgentId = activeAgent?.id || defaultAgentId;
  const modelSettingsClient = useMemo(() => modelAgentId ? createAgentModelSettingsClient(modelAgentId, apiJson) : null, [modelAgentId]);
  const workspaceBase = `/w/${encodeURIComponent(workspace.id)}`;
  const requestScopeKey = JSON.stringify([workspace.id, conversationAgentId, workspaceDraft, requestedSessionId, startFresh, requestedProjectId]);
  const requestScopeRef = useRef({ key: requestScopeKey, active: true });
  if (requestScopeRef.current.key !== requestScopeKey) requestScopeRef.current = { key: requestScopeKey, active: true };
  const navigationSnapshotRef = useRef({ scope: requestScopeKey, sessions: null });
  if (navigationSnapshotRef.current.scope !== requestScopeKey) navigationSnapshotRef.current = { scope: requestScopeKey, sessions: null };
  const streamAbortRef = useRef(null);
  const chatControllerRef = useRef(null);
  // A submit receipt can identify cancellable work before presentation catches up.
  const [acceptedRunTarget, setAcceptedRunTarget] = useState(null);
  const acceptedRunTargetRef = useRef(null);
  const terminalRunIdsRef = useRef(new Set());
  const runControlRevisionRef = useRef(0);
  const currentRunIdentityRef = useRef(undefined);
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
  const [sessionId, setSessionId] = useState(() => workspaceDraft || startFresh ? "" : requestedSessionId);
  const [presentationAttempt, setPresentationAttempt] = useState(0);
  const [runControlAttempt, setRunControlAttempt] = useState(0);
  const [contextVersion, setContextVersion] = useState(null);
  const [readableThrough, setReadableThrough] = useState("0");
  const [readingLatest, setReadingLatest] = useState(true);
  const [pageVisible, setPageVisible] = useState(() => typeof document === "undefined" || !document.hidden);
  const enteredReadRef = useRef({ sessionId: "", handled: false });
  if (enteredReadRef.current.sessionId !== sessionId) enteredReadRef.current = { sessionId, handled: false };
  const presentationScopeKey = JSON.stringify([workspace.id, conversationAgentId, sessionId]);
  const presentationScopeRef = useRef(presentationScopeKey);
  presentationScopeRef.current = presentationScopeKey;
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
  const sessionActions = useSessionNavigationActions({ scopeKey: JSON.stringify([workspace.id, conversationAgentId]), sessions, onChangeSessions: setSessions, onError: setError, onDeleted: onSessionDeleted });
  const { deletingSessionId, deleteConfirmationSessionId, setDeleteConfirmationSessionId, setEditingSessionId, setOpenSessionMenuId, markSessionRead, deleteSession } = sessionActions;
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
  const transcriptPushedContentBytes = useSyncExternalStore(transcriptStore.subscribePresentationContent, transcriptStore.presentationContentBytes, transcriptStore.presentationContentBytes);
  const transcriptManagedBytes = transcriptBlockBytes + transcriptReferencedContentBytes + transcriptPushedContentBytes;
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
    if (!modelSelectionLocked) setThinkingMode(readModelThinkingMode(user.id, workspace.id, currentModel));
  }, [currentModel, user.id, workspace.id, modelSelectionLocked]);
  const selectThinkingMode = useCallback((mode) => {
    if (modelSelectionLocked || !currentModel) return;
    writeModelThinkingMode(user.id, workspace.id, currentModel, mode);
    setThinkingMode(mode);
  }, [currentModel, user.id, workspace.id, modelSelectionLocked]);
  const selectModel = useCallback((id) => {
    if (modelSelectionLocked) return;
    const selected = models.find((model) => model.id === id);
    if (selected) writePreferredModelIdentity(user.id, workspace.id, selected);
    setModelId(id);
  }, [models, user.id, workspace.id, modelSelectionLocked]);
  const acceptedTarget = acceptedRunTarget && (!sessionId || acceptedRunTarget.sessionId === sessionId)
    ? acceptedRunTarget : null;
  const activeAgentRunId = acceptedTarget?.agentRunId || activeTranscriptRun?.agentRunId || "";
  const hasActiveAgentRun = Boolean(activeAgentRunId);
  const acceptRunTarget = useCallback((receipt) => {
    runControlRevisionRef.current += 1;
    if (terminalRunIdsRef.current.has(receipt.agentRunId)) return;
    currentRunIdentityRef.current = receipt.agentRunId;
    const target = { sessionId: receipt.sessionId, agentRunId: receipt.agentRunId };
    acceptedRunTargetRef.current = target;
    setAcceptedRunTarget(target);
  }, []);
  const clearRunControl = useCallback(() => {
    runControlRevisionRef.current += 1;
    acceptedRunTargetRef.current = null;
    setAcceptedRunTarget(null);
    terminalRunIdsRef.current.clear();
    currentRunIdentityRef.current = undefined;
    messageRequestRef.current = null;
    auxiliaryRequestRef.current = null;
  }, []);
  const knownRunControl = useMemo(() => {
    if (!activeAgentRunId || !sessionId || (submissionSnapshot.target?.sessionId === sessionId
      && submissionSnapshot.target.agentRunId === activeAgentRunId)) return submissionControl;
    const key = JSON.stringify([user.id, workspace.id, sessionId, activeAgentRunId]);
    if (!knownRunControlsRef.current.has(key)) knownRunControlsRef.current.set(key, new SubmissionControl({
      operations: { pending: () => null, recover: async () => null },
      scope: { userId: user.id, workspaceId: workspace.id, controlId: JSON.stringify([sessionId, activeAgentRunId]) }, cancel: cancelSubmission,
    }));
    return knownRunControlsRef.current.get(key);
  }, [activeAgentRunId, sessionId, submissionControl, submissionSnapshot.target?.sessionId, submissionSnapshot.target?.agentRunId, user.id, workspace.id, cancelSubmission]);
  const knownRunSnapshot = useSyncExternalStore(knownRunControl.subscribe, knownRunControl.getSnapshot, knownRunControl.getSnapshot);
  const submissionIsVisible = submissionSnapshot.target
    ? submissionSnapshot.target.sessionId === sessionId || (!sessionId && submissionOriginRef.current?.scope === requestScopeRef.current)
    : submissionOriginRef.current?.scope === requestScopeRef.current;
  const useSubmissionControl = submissionIsVisible && (!activeAgentRunId || submissionSnapshot.target?.agentRunId === activeAgentRunId);
  const currentControl = useSubmissionControl ? submissionControl : knownRunControl;
  const controlSnapshot = useSubmissionControl ? submissionSnapshot : knownRunSnapshot;
  const controlApplies = useSubmissionControl || Boolean(activeAgentRunId && sessionId);
  const controlPending = controlApplies && !["idle", "terminal", "rejected"].includes(controlSnapshot.phase);
  const cancellingAgentRunId = controlApplies && controlSnapshot.phase === "cancelling"
    ? controlSnapshot.target?.agentRunId || "pending" : "";
  const recordRunTerminal = useCallback((target) => {
    const key = JSON.stringify([target.sessionId, target.agentRunId]);
    terminalFactsRef.current.set(key, target);
    const pendingOperation = operations.pending();
    if (pendingOperation?.receipt?.sessionId === target.sessionId && pendingOperation.receipt.agentRunId === target.agentRunId) {
      operations.complete(pendingOperation.operationId);
    }
    submissionControl.observeTerminal(target);
    const known = knownRunControlsRef.current.get(JSON.stringify([user.id, workspace.id, target.sessionId, target.agentRunId]));
    known?.observeTerminal(target);
    const binding = pendingBindingRef.current;
    if (binding?.sessionId === target.sessionId && binding.agentRunId === target.agentRunId
      && activeSessionIdRef.current === target.sessionId && messageRequestRef.current === binding.request) {
      messageRequestRef.current = null;
      setSending(false);
    }
    setPendingUserMessage(value => {
      if (!value || value.agentRunId !== target.agentRunId
        || (value.sessionId !== target.sessionId && !(value.sessionId === null && binding?.sessionId === target.sessionId && binding.agentRunId === target.agentRunId))) return value;
      const completedAtMs = target.completedAtMs ?? value.completedAtMs ?? Date.now();
      if (value.status === target.status && value.completedAtMs === completedAtMs) return value;
      return { ...value, status: target.status, completedAtMs };
    });
  }, [operations, submissionControl, user.id, workspace.id]);
  useEffect(() => {
    if (activeAgentRunId && sessionId) knownRunControl.setTarget({ sessionId, agentRunId: activeAgentRunId });
  }, [knownRunControl, activeAgentRunId, sessionId]);
  useEffect(() => {
    if (submissionControl.getSnapshot().stopRequested && submissionControl.getSnapshot().phase === "unknown") void submissionControl.recover();
  }, [submissionControl]);
  useEffect(() => {
    if (!controlApplies || controlSnapshot.phase !== "terminal" || !controlSnapshot.target || !controlSnapshot.terminal) return;
    const target = { ...controlSnapshot.target, ...controlSnapshot.terminal };
    recordRunTerminal(target);
    if (activeSessionIdRef.current !== target.sessionId) return;
    if (acceptedRunTargetRef.current?.agentRunId === target.agentRunId) {
      acceptedRunTargetRef.current = null; setAcceptedRunTarget(null);
    }
    if (activeTranscriptRunRef.current?.agentRunId === target.agentRunId) updateActiveTranscriptRun(null);
    runControlRevisionRef.current += 1;
    terminalRunIdsRef.current.add(target.agentRunId);
    if (currentRunIdentityRef.current === target.agentRunId) currentRunIdentityRef.current = null;
    setPresentationAttempt(value => value + 1);
  }, [controlApplies, controlSnapshot.phase, controlSnapshot.target, controlSnapshot.terminal, recordRunTerminal, updateActiveTranscriptRun]);
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
    if (modelSelectionLocked) return;
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
  }, [user.id, workspace.id, modelSelectionLocked]);

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

  // biome-ignore lint/correctness/useExhaustiveDependencies: The version is an explicit refresh signal from retained settings routes.
  useEffect(() => {
    const controller = new AbortController();
    async function load() {
      try {
        const [data, settings] = await Promise.all([
          apiResponse(`/api/models?workspaceId=${encodeURIComponent(workspace.id)}`, { signal: controller.signal }).then(response => response.json()),
          modelSelectionLocked && modelSettingsClient ? modelSettingsClient.load(controller.signal) : null,
        ]);
        if (!controller.signal.aborted) {
          acceptModels(data.models);
          if (settings) { setModelId(settings.status === "configured" ? settings.modelConfigRef : ""); setThinkingMode(settings.thinkingMode || ""); }
        }
      } catch {
        if (!controller.signal.aborted) reportLoadError("appRoute.unableToRefreshModelsPleaseTryAgain");
      }
    }
    load();
    return () => controller.abort();
  }, [modelsVersion, acceptModels, workspace.id, modelSelectionLocked, modelSettingsClient, activeAgent?.definitionVersionId]);

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
    setError("");
    const navigationScopeKey = requestScopeKey;
    setLoadingHistory(!workspaceDraft);
    clearRunControl();
    acceptedRouteSessionIdRef.current = "";
    setSessions([]);
    setSessionId(workspaceDraft || startFresh ? "" : requestedSessionId);
    clearConversationProjection();
    setDraft("");
    setSending(false);
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
    Promise.all([
      apiResponse(`/api/workspaces/${workspace.id}/sessions`).then((response) => response.json()),
      apiResponse(`/api/workspaces/${workspace.id}/session-projects`).then((response) => response.json()),
    ])
      .then(([data, projectData]) => {
        if (!active || !requestScopeRef.current.active || requestScopeRef.current.key !== navigationScopeKey) return;
        if (!Array.isArray(data.sessions) || !Array.isArray(projectData.projects)) throw new Error("session_navigation_response_invalid");
        const navigation = navigationSnapshotRef.current.sessions ?? data.sessions;
        if (navigationSnapshotRef.current.sessions === null) setSessions(items => sortSessions(mergeSessionNavigation(items, navigation)));
        setProjects(projectData.projects);
        const selectedSession = workspaceDraft || startFresh
          ? null
          : navigation.find((session) => session.id === requestedSessionId)
            || initialSession
            || navigation[0]
            || null;
        const selectedSessionId = selectedSession?.id || "";
        setSessionId(selectedSessionId);
        if (!selectedSessionId) setLoadingHistory(false);
      })
      .catch(() => {
        if (!active || !requestScopeRef.current.active || requestScopeRef.current.key !== navigationScopeKey) return;
        reportLoadError("appRoute.unableToLoadConversationsRefreshThePageAndTry");
        setLoadingHistory(false);
      });
    return () => { active = false; };
  }, [workspace.id, workspaceDraft, requestedSessionId, requestScopeKey, startFresh, initialSession, clearConversationProjection, clearContextPanel, clearRunControl, setEditingSessionId, setOpenSessionMenuId]);

  useEffect(() => {
    if (requestedPrompt) setDraft(requestedPrompt);
  }, [requestedPrompt]);

  useEffect(() => {
    if (!startFresh || loadingHistory || !workspace || !modelId) return;
    window.requestAnimationFrame(() => document.getElementById("messageDraft")?.focus());
  }, [loadingHistory, modelId, startFresh, workspace]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Pending acknowledgements and their cache retire when the reader identity changes.
  const readController = useMemo(() => new SessionReadController({
    send: async (session, throughSequence, signal) => {
      const data = await apiJson(`/api/sessions/${encodeURIComponent(session.id)}/read`, {
        method: "POST", body: JSON.stringify({ throughSequence }), signal,
      });
      return data.session;
    },
    onAcknowledged: record => setSessions(items => items.map(item => {
      if (item.id !== record.id) return item;
      const attention = Object.fromEntries(["latestAttentionSequence", "readThroughSequence", "attentionRevision", "isManuallyUnread", "isUnread"].map(key => [key, record[key]]));
      return mergeSessionAttention(item, { ...item, ...attention });
    })),
  }), [user.id, workspace.id, conversationAgentId]);
  useEffect(() => () => readController.dispose(), [readController]);
  useEffect(() => {
    if (typeof document === "undefined") return undefined;
    const visibilityTarget = document;
    const focusTarget = window;
    const update = () => setPageVisible(!visibilityTarget.hidden);
    visibilityTarget.addEventListener("visibilitychange", update);
    focusTarget.addEventListener("focus", update);
    return () => { visibilityTarget.removeEventListener("visibilitychange", update); focusTarget.removeEventListener("focus", update); };
  }, []);
  useEffect(() => {
    const visible = pageVisible && !loadingHistory;
    if (activeSession && visible && readingLatest && BigInt(readableThrough) >= BigInt(activeSession.latestAttentionSequence)) {
      const entered = enteredReadRef.current;
      if (entered.sessionId === sessionId && !entered.handled) {
        entered.handled = true;
        if (activeSession.isManuallyUnread) markSelectedSessionRead(activeSession);
      }
    }
    readController.observe({ session: activeSession ?? null, visible, readingLatest, appliedSourceHighWater: readableThrough });
  }, [activeSession, loadingHistory, pageVisible, readingLatest, readableThrough, readController, sessionId]);

  const showStreamError = useCallback((errorValue) => {
    if (errorValue?.name === "AbortError") return;
    setError(t("appRoute.theActionCouldNotBeCompletedPleaseTryAgain"));
  }, [t]);

  const handlePresentationFailure = useCallback((errorValue, targetSessionId) => {
    if (errorValue?.name === "AbortError" || (targetSessionId || "") !== activeSessionIdRef.current) return;
    if ([401, 403, 404, 410].includes(errorValue?.status)) {
      requestScopeRef.current.active = false;
      stopActiveStream();
      readController.dispose();
      navigationSnapshotRef.current.sessions = [];
      setSessions([]); setProjects([]); setReadableThrough("0");
      clearConversationProjection(); clearRunControl(); setStreamIssue(null); setWorkTime(null); setLoadingHistory(false);
      setError(t("appRoute.unableToLoadConversationHistoryRefreshThePageAnd"));
      return;
    }
    setLoadingHistory(false);
    setStreamIssue({ sessionId: targetSessionId || "", error: errorValue, reconnecting: false });
  }, [clearConversationProjection, clearRunControl, stopActiveStream, readController, t]);

  const restorePresentationSnapshot = useCallback(async (tail, signal, isCurrent) => {
    const previous = transcriptStore.getListSnapshot();
    const boundary = transcriptStore.getBlockSnapshot(previous.blockIds[0]);
    const stillCurrent = () => {
      if (signal.aborted || !isCurrent() || transcriptStore.getListSnapshot().viewEpoch !== previous.viewEpoch) {
        throw new DOMException("Aborted", "AbortError");
      }
    };
    olderHistoryRequestRef.current?.controller.abort();
    olderHistoryRequestRef.current = null;
    setLoadingOlderHistory(false);
    const identity = { sessionId: tail.sessionId, projectionVersion: tail.projectionVersion, projectionGeneration: tail.projectionGeneration, sourceHighWater: tail.sourceHighWater };
    const olderPages = [];
    const seen = new Set();
    let oldest = tail.blocks[0];
    let cursor = tail.olderCursor;
    while (boundary && cursor !== null && (!oldest || compareTranscriptBlocks(oldest, boundary) > 0)) {
      stillCurrent();
      if (seen.has(cursor)) throw new Error("transcript recovery cursor made no progress");
      seen.add(cursor);
      const page = await transcriptTransport.loadOlder(identity, cursor, signal);
      stillCurrent();
      if ((!page.blocks.length && page.hasOlder) || page.olderCursor === cursor) throw new Error("transcript recovery page made no progress");
      const last = page.blocks.at(-1);
      if (last && oldest && compareTranscriptBlocks(last, oldest) >= 0) throw new Error("transcript recovery pages overlap");
      olderPages.push(page); oldest = page.blocks[0] ?? oldest; cursor = page.olderCursor;
    }
    stillCurrent();
    return transcriptStore.openTail(tail, olderPages);
  }, [transcriptStore, transcriptTransport]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: presentationAttempt intentionally restarts the same subscription when Retry is chosen.
  useEffect(() => {
    if (!workspaceDraft && !startFresh && requestedSessionId && sessionId !== requestedSessionId) return undefined;
    stopActiveStream();
    const abortController = new AbortController();
    streamAbortRef.current = abortController;
    let active = true;
    const scope = presentationScopeKey;
    const isCurrent = () => active && requestScopeRef.current.active && !abortController.signal.aborted && presentationScopeRef.current === scope;
    const selectedSessionId = sessionId || null;
    const current = transcriptStore.getListSnapshot();
    if (current.sessionId !== selectedSessionId) {
      clearConversationProjection();
      setReadableThrough("0");
      setReadingLatest(true);
    }
    setLoadingHistory(Boolean(selectedSessionId && current.sessionId !== selectedSessionId));
    if (selectedSessionId) {
      Promise.all([
        apiResponse(`/api/sessions/${encodeURIComponent(selectedSessionId)}/assets`, { signal: abortController.signal }).then(response => response.json()),
        apiResponse(`/api/sessions/${encodeURIComponent(selectedSessionId)}`, { signal: abortController.signal }).then(response => response.json()),
      ]).then(([assetData, sessionData]) => {
        if (!isCurrent()) return;
        const origin = parseSessionInputOrigin(sessionData, workspace.id, conversationAgentId, selectedSessionId);
        setInitialInputOrigin({ scope: requestScopeRef.current.key, sessionId: selectedSessionId, value: origin });
        setAssets(assetData.assets);
      }).catch(errorValue => { if (isCurrent() && errorValue?.name !== "AbortError") showStreamError(errorValue); });
    }
    const controller = new WorkspacePresentationController({
      workspaceId: workspace.id, agentId: conversationAgentId, sessionId: selectedSessionId,
      store: transcriptStore, signal: abortController.signal, isCurrent,
      applySnapshot: (page, signal) => restorePresentationSnapshot(page, signal, isCurrent),
      onSnapshot: () => {
        if (!isCurrent()) return;
        setLoadingHistory(false);
        const pending = operations.pending();
        if (pending?.receipt?.sessionId === selectedSessionId) operations.complete(pending.operationId);
      },
      onNavigation: next => {
        if (!isCurrent()) return;
        navigationSnapshotRef.current.sessions = next;
        setSessions(items => sortSessions(mergeSessionNavigation(items, next)));
        if (selectedSessionId === null && !workspaceDraft && !startFresh) {
          setSessionId(next.find(session => session.id === requestedSessionId)?.id || next[0]?.id || "");
        }
      },
      onViewRevision: revision => { if (isCurrent()) setContextVersion({ sessionId: selectedSessionId, revision: revision.contextRevision }); },
      onOperation: detail => { if (isCurrent()) toolOperations.applyPresentationDetail(detail); },
      onCheckpoint: checkpoint => {
        if (!isCurrent() || selectedSessionId === null) return;
        runControlRevisionRef.current += 1;
        const run = checkpoint.agentRun;
        const active = run && ["queued", "running"].includes(run.status);
        if (run && !active) recordRunTerminal({ sessionId: selectedSessionId, agentRunId: run.agentRunId, status: run.status,
          ...(run.completedAt ? { completedAtMs: Date.parse(run.completedAt) } : {}) });
        const currentId = currentRunIdentityRef.current;
        if (active && terminalRunIdsRef.current.has(run.agentRunId)) return;
        if (active && currentId !== undefined && currentId !== run.agentRunId
          || !run && currentId) {
          // HTTP reads and presentation have no shared response order. Keep the
          // confirmed target until a fresh current-state read resolves a conflict.
          setRunControlAttempt(value => value + 1);
          return;
        }
        if (run && !active) {
          terminalRunIdsRef.current.add(run.agentRunId);
          if (currentId !== undefined && currentId !== run.agentRunId) return;
        }
        currentRunIdentityRef.current = active ? run.agentRunId : null;
        if (run && acceptedRunTargetRef.current?.agentRunId === run.agentRunId) {
          acceptedRunTargetRef.current = null;
          setAcceptedRunTarget(null);
        }
        updateActiveTranscriptRun(active ? run : null);
        if (active && run.startedAt) setWorkTime(value => ({ ...value, sessionId: selectedSessionId, startedAtMs: Date.parse(run.startedAt), completedAtMs: null }));
        if (!active && run?.completedAt) setWorkTime(value => ({ ...value, sessionId: selectedSessionId, completedAtMs: Date.parse(run.completedAt) }));
      },
      onApplied: checkpoint => {
        if (!isCurrent()) return;
        setReadableThrough(checkpoint.sourceHighWater);
        setLoadingHistory(false);
        setStreamIssue(null);
      },
    });
    chatControllerRef.current = controller;
    void streamWorkspacePresentation({
      workspaceId: workspace.id, agentId: conversationAgentId, sessionId: selectedSessionId,
      controller, signal: abortController.signal,
      onConnection: connection => {
        if (isCurrent()) setStreamIssue(connection === "reconnecting"
          ? { sessionId: selectedSessionId || "", reconnecting: true } : null);
      },
    }).catch(errorValue => { if (isCurrent()) handlePresentationFailure(errorValue, selectedSessionId); });
    return () => { active = false; abortController.abort(); controller.dispose(); };
  }, [presentationScopeKey, presentationAttempt, sessionId, workspace.id, conversationAgentId, operations, transcriptStore, toolOperations, clearConversationProjection, stopActiveStream, updateActiveTranscriptRun, restorePresentationSnapshot, handlePresentationFailure, showStreamError, recordRunTerminal, workspaceDraft, startFresh, requestedSessionId]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: Route ownership, Retry, and conflicting Run metadata each require a fresh authoritative read.
  useEffect(() => {
    if (!sessionId || !requestScopeRef.current.active) return;
    const controller = new AbortController();
    const scope = requestScopeRef.current;
    const revision = runControlRevisionRef.current;
    const isCurrentRead = () => !controller.signal.aborted && scope.active && requestScopeRef.current === scope
      && activeSessionIdRef.current === sessionId && runControlRevisionRef.current === revision;
    // Zero is a valid source waterline; this endpoint reads current Run state.
    transcriptTransport.loadActiveAgentRun({ sessionId, sourceHighWater: "0" }, controller.signal)
      .then(({ agentRun }) => {
        if (!isCurrentRead()) return;
        const currentRun = agentRun && !terminalRunIdsRef.current.has(agentRun.agentRunId) ? agentRun : null;
        currentRunIdentityRef.current = currentRun?.agentRunId || null;
        updateActiveTranscriptRun(currentRun);
        acceptedRunTargetRef.current = null;
        setAcceptedRunTarget(null);
      }).catch(errorValue => {
        if (isCurrentRead()) handlePresentationFailure(errorValue, sessionId);
      });
    return () => controller.abort();
  }, [sessionId, requestScopeKey, presentationAttempt, runControlAttempt, transcriptTransport, updateActiveTranscriptRun, handlePresentationFailure]);

  function retryTranscriptStream() {
    if (streamIssue?.reconnecting) return;
    setPresentationAttempt(value => value + 1);
  }

  function startNewChat(projectId = "", sidebarTab = "chat") {
    if (sending) return;
    clearRunControl();
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
    navigate(workspaceDraft || !activeAgent
      ? `/w/${encodeURIComponent(workspace.id)}/app${projectId ? `?${new URLSearchParams({ projectId })}` : ""}`
      : `/w/${encodeURIComponent(workspace.id)}/agents/${encodeURIComponent(conversationAgentId)}?${newSessionSearch}`, { state: { sidebarTab } });
  }

  function selectSession() {
    acceptedRouteSessionIdRef.current = "";
    setPendingUploadFiles([]);
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
  }

  async function createProject(name) {
    const projectAgentId = activeAgent?.id || defaultAgentId;
    const response = await apiResponse(`/api/workspaces/${workspace.id}/session-projects`, {
      method: "POST",
      body: JSON.stringify({ agentId: projectAgentId, name }),
    });
    const data = await response.json();
    if (
      !data?.project
      || data.project.workspaceId !== workspace.id
      || data.project.agentId !== projectAgentId
    ) throw new Error("session_project_response_invalid");
    setProjects((items) => [...items, data.project]);
    return data.project;
  }

  function onSessionDeleted(targetSessionId, remainingSessions) {
    if (targetSessionId === sessionId) {
      clearRunControl();
      acceptedRouteSessionIdRef.current = "";
      clearConversationProjection();
      setDraft("");
      setLoadingHistory(true);
      const next = remainingSessions[0];
      navigate(next ? sessionChatPath(workspace.id, next.id) : `/w/${encodeURIComponent(workspace.id)}/app`);
      if (!next) setLoadingHistory(false);
      clearContextPanel();
    }
  }

  async function cancelActiveAgentRun() {
    const targetSessionId = acceptedTarget?.sessionId || sessionId;
    if (activeAgentRunId && targetSessionId && currentControl !== submissionControl) {
      currentControl.setTarget({ sessionId: targetSessionId, agentRunId: activeAgentRunId });
    }
    if (!controlApplies && !messageRequestRef.current) {
      setPresentationAttempt(value => value + 1);
      return;
    }
    try { await currentControl.stop(); }
    catch { setPresentationAttempt(value => value + 1); }
  }

  async function sendMessage(event, replaceUnconfirmedInput = false) {
    event?.preventDefault();
    const submittedDraft = draft;
    const text = submittedDraft.trim();
    if (!workspace) return;
    if (!text) { document.getElementById?.("messageDraft")?.focus(); return; }
    if (messageRequestRef.current || auxiliaryRequestRef.current) return;
    if (loadingHistory) { setPresentationAttempt(value => value + 1); return; }
    const requestScope = requestScopeRef.current;
    let ownedSubmission = null;
    const isCurrentRequest = () => {
      const pathname = window.location.pathname;
      const currentAgentId = matchPath(`${workspaceBase}/agents/:agentId`, pathname)?.params.agentId;
      const currentSessionId = matchPath(`${workspaceBase}/sessions/:sessionId`, pathname)?.params.sessionId;
      const locationOwnsRequest = pathname === `${workspaceBase}/app`
        || currentAgentId === conversationAgentId
        || currentSessionId === requestedSessionId
        || pathname.startsWith(`${workspaceBase}/settings/`);
      return locationOwnsRequest && requestScope.active && requestScopeRef.current === requestScope
        && (!ownedSubmission || messageRequestRef.current === ownedSubmission);
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
      auxiliaryRequestRef.current = supplementId;
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
        setDraft(value => value === submittedDraft ? "" : value);
      } catch (requestError) {
        if (isCurrentRequest()) setError(requestError?.message === "agent_run_supplement_unavailable" ? t("appRoute.unableToDeliverSupplementalInputRightNowPleaseTry") : t("appRoute.unableToSendSupplementalInput"));
      } finally {
        if (auxiliaryRequestRef.current === supplementId) auxiliaryRequestRef.current = null;
        if (isCurrentRequest()) setSending(false);
      }
      return;
    }
    if (!modelId) return;
    let attemptId;
    try { attemptId = submissionControl.begin(); }
    catch { return; }
    const requestToken = { attemptId, scopeKey: requestScopeKey, scope: requestScope };
    messageRequestRef.current = requestToken;
    ownedSubmission = requestToken;
    submissionOriginRef.current = requestToken;
    pendingBindingRef.current = { request: requestToken, sessionId: null, agentRunId: null };
    setSending(true);
    const targetSessionId = workspaceDraft || startFresh ? "new" : requestedSessionId || sessionId || "new";
    const attachmentRefs = [...pendingAttachmentIds].sort();
    const uploadFiles = pendingUploadFiles;
    setError("");
    if (targetSessionId === "new") composerStartRectRef.current = composerRef.current?.getBoundingClientRect() || null;
    const startedAtMs = Date.now();
    setWorkTime({ startedAtMs, sessionId: targetSessionId });
    const pendingMessage = { text, startedAtMs, sessionId: targetSessionId === "new" ? null : targetSessionId, agentRunId: null };
    setPendingUserMessage(pendingMessage);
    setDraft("");
    let accepted = false;
    try {
      const messageData = await submissionControl.submit(hooks => operations.submit(`/api/workspaces/${workspace.id}/sessions/${targetSessionId}/messages`, {
          text,
          agentId: conversationAgentId,
          ...(targetSessionId === "new" && requestedProjectId ? { projectId: requestedProjectId } : {}),
          ...(modelSelectionLocked ? {} : { modelConfigRef: modelId, ...(thinkingMode ? { thinkingMode } : {}) }),
          ...(uploadFiles.length ? {} : { attachmentRefs }),
      }, uploadFiles, [], replaceUnconfirmedInput, hooks));
      accepted = true;
      // Navigation invalidates only this UI result, never the server's accepted work.
      if (!isCurrentRequest()) return;
      if (targetSessionId !== "new" && messageData.sessionId !== targetSessionId) throw new Error("AgentRun acceptance identity is invalid");
      acceptRunTarget(messageData);
      pendingBindingRef.current = { request: requestToken, sessionId: messageData.sessionId, agentRunId: messageData.agentRunId };
      const priorTerminal = terminalFactsRef.current.get(JSON.stringify([messageData.sessionId, messageData.agentRunId]));
      if (priorTerminal) operations.complete(messageData.operationId);
      setPendingUserMessage(value => value === pendingMessage ? { ...value, agentRunId: messageData.agentRunId,
        ...(priorTerminal ? { status: priorTerminal.status, completedAtMs: priorTerminal.completedAtMs ?? Date.now() } : {}) } : value);
      const { session: acceptedSession } = await (await apiResponse(`/api/sessions/${encodeURIComponent(messageData.sessionId)}`)).json();
      if (!isCurrentRequest()) return;
      if (
        acceptedSession?.id !== messageData.sessionId
        || acceptedSession.agentId !== conversationAgentId
        || (targetSessionId === "new" && (acceptedSession.projectId || "") !== requestedProjectId)
      ) {
        throw new Error("AgentRun acceptance identity is invalid");
      }
      const resolvedSessionId = messageData.sessionId;
      setPendingUserMessage(value => value?.agentRunId === messageData.agentRunId
        ? { ...value, sessionId: resolvedSessionId } : value);
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
        acceptedRouteSessionIdRef.current = resolvedSessionId;
        setSessionId(resolvedSessionId);
        setSending(false);
        onSessionAccepted(sessionChatPath(workspace.id, resolvedSessionId));
      } else {
        operations.complete(messageData.operationId);
        if (streamIssue && !streamIssue.reconnecting) setPresentationAttempt(value => value + 1);
      }
    } catch (errorValue) {
      if (!isCurrentRequest()) return;
      if (errorValue?.message === "operation_result_stale") return;
      if (errorValue?.message === "operation_dispatch_stopped") {
        setPendingUserMessage(value => value === pendingMessage ? null : value);
        setDraft(value => value || text);
        setWorkTime(null);
        return;
      }
      setDraft(value => value || text);
      if (!accepted && !operations.pending()) setPendingUserMessage(value => value === pendingMessage ? null : value);
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
          const response = await apiResponse(`/api/models?workspaceId=${encodeURIComponent(workspace.id)}`);
          const data = await response.json();
          if (isCurrentRequest()) {
            acceptModels(data.models);
            setError(t("appRoute.modelListChangedSelectModel"));
          }
        } catch {
          if (isCurrentRequest()) showStreamError(errorValue);
        }
      } else if (submissionControl.getSnapshot().phase === "unknown") {
        setError(t("operations.inputUnconfirmed"));
      } else {
        showStreamError(errorValue);
      }
    } finally {
      const current = isCurrentRequest();
      if (messageRequestRef.current === requestToken) messageRequestRef.current = null;
      if (current) setSending(false);
    }
  }

  async function checkPendingOperation(preserveChangedDraft = false) {
    if (auxiliaryRequestRef.current) return;
    const auxiliaryToken = {};
    auxiliaryRequestRef.current = auxiliaryToken;
    const operationId = operations.pending()?.operationId;
    const scope = requestScopeRef.current;
    const isCurrent = () => scope.active && requestScopeRef.current === scope;
    setSending(true);
    try {
      let accepted;
      const controlled = submissionControl.getSnapshot();
      if (controlled.stopRequested && controlled.operationId === operationId) {
        const recovered = await submissionControl.recover();
        if (recovered.error) throw recovered.error;
        if (recovered.target && recovered.operationId === operationId) {
          // These identities came from the validated receipt/cancel response;
          // this is local navigation data, not a fabricated protocol receipt.
          accepted = { operationId, ...recovered.target };
        }
      } else accepted = await operations.recover();
      if (accepted) {
        if (!isCurrent()) return;
        if (accepted.sessionId === activeSessionIdRef.current) {
          acceptRunTarget(accepted);
          if (pendingBindingRef.current) pendingBindingRef.current = { ...pendingBindingRef.current, sessionId: accepted.sessionId, agentRunId: accepted.agentRunId };
          const terminal = terminalFactsRef.current.get(JSON.stringify([accepted.sessionId, accepted.agentRunId]));
          setPendingUserMessage(value => value && (value.sessionId === accepted.sessionId || value.sessionId === null)
            ? { ...value, agentRunId: accepted.agentRunId, ...(terminal ? { status: terminal.status, completedAtMs: terminal.completedAtMs ?? Date.now() } : {}) } : value);
        }
        const { session } = await (await apiResponse(`/api/sessions/${encodeURIComponent(accepted.sessionId)}`)).json();
        if (!isCurrent()) return;
        if (session?.id !== accepted.sessionId || typeof session.agentId !== "string" || !session.agentId) throw new Error("session_response_invalid");
        if (session.id === activeSessionIdRef.current && session.agentId === conversationAgentId) {
          operations.complete(accepted.operationId);
          setError(preserveChangedDraft ? t("operations.inputUnconfirmed") : "");
          setPresentationAttempt(value => value + 1);
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
    } finally { if (auxiliaryRequestRef.current === auxiliaryToken) auxiliaryRequestRef.current = null; if (isCurrent()) setSending(false); }
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
  const visiblePendingMessage = pendingUserMessage && (pendingUserMessage.sessionId === sessionId
    || (pendingUserMessage.sessionId === null && submissionOriginRef.current?.scope === requestScopeRef.current)) ? pendingUserMessage : null;
  const isHome = !requestedSessionId && !sessionId && transcriptList.blockIds.length === 0 && !visiblePendingMessage;
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
      <AgentDocumentIdentity agent={activeAgent} />
      <div className="workspaceSidebarSlot">
        {isSidebarOpen ? <ShellSidebar
          workspace={workspace}
          agents={agents}
          activeAgent={activeAgent}
          onStartNewChat={startNewChat}
          onCollapse={() => setIsSidebarOpen(false)}
          initialTab={location.state?.sidebarTab || (workspaceDraft ? "home" : "chat")}
          sessionProps={{
            agentId: conversationAgentId,
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
          onReadingLatestChange={(targetSessionId, following) => { if (targetSessionId === activeSessionIdRef.current) setReadingLatest(following); }}
            toolOperations={toolOperations}
            sessionId={sessionId || null}
            initialInputOrigin={initialInputOrigin?.scope === requestScopeKey && initialInputOrigin.sessionId === sessionId ? initialInputOrigin.value : null}
            loadingHistory={loadingHistory}
            loadingOlderHistory={loadingOlderHistory}
            onLoadOlderHistory={loadOlderHistory}
            emptyState={isHome ? <HomePlane agent={activeAgent} /> : null}
            onShowCitation={showCitation}
            onShowArtifact={(artifact) => showArtifact(artifact, "conversation")}
            pendingUserMessage={visiblePendingMessage}
            historyMemoryPressure={transcriptManagedBytes >= TRANSCRIPT_MEMORY_WARNING_BYTES}
            onReleaseLoadedHistory={releaseLoadedTranscriptHistory}
            running={hasActiveAgentRun || (sending && !["failed", "cancelled", "completed"].includes(pendingUserMessage?.status))}
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
            activeAgentName={activeAgent?.name || "Centaeris"}
            workspaceAvailable={Boolean(workspace)}
            sending={sending}
            loadingHistory={loadingHistory}
            hasActiveAgentRun={hasActiveAgentRun || controlPending}
            controlBusy={controlApplies && ["preparing", "submitting", "recovering", "cancelling"].includes(controlSnapshot.phase)}
            uploadingAttachment={uploadingAttachment}
            onUploadAttachment={uploadAttachment}
            sessionId={sessionId}
            contextRevision={contextVersion?.sessionId === sessionId ? contextVersion.revision : "0"}
            modelSelectionDisabled={modelSelectionLocked}
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
