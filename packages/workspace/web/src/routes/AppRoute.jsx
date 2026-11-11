import { animate } from "@centaeris/ui/motion";
import { useTranslation } from "../i18n";
import { useCompactWorkspaceLayout, useSidebarOpen, useWorkspaceNavigation } from "../shell/workspaceNavigationState";
import { useCallback, useEffect, useEffectEvent, useId, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { matchPath, useNavigate, useRouteLoaderData } from "react-router";
import { apiJson, apiResponse } from "../api";
import { OperationClient, acceptedConversationReviewLink, consumeReviewedOperation, isDefiniteSubmissionRejection, isWorkspacePreservationPending } from "../chat/operationReceipts";
import { WorkspaceContextPanel, WorkspacePreviewToggle } from "../components/WorkspaceContextPanel";
import { DocumentPreview } from "../components/DocumentPreview";
import { officeFileType } from "../chat/officeFormats.mjs";
import { officePreviewUrl } from "../chat/attachments.mjs";
import { codePreviewCanRender } from "../chat/codePreviewFormats.mjs";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { useModalDialog } from "../components/useModalDialog";
import { dismissDetailsOnOutsideInteraction } from "../components/detailsDismissal";
import { streamWorkspacePresentation } from "../chat/workspacePresentationTransport";
import { createTranscriptViewStore } from "../chat/transcriptViewStore";
import { compareTranscriptBlocks } from "../chat/transcriptContract";
import { hasCommittedPendingMessage } from "../chat/transcriptTurns";
import { TranscriptToolOperationRegistry } from "../chat/transcriptToolOperations";
import { createWorkspaceTranscriptTransport } from "../chat/transcriptTransport";
import { useTranscriptSearchTarget } from "../chat/useTranscriptSearchTarget";
import { WorkspacePresentationController } from "../chat/workspacePresentationController";
import { TranscriptBlockList } from "../chat/TranscriptBlockList";
import { clearTranscriptContentRangeCache } from "../chat/transcriptContentRanges";
import { createSessionViewCache } from "../chat/sessionViewCache";
import { ConversationReadFeedback } from "../chat/ConversationReadFeedback";
import { WorkspaceComposer } from "../chat/WorkspaceComposer";
import { SessionQuestionCard } from "../chat/SessionQuestionCard";
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
import { ShellSidebar } from "../shell/ShellSidebar";
import { useWorkspaceSearch, WorkspaceSearch } from "../shell/WorkspaceSearch";
import { isWorkspaceCatalogPath, sessionChatPath, groupSessionNavigation, sortSessionNavigation as sortSessions } from "../shell/sessionNavigation.mjs";
import { SessionNavigationRow } from "../shell/SessionNavigationRow";
import { useSessionNavigationActions } from "../shell/useSessionNavigationActions";
import { SessionReadController, mergeSessionAttention } from "../shell/sessionReadState";
import { createSessionNavigationStore } from "../shell/sessionNavigationStore";
import { useNavigationRowActions, useLatestCallback } from "../shell/useNavigationRowActions";
import {
  Archive,
  Image as ImageIcon,
  Ellipsis,
  Pencil,
  Pin,
  PinOff,
  PanelLeft,
  Search,
  Trash2,
  X,
} from "lucide-react";

const CLOSED_CONTEXT_PANEL = Object.freeze({ mode: "closed" });

const ARTIFACT_PREVIEW_MAX_BYTES = 1024 * 1024;
const ARTIFACT_PREVIEW_CONTENT_TYPES = new Set([
  "text/plain",
  "text/markdown",
  "application/pdf",
  "image/png",
  "image/jpeg",
  "image/webp",
]);

function useWorkbenchProgress(ref, property, open, { initial, duration, immediate = false }) {
  useLayoutEffect(() => {
    const workbench = ref.current;
    if (!workbench) return;
    const from = Number(workbench.style.getPropertyValue(property) || initial);
    const target = open ? 1 : 0;
    if (from === target) return;
    const write = value => workbench.style.setProperty(property, String(value));
    const preference = window.matchMedia("(prefers-reduced-motion: reduce)");
    if (immediate || preference.matches) {
      write(target);
      return;
    }
    const playback = animate(from, target, {
      duration,
      ease: [0.16, 1, 0.3, 1],
      onUpdate: write,
    });
    const settle = () => {
      if (preference.matches) {
        playback.complete();
        write(target);
      }
    };
    preference.addEventListener("change", settle);
    return () => {
      preference.removeEventListener("change", settle);
      playback.stop();
    };
  }, [ref, property, open, initial, duration, immediate]);
}

function upsertBy(items, key, value) {
  const index = items.findIndex((item) => item[key] === value[key]);
  if (index < 0) return [...items, value];
  return items.map((item, itemIndex) => (itemIndex === index ? value : item));
}

function ConversationTitle({ session, actions, running }) {
  const { t } = useTranslation();
  const menuRef = useRef(null);
  const titleRef = useRef(null);
  const [renaming, setRenaming] = useState(false);
  useEffect(() => {
    if (!renaming) return dismissDetailsOnOutsideInteraction(menuRef.current);
  }, [renaming]);

  function closeMenu() {
    menuRef.current.open = false;
    titleRef.current?.focus();
  }

  function finishRename(value, returnFocus = false) {
    setRenaming(false);
    if (value !== undefined) void actions.renameSession(session, value);
    if (returnFocus) window.requestAnimationFrame(() => titleRef.current?.focus());
  }

  if (renaming) return <form className="workspaceConversationRename" onSubmit={event => {
    event.preventDefault();
    finishRename(event.currentTarget.elements.title.value, true);
  }}>
    <input name="title" aria-label={t("appRoute.renameValue", { value1: session.title })} autoFocus maxLength={200}
      defaultValue={session.title} onFocus={event => event.currentTarget.select()}
      onBlur={event => finishRename(event.currentTarget.value)}
      onKeyDown={event => {
        if (event.nativeEvent.isComposing) return;
        if (event.key === "Escape") {
          event.preventDefault();
          finishRename(undefined, true);
        }
      }} />
  </form>;

  return <div className="workspaceConversationHeading">
    <h1 className="workspaceConversationTitle" title={session.title}>{session.title || t("searchOverlay.untitledConversation")}</h1>
    <details className="workspaceConversationActions" ref={menuRef}>
      <summary ref={titleRef} className="workspacePanelToggle" role="button" aria-label={t("appRoute.conversationActionsForValue", { value1: session.title })}><Ellipsis aria-hidden="true" /></summary>
      <div className="workspaceSessionMenu">
        <button type="button" disabled={Boolean(actions.savingSessionId)} onClick={() => { closeMenu(); setRenaming(true); }}><Pencil aria-hidden="true" />{t("appRoute.rename")}</button>
        <button type="button" disabled={Boolean(actions.savingSessionId)} onClick={() => { closeMenu(); actions.onTogglePin(session); }}>{session.isPinned ? <PinOff aria-hidden="true" /> : <Pin aria-hidden="true" />}{t(session.isPinned ? "appRoute.unpin" : "appRoute.pin")}</button>
        <button type="button" disabled={running || Boolean(actions.archivingSessionId)} onClick={() => { closeMenu(); void actions.onArchive(session); }}><Archive aria-hidden="true" />{t("archive.title")}</button>
        <button type="button" className="isDanger" disabled={Boolean(actions.deletingSessionId)} onClick={() => { closeMenu(); actions.onRequestDelete(session); }}><Trash2 aria-hidden="true" />{t("appRoute.delete")}</button>
      </div>
    </details>
  </div>;
}

export function AppPageContent({ initialSession = null, workspaceDraft, visible = true, location, navigationKey = location.key, catalogContent = null, modelsVersion, onSessionAccepted }) {
  const { t } = useTranslation();
  const { user } = useRouteLoaderData("authenticated");
  const { workspace } = useRouteLoaderData("workspace");
  const workspaceNavigation = useWorkspaceNavigation(user.id, workspace.id);
  const operations = useMemo(() => new OperationClient({ userId: user.id, workspaceId: workspace.id, command: "submitMessage" }), [user.id, workspace.id]);
  const questionOperations = useMemo(() => new OperationClient({ userId: user.id, workspaceId: workspace.id, command: "respondQuestion" }), [user.id, workspace.id]);
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
  const requestedSessionId = initialSession?.id || "";
  const requestedProjectId = searchParams.get("projectId") || "";
  const requestedPrompt = searchParams.get("prompt") || "";
  const startFresh = searchParams.get("new") === "1";
  const workspaceBase = `/w/${encodeURIComponent(workspace.id)}`;
  const requestScopeKey = JSON.stringify([workspace.id, workspaceDraft, requestedSessionId, startFresh, requestedProjectId]);
  const requestScopeRef = useRef({ key: requestScopeKey, active: true });
  if (requestScopeRef.current.key !== requestScopeKey) requestScopeRef.current = { key: requestScopeKey, active: true };
  const navigationScopeKey = JSON.stringify([user.id, workspace.id]);
  const navigationSnapshotRef = useRef({ scope: navigationScopeKey, sessions: null });
  if (navigationSnapshotRef.current.scope !== navigationScopeKey) navigationSnapshotRef.current = { scope: navigationScopeKey, sessions: null };
  const navigationOwner = useMemo(() => ({ scope: navigationScopeKey,
    store: createSessionNavigationStore(), views: createSessionViewCache() }), [navigationScopeKey]);
  const navigationStore = navigationOwner.store;
  const { sessions, projects } = useSyncExternalStore(navigationStore.subscribe, navigationStore.getSnapshot, navigationStore.getSnapshot);
  const { setSessions, setProjects } = navigationStore;
  const sessionViewCache = navigationOwner.views;
  const resumedViewRef = useRef(null);
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
  const searchEventId = requestedSessionId ? searchParams.get("at") || "" : "";
  const searchHighlight = (searchParams.get("highlight") || "").slice(0, 256);
  const focusedTranscript = useTranscriptSearchTarget(requestedSessionId, searchEventId, transcriptTransport);
  function returnToLatest() {
    const params = new URLSearchParams(location.search);
    params.delete("at"); params.delete("highlight");
    navigate({ pathname: location.pathname, search: params.size ? `?${params}` : "", hash: location.hash }, { replace: true });
  }
  const fileInputRef = useRef(null);
  const composerRef = useRef(null);
  const composerStartRectRef = useRef(null);
  const [models, setModels] = useState([]);
  const modelsRef = useRef([]);
  const [modelId, setModelId] = useState("");
  const [thinkingMode, setThinkingMode] = useState("");
  const [sessionId, setSessionId] = useState(() => workspaceDraft || startFresh ? "" : requestedSessionId);
  const [presentationAttempt, setPresentationAttempt] = useState(0);
  const [runControlAttempt, setRunControlAttempt] = useState(0);
  const [contextVersion, setContextVersion] = useState(null);
  const [readableThrough, setReadableThrough] = useState("0");
  const [readingLatest, setReadingLatest] = useState(true);
  const [pageVisible, setPageVisible] = useState(() => typeof document === "undefined" || !document.hidden);
  const enteredReadRef = useRef({ sessionId: "", handled: false });
  if (enteredReadRef.current.sessionId !== sessionId) enteredReadRef.current = { sessionId, handled: false };
  const presentationScopeKey = JSON.stringify([workspace.id, sessionId]);
  const presentationScopeRef = useRef(presentationScopeKey);
  presentationScopeRef.current = presentationScopeKey;
  const [assets, setAssets] = useState([]);
  const [assetsUnavailable, setAssetsUnavailable] = useState(false);
  const [pendingAttachmentIds, setPendingAttachmentIds] = useState([]);
  const [pendingUploadFiles, setPendingUploadFiles] = useState([]);
  const [draft, setDraft] = useState("");
  const [rejectedSubmissionScopes, setRejectedSubmissionScopes] = useState(() => new Set());
  const [pendingUserMessage, setPendingUserMessage] = useState(null);
  const pendingUserMessageRef = useRef(null);
  useEffect(() => { pendingUserMessageRef.current = pendingUserMessage; }, [pendingUserMessage]);
  const [workTime, setWorkTime] = useState(null);
  const [workActivity, setWorkActivity] = useState(null);
  const [pendingQuestion, setPendingQuestion] = useState(null);
  const answeredQuestionIdsRef = useRef(new Set());
  const enterStartsNewLine = useEnterStartsNewLine(user.id);
  const [composerError, setComposerError] = useState("");
  const [conversationError, setConversationError] = useState("");
  const [navigationError, setNavigationError] = useState(false);
  const [navigationLoaded, setNavigationLoaded] = useState(false);
  const [olderHistoryError, setOlderHistoryError] = useState(false);
  const [streamIssue, setStreamIssue] = useState(null);
  const [sending, setSending] = useState(false);
  const [uploadingAttachment, setUploadingAttachment] = useState(false);
  const [attachmentPreview, setAttachmentPreview] = useState(null);
  const sessionActions = useSessionNavigationActions({ scopeKey: JSON.stringify([workspace.id]), sessions, onChangeSessions: setSessions, onDeleted: onSessionDeleted });
  const rowActions = useNavigationRowActions(sessionActions);
  const { deletingSessionId, deleteConfirmationSessionId, setDeleteConfirmationSessionId, setEditingSessionId, setOpenSessionMenuId, markSessionRead, deleteSession } = sessionActions;
  const [loadingHistory, setLoadingHistory] = useState(!workspaceDraft);
  const [presentationReady, setPresentationReady] = useState(false);
  const [loadingOlderHistory, setLoadingOlderHistory] = useState(false);
  const [activeTranscriptRun, setActiveTranscriptRun] = useState(null);
  const [contextPanel, setContextPanel] = useState(CLOSED_CONTEXT_PANEL);
  const [filePreviewOpen, setFilePreviewOpen] = useState(false);
  const previewPanelId = useId();
  const previewToggleRef = useRef(null);
  const [browserPanelWidthPx, setBrowserPanelWidthPx] = useState(null);
  const isSidebarOpen = useSidebarOpen(workspaceNavigation);
  const setIsSidebarOpen = workspaceNavigation.setDesktopOpen;
  const initialSidebarProgress = useRef(isSidebarOpen ? 1 : 0).current;
  const compactLayout = useCompactWorkspaceLayout();
  const [drawerFor, setDrawerFor] = useState(null);
  const workbenchRef = useRef(null);
  const transcriptRef = useRef(null);
  const sidebarRef = useRef(null);
  const search = useWorkspaceSearch(navigationKey, visible || Boolean(catalogContent));
  const drawerOpen = compactLayout && drawerFor === navigationKey && !search.open && (visible || Boolean(catalogContent));
  useWorkbenchProgress(workbenchRef, "--workspace-sidebar-progress", isSidebarOpen,
    { initial: initialSidebarProgress, duration: 0.22, immediate: compactLayout });
  useWorkbenchProgress(workbenchRef, "--workspace-preview-progress", visible && filePreviewOpen,
    { initial: 0, duration: 0.24, immediate: !visible });
  useLayoutEffect(() => {
    if (!compactLayout) return;
    const drawer = sidebarRef.current;
    if (drawerOpen && !drawer.open) drawer.showModal();
    else if (!drawerOpen && drawer.open) drawer.close();
  }, [compactLayout, drawerOpen]);
  const attachmentDialogRef = useModalDialog({ open: visible && Boolean(attachmentPreview), onClose: () => setAttachmentPreview(null) });
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
    setModelId((current) => {
      if (nextModels.some((model) => model.id === current)) return current;
      if (!current) {
        if (previousModels.length) return "";
        const preferred = readPreferredModelIdentity(user.id, workspace.id);
        return nextModels.find((model) => JSON.stringify([model.providerId, model.modelName]) === preferred)?.id
          || "";
      }
      const previous = previousModels.find((model) => model.id === current);
      return nextModels.find((model) =>
        model.providerId === previous?.providerId && model.modelName === previous?.modelName
      )?.id || "";
    });
  }, [user.id, workspace.id]);

  const clearContextPanel = useCallback(() => {
    previewRequestIdRef.current += 1;
    setFilePreviewOpen(false);
    setContextPanel(CLOSED_CONTEXT_PANEL);
  }, []);
  const toggleFilePreview = useCallback(() => {
    transcriptRef.current?.preserveReadingPosition();
    setFilePreviewOpen(open => !open);
  }, []);
  const clearConversationProjection = useCallback(() => {
    transcriptStore.clear();
    toolOperations.clear();
    clearTranscriptContentRangeCache();
    updateActiveTranscriptRun(null);
    setWorkActivity(null);
    setPendingQuestion(null);
    olderHistoryRequestRef.current?.controller.abort();
    olderHistoryRequestRef.current = null;
    setOlderHistoryError(false);
    setConversationError("");
    setAssets([]);
    setAssetsUnavailable(false);
    setPendingAttachmentIds([]);
    setPendingUploadFiles([]);
  }, [transcriptStore, toolOperations, updateActiveTranscriptRun]);
  const stopActiveStream = useCallback(() => {
    streamAbortRef.current?.abort();
    chatControllerRef.current?.dispose();
  }, []);
  const reportModelLoadError = useEffectEvent(() => setComposerError(t("appRoute.unableToRefreshModelsPleaseTryAgain")));
  const markSelectedSessionRead = useEffectEvent(markSessionRead);

  // biome-ignore lint/correctness/useExhaustiveDependencies: The version is an explicit refresh signal from retained settings routes.
  useEffect(() => {
    const controller = new AbortController();
    async function load() {
      try {
        const data = await apiResponse(`/api/models?workspaceId=${encodeURIComponent(workspace.id)}`, { signal: controller.signal }).then(response => response.json());
        if (!controller.signal.aborted) {
          acceptModels(data.models);
        }
      } catch {
        if (!controller.signal.aborted) reportModelLoadError();
      }
    }
    load();
    return () => controller.abort();
  }, [modelsVersion, acceptModels, workspace.id]);

  useEffect(() => {
    if (contextPanel.mode !== "filePreview" || !contextPanel.objectUrl) return undefined;
    return () => URL.revokeObjectURL(contextPanel.objectUrl);
  }, [contextPanel]);

  useEffect(() => {
    if (!visible || search.open || !filePreviewOpen) return undefined;
    const closeOnEscape = (event) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (document.getElementById(previewPanelId)?.contains(document.activeElement)) {
        previewToggleRef.current?.focus();
      }
      toggleFilePreview();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [visible, search.open, filePreviewOpen, previewPanelId, toggleFilePreview]);

  useEffect(() => {
    requestScopeRef.current.active = true;
    return () => {
      requestScopeRef.current.active = false;
      stopActiveStream();
    };
  }, [stopActiveStream]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: changing route intent (including its project) starts a new conversation view scope.
  useLayoutEffect(() => {
    if (!startFresh && requestedSessionId && acceptedRouteSessionIdRef.current === requestedSessionId) {
      acceptedRouteSessionIdRef.current = "";
      setLoadingHistory(false);
      return undefined;
    }
    const previousController = chatControllerRef.current;
    const previousView = previousController?.captureAppliedView();
    if (previousView && previousController.identity.sessionId) sessionViewCache.set(previousController.identity.sessionId, previousView);
    setComposerError("");
    setLoadingHistory(!workspaceDraft);
    clearRunControl();
    acceptedRouteSessionIdRef.current = "";
    setSessionId(workspaceDraft || startFresh ? "" : requestedSessionId);
    clearConversationProjection();
    resumedViewRef.current = !workspaceDraft && !startFresh && requestedSessionId ? sessionViewCache.get(requestedSessionId) ?? null : null;
    resumedViewRef.current?.restore();
    setPresentationReady(Boolean(resumedViewRef.current));
    setReadableThrough("0");
    setReadingLatest(true);
    setDraft("");
    setSending(false);
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
  }, [workspaceDraft, requestedSessionId, requestScopeKey, startFresh, sessionViewCache, clearConversationProjection, clearContextPanel, clearRunControl, setEditingSessionId, setOpenSessionMenuId]);

  const selectInitialDirectory = useEffectEvent(navigation => {
    if (!workspaceDraft && !startFresh && !requestedSessionId && !activeSessionIdRef.current) {
      setSessionId(navigation[0]?.id || "");
      if (!navigation.length) setLoadingHistory(false);
    }
  });
  useEffect(() => {
    let active = true;
    setNavigationError(false);
    const scope = navigationSnapshotRef.current;
    Promise.all([
      apiResponse(`/api/workspaces/${workspace.id}/sessions`).then((response) => response.json()),
      apiResponse(`/api/workspaces/${workspace.id}/session-projects`).then((response) => response.json()),
    ])
      .then(([data, projectData]) => {
        if (!active || !requestScopeRef.current.active || navigationSnapshotRef.current !== scope) return;
        if (!Array.isArray(data.sessions) || !Array.isArray(projectData.projects)) throw new Error("session_navigation_response_invalid");
        const navigation = navigationSnapshotRef.current.sessions ?? data.sessions;
        if (navigationSnapshotRef.current.sessions === null) setSessions(navigation);
        setProjects(projectData.projects);
        setNavigationLoaded(true);
        // Only an entry without an explicit Session chooses a first row.
        // A late workspace response never changes an explicit current selection.
        selectInitialDirectory(navigation);
      })
      .catch(() => {
        if (!active || !requestScopeRef.current.active || navigationSnapshotRef.current !== scope) return;
        setNavigationError(true);
      });
    return () => { active = false; };
  }, [workspace.id, setSessions, setProjects]);

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
  }), [user.id, workspace.id]);
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
    const canObserve = visible && pageVisible && !loadingHistory && !searchEventId;
    if (activeSession && canObserve && readingLatest && BigInt(readableThrough) >= BigInt(activeSession.latestAttentionSequence)) {
      const entered = enteredReadRef.current;
      if (entered.sessionId === sessionId && !entered.handled) {
        entered.handled = true;
        if (activeSession.isManuallyUnread) markSelectedSessionRead(activeSession);
      }
    }
    readController.observe({ session: activeSession ?? null, visible: canObserve, readingLatest, appliedSourceHighWater: readableThrough });
  }, [activeSession, loadingHistory, pageVisible, readingLatest, readableThrough, readController, sessionId, visible, searchEventId]);

  const handlePresentationFailure = useCallback((errorValue, targetSessionId) => {
    if (errorValue?.name === "AbortError" || (targetSessionId || "") !== activeSessionIdRef.current) return;
    const accessDenied = [401, 403].includes(errorValue?.status)
      || (errorValue?.status === 404 && errorValue.message === "presentation_scope_not_found");
    if (accessDenied) {
      requestScopeRef.current.active = false;
      stopActiveStream();
      readController.dispose();
      sessionViewCache.clear();
      resumedViewRef.current = null;
      setPresentationReady(false);
      navigationSnapshotRef.current.sessions = [];
      setSessions([]); setProjects([]); setReadableThrough("0");
      clearConversationProjection(); clearRunControl(); setStreamIssue(null); setWorkTime(null); setLoadingHistory(false);
      setConversationError("appRoute.unableToLoadConversationHistoryRefreshThePageAnd");
      return;
    }
    setLoadingHistory(false);
    setStreamIssue({ sessionId: targetSessionId || "", error: errorValue, reconnecting: false });
  }, [clearConversationProjection, clearRunControl, stopActiveStream, readController, sessionViewCache, setSessions, setProjects]);

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
    let presentedRunId = null;
    const current = transcriptStore.getListSnapshot();
    if (current.sessionId !== selectedSessionId) {
      clearConversationProjection();
      setReadableThrough("0");
      setReadingLatest(true);
    }
    setLoadingHistory(Boolean(selectedSessionId && (current.sessionId !== selectedSessionId || resumedViewRef.current)));
    if (selectedSessionId) {
      const url = `/api/sessions/${encodeURIComponent(selectedSessionId)}`;
      const options = { signal: abortController.signal };
      apiResponse(url, options).then(response => response.json()).then(sessionData => {
        if (!isCurrent()) return;
        if (sessionData.session?.id !== selectedSessionId || sessionData.session.workspaceId !== workspace.id) throw new Error("session_response_invalid");
        setSessions(items => sortSessions(upsertBy(items, "id", sessionData.session)));
        setConversationError(value => value === "appRoute.sessionDetailsUnavailable" ? "" : value);
      }).catch(errorValue => {
        if (isCurrent() && [401, 403].includes(errorValue?.status)) { handlePresentationFailure(errorValue, selectedSessionId); return; }
        if (isCurrent() && errorValue?.name !== "AbortError") setConversationError("appRoute.sessionDetailsUnavailable");
      });
      apiResponse(`${url}/assets`, options).then(response => response.json()).then(assetData => {
        if (!isCurrent()) return;
        setAssets(assetData.assets);
        setAssetsUnavailable(false);
      }).catch(errorValue => {
        if (isCurrent() && errorValue?.name !== "AbortError") setAssetsUnavailable(true);
      });
    }
    const resumedCursor = resumedViewRef.current?.cursor ?? null;
    resumedViewRef.current = null;
    const controller = new WorkspacePresentationController({
      workspaceId: workspace.id, sessionId: selectedSessionId,
      store: transcriptStore, signal: abortController.signal, isCurrent,
      cursor: selectedSessionId ? resumedCursor : null,
      applySnapshot: (page, signal) => restorePresentationSnapshot(page, signal, isCurrent),
      onSnapshot: () => {
        if (!isCurrent()) return;
        setLoadingHistory(false);
        if (hasCommittedPendingMessage(pendingUserMessageRef.current, selectedSessionId, transcriptStore.getListSnapshot().blockIds, transcriptStore.getBlockSnapshot)) setPendingUserMessage(null);
        const pending = operations.pending();
        if (pending?.receipt?.sessionId === selectedSessionId) operations.complete(pending.operationId);
      },
      onNavigation: next => {
        if (!isCurrent()) return;
        navigationSnapshotRef.current.sessions = next;
        setSessions(next);
        setNavigationError(false);
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
        const currentId = currentRunIdentityRef.current;
        if (run && !active) recordRunTerminal({ sessionId: selectedSessionId, agentRunId: run.agentRunId, status: run.status,
          ...(run.completedAt ? { completedAtMs: Date.parse(run.completedAt) } : {}) });
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
          if (currentId && currentId !== run.agentRunId) return;
        }
        presentedRunId = run?.agentRunId ?? null;
        if (active) setWorkActivity(value => value?.agentRunId === run.agentRunId ? value : null);
        currentRunIdentityRef.current = active ? run.agentRunId : null;
        if (run && acceptedRunTargetRef.current?.agentRunId === run.agentRunId) {
          acceptedRunTargetRef.current = null;
          setAcceptedRunTarget(null);
        }
        updateActiveTranscriptRun(active ? run : null);
        if (!active) {
          setWorkActivity(null);
          setPendingQuestion(null);
        }
        if (active && run.startedAt) setWorkTime(value => ({ ...value, sessionId: selectedSessionId, startedAtMs: Date.parse(run.startedAt), completedAtMs: null }));
        if (!active && run?.completedAt) setWorkTime(value => ({ ...value, sessionId: selectedSessionId, completedAtMs: Date.parse(run.completedAt) }));
      },
      onActivity: activity => {
        if (!isCurrent() || selectedSessionId === null || presentedRunId !== activity.agentRunId) return;
        if (activity.activity === null) {
          setWorkActivity(value => value?.agentRunId === activity.agentRunId ? null : value);
          return;
        }
        if (activity.activity === "preserving" || currentRunIdentityRef.current !== activity.agentRunId
          || terminalRunIdsRef.current.has(activity.agentRunId)) return;
        setWorkActivity({ sessionId: selectedSessionId, agentRunId: activity.agentRunId, activity: activity.activity });
      },
      onQuestion: frame => {
        if (!isCurrent() || selectedSessionId === null || currentRunIdentityRef.current !== frame.agentRunId
          || terminalRunIdsRef.current.has(frame.agentRunId)) return;
        if (frame.question && answeredQuestionIdsRef.current.has(frame.question.continuationId)) return;
        setPendingQuestion(frame.question ? { ...frame.question, sessionId: selectedSessionId, agentRunId: frame.agentRunId } : null);
      },
      onApplied: checkpoint => {
        if (!isCurrent()) return;
        const view = controller.captureAppliedView();
        if (view && selectedSessionId) sessionViewCache.set(selectedSessionId, view);
        setPresentationReady(true);
        if (hasCommittedPendingMessage(pendingUserMessageRef.current, selectedSessionId, transcriptStore.getListSnapshot().blockIds, transcriptStore.getBlockSnapshot)) setPendingUserMessage(null);
        setReadableThrough(checkpoint.sourceHighWater);
        setLoadingHistory(false);
        setStreamIssue(null);
      },
      onReset: () => { if (selectedSessionId) sessionViewCache.delete(selectedSessionId); resumedViewRef.current = null; setPresentationReady(false); },
    });
    chatControllerRef.current = controller;
    void streamWorkspacePresentation({
      workspaceId: workspace.id, sessionId: selectedSessionId,
      controller, signal: abortController.signal,
      onConnection: connection => {
        if (isCurrent()) setStreamIssue(connection === "reconnecting"
          ? { sessionId: selectedSessionId || "", reconnecting: true } : null);
      },
    }).catch(errorValue => { if (isCurrent()) handlePresentationFailure(errorValue, selectedSessionId); });
    return () => { active = false; abortController.abort(); controller.dispose(); };
  }, [presentationScopeKey, presentationAttempt, sessionId, workspace.id, operations, transcriptStore, toolOperations, clearConversationProjection, stopActiveStream, updateActiveTranscriptRun, restorePresentationSnapshot, handlePresentationFailure, recordRunTerminal, workspaceDraft, startFresh, requestedSessionId, sessionViewCache, setSessions]);

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
      }).catch(() => {
        // The presentation stream still owns live delivery and access checks.
        // A failed supplementary read cannot retire its current Run or scope.
      });
    return () => controller.abort();
  }, [sessionId, requestScopeKey, presentationAttempt, runControlAttempt, transcriptTransport, updateActiveTranscriptRun]);

  function retryTranscriptStream() {
    if (streamIssue?.reconnecting) return;
    setPresentationAttempt(value => value + 1);
  }

  function startNewChat(projectId = "") {
    if (sending) return;
    if (!visible && workspaceDraft && !sessionId && !projectId) {
      navigate(location.pathname + location.search + (location.hash || ""));
      return;
    }
    clearRunControl();
    acceptedRouteSessionIdRef.current = "";
    stopActiveStream();
    setSessionId("");
    setLoadingHistory(false);
    clearConversationProjection();
    setDraft("");
    setComposerError("");
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
    navigate(`${workspaceBase}/app${projectId ? `?${new URLSearchParams({ projectId })}` : ""}`);
  }

  function selectSession(target) {
    if (target?.id === sessionId) return;
    acceptedRouteSessionIdRef.current = "";
    setPendingUploadFiles([]);
    setEditingSessionId("");
    setOpenSessionMenuId("");
    clearContextPanel();
  }

  async function createProject(name) {
    const response = await apiResponse(`/api/workspaces/${workspace.id}/session-projects`, {
      method: "POST",
      body: JSON.stringify({ name }),
    });
    const data = await response.json();
    if (
      !data?.project
      || data.project.workspaceId !== workspace.id
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
      if (visible) navigate(next ? sessionChatPath(workspace.id, next.id) : `${workspaceBase}/app`);
      else setSessionId("");
      if (!visible || !next) setLoadingHistory(false);
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

  function setSubmissionRejected(rejected) {
    setRejectedSubmissionScopes(scopes => {
      if (scopes.has(requestScopeKey) === rejected) return scopes;
      const next = new Set(scopes);
      if (rejected) next.add(requestScopeKey);
      else next.delete(requestScopeKey);
      return next;
    });
  }

  async function sendMessage(event, replaceUnconfirmedInput = false) {
    event?.preventDefault();
    const submittedDraft = draft;
    const text = submittedDraft.trim();
    if (!workspace) return;
    if (!text) { document.getElementById?.("messageDraft")?.focus(); return; }
    if (searchEventId) returnToLatest();
    if (messageRequestRef.current || auxiliaryRequestRef.current) return;
    if (loadingHistory) { setPresentationAttempt(value => value + 1); return; }
    const requestScope = requestScopeRef.current;
    let ownedSubmission = null;
    const isCurrentRequest = () => {
      const pathname = window.location.pathname;
      const currentSessionId = matchPath(`${workspaceBase}/sessions/:sessionId`, pathname)?.params.sessionId;
      const locationOwnsRequest = pathname === `${workspaceBase}/app`
        || currentSessionId === requestedSessionId
        || isWorkspaceCatalogPath(workspaceBase, pathname)
        || pathname.startsWith(`${workspaceBase}/settings/`);
      return locationOwnsRequest && requestScope.active && requestScopeRef.current === requestScope
        && (!ownedSubmission || messageRequestRef.current === ownedSubmission);
    };
    if (hasActiveAgentRun && !operations.pending()) {
      if (!sessionId || !activeAgentRunId) return;
      if (pendingAttachmentIds.length || pendingUploadFiles.length) {
        setComposerError(t("appRoute.supplementalInputDoesNotSupportAttachmentsYetRemoveThe"));
        return;
      }
      setSending(true);
      setComposerError("");
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
        setSubmissionRejected(false);
      } catch (requestError) {
        if (isCurrentRequest()) setComposerError(requestError?.message === "agent_run_supplement_unavailable" ? t("appRoute.unableToDeliverSupplementalInputRightNowPleaseTry") : t("appRoute.unableToSendSupplementalInput"));
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
    setComposerError("");
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
          ...(targetSessionId === "new" && requestedProjectId ? { projectId: requestedProjectId } : {}),
          modelConfigRef: modelId, ...(thinkingMode ? { thinkingMode } : {}),
          ...(uploadFiles.length ? {} : { attachmentRefs }),
      }, uploadFiles, [], replaceUnconfirmedInput, hooks));
      accepted = true;
      // Navigation invalidates only this UI result, never the server's accepted work.
      if (!isCurrentRequest()) return;
      if (targetSessionId !== "new" && messageData.sessionId !== targetSessionId) throw new Error("AgentRun acceptance identity is invalid");
      setSubmissionRejected(false);
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
        || acceptedSession.workspaceId !== workspace.id
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
        onSessionAccepted(sessionChatPath(workspace.id, resolvedSessionId), acceptedSession);
      } else {
        operations.complete(messageData.operationId);
        if (streamIssue && !streamIssue.reconnecting) setPresentationAttempt(value => value + 1);
      }
    } catch (errorValue) {
      if (!isCurrentRequest()) return;
      if (errorValue?.message === "operation_result_stale") return;
      if (errorValue?.message === "operation_dispatch_stopped") {
        setPendingUserMessage(value => value === pendingMessage ? null : value);
        setDraft(value => value || submittedDraft);
        setWorkTime(null);
        return;
      }
      const rejected = !accepted && isDefiniteSubmissionRejection(errorValue) && !operations.pending();
      if (!accepted) {
        setDraft(value => value || submittedDraft);
        setPendingUserMessage(value => value === pendingMessage ? null : value);
        setWorkTime(null);
        if (rejected) setSubmissionRejected(true);
        if (isWorkspacePreservationPending(errorValue)) return;
      }
      if (accepted) {
        setConversationError("operations.acceptedReadFailed");
      } else if (errorValue?.message === "operation_pending_input_changed") {
        setComposerError(t("operations.deliveryUnconfirmed"));
        if (operations.pending()?.receipt) await checkPendingOperation(true);
      } else if (errorValue?.message === "operation_input_acceptance_unconfirmed") {
        await checkPendingOperation(true);
      } else if (errorValue?.message === "model_not_found") {
        try {
          const response = await apiResponse(`/api/models?workspaceId=${encodeURIComponent(workspace.id)}`);
          const data = await response.json();
          if (isCurrentRequest()) {
            acceptModels(data.models);
            setComposerError(t("appRoute.modelListChangedSelectModel"));
          }
        } catch {
          if (isCurrentRequest()) setComposerError(t("appRoute.unableToRefreshModelsPleaseTryAgain"));
        }
      } else if (submissionControl.getSnapshot().phase === "unknown" || operations.pending()?.receipt === null) {
        setComposerError(t("operations.deliveryUnconfirmed"));
      } else if (errorValue?.status === 413) {
        setComposerError(t("appRoute.attachmentTooLarge"));
      } else if (errorValue?.status === 429) {
        setComposerError(t("appRoute.sendingTemporarilyUnavailable"));
      } else if (errorValue?.status === 409 && errorValue.message === "session_has_active_agent_run") {
        setComposerError(t("appRoute.conversationAlreadyRunning"));
      } else if (!rejected && errorValue?.name !== "AbortError") {
        setComposerError(t("appRoute.theActionCouldNotBeCompletedPleaseTryAgain"));
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
        if (accepted.sessionId === activeSessionIdRef.current
          || (submissionOriginRef.current?.scope === scope && controlled.operationId === accepted.operationId)) {
          setSubmissionRejected(false);
        }
        if (accepted.sessionId === activeSessionIdRef.current) {
          acceptRunTarget(accepted);
          if (pendingBindingRef.current) pendingBindingRef.current = { ...pendingBindingRef.current, sessionId: accepted.sessionId, agentRunId: accepted.agentRunId };
          const terminal = terminalFactsRef.current.get(JSON.stringify([accepted.sessionId, accepted.agentRunId]));
          setPendingUserMessage(value => value && (value.sessionId === accepted.sessionId || value.sessionId === null)
            ? { ...value, agentRunId: accepted.agentRunId, ...(terminal ? { status: terminal.status, completedAtMs: terminal.completedAtMs ?? Date.now() } : {}) } : value);
        }
        const { session } = await (await apiResponse(`/api/sessions/${encodeURIComponent(accepted.sessionId)}`)).json();
        if (!isCurrent()) return;
        if (session?.id !== accepted.sessionId || session.workspaceId !== workspace.id) throw new Error("session_response_invalid");
        setConversationError(value => value === "operations.acceptedReadFailed" ? "" : value);
        if (session.id === activeSessionIdRef.current) {
          operations.complete(accepted.operationId);
          setComposerError(preserveChangedDraft ? t("operations.inputUnconfirmed") : "");
          setPresentationAttempt(value => value + 1);
        } else {
          setOperationReviewLink({ operationId: accepted.operationId, ...acceptedConversationReviewLink(workspace.id, session) });
          setComposerError(t("operations.inputUnconfirmed"));
        }
      }
    } catch (requestError) {
      if (!isCurrent()) return;
      if (requestError?.message === "operation_resource_unavailable" && requestError.status === 410) {
        if (operationId) operations.complete(operationId);
        setComposerError(t("operations.unavailable"));
      } else if (operations.pending()?.receipt) {
        setConversationError("operations.acceptedReadFailed");
      } else {
        setComposerError(t("operations.deliveryUnconfirmed"));
      }
    } finally { if (auxiliaryRequestRef.current === auxiliaryToken) auxiliaryRequestRef.current = null; if (isCurrent()) setSending(false); }
  }

  async function consumeOperationReview(operationId) {
    const scope = requestScopeRef.current;
    const isCurrent = () => scope.active && requestScopeRef.current === scope;
    setSending(true);
    try {
      await consumeReviewedOperation(operations, operationId, async (id) => (await (await apiResponse(`/api/sessions/${encodeURIComponent(id)}`)).json()).session);
      if (isCurrent()) setComposerError("");
    } catch { if (isCurrent()) setComposerError(t("operations.checkFailed")); }
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
      const view = chatControllerRef.current?.captureAppliedView();
      if (view) sessionViewCache.set(requestedForSessionId, view);
      setOlderHistoryError(false);
    } catch {
      if (!controller.signal.aborted) setOlderHistoryError(true);
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
      setComposerError(t("appRoute.addUpToValueMaterialsAtATime", { value1: MAX_UPLOAD_BATCH_FILES }));
      return;
    }
    if (!sessionId) {
      setComposerError("");
      setPendingUploadFiles((items) => [...items, ...files]);
      input.value = "";
      return;
    }
    const uploadSessionId = sessionId;
    const body = new FormData();
    files.forEach((file) => body.append("files", file));
    setComposerError("");
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
    } catch (errorValue) {
      if (activeSessionIdRef.current === uploadSessionId) {
        setComposerError(t(errorValue?.status === 413 ? "appRoute.attachmentTooLarge" : "appRoute.attachmentUploadFailed"));
      }
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

  function closeFilePreview() {
    if (contextPanel.mode !== "filePreview") return;
    const { origin } = contextPanel;
    transcriptRef.current?.preserveReadingPosition();
    clearContextPanel();
    window.requestAnimationFrame(() => {
      document.getElementById(origin.elementId)?.focus({ preventScroll: true });
    });
  }

  async function showCitation(citation, origin) {
    transcriptRef.current?.preserveReadingPosition();
    setFilePreviewOpen(true);
    const requestId = previewRequestIdRef.current + 1;
    previewRequestIdRef.current = requestId;
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
    transcriptRef.current?.preserveReadingPosition();
    setFilePreviewOpen(true);
    const requestId = previewRequestIdRef.current + 1;
    previewRequestIdRef.current = requestId;
    const panelIdentity = {
      artifactRef: artifact.artifactRef,
      displayName: artifact.filename,
      downloadUrl: artifact.downloadUrl,
      origin,
    };
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

  const visiblePendingMessage = pendingUserMessage && (pendingUserMessage.sessionId === sessionId
    || (pendingUserMessage.sessionId === null && submissionOriginRef.current?.scope === requestScopeRef.current)) ? pendingUserMessage : null;
  const isHome = !requestedSessionId && !sessionId && !visiblePendingMessage;
  const streamPaused = streamIssue?.sessionId === sessionId && !streamIssue.reconnecting;
  const acceptedReadFailed = conversationError === "operations.acceptedReadFailed";
  const readFeedbackProps = { store: transcriptStore, error: conversationError, paused: streamPaused,
    reconnecting: streamIssue?.sessionId === sessionId && streamIssue.reconnecting, ready: presentationReady, loading: loadingHistory,
    checking: sending, onCheck: () => void checkPendingOperation(), onRetry: () => void retryTranscriptStream() };
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
    if (hasCommittedPendingMessage(pendingUserMessage, sessionId || null, transcriptStore.getListSnapshot().blockIds, transcriptStore.getBlockSnapshot)) {
      setPendingUserMessage(null);
    }
  }, [pendingUserMessage, sessionId, transcriptStore]);
  const updateBrowserPanelWidth = useCallback((widthPx) => {
    transcriptRef.current?.preserveReadingPosition();
    const sidebarWidth = isSidebarOpen && window.innerWidth > 760
      ? Number.parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--workspace-sidebar-width"))
      : 0;
    const mainWidth = window.innerWidth - sidebarWidth;
    const wide = window.innerWidth >= (isSidebarOpen ? 1170 : 900);
    const maxWidthPx = Math.max(270, Math.floor(wide ? Math.min(window.innerWidth * 0.75, mainWidth - 320) : mainWidth - 32));
    setBrowserPanelWidthPx(Math.min(maxWidthPx, Math.max(270, Math.round(widthPx))));
  }, [isSidebarOpen]);
  function setWorkbenchSidebar(open) {
    transcriptRef.current?.preserveReadingPosition();
    setIsSidebarOpen(open);
  }
  const groupedSessions = useMemo(() => groupSessionNavigation(sessions, projects), [projects, sessions]);
  const SidebarContainer = compactLayout ? "dialog" : "div";
  function handleQuickAction(prompt) {
    setDraft(prompt);
    window.requestAnimationFrame(() => {
      document.getElementById("messageDraft")?.focus();
    });
  }

  const selectNavigationSession = useLatestCallback(selectSession);
  const startNavigationChat = useLatestCallback(startNewChat);
  const createNavigationProject = useLatestCallback(createProject);
  const collapseNavigation = useCallback(() => setDrawerFor(null), []);
  const runControlUnknown = currentRunIdentityRef.current === undefined;
  const renderSessionRow = useCallback((session, options = {}) => <SessionNavigationRow key={session.id} workspaceId={workspace.id} session={session} actions={rowActions} active={session.id === sessionId} running={session.id === sessionId ? hasActiveAgentRun || runControlUnknown && session.hasActiveAgentRun : session.hasActiveAgentRun} onSelect={selectNavigationSession} {...options} />,
    [workspace.id, rowActions, sessionId, hasActiveAgentRun, runControlUnknown, selectNavigationSession]);
  const sidebarSessions = useMemo(() => ({ sessions, projects, groupedSessions, renderSessionRow, onCreateProject: createNavigationProject, ready: navigationLoaded }),
    [sessions, projects, groupedSessions, renderSessionRow, createNavigationProject, navigationLoaded]);
  const recentNavigation = useMemo(() => navigationError ? { error: true } : undefined, [navigationError]);

  async function answerQuestion(question, answer) {
    const scope = requestScopeRef.current;
    const isCurrent = () => scope.active && requestScopeRef.current === scope
      && activeSessionIdRef.current === question.sessionId && currentRunIdentityRef.current === question.agentRunId
      && !terminalRunIdsRef.current.has(question.agentRunId);
    const receipt = await questionOperations.submit(
      `/api/sessions/${encodeURIComponent(question.sessionId)}/agent-runs/${encodeURIComponent(question.agentRunId)}/questions/${encodeURIComponent(question.questionId)}/responses`,
      { continuationId: question.continuationId, ...answer }, [], [], false, { shouldDispatch: isCurrent },
    );
    if (receipt.sessionId !== question.sessionId || receipt.agentRunId !== question.agentRunId) throw new Error("question_response_binding_invalid");
    questionOperations.complete(receipt.operationId);
    answeredQuestionIdsRef.current.add(question.continuationId);
    if (!isCurrent()) return;
    setPendingQuestion(value => value?.continuationId === question.continuationId ? null : value);
  }

  return (
    <main
      ref={workbenchRef}
      className={`workspaceWorkbench ${isSidebarOpen ? "withSidebar" : "withoutSidebar"}${catalogContent ? " isBrowsing" : ""}${search.open ? " isSearching" : ""}`}
      style={{ "--workspace-sidebar-progress": initialSidebarProgress, "--browser-panel-width": browserPanelWidthPx == null ? undefined : `${browserPanelWidthPx}px` }}
    >
      {!compactLayout ? <button
        className="workspaceSidebarToggle shAvatarToggle"
        type="button"
        onClick={() => setWorkbenchSidebar(!isSidebarOpen)}
        aria-label={t(isSidebarOpen ? "shellSidebar.hideSidebar" : "appRoute.showSidebar")}
        aria-expanded={isSidebarOpen}
        title={t(isSidebarOpen ? "shellSidebar.hideSidebar" : "appRoute.showSidebar")}
        disabled={search.open}
      ><PanelLeft aria-hidden="true" /></button> : null}

      <SidebarContainer className={compactLayout ? "workspaceDrawer" : "workspaceSidebarSlot"} ref={sidebarRef}
        inert={!compactLayout && !isSidebarOpen}
        aria-label={compactLayout ? t("shellSidebar.conversationNavigation") : undefined}
        onCancel={compactLayout ? () => setDrawerFor(null) : undefined}
        onClick={compactLayout ? event => { if (event.target === event.currentTarget) setDrawerFor(null); } : undefined}>
        <ShellSidebar
          workspace={workspace}
          navigationVisible={compactLayout ? drawerOpen : isSidebarOpen}
          menuContainer={compactLayout ? sidebarRef : undefined}
          search={compactLayout ? undefined : search}
          onCollapse={compactLayout ? collapseNavigation : undefined}
          onStartNewChat={startNavigationChat}
          recentNavigation={recentNavigation}
          sessionProps={sidebarSessions}
        />
      </SidebarContainer>

      <header className="workspaceTopbar" inert={search.open}>
        <div className="workspaceTopbarStart">
          {compactLayout ? <button
            className="workspacePanelToggle"
            type="button"
            onClick={() => setDrawerFor(navigationKey)}
            aria-label={t("appRoute.showSidebar")}
            aria-expanded={drawerOpen}
            title={t("appRoute.showSidebar")}
          >
            <PanelLeft aria-hidden="true" />
          </button> : null}
          {!catalogContent && activeSession ? <ConversationTitle key={activeSession.id} session={activeSession} actions={sessionActions} running={hasActiveAgentRun} />
            : !catalogContent && requestedSessionId && loadingHistory ? <span className="workspaceConversationTitle" role="status">{t("agentChat.loading")}</span> : null}
          {compactLayout ? <button className="workspacePanelToggle" ref={search.triggerRef} type="button" onClick={search.toggle}
            aria-label={t("searchOverlay.searchConversationsAndNotes")} title={t("shellSidebar.searchCtrlK")}><Search aria-hidden="true" /></button> : null}
        </div>
        {visible && contextPanel.mode === "filePreview" ? <WorkspacePreviewToggle
          ref={previewToggleRef} panelId={previewPanelId} open={filePreviewOpen} onToggle={toggleFilePreview}
        /> : null}
      </header>

      <section className="workspaceChatColumn" inert={search.open || Boolean(catalogContent)}>
        <div className={`workspaceConversationPlane ${isHome ? "isEmpty" : ""}`}>
          {searchEventId ? <div className="workspaceToolOutputStatus" role={focusedTranscript.error ? "alert" : "status"}>
            <span>{t(focusedTranscript.error ? "searchOverlay.targetUnavailable" : "searchOverlay.viewingMatch")}</span>
            {focusedTranscript.error ? <button type="button" onClick={focusedTranscript.retry}>{t("agentChat.retry")}</button> : null}
            <button type="button" onClick={returnToLatest}>{t("virtualAgentRunList.jumpToLatest")}</button>
          </div> : null}
          <TranscriptBlockList
            key={searchEventId ? `search:${sessionId}:${searchEventId}` : "live"}
            ref={transcriptRef}
            store={searchEventId ? focusedTranscript.store : transcriptStore}
            searchTarget={searchEventId ? { blockId: focusedTranscript.blockId, query: searchHighlight, onReturnLatest: returnToLatest } : undefined}
          onReadingLatestChange={(targetSessionId, following) => { if (targetSessionId === activeSessionIdRef.current) setReadingLatest(following); }}
            toolOperations={toolOperations}
            sessionId={sessionId || null}
            loadingHistory={searchEventId ? focusedTranscript.loading : loadingHistory}
            loadingOlderHistory={searchEventId ? focusedTranscript.loadingOlder : loadingOlderHistory}
            onLoadOlderHistory={searchEventId ? focusedTranscript.loadOlder : loadOlderHistory}
            olderHistoryError={searchEventId ? focusedTranscript.olderError : olderHistoryError}
            emptyState={isHome ? <HomePlane /> : <ConversationReadFeedback {...readFeedbackProps} placement="empty" />}
            onShowCitation={showCitation}
            onShowArtifact={showArtifact}
            pendingUserMessage={searchEventId ? null : visiblePendingMessage}
            submissionRejected={!searchEventId && rejectedSubmissionScopes.has(requestScopeKey)}
            onReleaseLoadedHistory={searchEventId ? undefined : releaseLoadedTranscriptHistory}
            running={!searchEventId && (hasActiveAgentRun || Boolean(sending && visiblePendingMessage && !["failed", "cancelled", "completed"].includes(visiblePendingMessage.status)))}
            activity={searchEventId ? undefined : controlApplies && controlSnapshot.stopRequested && controlPending ? "stopping" : workActivity?.sessionId === sessionId
              && workActivity.agentRunId === activeAgentRunId ? workActivity.activity : undefined}
            startedAtMs={!searchEventId && workTime?.sessionId === sessionId ? workTime.startedAtMs : undefined}
            completedAtMs={!searchEventId && workTime?.sessionId === sessionId ? workTime.completedAtMs : undefined}
          />

          <div className="workspaceComposerDock">
          {(!isSidebarOpen || compactLayout && !drawerOpen) && sessionActions.actionError?.sessionId === sessionId && deleteConfirmationSessionId !== sessionId
            ? <div className="workspaceConversationFeedback workspaceToolOutputStatus isError" role="alert">{sessionActions.actionError.message}</div> : null}
          <ConversationReadFeedback {...readFeedbackProps} placement="content" />
          {pendingQuestion?.sessionId === sessionId && pendingQuestion.agentRunId === activeAgentRunId ? <SessionQuestionCard
            key={pendingQuestion.continuationId}
            question={pendingQuestion} onAnswer={answer => answerQuestion(pendingQuestion, answer)}
          /> : null}
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
            feedback={composerError || assetsUnavailable ? <div className="workspaceToolOutputStatus isError" role="alert">
              <span>{composerError || t("appRoute.sessionAttachmentsUnavailable")}</span>
              {composerError && operations.pending() && !acceptedReadFailed ? <button type="button" disabled={sending} onClick={() => void checkPendingOperation()}>{t("operations.check")}</button> : null}
              {composerError && operationReviewLink && operationReviewLink.operationId === operations.pending()?.operationId ? <a
                href={operationReviewLink.href} target={operationReviewLink.target} rel={operationReviewLink.rel}
                onClick={() => void consumeOperationReview(operationReviewLink.operationId)}
              >{t("operations.reviewSeparate")}</a> : null}
            </div> : null}
            onDraftChange={setDraft}
            enterStartsNewLine={enterStartsNewLine}
            workspaceAvailable={Boolean(workspace)}
            sending={sending}
            loadingHistory={loadingHistory}
            hasActiveAgentRun={hasActiveAgentRun || controlPending}
            controlBusy={controlApplies && ["preparing", "submitting", "recovering", "cancelling"].includes(controlSnapshot.phase)}
            uploadingAttachment={uploadingAttachment}
            onUploadAttachment={uploadAttachment}
            sessionId={sessionId}
            contextRevision={contextVersion?.sessionId === sessionId ? contextVersion.revision : "0"}
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
          </div>
          {isHome ? <div className="shHomeFollowups">
            <HomeQuickActions onQuickAction={handleQuickAction} />
          </div> : null}
        </div>
      </section>

      {catalogContent ? <section className="workspaceCatalog" inert={search.open}>
        {catalogContent}
      </section> : null}
      {search.open ? <WorkspaceSearch workspaceId={workspace.id} onClose={search.close} inputRef={search.inputRef} /> : null}
      {visible ? <WorkspaceContextPanel
        id={previewPanelId}
        panel={contextPanel}
        open={filePreviewOpen}
        browserWidthPx={browserPanelWidthPx}
        onBrowserWidthChange={updateBrowserPanelWidth}
        onClose={closeFilePreview}
      /> : null}
      <ConfirmDialog
        open={(visible || Boolean(catalogContent)) && Boolean(deleteConfirmationSessionId)}
        title={t("appRoute.deleteThisConversation")}
        message={sessionActions.actionError?.sessionId === deleteConfirmationSessionId ? sessionActions.actionError.message : undefined}
        busy={Boolean(deletingSessionId)}
        onCancel={() => setDeleteConfirmationSessionId("")}
        onConfirm={() => void deleteSession(deleteConfirmationSessionId)}
      />
      {visible && attachmentPreview ? (
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
